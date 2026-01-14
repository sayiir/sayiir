//! Persistent backend trait for storing and retrieving workflow snapshots.
//!
//! This trait abstracts the storage mechanism, allowing implementations
//! for various backends (in-memory, Redis, PostgreSQL, etc.).

use crate::persistence::snapshot::WorkflowSnapshot;
use async_trait::async_trait;
use chrono::Duration;
use workflow_core::task_claim::AvailableTask;
use workflow_core::task_claim::TaskClaim;

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
}

/// Trait for persistent storage of workflow snapshots.
///
/// Implementations of this trait provide the storage layer for distributed
/// workflow execution. Snapshots are saved after each task completion,
/// enabling recovery and resumption of workflows.
///
/// # Example
///
/// ```rust,ignore
/// use workflow_runtime::persistence::{PersistentBackend, InMemoryBackend};
///
/// let backend = InMemoryBackend::new();
/// let snapshot = WorkflowSnapshot::new("instance-123", "hash-abc".to_string());
/// backend.save_snapshot(snapshot).await?;
/// let restored = backend.load_snapshot("instance-123").await?;
/// ```
#[async_trait]
pub trait PersistentBackend: Send + Sync {
    /// Save a workflow snapshot.
    ///
    /// If a snapshot with the same instance_id already exists, it should be overwritten.
    ///
    /// # Errors
    ///
    /// Returns `BackendError` if the snapshot cannot be saved.
    async fn save_snapshot(&self, snapshot: WorkflowSnapshot) -> Result<(), BackendError>;

    /// Load a workflow snapshot by instance ID.
    ///
    /// # Errors
    ///
    /// Returns `BackendError::NotFound` if no snapshot exists for the given instance ID.
    async fn load_snapshot(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError>;

    /// Delete a workflow snapshot.
    ///
    /// # Errors
    ///
    /// Returns `BackendError::NotFound` if no snapshot exists for the given instance ID.
    async fn delete_snapshot(&self, instance_id: &str) -> Result<(), BackendError>;

    /// List all snapshot instance IDs.
    ///
    /// Returns an empty vector if no snapshots exist.
    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError>;

    /// Claim a task for execution by a worker node.
    ///
    /// This atomically claims a task, preventing other nodes from executing it.
    /// Returns `Ok(Some(claim))` if the claim was successful, `Ok(None)` if the
    /// task is already claimed or not available, and `Err` on backend errors.
    ///
    /// # Parameters
    ///
    /// - `instance_id`: The workflow instance ID
    /// - `task_id`: The task ID to claim
    /// - `worker_id`: The ID of the worker node claiming the task
    /// - `ttl`: Optional time-to-live duration for the claim
    ///
    /// # Errors
    ///
    /// Returns `BackendError` if the operation fails.
    async fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        ttl: Option<Duration>,
    ) -> Result<Option<TaskClaim>, BackendError>;

    /// Release a task claim.
    ///
    /// This releases a claim, making the task available for other workers.
    /// Only the worker that owns the claim can release it.
    ///
    /// # Errors
    ///
    /// Returns `BackendError::NotFound` if the claim doesn't exist or doesn't belong to the worker.
    async fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> Result<(), BackendError>;

    /// Extend a task claim's expiration time.
    ///
    /// Useful for long-running tasks to prevent expiration.
    ///
    /// # Errors
    ///
    /// Returns `BackendError::NotFound` if the claim doesn't exist or doesn't belong to the worker.
    async fn extend_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        additional_duration: Duration,
    ) -> Result<(), BackendError>;

    /// Find available tasks across all workflow instances.
    ///
    /// Returns tasks that are ready to execute (dependencies met, not claimed).
    /// This is used by worker nodes to discover work.
    ///
    /// # Parameters
    ///
    /// - `worker_id`: The worker node ID (for filtering if needed)
    /// - `limit`: Maximum number of tasks to return
    ///
    /// # Errors
    ///
    /// Returns `BackendError` if the operation fails.
    async fn find_available_tasks(
        &self,
        worker_id: &str,
        limit: usize,
    ) -> Result<Vec<AvailableTask>, BackendError>;
}
