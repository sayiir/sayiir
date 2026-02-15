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

/// Convert a persistence `BackendError` to a napi `Error`.
pub fn backend_err_to_napi(e: sayiir_persistence::BackendError) -> Error {
    backend_error(e.to_string())
}
