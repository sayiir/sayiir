//! Typed error for the sayiir runtime layer.

use sayiir_core::error::{BoxError, WorkflowError};
use sayiir_persistence::BackendError;

/// Typed error for the sayiir runtime layer.
///
/// Replaces `BoxError` in internal runtime APIs, keeping `BoxError` only at
/// true user boundaries (codec traits, user task callbacks).
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Workflow logic error (cancellation, definition mismatch, task not found, etc.)
    #[error(transparent)]
    Workflow(#[from] WorkflowError),

    /// Persistent backend error (storage failures).
    #[error(transparent)]
    Backend(#[from] BackendError),

    /// User task execution or codec error (opaque — from user-provided code).
    #[error(transparent)]
    Task(#[from] BoxError),

    /// Tokio task join error (branch spawn failures).
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}
