mod json;
mod rkyv;

#[cfg(feature = "json")]
pub use json::JsonCodec;

#[cfg(feature = "rkyv")]
pub use rkyv::RkyvCodec;

/// Re-export the codec traits from `workflow-core`.
pub use workflow_core::codec::{Decoder, Encoder};
