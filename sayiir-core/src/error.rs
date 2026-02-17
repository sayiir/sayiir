//! Error types for sayiir-core.

/// Generic boxed error type used throughout the crate.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

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

    /// The workflow was paused.
    #[error("Workflow paused{}", reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default())]
    Paused {
        /// Optional reason for the pause.
        reason: Option<String>,
        /// Optional identifier of who paused the workflow.
        paused_by: Option<String>,
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

    /// The workflow is waiting for a delay to expire.
    #[error("Workflow waiting until {wake_at}")]
    Waiting {
        /// When the delay expires.
        wake_at: chrono::DateTime<chrono::Utc>,
    },

    /// Task exceeded its configured timeout duration.
    ///
    /// This marks the entire workflow as `Failed`. The task future is actively
    /// dropped (cancelled mid-flight) via `tokio::select!` in all runners.
    #[error("Task '{task_id}' timed out after {timeout:?}")]
    TaskTimedOut {
        /// The task that timed out.
        task_id: String,
        /// The configured timeout duration.
        timeout: std::time::Duration,
    },

    /// The workflow is waiting for an external signal.
    #[error("Workflow awaiting signal '{signal_name}' at node '{signal_id}'")]
    AwaitingSignal {
        /// The signal node ID.
        signal_id: String,
        /// The named signal being waited on.
        signal_name: String,
        /// Optional timeout deadline.
        wake_at: Option<chrono::DateTime<chrono::Utc>>,
    },

    /// A buffered signal was consumed during park — execution should continue.
    ///
    /// This is an internal sentinel used by `park_at_signal` when a signal is
    /// already buffered. The executor should re-enter the loop.
    #[error("Signal consumed (internal)")]
    SignalConsumed,
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

    /// Create a new `Paused` error with no reason or source.
    #[must_use]
    pub fn paused() -> Self {
        Self::Paused {
            reason: None,
            paused_by: None,
        }
    }
}
