//! Error types for sayiir-core.

/// Generic boxed error type used throughout the crate.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Errors produced when encoding or decoding task inputs/outputs.
///
/// These typed errors carry the task ID and expected type, enabling the runtime
/// (and future "cascade re-execution") to distinguish schema-mismatch failures
/// from task logic errors.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// Failed to decode a task's input (or a loop/branch envelope).
    #[error("Failed to decode input for task '{task_id}' (expected {expected_type}): {source}")]
    DecodeFailed {
        /// The task whose input could not be decoded.
        task_id: String,
        /// The Rust type name that was expected (via `std::any::type_name`).
        expected_type: &'static str,
        /// The underlying deserialization error.
        source: BoxError,
    },
    /// Failed to encode a task's output.
    #[error("Failed to encode output for task '{task_id}': {source}")]
    EncodeFailed {
        /// The task whose output could not be encoded.
        task_id: String,
        /// The underlying serialization error.
        source: BoxError,
    },
}

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

    /// A fork has no branches.
    #[error("Fork must have at least one branch")]
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

    /// A loop's `max_iterations` was set to zero.
    #[error("Loop '{0}': max_iterations must be at least 1")]
    InvalidMaxIterations(String),

    /// The workflow has no tasks.
    #[error("Workflow must have at least one task")]
    EmptyWorkflow,

    /// A duration value is not finite or is negative.
    #[error("{0} must be a finite non-negative number")]
    InvalidDuration(String),

    /// The workflow definition hash doesn't match during hydration.
    #[error("Workflow definition mismatch: expected hash '{expected}', found '{found}'")]
    DefinitionMismatch {
        /// The expected hash (from current workflow).
        expected: String,
        /// The hash found in the serialized state.
        found: String,
    },

    /// A `#[task]` requires a dependency that is missing from the `Deps`
    /// container passed to `workflow! { deps: … }`.
    ///
    /// Emitted by `verify_deps` codegen at workflow construction time, so the
    /// failure surfaces as a `BuildErrors` rather than panicking at first task
    /// invocation.
    #[error("Task '{task_id}': missing dependency `{type_name}` in Deps container")]
    MissingDep {
        /// The task that requires the dependency.
        task_id: &'static str,
        /// The Rust type name of the missing dependency (via `std::any::type_name`).
        type_name: &'static str,
    },

    /// A task auto-registered via `workflow! { deps: … }` is already present in
    /// the pre-built `TaskRegistry` passed via `workflow! { registry: … }`.
    ///
    /// Without this check the duplicate registration would be silently deduped,
    /// and the resulting task instance would depend on registration order
    /// rather than the user's expressed intent. Surfacing the conflict forces
    /// an explicit choice between the two sources.
    #[error(
        "Task '{task_id}' is present in both the pre-built `registry:` and would \
         be auto-registered via `deps:` — drop one to resolve the conflict"
    )]
    RegistryDepsConflict {
        /// The task whose registration source is ambiguous.
        task_id: &'static str,
    },
}

/// A collection of [`BuildError`]s accumulated during workflow construction.
///
/// Builder `build()` methods return this type so that all validation errors
/// can be reported at once rather than failing on the first one.
#[derive(Debug, Clone)]
pub struct BuildErrors(Vec<BuildError>);

impl std::fmt::Display for BuildErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.len() == 1
            && let Some(single) = self.0.first()
        {
            return write!(f, "{single}");
        }
        writeln!(f, "{} build errors:", self.0.len())?;
        for error in &self.0 {
            writeln!(f, "  - {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for BuildErrors {}

impl BuildErrors {
    /// Create an empty error collection.
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Append a single error.
    pub fn push(&mut self, error: BuildError) {
        self.0.push(error);
    }

    /// Returns `true` if no errors have been collected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of collected errors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterate over the individual errors.
    pub fn iter(&self) -> std::slice::Iter<'_, BuildError> {
        self.0.iter()
    }

    /// Consume the wrapper and return the inner vector.
    #[must_use]
    pub fn into_vec(self) -> Vec<BuildError> {
        self.0
    }

    /// Extend with errors from another collection.
    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }
}

impl Default for BuildErrors {
    fn default() -> Self {
        Self::new()
    }
}

impl From<BuildError> for BuildErrors {
    fn from(error: BuildError) -> Self {
        Self(vec![error])
    }
}

impl IntoIterator for BuildErrors {
    type Item = BuildError;
    type IntoIter = std::vec::IntoIter<BuildError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a BuildErrors {
    type Item = &'a BuildError;
    type IntoIter = std::slice::Iter<'a, BuildError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
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

    /// A fork has no branches.
    #[error("Fork must have at least one branch")]
    EmptyFork,

    /// A task panicked during execution.
    #[error("Task panicked: {0}")]
    TaskPanicked(String),

    /// Cannot resume workflow from saved state.
    #[error("Cannot resume workflow: {0}")]
    ResumeError(String),

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

    /// A loop exceeded its maximum iteration count with `MaxIterationsPolicy::Fail`.
    #[error("Loop '{loop_id}' exceeded max iterations ({max_iterations})")]
    MaxIterationsExceeded {
        /// The loop node ID.
        loop_id: String,
        /// The configured maximum.
        max_iterations: u32,
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

    /// Create a new `Paused` error with no reason or source.
    #[must_use]
    pub fn paused() -> Self {
        Self::Paused {
            reason: None,
            paused_by: None,
        }
    }
}
