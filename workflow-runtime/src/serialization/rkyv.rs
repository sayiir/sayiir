use anyhow::{Context, Result};
use bytecheck::CheckBytes;
use bytes::Bytes;
use rkyv::rancor::{Error, Strategy};
use rkyv::{Archive, Deserialize, Serialize, from_bytes, to_bytes};
use workflow_core::codec::{Decoder, Encoder};

pub struct RkyvCodec;

impl<T> Encoder<T> for RkyvCodec
where
    T: for<'a> Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            Error,
        >,
    >,
{
    fn encode(&self, value: &T) -> Result<Bytes> {
        let aligned_vec =
            to_bytes::<Error>(value).context("Failed to serialize value with rkyv")?;
        let vec: Vec<u8> = aligned_vec.into();
        Ok(Bytes::from(vec))
    }
}

impl<T> Decoder<T> for RkyvCodec
where
    T: Archive,
    for<'a> T::Archived: CheckBytes<rkyv::api::high::HighValidator<'a, Error>>,
    T::Archived: Deserialize<T, Strategy<rkyv::de::Pool, Error>>,
{
    fn decode(&self, bytes: Bytes) -> Result<T> {
        from_bytes::<T, Error>(&bytes).context("Failed to deserialize value with rkyv")
    }
}
