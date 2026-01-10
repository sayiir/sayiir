use anyhow::Result;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use workflow_core::codec::{Decoder, Encoder};

/// A codec that can serialize and deserialize values using serde_json.
///
/// This codec uses serde_json to serialize and deserialize values.
///
/// # Example
///
/// ```rust
/// use workflow_runtime::serialization::JsonCodec;
/// use workflow_core::codec::*;
///
/// let codec = JsonCodec;
/// let value = 42;
/// let encoded = codec.encode(&value).unwrap();
/// let decoded: i32 = codec.decode(encoded).unwrap();
/// assert_eq!(decoded, value);
/// ```
pub struct JsonCodec;

impl<T> Encoder<T> for JsonCodec
where
    T: Serialize,
{
    fn encode(&self, value: &T) -> Result<Bytes> {
        Ok(Bytes::from(serde_json::to_vec(value)?))
    }
}

impl<T> Decoder<T> for JsonCodec
where
    T: for<'de> Deserialize<'de>,
{
    fn decode(&self, bytes: Bytes) -> Result<T> {
        Ok(serde_json::from_slice(&bytes)?)
    }
}
