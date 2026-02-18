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

/// Convert a `PascalCase` identifier to `snake_case`.
pub fn pascal_to_snake(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            for lower in ch.to_lowercase() {
                result.push(lower);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Create a `syn::Error` at the given span.
pub fn err(span: Span, msg: impl std::fmt::Display) -> syn::Error {
    syn::Error::new(span, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pascal_to_snake() {
        assert_eq!(pascal_to_snake("Billing"), "billing");
        assert_eq!(pascal_to_snake("TechSupport"), "tech_support");
        assert_eq!(pascal_to_snake("A"), "a");
        assert_eq!(pascal_to_snake("ABC"), "a_b_c");
        assert_eq!(pascal_to_snake("already"), "already");
    }

    #[test]
    fn test_snake_to_pascal() {
        assert_eq!(snake_to_pascal("charge"), "Charge");
        assert_eq!(snake_to_pascal("send_email"), "SendEmail");
        assert_eq!(snake_to_pascal("update_inventory"), "UpdateInventory");
        assert_eq!(snake_to_pascal("a_b_c"), "ABC");
        assert_eq!(snake_to_pascal("already"), "Already");
    }
}
