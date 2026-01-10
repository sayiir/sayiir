use anyhow::Result;
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
    use super::*;

    /// Helper trait for encoding with custom bounds.
    pub trait EncodeValue<T>: Send + 'static {
        fn encode_value(&self, value: &T) -> Result<Bytes>;
    }

    /// Helper trait for decoding with custom bounds.
    pub trait DecodeValue<T>: Send + 'static {
        fn decode_value(&self, bytes: Bytes) -> Result<T>;
    }
}

/// An encoder that can serialize a value into a byte stream.
pub trait Encoder: Send + 'static {
    fn encode<T>(&self, value: &T) -> Result<Bytes>
    where
        Self: sealed::EncodeValue<T>,
    {
        sealed::EncodeValue::encode_value(self, value)
    }
}

/// A decoder that can deserialize a value from a byte stream.
pub trait Decoder: Send + 'static {
    fn decode<T>(&self, bytes: Bytes) -> Result<T>
    where
        Self: sealed::DecodeValue<T>,
    {
        sealed::DecodeValue::decode_value(self, bytes)
    }
}

/// A codec that can serialize and deserialize a value.
pub trait Codec: Encoder + Decoder + Send + 'static {}

/// Blanket impl `Codec` for any type that implements Encoder and Decoder.
impl<U> Codec for U where U: Encoder + Decoder + Send + 'static {}
