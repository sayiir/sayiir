//! Error types for sayiir-core.

/// Generic boxed error type used throughout the crate.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Errors produced during workflow construction (builder / hydration).
#[derive(Debug, Clone, thiserror::Error)]
pub enum BuildError {
    /// A duplicate task ID was found during workflow building.
    #[error("Duplicate task id: '{0}'")]
    DuplicateTaskId(String),

    /// A referenced task ID was not found in the registry.
    #[error("Task '{0}' not found in registry")]
    TaskNotFound(String),

    /// A branch closure produced an empty sub-builder (no steps added).
    #[error("Branch must have at least one step")]
    EmptyBranch,

    /// A fork has no branches and no join task.
    #[error("Fork has no branches and no join task")]
    EmptyFork,

    /// One or more declared branch keys have no corresponding `.branch()` call
    /// and no default branch was provided.
    #[error("Branch node '{branch_id}': missing branches for keys: {}", missing_keys.join(", "))]
    MissingBranches {
        /// The `route` node ID.
        branch_id: String,
        /// Keys declared in `BranchKey::all_keys()` with no matching branch.
        missing_keys: Vec<String>,
    },

    /// One or more `.branch()` calls use keys not declared in the `BranchKey` enum.
    #[error("Branch node '{branch_id}': orphan branches for keys: {}", orphan_keys.join(", "))]
    OrphanBranches {
        /// The `route` node ID.
        branch_id: String,
        /// Keys passed to `.branch()` that are not in `BranchKey::all_keys()`.
        orphan_keys: Vec<String>,
    },

    /// The workflow definition hash doesn't match during hydration.
    #[error("Workflow definition mismatch: expected hash '{expected}', found '{found}'")]
    DefinitionMismatch {
        /// The expected hash (from current workflow).
        expected: String,
        /// The hash found in the serialized state.
        found: String,
    },
}

/// Errors produced during workflow execution (runtime).
#[derive(Debug, Clone, thiserror::Error)]
pub enum WorkflowError {
    /// A referenced task ID was not found at runtime.
    #[error("Task '{0}' not found in registry")]
    TaskNotFound(String),

    /// The task has no implementation (function body).
    ///
    /// Unreachable for pure-Rust workflows (the builder always fills `func`).
    /// Exists for Node.js/Python bindings which build `func: None` trees and
    /// rely on `ExternalTaskExecutor` to dispatch to the host language.
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

    /// A routing key did not match any branch in a `route` node.
    #[error("Branch node '{branch_id}': no branch matches key '{key}'")]
    BranchKeyNotFound {
        /// The `route` node ID.
        branch_id: String,
        /// The routing key that was produced.
        key: String,
    },

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
