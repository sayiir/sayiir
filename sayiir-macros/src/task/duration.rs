use darling::FromMeta;
use proc_macro2::Span;
use quote::quote;

/// A duration literal like `"30s"`, `"100ms"`, `"5m"`, `"1h"`.
///
/// Parses from a string attribute and can generate a `TokenStream`
/// for `std::time::Duration`.
#[derive(Debug, Clone)]
pub struct DurationLit {
    pub millis: u64,
    _span: Span,
}

impl DurationLit {
    /// Parse a duration string like "30s", "100ms", "5m", "1h".
    pub fn parse(s: &str, span: Span) -> Result<Self, syn::Error> {
        let s = s.trim();
        if s.is_empty() {
            return Err(syn::Error::new(span, "empty duration string"));
        }

        // Try each suffix from longest to shortest
        let (num_str, multiplier) = if let Some(num) = s.strip_suffix("ms") {
            (num, 1u64)
        } else if let Some(num) = s.strip_suffix('s') {
            (num, 1_000)
        } else if let Some(num) = s.strip_suffix('m') {
            (num, 60_000)
        } else if let Some(num) = s.strip_suffix('h') {
            (num, 3_600_000)
        } else {
            return Err(syn::Error::new(
                span,
                format!("invalid duration suffix in \"{s}\"; expected ms, s, m, or h"),
            ));
        };

        let num: u64 = num_str
            .parse()
            .map_err(|_| syn::Error::new(span, format!("invalid number in duration \"{s}\"")))?;

        Ok(Self {
            millis: num * multiplier,
            _span: span,
        })
    }

    /// Generate a `TokenStream` for `::std::time::Duration::from_millis(...)`.
    pub fn to_tokens(&self) -> proc_macro2::TokenStream {
        let ms = self.millis;
        quote! { ::std::time::Duration::from_millis(#ms) }
    }
}

impl FromMeta for DurationLit {
    fn from_string(value: &str) -> darling::Result<Self> {
        Self::parse(value, Span::call_site()).map_err(|e| darling::Error::custom(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> u64 {
        DurationLit::parse(s, Span::call_site()).unwrap().millis
    }

    #[test]
    fn test_milliseconds() {
        assert_eq!(parse("100ms"), 100);
        assert_eq!(parse("0ms"), 0);
    }

    #[test]
    fn test_seconds() {
        assert_eq!(parse("30s"), 30_000);
        assert_eq!(parse("1s"), 1_000);
    }

    #[test]
    fn test_minutes() {
        assert_eq!(parse("5m"), 300_000);
    }

    #[test]
    fn test_hours() {
        assert_eq!(parse("1h"), 3_600_000);
    }

    #[test]
    fn test_invalid_suffix() {
        assert!(DurationLit::parse("30x", Span::call_site()).is_err());
    }

    #[test]
    fn test_invalid_number() {
        assert!(DurationLit::parse("abcs", Span::call_site()).is_err());
    }

    #[test]
    fn test_empty() {
        assert!(DurationLit::parse("", Span::call_site()).is_err());
    }
}
