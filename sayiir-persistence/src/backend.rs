//! Persistent backend traits for storing and retrieving workflow snapshots.
//!
//! The trait hierarchy is decomposed into focused sub-traits:
//!
//! - [`SnapshotStore`]: Core CRUD for workflow snapshots (5 methods).
//! - [`SignalStore`]: Cancel + pause signal primitives with default composite
//!   implementations (3 required + 3 default methods).
//! - [`TaskClaimStore`]: Distributed task claiming (4 methods, opt-in).
//! - [`PersistentBackend`]: Supertrait = `SnapshotStore + SignalStore`, blanket-implemented.
//!
//! A minimal backend only needs to implement `SnapshotStore` + 3 `SignalStore` primitives
//! (8 methods total) to satisfy `PersistentBackend`.

use chrono::Duration;
use sayiir_core::snapshot::{
    PauseRequest, SignalKind, SignalRequest, WorkflowSnapshot, WorkflowSnapshotState,
};
use sayiir_core::task_claim::{AvailableTask, TaskClaim};

/// Error type for backend operations.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// Snapshot not found.
    #[error("Snapshot not found: {0}")]
    NotFound(String),
    /// Serialization/deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(String),
    /// Backend-specific error.
    #[error("Backend error: {0}")]
    Backend(String),
    /// Cannot cancel workflow in current state.
    #[error("Cannot cancel workflow in state: {0}")]
    CannotCancel(String),
    /// Cannot pause workflow in current state.
    #[error("Cannot pause workflow in state: {0}")]
    CannotPause(String),
}

// ---------------------------------------------------------------------------
// SnapshotStore — core CRUD, every backend implements this
// ---------------------------------------------------------------------------

