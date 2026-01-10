use anyhow::Result;
use bytes::Bytes;

/// An encoder that can serialize a value into a byte stream.
pub trait Encoder<T>: Send + 'static {
    fn encode(&self, value: &T) -> Result<Bytes>;
}

/// A decoder that can deserialize a value from a byte stream.
pub trait Decoder<T>: Send + 'static {
    fn decode(&self, bytes: Bytes) -> Result<T>;
}

/// A codec that can serialize and deserialize a value.
pub trait Codec<T>: Encoder<T> + Decoder<T> + Send + 'static {}

/// Blanket impl `Codec` for any type that implements Encoder and Decoder.
impl<U, T> Codec<T> for U where U: Encoder<T> + Decoder<T> + Send + 'static {}
