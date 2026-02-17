use proc_macro2::Span;

/// Convert a `snake_case` identifier to `PascalCase`.
pub fn snake_to_pascal(s: &str) -> String {
    s.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + chars.as_str()
                }
                None => String::new(),
            }
        })
        .collect()
}

/// Create a `syn::Error` at the given span.
pub fn err(span: Span, msg: impl std::fmt::Display) -> syn::Error {
    syn::Error::new(span, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snake_to_pascal() {
        assert_eq!(snake_to_pascal("charge"), "Charge");
        assert_eq!(snake_to_pascal("send_email"), "SendEmail");
        assert_eq!(snake_to_pascal("update_inventory"), "UpdateInventory");
        assert_eq!(snake_to_pascal("a_b_c"), "ABC");
        assert_eq!(snake_to_pascal("already"), "Already");
    }
}
