use bytecheck::CheckBytes;
use bytes::Bytes;
use rkyv::rancor::{Error, Strategy};
use rkyv::{Archive, Deserialize, Serialize, from_bytes, to_bytes};
use sayiir_core::codec::{Decoder, Encoder, EnvelopeCodec, sealed};
use sayiir_core::error::BoxError;

/// A codec that can serialize and deserialize values using rkyv.
///
/// This codec uses rkyv's zero-copy deserialization framework to serialize and deserialize values.
///
/// # Example
///
/// ```rust,no_run
/// use rkyv::{Archive, Deserialize, Serialize};
/// use sayiir_runtime::serialization::RkyvCodec;
/// use sayiir_core::codec::*;
///
/// #[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
/// struct Example {
///     value: i32,
/// }
///
/// let codec = RkyvCodec;
/// let value = Example { value: 42 };
/// let encoded = codec.encode(&value).unwrap();
/// let decoded: Example = codec.decode(encoded).unwrap();
/// assert_eq!(decoded, value);
/// ```
#[derive(Clone, Copy)]
pub struct RkyvCodec;

impl Encoder for RkyvCodec {}

impl<T> sealed::EncodeValue<T> for RkyvCodec
where
    T: for<'a> Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            Error,
        >,
    >,
{
    fn encode_value(&self, value: &T) -> Result<Bytes, BoxError> {
        let aligned_vec = to_bytes::<Error>(value).map_err(|e| -> BoxError {
            format!("Failed to serialize value with rkyv: {e}").into()
        })?;
        let vec: Vec<u8> = aligned_vec.into();
        Ok(Bytes::from(vec))
    }
}

impl Decoder for RkyvCodec {}

impl<T> sealed::DecodeValue<T> for RkyvCodec
where
    T: Archive,
    for<'a> T::Archived: CheckBytes<rkyv::api::high::HighValidator<'a, Error>>,
    T::Archived: Deserialize<T, Strategy<rkyv::de::Pool, Error>>,
{
    fn decode_value(&self, bytes: Bytes) -> Result<T, BoxError> {
        from_bytes::<T, Error>(&bytes).map_err(|e| -> BoxError {
            format!("Failed to deserialize value with rkyv: {e}").into()
        })
    }
}

impl EnvelopeCodec for RkyvCodec {
    fn decode_string(&self, bytes: &[u8]) -> Result<String, BoxError> {
        from_bytes::<String, Error>(bytes)
            .map_err(|e| -> BoxError { format!("Failed to decode string with rkyv: {e}").into() })
    }

    fn encode_branch_envelope(&self, key: &str, result_bytes: &[u8]) -> Result<Bytes, BoxError> {
        use sayiir_core::task::BranchEnvelope;

        let envelope = BranchEnvelope {
            branch: key.to_string(),
            result: Bytes::copy_from_slice(result_bytes),
        };
        let aligned_vec = to_bytes::<Error>(&envelope).map_err(|e| -> BoxError {
            format!("Failed to encode branch envelope with rkyv: {e}").into()
        })?;
        let vec: Vec<u8> = aligned_vec.into();
        Ok(Bytes::from(vec))
    }

    fn encode_named_results(&self, results: &[(String, Bytes)]) -> Result<Bytes, BoxError> {
        use sayiir_core::branch_results::NamedBranchResults;

        let nbr = NamedBranchResults::new(results.to_vec());
        let aligned_vec = to_bytes::<Error>(&nbr).map_err(|e| -> BoxError {
            format!("Failed to encode named results with rkyv: {e}").into()
        })?;
        let vec: Vec<u8> = aligned_vec.into();
        Ok(Bytes::from(vec))
    }
}
