pub mod ast;
pub mod codegen;
pub mod parse;

use proc_macro2::TokenStream;

use self::ast::WorkflowDef;

/// Entry point: parse workflow! invocation and generate builder code.
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let def: WorkflowDef = syn::parse2(input)?;
    generate(&def)
}

/// How `workflow!`-referenced task structs should be instantiated.
///
/// `FromDeps` mode also requires emitting a verify/conflict-check preamble —
/// see [`gen_check_block`].
#[derive(Clone, Copy)]
enum BuildMode {
    New,
    FromDeps,
}

impl BuildMode {
    /// The instance-construction expression for a single task reference.
    /// In `FromDeps` mode, the generated code references the `__deps` local
    /// materialized by [`gen_check_block`].
    fn instance(self, name: &syn::Ident) -> TokenStream {
        match self {
            Self::New => quote::quote! { #name::new() },
            Self::FromDeps => quote::quote! { #name::from_deps(__deps) },
        }
    }
}

/// Emit the build-check preamble — empty unless the workflow has a `deps:`
/// field. Two error classes aggregate into one `BuildErrors`:
///
/// 1. **`MissingDep`** — any `#[inject]` type absent from `__deps`. Avoids the
///    runtime panic that `from_deps` would otherwise produce.
/// 2. **`RegistryDepsConflict`** — the user passed a pre-built `registry:`
///    that already contains a task this macro would otherwise re-register via
///    `from_deps`. Surfacing it forces an explicit choice rather than relying
///    on silent registry dedup.
///
/// The emitted code materializes the `__deps` local used by every
/// `from_deps(__deps)` call site downstream.
fn gen_check_block(deps_expr: Option<&syn::Expr>, names: &[&syn::Ident]) -> TokenStream {
    let Some(deps_expr) = deps_expr else {
        return TokenStream::new();
    };
    let stmts = names.iter().map(|name| {
        quote::quote! {
            for __m in #name::verify_deps(__deps) {
                __build_errors.push(::sayiir_core::error::BuildError::MissingDep {
                    task_id: #name::task_id(),
                    type_name: __m.type_name,
                });
            }
            if __registry.contains(#name::task_id()) {
                __build_errors.push(::sayiir_core::error::BuildError::RegistryDepsConflict {
                    task_id: #name::task_id(),
                });
            }
        }
    });
    quote::quote! {
        let __deps: &::sayiir_core::deps::Deps = #deps_expr;
        let mut __build_errors = ::sayiir_core::error::BuildErrors::new();
        #(#stmts)*
        if !__build_errors.is_empty() {
            return ::std::result::Result::Err(__build_errors);
        }
    }
}

fn generate(def: &WorkflowDef) -> syn::Result<TokenStream> {
    let id = &def.id;
    let codec = &def.codec;

    let registry_expr = match &def.registry {
        Some(expr) => quote::quote! { #expr },
        None => quote::quote! { ::sayiir_core::registry::TaskRegistry::new() },
    };

    let metadata_expr = match &def.metadata {
        Some(expr) => quote::quote! { ::std::sync::Arc::new(#expr) },
        None => quote::quote! { ::std::sync::Arc::new(()) },
    };

    let step_chain = codegen::gen_step_chain(&def.steps)?;

    let task_refs = codegen::collect_task_refs(&def.steps);
    let aliases = codegen::collect_route_aliases(&def.steps);

    let mode = if def.deps.is_some() {
        BuildMode::FromDeps
    } else {
        BuildMode::New
    };

    let check_block = {
        let mut all: Vec<&syn::Ident> = task_refs.to_vec();
        all.extend(aliases.iter().map(|(name, _)| *name));
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        all.retain(|n| seen.insert(n.to_string()));
        gen_check_block(def.deps.as_ref(), &all)
    };

    let register_stmts = task_refs.iter().map(|name| {
        let instance = mode.instance(name);
        quote::quote! {
            #name::register(&mut __registry, __codec.clone(), #instance);
        }
    });

    let alias_stmts = aliases.iter().map(|(name, alias_id)| {
        let instance = mode.instance(name);
        quote::quote! {
            __registry.register_with_metadata(
                #alias_id,
                __codec.clone(),
                #instance,
                #name::metadata(),
            );
        }
    });

    // Compile-time exhaustiveness checks for typed route nodes.
    let exhaustiveness_checks = codegen::collect_exhaustiveness_checks(&def.steps);

    Ok(quote::quote! {
        (|| -> ::std::result::Result<_, ::sayiir_core::error::BuildErrors> {
            #(#exhaustiveness_checks)*
            let __codec = ::std::sync::Arc::new(#codec);
            let mut __registry = #registry_expr;
            #check_block
            #(#register_stmts)*
            #(#alias_stmts)*
            let __ctx = ::sayiir_core::context::WorkflowContext::new(
                #id,
                __codec.clone(),
                #metadata_expr,
            );
            let __wf = ::sayiir_core::builder::WorkflowBuilder::new(__ctx)
                .with_existing_registry(__registry)
                #step_chain
                .build()?;
            ::std::result::Result::Ok(__wf)
        })()
    })
}
