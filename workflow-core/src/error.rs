//! Error types for workflow-core.

/// Unified error type for workflow operations.
#[derive(Debug, Clone)]
pub enum WorkflowError {
    /// A duplicate task ID was found during workflow building.
    DuplicateTaskId(String),
    /// A referenced task ID was not found in the registry.
    TaskNotFound(String),
    /// The workflow definition hash doesn't match.
    /// This indicates the serialized state was created with a different workflow definition.
    DefinitionMismatch {
        /// The expected hash (from current workflow).
        expected: String,
        /// The hash found in the serialized state.
        found: String,
    },
    /// The workflow was cancelled.
    Cancelled {
        /// Optional reason for the cancellation.
        reason: Option<String>,
        /// Optional identifier of who cancelled the workflow.
        cancelled_by: Option<String>,
    },
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

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::DuplicateTaskId(id) => write!(f, "Duplicate task id: '{id}'"),
            WorkflowError::TaskNotFound(id) => {
                write!(f, "Task '{id}' not found in registry")
            }
            WorkflowError::DefinitionMismatch { expected, found } => {
                write!(
                    f,
                    "Workflow definition mismatch: expected hash '{expected}', found '{found}'"
                )
            }
            WorkflowError::Cancelled { reason, .. } => {
                if let Some(reason) = reason {
                    write!(f, "Workflow cancelled: {reason}")
                } else {
                    write!(f, "Workflow cancelled")
                }
            }
        }
    }
}

impl std::error::Error for WorkflowError {}
