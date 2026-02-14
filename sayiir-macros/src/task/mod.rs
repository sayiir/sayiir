pub mod codegen;
pub mod duration;
pub mod parse;

use darling::ast::NestedMeta;
use darling::FromMeta;
use proc_macro2::TokenStream;
use syn::ItemFn;

use self::parse::{ParsedTask, TaskAttrs};

/// Entry point: parse attributes + function, validate, generate code.
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(attr)?;
    let attrs = TaskAttrs::from_list(&attr_args)
        .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e.to_string()))?;
    let item_fn: ItemFn = syn::parse2(item)?;

    let parsed = ParsedTask::parse(attrs, item_fn)?;
    Ok(codegen::generate(&parsed))
}
