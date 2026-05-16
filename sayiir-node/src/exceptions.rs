//! Custom error types for Sayiir Node.js bindings.
//!
//! These map to the error class hierarchy in the TypeScript layer.

use napi::bindgen_prelude::*;

/// Create a workflow error.
pub fn workflow_error(msg: impl Into<String>) -> Error {
    Error::new(Status::GenericFailure, msg.into())
}

/// Create a backend error.
pub fn backend_error(msg: impl Into<String>) -> Error {
    Error::new(Status::GenericFailure, msg.into())
}

/// Create a codec/deserialization error.
pub fn codec_error(msg: impl Into<String>) -> Error {
    Error::new(
        Status::GenericFailure,
        format!("CODEC_ERROR: {}", msg.into()),
    )
}

/// Create an instance-already-exists error.
pub fn instance_already_exists_error(msg: impl Into<String>) -> Error {
    Error::new(
        Status::GenericFailure,
        format!("INSTANCE_ALREADY_EXISTS: {}", msg.into()),
    )
}

/// Convert a `RuntimeError` to a napi `Error` with proper dispatch.
pub fn runtime_err_to_napi(e: sayiir_runtime::RuntimeError) -> Error {
    match &e {
        sayiir_runtime::RuntimeError::Codec(_) => codec_error(e.to_string()),
        sayiir_runtime::RuntimeError::Backend(_) => backend_error(e.to_string()),
        sayiir_runtime::RuntimeError::InstanceAlreadyExists(_) => {
            instance_already_exists_error(e.to_string())
        }
        _ => workflow_error(e.to_string()),
    }
}

/// Convert a persistence `BackendError` to a napi `Error`.
pub fn backend_err_to_napi(e: sayiir_persistence::BackendError) -> Error {
    backend_error(e.to_string())
}
