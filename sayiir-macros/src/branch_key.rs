use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields};

use crate::util::{err, pascal_to_snake};

/// Expand `#[derive(BranchKey)]` on a fieldless enum.
pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;

    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return Err(err(
                input.ident.span(),
                "BranchKey can only be derived on enums",
            ));
        }
    };

    // Collect (variant_ident, key_string) pairs
    let mut arms = Vec::new();
    for variant in variants {
        // Must be fieldless
        match &variant.fields {
            Fields::Unit => {}
            _ => {
                return Err(err(
                    variant.ident.span(),
                    "BranchKey variants must be fieldless (no data)",
                ));
            }
        }

        // Check for #[branch_key = "custom"] attribute override
        let key = extract_branch_key_attr(&variant.attrs)?
            .unwrap_or_else(|| pascal_to_snake(&variant.ident.to_string()));

        arms.push((variant.ident.clone(), key));
    }

    if arms.is_empty() {
        return Err(err(
            input.ident.span(),
            "BranchKey enum must have at least one variant",
        ));
    }

    let match_arms = arms.iter().map(|(ident, key)| {
        quote! { #name::#ident => #key }
    });

    let all_keys = arms.iter().map(|(_, key)| {
        quote! { #key }
    });

    Ok(quote! {
        impl ::sayiir_core::branch_key::BranchKey for #name {
            fn as_key(&self) -> &'static str {
                match self {
                    #(#match_arms,)*
                }
            }

            fn all_keys() -> &'static [&'static str] {
                &[#(#all_keys),*]
            }
        }
    })
}

/// Extract the value from `#[branch_key("custom")]` if present.
fn extract_branch_key_attr(attrs: &[syn::Attribute]) -> syn::Result<Option<String>> {
    for attr in attrs {
        if attr.path().is_ident("branch_key") {
            let value: syn::LitStr = attr.parse_args()?;
            return Ok(Some(value.value()));
        }
    }
    Ok(None)
}
