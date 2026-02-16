use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::task::parse::{ParsedTask, ReturnKind};
use crate::util::snake_to_pascal;

/// Generate all output code from a parsed task definition.
pub fn generate(parsed: &ParsedTask) -> TokenStream {
    let name = format_ident!("{}", snake_to_pascal(&parsed.fn_name.to_string()));

    let struct_def = gen_struct(parsed, &name);
    let impl_block = gen_impl(parsed, &name);
    let core_task_impl = gen_core_task(parsed, &name);
    let original_fn = &parsed.original_fn;

    quote! {
        #struct_def
        #impl_block
        #core_task_impl
        #original_fn
    }
}

/// Generate the struct definition.
fn gen_struct(parsed: &ParsedTask, name: &syn::Ident) -> TokenStream {
    let vis = &parsed.vis;

    if parsed.inject_params.is_empty() {
        quote! { #vis struct #name; }
    } else {
        let fields = parsed.inject_params.iter().map(|p| {
            let ident = &p.ident;
            let ty = &p.ty;
            quote! { #ident: #ty }
        });

        quote! {
            #vis struct #name {
                #(#fields,)*
            }
        }
    }
}

/// Generate the unified `impl` block with `new()`, `task_id()`, `metadata()`, and `register()`.
fn gen_impl(parsed: &ParsedTask, name: &syn::Ident) -> TokenStream {
    let task_id = &parsed.task_id;
    let input_ty = &parsed.input_param.ty;
    let output_ty = &parsed.output_type;
    let metadata_body = gen_metadata_body(parsed);

    let new_fn = if parsed.inject_params.is_empty() {
        quote! { pub fn new() -> Self { Self } }
    } else {
        let params = parsed.inject_params.iter().map(|p| {
            let ident = &p.ident;
            let ty = &p.ty;
            quote! { #ident: #ty }
        });
        let field_inits = parsed.inject_params.iter().map(|p| &p.ident);

        quote! {
            pub fn new(#(#params),*) -> Self {
                Self { #(#field_inits,)* }
            }
        }
    };

    quote! {
        impl #name {
            #new_fn

            pub const fn task_id() -> &'static str { #task_id }

            pub fn metadata() -> ::sayiir_core::task::TaskMetadata {
                #metadata_body
            }

            /// Register this task into a `TaskRegistry` with the given codec.
            pub fn register<C>(
                registry: &mut ::sayiir_core::registry::TaskRegistry,
                codec: ::std::sync::Arc<C>,
                task: Self,
            )
            where
                C: ::sayiir_core::codec::Codec
                    + ::sayiir_core::codec::sealed::DecodeValue<#input_ty>
                    + ::sayiir_core::codec::sealed::EncodeValue<#output_ty>
                    + 'static,
            {
                registry.register_with_metadata(#task_id, codec, task, Self::metadata());
            }
        }
    }
}

/// Generate the body of the `metadata()` method.
fn gen_metadata_body(parsed: &ParsedTask) -> TokenStream {
    let attrs = &parsed.attrs;

    let display_name = attrs.display_name.as_ref().map(|s| {
        quote! { display_name: Some(#s.to_string()), }
    });

    let description = attrs.description.as_ref().map(|s| {
        quote! { description: Some(#s.to_string()), }
    });

    let timeout = attrs.timeout.as_ref().map(|d| {
        let dur = d.to_tokens();
        quote! { timeout: Some(#dur), }
    });

    let retries = if let Some(max) = attrs.retries {
        let backoff = attrs
            .backoff
            .as_ref()
            .map(|d| d.to_tokens())
            .unwrap_or_else(|| quote! { ::std::time::Duration::from_millis(1000) });
        let multiplier = attrs.backoff_multiplier.unwrap_or(2.0_f32);

        Some(quote! {
            retries: Some(::sayiir_core::task::RetryPolicy {
                max_retries: #max,
                initial_delay: #backoff,
                backoff_multiplier: #multiplier,
                max_delay: None,
            }),
        })
    } else {
        None
    };

    let tags = if attrs.tags.is_empty() {
        None
    } else {
        let tag_strs = &attrs.tags;
        Some(quote! { tags: vec![#(#tag_strs.to_string()),*], })
    };

    quote! {
        ::sayiir_core::task::TaskMetadata {
            #display_name
            #description
            #timeout
            #retries
            #tags
            ..::std::default::Default::default()
        }
    }
}

/// Generate the `CoreTask` impl.
fn gen_core_task(parsed: &ParsedTask, name: &syn::Ident) -> TokenStream {
    let input_ty = &parsed.input_param.ty;
    let input_ident = &parsed.input_param.ident;
    let output_ty = &parsed.output_type;

    let clone_stmts = parsed.inject_params.iter().map(|p| {
        let ident = &p.ident;
        quote! { let #ident = self.#ident.clone(); }
    });

    let fn_name = &parsed.fn_name;
    let all_args = std::iter::once(&parsed.input_param)
        .chain(parsed.inject_params.iter())
        .map(|p| &p.ident);

    let call_expr = match parsed.return_kind {
        ReturnKind::Fallible => quote! {
            #fn_name(#(#all_args),*).await.map_err(::std::convert::Into::into)
        },
        ReturnKind::Infallible => quote! {
            Ok(#fn_name(#(#all_args),*).await)
        },
    };

    quote! {
        impl ::sayiir_core::task::CoreTask for #name {
            type Input = #input_ty;
            type Output = #output_ty;
            type Future = ::std::pin::Pin<
                Box<dyn ::std::future::Future<Output = Result<#output_ty, ::sayiir_core::error::BoxError>> + Send>
            >;

            fn run(&self, #input_ident: #input_ty) -> Self::Future {
                #(#clone_stmts)*
                Box::pin(async move { #call_expr })
            }
        }
    }
}
