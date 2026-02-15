#[cfg(feature = "json")]
mod json;
#[cfg(feature = "rkyv")]
mod rkyv;

#[cfg(feature = "json")]
pub use json::JsonCodec;

#[cfg(feature = "rkyv")]
pub use rkyv::RkyvCodec;

/// Re-export the codec traits from `sayiir-core`.
pub use sayiir_core::codec::{Decoder, Encoder};
