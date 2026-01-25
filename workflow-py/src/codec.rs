//! Codec for Python objects with JSON and Pickle support.
//!
//! This module provides serialization between Python objects and Bytes
//! with configurable serialization format.

use anyhow::Result;
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
use std::sync::Arc;
use workflow_core::codec::{sealed, Decoder, Encoder};

/// Serialization format for Python objects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SerializerKind {
    /// JSON serialization (default) - safe, human-readable, interoperable
    #[default]
    Json,
    /// Pickle serialization - supports arbitrary Python objects
    /// WARNING: Only use with trusted data sources
    Pickle,
}

/// Codec that serializes Python objects using JSON or Pickle.
///
/// JSON is the default and recommended for most use cases due to:
/// - Security (no code execution during deserialization)
/// - Interoperability with Rust and other languages
/// - Human-readable format for debugging
///
/// Pickle can be used when you need to serialize arbitrary Python objects
/// that aren't JSON-serializable, but should only be used with trusted data.
#[derive(Clone)]
pub struct PyCodec {
    #[allow(dead_code)]
    kind: SerializerKind,
}

impl PyCodec {
    /// Create a new PyCodec with JSON serialization (default).
    pub fn new() -> Self {
        Self {
            kind: SerializerKind::Json,
        }
    }

    /// Create a new PyCodec with the specified serialization format.
    pub fn with_kind(kind: SerializerKind) -> Self {
        Self { kind }
    }

    /// Create a PyCodec using JSON serialization.
    #[allow(dead_code)]
    pub fn json() -> Self {
        Self::with_kind(SerializerKind::Json)
    }

    /// Create a PyCodec using Pickle serialization.
    ///
    /// WARNING: Only use pickle with trusted data sources.
    /// Deserializing untrusted pickle data can execute arbitrary code.
    #[allow(dead_code)]
    pub fn pickle() -> Self {
        Self::with_kind(SerializerKind::Pickle)
    }

    /// Get the serialization kind.
    #[allow(dead_code)]
    pub fn kind(&self) -> SerializerKind {
        self.kind
    }

    /// Encode a Python object to bytes.
    pub fn encode_pyobject(
        py: Python,
        obj: &Bound<'_, PyAny>,
        kind: SerializerKind,
    ) -> Result<Bytes> {
        match kind {
            SerializerKind::Json => {
                let json_mod = py.import("json")?;
                let json_str: String = json_mod.call_method1("dumps", (obj,))?.extract()?;
                Ok(Bytes::from(json_str))
            }
            SerializerKind::Pickle => {
                let pickle_mod = py.import("pickle")?;
                let pickled: Vec<u8> = pickle_mod.call_method1("dumps", (obj,))?.extract()?;
                Ok(Bytes::from(pickled))
            }
        }
    }

    /// Decode bytes to a Python object.
    pub fn decode_to_pyobject(py: Python, bytes: &Bytes, kind: SerializerKind) -> Result<PyObject> {
        match kind {
            SerializerKind::Json => {
                let json_mod = py.import("json")?;
                let json_str = std::str::from_utf8(bytes)?;
                let py_str = PyString::new(py, json_str);
                Ok(json_mod.call_method1("loads", (py_str,))?.into())
            }
            SerializerKind::Pickle => {
                let pickle_mod = py.import("pickle")?;
                let py_bytes = PyBytes::new(py, bytes);
                Ok(pickle_mod.call_method1("loads", (py_bytes,))?.into())
            }
        }
    }

    /// Encode using this codec's configured serializer.
    #[allow(dead_code)]
    pub fn encode_py(&self, py: Python, obj: &Bound<'_, PyAny>) -> Result<Bytes> {
        Self::encode_pyobject(py, obj, self.kind)
    }

    /// Decode using this codec's configured serializer.
    #[allow(dead_code)]
    pub fn decode_py(&self, py: Python, bytes: &Bytes) -> Result<PyObject> {
        Self::decode_to_pyobject(py, bytes, self.kind)
    }
}

impl Default for PyCodec {
    fn default() -> Self {
        Self::new()
    }
}

// Implement the marker traits for Encoder and Decoder
impl Encoder for PyCodec {}
impl Decoder for PyCodec {}

// Implement sealed traits for Bytes (pass-through for raw bytes)
impl sealed::EncodeValue<Bytes> for PyCodec {
    fn encode_value(&self, value: &Bytes) -> Result<Bytes> {
        Ok(value.clone())
    }
}

impl sealed::DecodeValue<Bytes> for PyCodec {
    fn decode_value(&self, bytes: Bytes) -> Result<Bytes> {
        Ok(bytes)
    }
}

/// Create an Arc-wrapped PyCodec for shared usage.
#[allow(dead_code)]
pub fn py_codec() -> Arc<PyCodec> {
    Arc::new(PyCodec::new())
}

/// Create an Arc-wrapped PyCodec with the specified serialization format.
#[allow(dead_code)]
pub fn py_codec_with_kind(kind: SerializerKind) -> Arc<PyCodec> {
    Arc::new(PyCodec::with_kind(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytes_passthrough() {
        let codec = PyCodec::new();
        let original = Bytes::from("test data");
        let encoded: Bytes = codec.encode(&original).unwrap();
        let decoded: Bytes = codec.decode(encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_serializer_kind_default() {
        let codec = PyCodec::new();
        assert_eq!(codec.kind(), SerializerKind::Json);
    }

    #[test]
    fn test_serializer_kind_pickle() {
        let codec = PyCodec::pickle();
        assert_eq!(codec.kind(), SerializerKind::Pickle);
    }
}
