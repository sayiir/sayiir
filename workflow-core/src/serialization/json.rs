use super::{TaskInput, TaskOutput};
use anyhow::Result;
use bytes::Bytes;

/// A newtype wrapper for JSON serialization.
///
/// This wrapper implements `TaskInput` and `TaskOutput` for any `T` that implements
/// `serde::Serialize` and `serde::Deserialize`. We use a wrapper instead of implementing
/// directly on `T` to allow multiple serialization formats (e.g., bincode) to coexist.
///
/// # Example
/// ```
/// use workflow_core::serialization::json::Json;
/// let _t = Json::new(5);
pub struct Json<T>(T);

impl<T> Json<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T> AsRef<T> for Json<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T> TaskInput for Json<T>
where
    T: for<'a> serde::Deserialize<'a> + Send + 'static,
{
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        Ok(Self(serde_json::from_slice(&bytes)?))
    }
}

impl<T> TaskOutput for Json<T>
where
    T: serde::Serialize + Send + 'static,
{
    fn to_bytes(&self) -> Result<Bytes> {
        Ok(Bytes::from(serde_json::to_vec(self.as_ref())?))
    }
}
