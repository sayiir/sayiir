use proc_macro2::Span;
use quote::quote;
use syn::{FnArg, GenericArgument, Ident, Pat, PatIdent, PatType, Stmt, Token, Type};

// Helper to check if a type is already a wrapper (likely implements TaskInput/TaskOutput)
// This checks for:
// 1. Json<T> - our default wrapper
// 2. Any generic type with a single type parameter (likely a custom wrapper)
pub(crate) fn is_already_wrapped(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                // Check if it's Json<T>
                if segment.ident == "Json" {
                    return true;
                }
                // Check if it's a generic type with type arguments
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    // If it has exactly one type argument, it's likely a wrapper
                    let type_args: Vec<_> = args
                        .args
                        .iter()
                        .filter_map(|arg| {
                            if let GenericArgument::Type(_) = arg {
                                Some(())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if type_args.len() == 1 {
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

// Helper to wrap a type T into Json<T>
pub(crate) fn wrap_in_json(ty: &Type) -> Type {
    syn::parse2(quote! {
        workflow_core::serialization::json::Json<#ty>
    })
    .expect("Failed to parse Json<T> type")
}

// Helper to create a compile error
pub(crate) fn compile_error<T: quote::ToTokens>(
    tokens: T,
    message: &str,
) -> proc_macro::TokenStream {
    syn::Error::new_spanned(tokens, message)
        .to_compile_error()
        .into()
}

// Extract input parameter from function signature
pub(crate) fn extract_input_param(
    inputs: &syn::punctuated::Punctuated<FnArg, Token![,]>,
) -> Result<(Ident, Type), proc_macro::TokenStream> {
    match inputs.first() {
        Some(FnArg::Typed(PatType { pat, ty, .. })) => {
            let pat_ident = match pat.as_ref() {
                Pat::Ident(ident) => &ident.ident,
                _ => {
                    return Err(compile_error(
                        pat,
                        "task function must have a single named parameter",
                    ));
                }
            };
            Ok((pat_ident.clone(), ty.as_ref().clone()))
        }
        _ => Err(compile_error(
            inputs,
            "task function must have exactly one input parameter",
        )),
    }
}

// Extract Result<O, E> types from return type
pub(crate) fn extract_result_types(
    return_type: &Type,
) -> Result<(Type, Type), proc_macro::TokenStream> {
    match return_type {
        Type::Path(type_path) => {
            let segment = type_path.path.segments.last().ok_or_else(|| {
                compile_error(return_type, "task function must return Result<O, E>")
            })?;

            if segment.ident != "Result" {
                return Err(compile_error(
                    return_type,
                    "task function must return Result<O, E>",
                ));
            }

            let args = match &segment.arguments {
                syn::PathArguments::AngleBracketed(args) => args,
                _ => {
                    return Err(compile_error(
                        return_type,
                        "Result must have generic arguments",
                    ));
                }
            };

            if args.args.len() < 2 {
                return Err(compile_error(
                    return_type,
                    "Result must have both output and error types",
                ));
            }

            let output_ty = match &args.args[0] {
                GenericArgument::Type(ty) => ty.clone(),
                _ => {
                    return Err(compile_error(
                        return_type,
                        "Result output type must be a type",
                    ));
                }
            };

            let error_ty = match &args.args[1] {
                GenericArgument::Type(ty) => ty.clone(),
                _ => {
                    return Err(compile_error(
                        return_type,
                        "Result error type must be a type",
                    ));
                }
            };

            Ok((output_ty, error_ty))
        }
        _ => Err(compile_error(
            return_type,
            "task function must return Result<O, E>",
        )),
    }
}

// Create unwrap statement for input
pub(crate) fn create_unwrap_stmt(input_ident: &Ident) -> Stmt {
    syn::parse2(quote! {
        let #input_ident = #input_ident.as_ref().clone();
    })
    .expect("Failed to parse unwrap statement")
}

// Wrap return expression in Json::new
pub(crate) fn wrap_return_expr(ret_expr: &syn::Expr) -> Stmt {
    syn::parse2(quote! {
        return Ok(workflow_core::serialization::json::Json::new(#ret_expr));
    })
    .expect("Failed to parse wrapped return")
}

// Transform statements to wrap return expressions
pub(crate) fn transform_returns(stmts: &[Stmt]) -> Vec<Stmt> {
    let mut new_stmts = Vec::new();
    for stmt in stmts {
        if let Stmt::Expr(expr, _) = stmt
            && let syn::Expr::Return(ret) = expr
                && let Some(ret_expr) = &ret.expr {
                    new_stmts.push(wrap_return_expr(ret_expr));
                    continue;
                }
        new_stmts.push(stmt.clone());
    }
    new_stmts
}

// Create function input parameter
pub(crate) fn create_input_param(input_ident: &Ident, ty: Type) -> PatType {
    PatType {
        attrs: Vec::new(),
        pat: Box::new(Pat::Ident(PatIdent {
            attrs: Vec::new(),
            by_ref: None,
            mutability: None,
            ident: input_ident.clone(),
            subpat: None,
        })),
        colon_token: Token![:](Span::call_site()),
        ty: Box::new(ty),
    }
}
