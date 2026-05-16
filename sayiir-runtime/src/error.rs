//! Typed error for the sayiir runtime layer.

use sayiir_core::error::{BoxError, BuildError, BuildErrors, CodecError, WorkflowError};
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

    /// Build/hydration errors (duplicate IDs, missing tasks, empty branches).
    #[error(transparent)]
    Build(#[from] BuildErrors),

    /// Persistent backend error (storage failures).
    #[error(transparent)]
    Backend(#[from] BackendError),

    /// Codec encode/decode error (schema mismatch, serialization failure).
    #[error(transparent)]
    Codec(#[from] CodecError),

    /// User task execution error (opaque — from user-provided code).
    #[error(transparent)]
    Task(BoxError),

    /// Tokio task join error (branch spawn failures).
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// A workflow instance with this ID already exists (conflict policy = Fail).
    #[error("Workflow instance already exists: {0}")]
    InstanceAlreadyExists(String),
}

impl From<BoxError> for RuntimeError {
    fn from(err: BoxError) -> Self {
        match err.downcast::<CodecError>() {
            Ok(codec_err) => Self::Codec(*codec_err),
            Err(other) => Self::Task(other),
        }
    }
}

impl From<BuildError> for RuntimeError {
    fn from(error: BuildError) -> Self {
        Self::Build(BuildErrors::from(error))
    }
}

impl RuntimeError {
    /// Returns `true` if this error is a `TaskTimedOut` workflow error.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Workflow(WorkflowError::TaskTimedOut { .. }))
    }

    /// Returns `true` if this error is a codec decode failure (schema mismatch).
    #[must_use]
    pub fn is_decode_error(&self) -> bool {
        matches!(self, Self::Codec(CodecError::DecodeFailed { .. }))
    }
}
