//! Procedural macros for the workflow system.

use darling::FromMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::{Block, FnArg, ItemFn, Meta, ReturnType, parse_macro_input};
mod utils;

/// Arguments for the `task` macro.
#[derive(Debug, FromMeta, Default)]
#[darling(default)]
struct TaskArgs {
    /// Use custom serialization format instead of automatic JSON wrapping.
    /// When set to `true`, types are used as-is without wrapping in `Json<T>`.
    /// This allows you to use custom wrappers like `Bincode<T>`, `Postcard<T>`, etc.
    #[darling(default)]
    custom_serialization: bool,
}

/// Macro to convert an async function into a CoreTask.
///
/// This macro automatically wraps input/output types in `Json<T>` to provide
/// clean function signatures. By default, it detects if types are already wrappers
/// (like `Json<T>`, `Bincode<T>`, or custom wrappers) and won't double-wrap them.
///
/// **Note:** This macro is re-exported from [`workflow_core::task`] with full documentation
/// and examples. Import it from `workflow_core` instead:
///
/// ```rust,ignore
/// use workflow_core::task;  // Preferred
/// ```
#[proc_macro_attribute]
pub fn task(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);

    // Parse attribute arguments
    let task_args = if args.is_empty() {
        TaskArgs::default()
    } else {
        let meta = parse_macro_input!(args as Meta);
        // Handle bare flag syntax for custom_serialization
        if let Meta::Path(path) = &meta {
            if path.is_ident("custom_serialization") {
                TaskArgs {
                    custom_serialization: true,
                }
            } else {
                return utils::compile_error(
                    path,
                    "task macro only accepts 'custom_serialization' as an argument",
                );
            }
        } else {
            match TaskArgs::from_meta(&meta) {
                Ok(args) => args,
                Err(e) => return e.write_errors().into(),
            }
        }
    };

    let auto_wrap = !task_args.custom_serialization;

    // Extract function details
    let fn_vis = &input_fn.vis;
    let fn_sig = &input_fn.sig;
    let fn_attrs = &input_fn.attrs;
    let fn_block = &input_fn.block;

    if fn_sig.asyncness.is_none() {
        return utils::compile_error(fn_sig, "task macro can only be applied to async functions");
    }

    // Extract input parameter
    let (input_ident, input_ty) = match utils::extract_input_param(&fn_sig.inputs) {
        Ok(val) => val,
        Err(e) => return e,
    };

    // Extract return type
    let return_type = match &fn_sig.output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        _ => {
            return utils::compile_error(&fn_sig.output, "task function must return a Result type");
        }
    };

    // Extract inner types from Result<O, E>
    let (output_type, error_type) = match utils::extract_result_types(return_type) {
        Ok(val) => val,
        Err(e) => return e,
    };

    // Determine if types need wrapping
    let needs_input_wrap = auto_wrap && !utils::is_already_wrapped(&input_ty);
    let needs_output_wrap = auto_wrap && !utils::is_already_wrapped(&output_type);

    // Transform types
    let wrapped_input_ty = if needs_input_wrap {
        utils::wrap_in_json(&input_ty)
    } else {
        input_ty.clone()
    };

    let wrapped_output_ty = if needs_output_wrap {
        utils::wrap_in_json(&output_type)
    } else {
        output_type.clone()
    };

    let transformed_body =
        transform_body(fn_block, &input_ident, needs_input_wrap, needs_output_wrap);

    // Build new function signature
    let mut new_sig = fn_sig.clone();
    new_sig.inputs = {
        let mut inputs = syn::punctuated::Punctuated::new();
        inputs.push(FnArg::Typed(utils::create_input_param(
            &input_ident,
            wrapped_input_ty,
        )));
        inputs
    };
    new_sig.output = ReturnType::Type(
        syn::Token![->](proc_macro2::Span::call_site()),
        Box::new(
            syn::parse2(quote! {
                std::result::Result<#wrapped_output_ty, #error_type>
            })
            .expect("Failed to parse return type"),
        ),
    );

    TokenStream::from(quote! {
        #(#fn_attrs)*
        #fn_vis #new_sig {
            #transformed_body
        }
    })
}

// Transform function body based on wrapping needs
fn transform_body(
    fn_block: &Block,
    input_ident: &syn::Ident,
    needs_input_wrap: bool,
    needs_output_wrap: bool,
) -> Block {
    match (needs_input_wrap, needs_output_wrap) {
        (true, true) => {
            let mut stmts = vec![utils::create_unwrap_stmt(input_ident)];
            stmts.extend(utils::transform_returns(&fn_block.stmts));
            Block {
                brace_token: fn_block.brace_token,
                stmts,
            }
        }
        (true, false) => {
            let mut stmts = vec![utils::create_unwrap_stmt(input_ident)];
            stmts.extend(fn_block.stmts.iter().cloned());
            Block {
                brace_token: fn_block.brace_token,
                stmts,
            }
        }
        (false, true) => Block {
            brace_token: fn_block.brace_token,
            stmts: utils::transform_returns(&fn_block.stmts),
        },
        (false, false) => {
            // Neither needs wrapping
            Block {
                brace_token: fn_block.brace_token,
                stmts: fn_block.stmts.clone(),
            }
        }
    }
}
