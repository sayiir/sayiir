//! Pluggable serialization codec traits.
//!
//! Every workflow carries a codec that serializes task inputs/outputs to
//! [`Bytes`] and back. The trait hierarchy is:
//!
//! - [`Encoder`] / [`Decoder`] — marker traits
//! - [`sealed::EncodeValue<T>`] / [`sealed::DecodeValue<T>`] — per-type encode/decode
//! - [`Codec`] — blanket `Encoder + Decoder`
//! - [`EnvelopeCodec`] — object-safe helpers for branch routing envelopes
//!
//! Concrete implementations live in `sayiir-runtime` (`JsonCodec`, `RkyvCodec`).

use crate::error::BoxError;
use bytes::Bytes;

/// Sealed helper traits for codec implementations.
/// These traits allow implementations to specify their own type bounds.
///
/// # Implementation Note
///
/// To implement `Encoder` or `Decoder`, you need to:
/// 1. Implement the `Encoder` or `Decoder` trait (empty impl is fine)
/// 2. Implement `sealed::EncodeValue<T>` or `sealed::DecodeValue<T>` with your desired bounds
pub mod sealed {
    use super::{BoxError, Bytes};

    /// Helper trait for encoding with custom bounds.
    pub trait EncodeValue<T>: Send + Sync + 'static {
        /// Encode the value into bytes.
        ///
        /// # Errors
        ///
        /// Returns an error if serialization fails.
        fn encode_value(&self, value: &T) -> Result<Bytes, BoxError>;
    }

    /// Helper trait for decoding with custom bounds.
    pub trait DecodeValue<T>: Send + Sync + 'static {
        /// Decode a value from bytes.
        ///
        /// # Errors
        ///
        /// Returns an error if deserialization fails.
        fn decode_value(&self, bytes: Bytes) -> Result<T, BoxError>;
    }
}

/// An encoder that can serialize a value into a byte stream.
pub trait Encoder: Send + Sync + 'static {
    /// Encode a value into bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    fn encode<T>(&self, value: &T) -> Result<Bytes, BoxError>
    where
        Self: sealed::EncodeValue<T>,
    {
        sealed::EncodeValue::encode_value(self, value)
    }
}

/// A decoder that can deserialize a value from a byte stream.
pub trait Decoder: Send + Sync + 'static {
    /// Decode a value from bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if deserialization fails.
    fn decode<T>(&self, bytes: Bytes) -> Result<T, BoxError>
    where
        Self: sealed::DecodeValue<T>,
    {
        sealed::DecodeValue::decode_value(self, bytes)
    }
}

/// A codec that can serialize and deserialize a value.
pub trait Codec: Encoder + Decoder {}

/// Blanket impl `Codec` for any type that implements Encoder and Decoder.
impl<U> Codec for U where U: Encoder + Decoder {}

/// Blanket implementations for `Arc<C>` to allow passing Arc-wrapped codecs.
impl<C, T> sealed::EncodeValue<T> for std::sync::Arc<C>
where
    C: sealed::EncodeValue<T>,
{
    fn encode_value(&self, value: &T) -> Result<Bytes, BoxError> {
        (**self).encode_value(value)
    }
}

impl<C, T> sealed::DecodeValue<T> for std::sync::Arc<C>
where
    C: sealed::DecodeValue<T>,
{
    fn decode_value(&self, bytes: Bytes) -> Result<T, BoxError> {
        (**self).decode_value(bytes)
    }
}

impl<C> Encoder for std::sync::Arc<C> where C: Encoder {}

impl<C> Decoder for std::sync::Arc<C> where C: Decoder {}

/// Object-safe trait for branch envelope operations in the execution layer.
///
/// [`Codec`] is generic over `T`, which makes it non-object-safe — callers must
/// know the concrete type at compile time. The execution layer, however, is
/// type-erased: it shuffles opaque `Bytes` between tasks without knowing their
/// Rust types, so it cannot call `Codec` directly.
///
/// `EnvelopeCodec` bridges this gap by exposing **byte-level** operations that
/// the runtime executor can call through a trait object (`dyn EnvelopeCodec`).
/// It abstracts the serialization format used for:
/// - Deserializing routing keys from branch key functions
/// - Constructing discriminated `{"branch": key, "result": value}` envelopes
/// - Serializing named fork/join results
///
/// By default, `JsonCodec` implements this with `serde_json`. Other codecs
/// (e.g. `RkyvCodec`) can return clear errors if envelope operations are
/// unsupported.
pub trait EnvelopeCodec: Send + Sync {
    /// Decode a routing key (String) from bytes produced by a branch key function.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be decoded as a string.
    fn decode_string(&self, bytes: &[u8]) -> Result<String, BoxError>;

    /// Encode a branch envelope containing the routing key and result bytes.
    ///
    /// Produces a discriminated envelope (e.g. `{"branch": key, "result": value}`)
    /// so downstream tasks know which branch produced the result.
    ///
    /// # Errors
    ///
    /// Returns an error if envelope construction or serialization fails.
    fn encode_branch_envelope(&self, key: &str, result_bytes: &[u8]) -> Result<Bytes, BoxError>;

    /// Serialize named branch results for a fork/join.
    ///
    /// Encodes a `Vec<(String, Bytes)>` of branch results into a single `Bytes`
    /// value suitable for passing to a join task.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    fn encode_named_results(&self, results: &[(String, Bytes)]) -> Result<Bytes, BoxError>;
}

/// Blanket implementation for `&C` to allow passing references generically.
impl<C: EnvelopeCodec> EnvelopeCodec for &C {
    fn decode_string(&self, bytes: &[u8]) -> Result<String, BoxError> {
        (**self).decode_string(bytes)
    }

    fn encode_branch_envelope(&self, key: &str, result_bytes: &[u8]) -> Result<Bytes, BoxError> {
        (**self).encode_branch_envelope(key, result_bytes)
    }

    fn encode_named_results(&self, results: &[(String, Bytes)]) -> Result<Bytes, BoxError> {
        (**self).encode_named_results(results)
    }
}

/// Blanket implementation for `Arc<C>` to allow passing Arc-wrapped envelope codecs.
impl<C: EnvelopeCodec> EnvelopeCodec for std::sync::Arc<C> {
    fn decode_string(&self, bytes: &[u8]) -> Result<String, BoxError> {
        (**self).decode_string(bytes)
    }

    fn encode_branch_envelope(&self, key: &str, result_bytes: &[u8]) -> Result<Bytes, BoxError> {
        (**self).encode_branch_envelope(key, result_bytes)
    }

    fn encode_named_results(&self, results: &[(String, Bytes)]) -> Result<Bytes, BoxError> {
        (**self).encode_named_results(results)
    }
}
