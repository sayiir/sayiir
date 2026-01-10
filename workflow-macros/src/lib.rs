//! Procedural macros for the workflow system.

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Macro to convert an async function into a CoreTask.
///
/// This macro provides a convenient way to mark async functions as tasks
/// that can be used with the workflow runtime.
#[proc_macro_attribute]
pub fn task(_args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);

    // For now, just return the function as-is
    // This can be extended to add additional functionality
    TokenStream::from(quote! {
        #input_fn
    })
}
