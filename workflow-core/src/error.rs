//! Error types for workflow-core.

/// Unified error type for workflow operations.
#[derive(Debug)]
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
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::DuplicateTaskId(id) => write!(f, "Duplicate task id: '{}'", id),
            WorkflowError::TaskNotFound(id) => {
                write!(f, "Task '{}' not found in registry", id)
            }
            WorkflowError::DefinitionMismatch { expected, found } => {
                write!(
                    f,
                    "Workflow definition mismatch: expected hash '{}', found '{}'",
                    expected, found
                )
            }
        }
    }
}

impl std::error::Error for WorkflowError {}
