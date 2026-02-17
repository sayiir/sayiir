use darling::FromMeta;
use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{FnArg, ItemFn, Pat, PatIdent, PatType, ReturnType, Type};

use crate::task::duration::DurationLit;
use crate::util::err;

/// Parsed task attributes from `#[task(...)]`.
#[derive(Debug, FromMeta)]
pub struct TaskAttrs {
    /// Custom task ID (default: function name).
    #[darling(default)]
    pub id: Option<String>,

    /// Human-readable display name.
    #[darling(default)]
    pub display_name: Option<String>,

    /// Task description.
    #[darling(default)]
    pub description: Option<String>,

    /// Timeout duration, e.g. `"30s"`.
    #[darling(default)]
    pub timeout: Option<DurationLit>,

    /// Max retry count.
    #[darling(default)]
    pub retries: Option<u32>,

    /// Initial retry backoff, e.g. `"100ms"`.
    #[darling(default)]
    pub backoff: Option<DurationLit>,

    /// Backoff multiplier (default: 2.0).
    #[darling(default)]
    pub backoff_multiplier: Option<f32>,

    /// Categorization tags.
    #[darling(default, multiple)]
    pub tags: Vec<String>,
}

/// A parameter classified as either the task input or an injected dependency.
#[derive(Debug)]
pub struct Param {
    pub ident: syn::Ident,
    pub ty: Box<Type>,
}

/// How the task function returns its output.
#[derive(Debug)]
pub enum ReturnKind {
    /// `Result<T, BoxError>` or `Result<T, E>` — the function is fallible.
    /// The error type will be converted via `Into<BoxError>`.
    Fallible,
    /// Plain `T` — the function is infallible; output is wrapped in `Ok(...)`.
    Infallible,
}

/// The fully parsed task definition.
#[derive(Debug)]
pub struct ParsedTask {
    pub attrs: TaskAttrs,
    pub fn_name: syn::Ident,
    pub task_id: String,
    pub vis: syn::Visibility,
    pub input_param: Param,
    pub inject_params: Vec<Param>,
    pub output_type: Box<Type>,
    pub return_kind: ReturnKind,
    pub original_fn: ItemFn,
}

impl ParsedTask {
    pub fn parse(attrs: TaskAttrs, mut item_fn: ItemFn) -> syn::Result<Self> {
        // Validation 1: must be async
        if item_fn.sig.asyncness.is_none() {
            return Err(err(
                item_fn.sig.fn_token.span,
                "#[task] function must be async",
            ));
        }

        // Validation 2: no self parameter
        if let Some(FnArg::Receiver(r)) = item_fn.sig.inputs.first() {
            return Err(err(
                r.self_token.span,
                "#[task] function cannot have `self`",
            ));
        }

        // Classify parameters
        let mut input_param: Option<Param> = None;
        let mut inject_params: Vec<Param> = Vec::new();

        for arg in &mut item_fn.sig.inputs {
            let FnArg::Typed(pat_type) = arg else {
                continue;
            };

            let is_inject = has_inject_attr(pat_type);
            // Remove #[inject] from the AST so it doesn't appear in the preserved fn
            strip_inject_attr(pat_type);

            let ident = extract_ident(&pat_type.pat)?;
            let param = Param {
                ident,
                ty: pat_type.ty.clone(),
            };

            if is_inject {
                inject_params.push(param);
            } else if input_param.is_some() {
                return Err(err(
                    pat_type.pat.span(),
                    "#[task] function must have exactly one non-#[inject] parameter (the task input)",
                ));
            } else {
                input_param = Some(param);
            }
        }

        let input_param = input_param.ok_or_else(|| {
            err(
                item_fn.sig.paren_token.span.join(),
                "#[task] function must have exactly one non-#[inject] parameter (the task input)",
            )
        })?;

        // Validation 3: extract return type — Result<T, E> or plain T
        let (output_type, return_kind) =
            extract_output_type(&item_fn.sig.output, item_fn.sig.fn_token.span)?;

        let fn_name = item_fn.sig.ident.clone();
        let task_id = attrs.id.clone().unwrap_or_else(|| fn_name.to_string());
        let vis = item_fn.vis.clone();

        Ok(Self {
            attrs,
            fn_name,
            task_id,
            vis,
            input_param,
            inject_params,
            output_type,
            return_kind,
            original_fn: item_fn,
        })
    }
}

/// Check if a `PatType` has `#[inject]` attribute.
fn has_inject_attr(pat_type: &PatType) -> bool {
    pat_type.attrs.iter().any(|a| a.path().is_ident("inject"))
}

/// Remove `#[inject]` attributes from a `PatType`.
fn strip_inject_attr(pat_type: &mut PatType) {
    pat_type.attrs.retain(|a| !a.path().is_ident("inject"));
}

/// Extract the identifier from a pattern.
fn extract_ident(pat: &Pat) -> syn::Result<syn::Ident> {
    match pat {
        Pat::Ident(PatIdent { ident, .. }) => Ok(ident.clone()),
        _ => Err(err(
            syn::spanned::Spanned::span(pat),
            "#[task] parameters must be simple identifiers (e.g. `order: Order`)",
        )),
    }
}

/// Extract the output type and return kind from the function's return type.
///
/// - `Result<T, E>` → `(T, Fallible)` — error converted via `Into<BoxError>`
/// - `T`            → `(T, Infallible)` — wrapped in `Ok(...)`
/// - No return type → error
fn extract_output_type(ret: &ReturnType, fn_span: Span) -> syn::Result<(Box<Type>, ReturnKind)> {
    let ty = match ret {
        ReturnType::Default => {
            return Err(err(
                fn_span,
                "#[task] function must have a return type (e.g. `-> Result<T, E>` or `-> T`)",
            ));
        }
        ReturnType::Type(_, ty) => ty,
    };

    // Try to extract Result<T, E>
    if let Type::Path(type_path) = ty.as_ref()
        && let Some(segment) = type_path.path.segments.last()
        && segment.ident == "Result"
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(ok_ty)) = args.args.first()
    {
        return Ok((Box::new(ok_ty.clone()), ReturnKind::Fallible));
    }

    // Not a Result — treat the whole type as infallible output
    Ok((ty.clone(), ReturnKind::Infallible))
}
