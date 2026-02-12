//! Error types for sayiir-core.

/// Unified error type for workflow operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WorkflowError {
    /// A duplicate task ID was found during workflow building.
    #[error("Duplicate task id: '{0}'")]
    DuplicateTaskId(String),

    /// A referenced task ID was not found in the registry.
    #[error("Task '{0}' not found in registry")]
    TaskNotFound(String),

    /// The task has no implementation (function body).
    #[error("Task '{0}' has no implementation")]
    TaskNotImplemented(String),

    /// The workflow definition hash doesn't match.
    /// This indicates the serialized state was created with a different workflow definition.
    #[error("Workflow definition mismatch: expected hash '{expected}', found '{found}'")]
    DefinitionMismatch {
        /// The expected hash (from current workflow).
        expected: String,
        /// The hash found in the serialized state.
        found: String,
    },

    /// The workflow was cancelled.
    #[error("Workflow cancelled{}", reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default())]
    Cancelled {
        /// Optional reason for the cancellation.
        reason: Option<String>,
        /// Optional identifier of who cancelled the workflow.
        cancelled_by: Option<String>,
    },

    /// A fork has no branches and no join task.
    #[error("Fork has no branches and no join task")]
    EmptyFork,

    /// A task panicked during execution.
    #[error("Task panicked: {0}")]
    TaskPanicked(String),

    /// Cannot resume workflow from saved state.
    #[error("Cannot resume workflow: {0}")]
    ResumeError(String),

    /// Deserialization of binary data failed.
    #[error("Deserialization error: {0}")]
    Deserialization(String),

    /// A named branch was not found in the outputs.
    #[error("Branch '{0}' not found")]
    BranchNotFound(String),
}

impl WorkflowError {
    /// Create a new `Cancelled` error with no reason or source.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::Cancelled {
            reason: None,
            cancelled_by: None,
        }
    }
}
