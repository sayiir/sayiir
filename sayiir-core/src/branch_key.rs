//! The `BranchKey` trait for type-safe conditional branching.
//!
//! Implement this trait on a fieldless enum to constrain the routing keys
//! that a `route` node can produce. The derive macro
//! `BranchKey` (from `sayiir-macros`) generates the implementation
//! automatically.
//!
//! # Example
//!
//! ```rust
//! use sayiir_core::branch_key::BranchKey;
//!
//! enum Intent {
//!     Billing,
//!     Tech,
//! }
//!
//! impl BranchKey for Intent {
//!     fn as_key(&self) -> &'static str {
//!         match self {
//!             Intent::Billing => "billing",
//!             Intent::Tech => "tech",
//!         }
//!     }
//!
//!     fn all_keys() -> &'static [&'static str] {
//!         &["billing", "tech"]
//!     }
//! }
//! ```

/// A routing key for conditional branching.
///
/// Implementors are fieldless enums whose variants map 1-to-1 to the named
/// branches in a `route` node. The builder uses [`all_keys`](BranchKey::all_keys)
/// to verify exhaustiveness at build time.
pub trait BranchKey: Send + Sync + 'static {
    /// The string key corresponding to this variant.
    fn as_key(&self) -> &'static str;

    /// All possible keys for this type (one per variant).
    fn all_keys() -> &'static [&'static str];
}
