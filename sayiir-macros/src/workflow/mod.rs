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

    // Auto-register all #[task] struct refs into the registry.
    let task_refs = codegen::collect_task_refs(&def.steps);
    let register_stmts = task_refs.iter().map(|name| {
        quote::quote! {
            #name::register(&mut __registry, __codec.clone(), #name::new());
        }
    });

    // Register route key function aliases (e.g. "route::key_fn").
    let aliases = codegen::collect_route_aliases(&def.steps);
    let alias_stmts = aliases.iter().map(|(name, alias_id)| {
        quote::quote! {
            __registry.register_with_metadata(
                #alias_id,
                __codec.clone(),
                #name::new(),
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
