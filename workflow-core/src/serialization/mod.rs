pub mod json;

use anyhow::Result;
use bytes::Bytes;

pub trait TaskInput: Send + 'static {
    /// Deserialize a task input from bytes.
    ///
    fn from_bytes(bytes: Bytes) -> Result<Self>
    where
        Self: Sized;
}

pub trait TaskOutput: Send + 'static {
    /// Serialize a task output to bytes.
    ///
    fn to_bytes(&self) -> Result<Bytes>;
}
