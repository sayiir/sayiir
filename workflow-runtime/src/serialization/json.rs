use anyhow::Result;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use workflow_core::codec::{Decoder, Encoder};

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
