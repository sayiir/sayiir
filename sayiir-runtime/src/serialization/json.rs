use bytes::Bytes;
use sayiir_core::branch_results::NamedBranchResults;
use sayiir_core::codec::{Decoder, Encoder, EnvelopeCodec, sealed};
use sayiir_core::error::BoxError;
use serde::{Deserialize, Serialize};

/// A codec that can serialize and deserialize values using `serde_json`.
///
/// This codec uses ``serde_json`` to serialize and deserialize values.
///
/// # Example
///
/// ```rust
/// use sayiir_runtime::serialization::JsonCodec;
/// use sayiir_core::codec::*;
///
/// let codec = JsonCodec;
/// let value = 42;
/// let encoded = codec.encode(&value).unwrap();
/// let decoded: i32 = codec.decode(encoded).unwrap();
/// assert_eq!(decoded, value);
/// ```
#[derive(Default, Clone, Copy)]
pub struct JsonCodec;

impl Encoder for JsonCodec {}

impl<T> sealed::EncodeValue<T> for JsonCodec
where
    T: Serialize,
{
    fn encode_value(&self, value: &T) -> Result<Bytes, BoxError> {
        Ok(Bytes::from(serde_json::to_vec(value)?))
    }
}

impl Decoder for JsonCodec {}

impl<T> sealed::DecodeValue<T> for JsonCodec
where
    T: for<'de> Deserialize<'de>,
{
    fn decode_value(&self, bytes: Bytes) -> Result<T, BoxError> {
        Ok(serde_json::from_slice(&bytes)?)
    }
}

impl EnvelopeCodec for JsonCodec {
    fn decode_string(&self, bytes: &[u8]) -> Result<String, BoxError> {
        Ok(serde_json::from_slice(bytes)?)
    }

    fn encode_branch_envelope(&self, key: &str, result_bytes: &[u8]) -> Result<Bytes, BoxError> {
        let result_value: serde_json::Value =
            serde_json::from_slice(result_bytes).unwrap_or(serde_json::Value::Null);
        let envelope = serde_json::json!({ "branch": key, "result": result_value });
        Ok(Bytes::from(serde_json::to_vec(&envelope)?))
    }

    fn encode_named_results(&self, results: &[(String, Bytes)]) -> Result<Bytes, BoxError> {
        let nbr = NamedBranchResults::new(results.to_vec());
        Ok(Bytes::from(serde_json::to_vec(&nbr)?))
    }
}
