//! Serialization codecs for workflow snapshots.
//!
//! Enable one (or both) via Cargo features:
//!
//! | Feature | Codec | Best for |
//! |---------|-------|----------|
//! | `json`  | [`JsonCodec`] | Debugging, human-readable snapshots |
//! | `rkyv`  | [`RkyvCodec`] | Production, zero-copy speed |
//!
//! At least one codec feature **must** be enabled (enforced at compile time).
//!
//! The codec traits ([`Encoder`], [`Decoder`], [`EnvelopeCodec`]) are
//! re-exported from [`sayiir_core::codec`] for convenience.

#[cfg(feature = "json")]
mod json;
#[cfg(feature = "rkyv")]
mod rkyv;

#[cfg(feature = "json")]
pub use json::JsonCodec;

#[cfg(feature = "rkyv")]
pub use rkyv::RkyvCodec;

/// Re-export the codec traits from `sayiir-core`.
pub use sayiir_core::codec::{Decoder, Encoder, EnvelopeCodec};
