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
    let registry = &def.registry;

    let step_chain = codegen::gen_step_chain(&def.steps)?;

    Ok(quote::quote! {
        (|| -> ::std::result::Result<_, ::sayiir_core::error::WorkflowError> {
            let __codec = ::std::sync::Arc::new(#codec);
            let __ctx = ::sayiir_core::context::WorkflowContext::new(
                #id,
                __codec.clone(),
                ::std::sync::Arc::new(()),
            );
            let __wf = ::sayiir_core::builder::WorkflowBuilder::new(__ctx)
                .with_existing_registry(#registry)
                #step_chain
                .build()?;
            ::std::result::Result::Ok(__wf)
        })()
    })
}