/// Core snapshot CRUD operations.
///
/// Every persistent backend must implement these 5 methods.
pub trait SnapshotStore: Send + Sync {
    /// Save a workflow snapshot.
    ///
    /// If a snapshot with the same instance_id already exists, it should be overwritten.
    fn save_snapshot(
        &self,
        snapshot: &WorkflowSnapshot,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Save a single task result atomically.
    ///
    /// This is more granular than `save_snapshot` and allows concurrent task
    /// completions (e.g., in fork branches) without overwriting each other.
    fn save_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
        output: bytes::Bytes,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Load a workflow snapshot by instance ID.
    fn load_snapshot(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<WorkflowSnapshot, BackendError>> + Send;

    /// Delete a workflow snapshot.
    fn delete_snapshot(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// List all snapshot instance IDs.
    fn list_snapshots(&self) -> impl Future<Output = Result<Vec<String>, BackendError>> + Send;
}

// ---------------------------------------------------------------------------
// SignalStore — cancel + pause via SignalKind
// ---------------------------------------------------------------------------

/// Signal storage for cancel and pause workflows.
///
/// Backends implement the 3 primitives (`store_signal`, `get_signal`,
/// `clear_signal`). The 3 composite methods (`check_and_cancel`,
/// `check_and_pause`, `unpause`) have default implementations built from
/// the primitives + `SnapshotStore`. Backends may override them for atomicity.
pub trait SignalStore: SnapshotStore {
    // --- 3 primitives (backend must implement) ---

    /// Store a signal (cancel or pause) for a workflow instance.
    fn store_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
        request: SignalRequest,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Get the pending signal of the given kind, if any.
    fn get_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> impl Future<Output = Result<Option<SignalRequest>, BackendError>> + Send;

    /// Clear the signal of the given kind.
    fn clear_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Send an external event to a workflow instance.
    ///
    /// Events are buffered per `(instance_id, signal_name)` in FIFO order.
    /// Sending to a nonexistent or terminal instance silently stores the event.
    fn send_event(
        &self,
        instance_id: &str,
        signal_name: &str,
        payload: bytes::Bytes,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Consume the oldest buffered event for the given signal name, if any.
    ///
    /// Returns `Some(payload)` if an event was consumed, `None` otherwise.
    fn consume_event(
        &self,
        instance_id: &str,
        signal_name: &str,
    ) -> impl Future<Output = Result<Option<bytes::Bytes>, BackendError>> + Send;

    // --- 3 composites with default impls (overridable for atomicity) ---

    /// Atomically check for cancellation and transition to cancelled state.
    ///
    /// Returns `true` if the workflow was cancelled, `false` if no cancellation
    /// was pending.
    fn check_and_cancel(
        &self,
        instance_id: &str,
        interrupted_at_task: Option<&str>,
    ) -> impl Future<Output = Result<bool, BackendError>> + Send {
        async move {
            let Some(request) = self.get_signal(instance_id, SignalKind::Cancel).await? else {
                return Ok(false);
            };
            let mut snapshot = self.load_snapshot(instance_id).await?;
            if !snapshot.state.is_in_progress() {
                return Ok(false);
            }
            snapshot.mark_cancelled(
                request.reason,
                request.requested_by,
                interrupted_at_task.map(String::from),
            );
            self.save_snapshot(&snapshot).await?;
            self.clear_signal(instance_id, SignalKind::Cancel).await?;
            Ok(true)
        }
    }

    /// Atomically check for a pause request and transition to paused state.
    ///
    /// Returns `true` if the workflow was paused, `false` if no pause was pending.
    fn check_and_pause(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<bool, BackendError>> + Send {
        async move {
            let Some(request) = self.get_signal(instance_id, SignalKind::Pause).await? else {
                return Ok(false);
            };
            let mut snapshot = self.load_snapshot(instance_id).await?;
            if !snapshot.state.is_in_progress() {
                return Ok(false);
            }
            let pause_request: PauseRequest = request.into();
            snapshot.mark_paused(&pause_request);
            self.save_snapshot(&snapshot).await?;
            self.clear_signal(instance_id, SignalKind::Pause).await?;
            Ok(true)
        }
    }

    /// Transition a paused workflow back to in-progress and return the updated snapshot.
    fn unpause(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<WorkflowSnapshot, BackendError>> + Send {
        async move {
            let mut snapshot = self.load_snapshot(instance_id).await?;
            if !snapshot.state.is_paused() {
                let state_name = match &snapshot.state {
                    WorkflowSnapshotState::InProgress { .. } => "InProgress",
                    WorkflowSnapshotState::Completed { .. } => "Completed",
                    WorkflowSnapshotState::Failed { .. } => "Failed",
                    WorkflowSnapshotState::Cancelled { .. } => "Cancelled",
                    WorkflowSnapshotState::Paused { .. } => "Paused",
                };
                return Err(BackendError::CannotPause(format!(
                    "Workflow is not paused (current state: {state_name:?})"
                )));
            }
            snapshot.mark_unpaused();
            self.save_snapshot(&snapshot).await?;
            Ok(snapshot)
        }
    }
}

// ---------------------------------------------------------------------------
// TaskClaimStore — only for distributed workers
// ---------------------------------------------------------------------------

/// Task claiming for distributed multi-worker execution.
///
/// Only needed when using [`PooledWorker`](crate). Single-process backends
/// (used with `CheckpointingRunner`) do not need to implement this.
pub trait TaskClaimStore: Send + Sync {
    /// Claim a task for execution by a worker node.
    ///
    /// Returns `Ok(Some(claim))` if successful, `Ok(None)` if already claimed.
    fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        ttl: Option<Duration>,
    ) -> impl Future<Output = Result<Option<TaskClaim>, BackendError>> + Send;

    /// Release a task claim.
    fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Extend a task claim's expiration time.
    fn extend_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        additional_duration: Duration,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Find available tasks across all workflow instances.
    fn find_available_tasks(
        &self,
        worker_id: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<AvailableTask>, BackendError>> + Send;
}

// ---------------------------------------------------------------------------
// PersistentBackend — supertrait + blanket impl
// ---------------------------------------------------------------------------

/// Supertrait combining [`SnapshotStore`] and [`SignalStore`].
///
/// This is the bound used by `CheckpointingRunner` and most of the runtime.
/// It is blanket-implemented for any type that implements both sub-traits,
/// so backends never need to implement it directly.
pub trait PersistentBackend: SnapshotStore + SignalStore {}

impl<T: SnapshotStore + SignalStore> PersistentBackend for T {}

// Re-export Future so the trait method return types resolve.
use std::future::Future;
