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
    pub trait EncodeValue<T>: Send + Sync + 'static {
        fn encode_value(&self, value: &T) -> Result<Bytes>;
    }

    /// Helper trait for decoding with custom bounds.
    pub trait DecodeValue<T>: Send + Sync + 'static {
        fn decode_value(&self, bytes: Bytes) -> Result<T>;
    }
}

/// An encoder that can serialize a value into a byte stream.
pub trait Encoder: Send + Sync + 'static {
    fn encode<T>(&self, value: &T) -> Result<Bytes>
    where
        Self: sealed::EncodeValue<T>,
    {
        sealed::EncodeValue::encode_value(self, value)
    }
}

/// A decoder that can deserialize a value from a byte stream.
pub trait Decoder: Send + Sync + 'static {
    fn decode<T>(&self, bytes: Bytes) -> Result<T>
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
    fn encode_value(&self, value: &T) -> Result<Bytes> {
        (**self).encode_value(value)
    }
}

impl<C, T> sealed::DecodeValue<T> for std::sync::Arc<C>
where
    C: sealed::DecodeValue<T>,
{
    fn decode_value(&self, bytes: Bytes) -> Result<T> {
        (**self).decode_value(bytes)
    }
}

impl<C> Encoder for std::sync::Arc<C> where C: Encoder {}

impl<C> Decoder for std::sync::Arc<C> where C: Decoder {}
