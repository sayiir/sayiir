//! In-memory implementation of PersistentBackend.
//!
//! This is a simple implementation that stores snapshots in a HashMap.
//! Useful for testing and as a reference implementation.

use crate::backend::{BackendError, PersistentBackend};
use async_trait::async_trait;
use chrono::Duration;
use sayiir_core::snapshot::{
    CancellationRequest, ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState,
};
use sayiir_core::task_claim::AvailableTask;
use sayiir_core::task_claim::TaskClaim;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// In-memory backend that stores snapshots in a HashMap.
///
/// This implementation is thread-safe and suitable for testing.
/// For production use, consider implementing PersistentBackend for
/// a more durable storage backend (Redis, PostgreSQL, etc.).
///
/// # Example
///
/// ```rust,ignore
/// use sayiir_persistence::{InMemoryBackend, PersistentBackend};
/// use sayiir_core::snapshot::WorkflowSnapshot;
///
/// let backend = InMemoryBackend::new();
/// let snapshot = WorkflowSnapshot::new("instance-123".to_string(), "hash-abc".to_string());
/// backend.save_snapshot(snapshot).await?;
/// ```
#[derive(Clone, Default)]
pub struct InMemoryBackend {
    snapshots: Arc<RwLock<HashMap<String, WorkflowSnapshot>>>,
    claims: Arc<RwLock<HashMap<String, TaskClaim>>>, // Key: "{instance_id}:{task_id}"
    cancellation_requests: Arc<RwLock<HashMap<String, CancellationRequest>>>, // Key: instance_id
}

impl InMemoryBackend {
    /// Create a new in-memory backend.
    pub fn new() -> Self {
        Default::default()
    }

    fn claim_key(instance_id: &str, task_id: &str) -> String {
        format!("{}:{}", instance_id, task_id)
    }

    /// Convert a lock error into a BackendError.
    fn lock_error<E: std::fmt::Display>(e: E) -> BackendError {
        BackendError::Backend(format!("Lock error: {e}"))
    }
}

#[async_trait]
impl PersistentBackend for InMemoryBackend {
    async fn save_snapshot(&self, snapshot: WorkflowSnapshot) -> Result<(), BackendError> {
        let instance_id = snapshot.instance_id.clone();
        let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;
        snapshots.insert(instance_id, snapshot);
        Ok(())
    }

    async fn save_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
        output: bytes::Bytes,
    ) -> Result<(), BackendError> {
        let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;

        let snapshot = snapshots
            .get_mut(instance_id)
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        snapshot.mark_task_completed(task_id.to_string(), output);
        Ok(())
    }

    async fn load_snapshot(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
        snapshots
            .get(instance_id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))
    }

    async fn delete_snapshot(&self, instance_id: &str) -> Result<(), BackendError> {
        let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;
        snapshots
            .remove(instance_id)
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))
            .map(|_| ())
    }

    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError> {
        let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
        Ok(snapshots.keys().cloned().collect())
    }

    async fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        ttl: Option<Duration>,
    ) -> Result<Option<TaskClaim>, BackendError> {
        let key = Self::claim_key(instance_id, task_id);
        let mut claims = self.claims.write().map_err(Self::lock_error)?;

        // Check if already claimed and not expired
        if let Some(existing_claim) = claims.get(&key) {
            if !existing_claim.is_expired() {
                return Ok(None); // Already claimed
            }
            // Expired claim, remove it
            claims.remove(&key);
        }

        // Create new claim
        let claim = TaskClaim::new(
            instance_id.to_string(),
            task_id.to_string(),
            worker_id.to_string(),
            ttl,
        );
        claims.insert(key, claim.clone());
        Ok(Some(claim))
    }

    async fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> Result<(), BackendError> {
        let key = Self::claim_key(instance_id, task_id);
        let mut claims = self.claims.write().map_err(Self::lock_error)?;

        if let Some(claim) = claims.get(&key) {
            if claim.worker_id != worker_id {
                return Err(BackendError::Backend(format!(
                    "Claim owned by different worker: {}",
                    claim.worker_id
                )));
            }
            claims.remove(&key);
            Ok(())
        } else {
            Err(BackendError::NotFound(format!(
                "{}:{}",
                instance_id, task_id
            )))
        }
    }

    async fn extend_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        additional_duration: Duration,
    ) -> Result<(), BackendError> {
        let key = Self::claim_key(instance_id, task_id);
        let mut claims = self.claims.write().map_err(Self::lock_error)?;

        if let Some(claim) = claims.get_mut(&key) {
            if claim.worker_id != worker_id {
                return Err(BackendError::Backend(format!(
                    "Claim owned by different worker: {}",
                    claim.worker_id
                )));
            }
            if let Some(expires_at) = claim.expires_at {
                let expires_datetime = chrono::DateTime::from_timestamp(expires_at as i64, 0)
                    .ok_or_else(|| BackendError::Backend("Invalid timestamp".to_string()))?;
                let new_expiry = expires_datetime
                    .checked_add_signed(additional_duration)
                    .ok_or_else(|| BackendError::Backend("Time overflow".to_string()))?;
                claim.expires_at = Some(new_expiry.timestamp() as u64);
            }
            Ok(())
        } else {
            Err(BackendError::NotFound(format!(
                "{}:{}",
                instance_id, task_id
            )))
        }
    }

    async fn find_available_tasks(
        &self,
        _worker_id: &str,
        limit: usize,
    ) -> Result<Vec<AvailableTask>, BackendError> {
        // Clean up expired claims first
        {
            let mut claims = self.claims.write().map_err(Self::lock_error)?;
            claims.retain(|_, claim| !claim.is_expired());
        }

        let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
        let claims = self.claims.read().map_err(Self::lock_error)?;
        let cancellation_requests = self
            .cancellation_requests
            .read()
            .map_err(Self::lock_error)?;

        let mut available = Vec::new();

        for (instance_id, snapshot) in snapshots.iter() {
            if !snapshot.state.is_in_progress() {
                continue;
            }

            // Skip workflows with pending cancellation requests
            if cancellation_requests.contains_key(instance_id) {
                continue;
            }

            // This is a simplified version - in a real implementation, we'd need
            // to traverse the workflow continuation to find ready tasks.
            // For now, we'll return tasks that are not completed and not claimed.
            if let WorkflowSnapshotState::InProgress {
                completed_tasks,
                position: ExecutionPosition::AtTask { task_id },
                ..
            } = &snapshot.state
            {
                let claim_key = Self::claim_key(instance_id, task_id);
                let is_claimed = claims.contains_key(&claim_key);
                let is_completed = completed_tasks.contains_key(task_id);

                if !is_completed && !is_claimed {
                    // Find input for this task using deterministic last task output
                    let input = if completed_tasks.is_empty() {
                        snapshot.initial_input_bytes()
                    } else {
                        snapshot.get_last_task_output()
                    };

                    if let Some(input_bytes) = input {
                        available.push(AvailableTask {
                            instance_id: instance_id.clone(),
                            task_id: task_id.clone(),
                            input: input_bytes,
                            workflow_definition_hash: snapshot.definition_hash.clone(),
                        });

                        if available.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        Ok(available)
    }

    async fn request_cancellation(
        &self,
        instance_id: &str,
        request: CancellationRequest,
    ) -> Result<(), BackendError> {
        // Check if workflow exists and is in a cancellable state
        {
            let snapshots = self.snapshots.read().map_err(Self::lock_error)?;

            let snapshot = snapshots
                .get(instance_id)
                .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

            // Cannot cancel workflows in terminal states
            if snapshot.state.is_completed() {
                return Err(BackendError::CannotCancel("Completed".to_string()));
            }
            if snapshot.state.is_failed() {
                return Err(BackendError::CannotCancel("Failed".to_string()));
            }
            if snapshot.state.is_cancelled() {
                // Already cancelled - idempotent success
                return Ok(());
            }
        }

        // Store the cancellation request
        let mut cancellation_requests = self
            .cancellation_requests
            .write()
            .map_err(Self::lock_error)?;

        cancellation_requests.insert(instance_id.to_string(), request);
        Ok(())
    }

    async fn get_cancellation_request(
        &self,
        instance_id: &str,
    ) -> Result<Option<CancellationRequest>, BackendError> {
        let cancellation_requests = self
            .cancellation_requests
            .read()
            .map_err(Self::lock_error)?;

        Ok(cancellation_requests.get(instance_id).cloned())
    }

    async fn clear_cancellation_request(&self, instance_id: &str) -> Result<(), BackendError> {
        let mut cancellation_requests = self
            .cancellation_requests
            .write()
            .map_err(Self::lock_error)?;

        cancellation_requests.remove(instance_id);
        Ok(())
    }

    async fn check_and_cancel(
        &self,
        instance_id: &str,
        interrupted_at_task: Option<&str>,
    ) -> Result<bool, BackendError> {
        // Get cancellation request
        let request = {
            let cancellation_requests = self
                .cancellation_requests
                .read()
                .map_err(Self::lock_error)?;

            match cancellation_requests.get(instance_id) {
                Some(req) => req.clone(),
                None => return Ok(false),
            }
        };

        // Update snapshot to cancelled state
        {
            let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;

            let snapshot = snapshots
                .get_mut(instance_id)
                .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

            // Only cancel if still in progress
            if !snapshot.state.is_in_progress() {
                return Ok(false);
            }

            snapshot.mark_cancelled(
                request.reason,
                request.requested_by,
                interrupted_at_task.map(String::from),
            );
        }

        // Clear the cancellation request
        {
            let mut cancellation_requests = self
                .cancellation_requests
                .write()
                .map_err(Self::lock_error)?;

            cancellation_requests.remove(instance_id);
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_save_and_load() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());

        backend.save_snapshot(snapshot.clone()).await.unwrap();
        let loaded = backend.load_snapshot("test-123").await.unwrap();

        assert_eq!(snapshot.instance_id, loaded.instance_id);
        assert_eq!(snapshot.definition_hash, loaded.definition_hash);
    }

    #[tokio::test]
    async fn test_not_found() {
        let backend = InMemoryBackend::new();
        let result = backend.load_snapshot("nonexistent").await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_delete() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());

        backend.save_snapshot(snapshot).await.unwrap();
        backend.delete_snapshot("test-123").await.unwrap();

        let result = backend.load_snapshot("test-123").await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_list_snapshots() {
        let backend = InMemoryBackend::new();

        backend
            .save_snapshot(WorkflowSnapshot::new(
                "test-1".to_string(),
                "hash-1".to_string(),
            ))
            .await
            .unwrap();
        backend
            .save_snapshot(WorkflowSnapshot::new(
                "test-2".to_string(),
                "hash-2".to_string(),
            ))
            .await
            .unwrap();

        let list = backend.list_snapshots().await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.contains(&"test-1".to_string()));
        assert!(list.contains(&"test-2".to_string()));
    }

    // Task claim tests

    #[tokio::test]
    async fn test_claim_task_success() {
        let backend = InMemoryBackend::new();

        let claim = backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-1",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();

        assert!(claim.is_some());
        let claim = claim.unwrap();
        assert_eq!(claim.instance_id, "workflow-1");
        assert_eq!(claim.task_id, "task-1");
        assert_eq!(claim.worker_id, "worker-1");
        assert!(claim.expires_at.is_some());
    }

    #[tokio::test]
    async fn test_claim_task_already_claimed() {
        let backend = InMemoryBackend::new();

        // First claim succeeds
        let claim1 = backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-1",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();
        assert!(claim1.is_some());

        // Second claim by different worker fails
        let claim2 = backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-2",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();
        assert!(claim2.is_none());
    }

    #[tokio::test]
    async fn test_claim_task_expired_claim_replaced() {
        let backend = InMemoryBackend::new();

        // Create a claim with 0 TTL (immediately expired)
        let claim1 = backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-1",
                Some(Duration::seconds(0)),
            )
            .await
            .unwrap();
        assert!(claim1.is_some());

        // Second claim should succeed because first is expired (0-second TTL)
        let claim2 = backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-2",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();
        assert!(claim2.is_some());
        let claim2 = claim2.unwrap();
        assert_eq!(claim2.worker_id, "worker-2");
    }

    #[tokio::test]
    async fn test_claim_task_no_ttl() {
        let backend = InMemoryBackend::new();

        let claim = backend
            .claim_task("workflow-1", "task-1", "worker-1", None)
            .await
            .unwrap();

        assert!(claim.is_some());
        let claim = claim.unwrap();
        assert!(claim.expires_at.is_none());
        assert!(!claim.is_expired()); // Never expires
    }

    #[tokio::test]
    async fn test_release_task_claim_success() {
        let backend = InMemoryBackend::new();

        // Claim a task
        backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-1",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();

        // Release it
        let result = backend
            .release_task_claim("workflow-1", "task-1", "worker-1")
            .await;
        assert!(result.is_ok());

        // Can claim again
        let claim = backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-2",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();
        assert!(claim.is_some());
    }

    #[tokio::test]
    async fn test_release_task_claim_wrong_worker() {
        let backend = InMemoryBackend::new();

        // Claim a task as worker-1
        backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-1",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();

        // Try to release as worker-2
        let result = backend
            .release_task_claim("workflow-1", "task-1", "worker-2")
            .await;
        assert!(matches!(result, Err(BackendError::Backend(_))));
    }

    #[tokio::test]
    async fn test_release_task_claim_not_found() {
        let backend = InMemoryBackend::new();

        let result = backend
            .release_task_claim("workflow-1", "task-1", "worker-1")
            .await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_extend_task_claim_success() {
        let backend = InMemoryBackend::new();

        // Claim a task with short TTL
        let claim = backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-1",
                Some(Duration::seconds(10)),
            )
            .await
            .unwrap()
            .unwrap();
        let original_expiry = claim.expires_at.unwrap();

        // Extend it
        backend
            .extend_task_claim("workflow-1", "task-1", "worker-1", Duration::seconds(300))
            .await
            .unwrap();

        // Verify extension by checking internal state
        let claims = backend.claims.read().unwrap();
        let key = InMemoryBackend::claim_key("workflow-1", "task-1");
        let extended_claim = claims.get(&key).unwrap();
        assert!(extended_claim.expires_at.unwrap() > original_expiry);
    }

    #[tokio::test]
    async fn test_extend_task_claim_wrong_worker() {
        let backend = InMemoryBackend::new();

        // Claim a task as worker-1
        backend
            .claim_task(
                "workflow-1",
                "task-1",
                "worker-1",
                Some(Duration::seconds(300)),
            )
            .await
            .unwrap();

        // Try to extend as worker-2
        let result = backend
            .extend_task_claim("workflow-1", "task-1", "worker-2", Duration::seconds(300))
            .await;
        assert!(matches!(result, Err(BackendError::Backend(_))));
    }

    #[tokio::test]
    async fn test_extend_task_claim_not_found() {
        let backend = InMemoryBackend::new();

        let result = backend
            .extend_task_claim("workflow-1", "task-1", "worker-1", Duration::seconds(300))
            .await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_extend_task_claim_no_expiry() {
        let backend = InMemoryBackend::new();

        // Claim a task with no TTL
        backend
            .claim_task("workflow-1", "task-1", "worker-1", None)
            .await
            .unwrap();

        // Extending should succeed but not change anything (expires_at stays None)
        backend
            .extend_task_claim("workflow-1", "task-1", "worker-1", Duration::seconds(300))
            .await
            .unwrap();

        let claims = backend.claims.read().unwrap();
        let key = InMemoryBackend::claim_key("workflow-1", "task-1");
        let claim = claims.get(&key).unwrap();
        assert!(claim.expires_at.is_none());
    }

    #[tokio::test]
    async fn test_request_cancellation_success() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(snapshot).await.unwrap();

        let result = backend
            .request_cancellation(
                "test-123",
                CancellationRequest::new(
                    Some("User requested".to_string()),
                    Some("admin".to_string()),
                ),
            )
            .await;
        assert!(result.is_ok(), "request_cancellation should succeed");

        let stored = backend.get_cancellation_request("test-123").await.unwrap();
        assert!(stored.is_some(), "cancellation request should be stored");
        let stored = stored.unwrap();
        assert_eq!(stored.reason, Some("User requested".to_string()));
        assert_eq!(stored.requested_by, Some("admin".to_string()));
    }

    #[tokio::test]
    async fn test_request_cancellation_not_found() {
        let backend = InMemoryBackend::new();

        let result = backend
            .request_cancellation("nonexistent", CancellationRequest::new(None, None))
            .await;
        assert!(
            matches!(result, Err(BackendError::NotFound(_))),
            "should return NotFound for non-existent workflow"
        );
    }

    #[tokio::test]
    async fn test_request_cancellation_completed_workflow() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_completed(bytes::Bytes::from("result"));
        backend.save_snapshot(snapshot).await.unwrap();

        let result = backend
            .request_cancellation("test-123", CancellationRequest::new(None, None))
            .await;
        assert!(
            matches!(result, Err(BackendError::CannotCancel(_))),
            "should return CannotCancel for completed workflow"
        );
    }

    #[tokio::test]
    async fn test_request_cancellation_failed_workflow() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_failed("Some error".to_string());
        backend.save_snapshot(snapshot).await.unwrap();

        let result = backend
            .request_cancellation("test-123", CancellationRequest::new(None, None))
            .await;
        assert!(
            matches!(result, Err(BackendError::CannotCancel(_))),
            "should return CannotCancel for failed workflow"
        );
    }

    #[tokio::test]
    async fn test_request_cancellation_already_cancelled_idempotent() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_cancelled(Some("First cancel".to_string()), None, None);
        backend.save_snapshot(snapshot).await.unwrap();

        let result = backend
            .request_cancellation(
                "test-123",
                CancellationRequest::new(Some("Second cancel".to_string()), None),
            )
            .await;
        assert!(
            result.is_ok(),
            "cancelling already-cancelled workflow should be idempotent"
        );
    }

    #[tokio::test]
    async fn test_get_cancellation_request_none() {
        let backend = InMemoryBackend::new();

        let result = backend.get_cancellation_request("test-123").await.unwrap();
        assert!(
            result.is_none(),
            "should return None when no cancellation request exists"
        );
    }

    #[tokio::test]
    async fn test_clear_cancellation_request() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(snapshot).await.unwrap();

        backend
            .request_cancellation(
                "test-123",
                CancellationRequest::new(Some("Test".to_string()), None),
            )
            .await
            .unwrap();

        assert!(
            backend
                .get_cancellation_request("test-123")
                .await
                .unwrap()
                .is_some(),
            "cancellation request should exist before clearing"
        );

        backend
            .clear_cancellation_request("test-123")
            .await
            .unwrap();

        assert!(
            backend
                .get_cancellation_request("test-123")
                .await
                .unwrap()
                .is_none(),
            "cancellation request should be gone after clearing"
        );
    }

    #[tokio::test]
    async fn test_check_and_cancel_success() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(snapshot).await.unwrap();

        backend
            .request_cancellation(
                "test-123",
                CancellationRequest::new(Some("Timeout".to_string()), Some("system".to_string())),
            )
            .await
            .unwrap();

        let result = backend
            .check_and_cancel("test-123", Some("task-1"))
            .await
            .unwrap();
        assert!(
            result,
            "check_and_cancel should return true when cancellation pending"
        );

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        assert!(
            snapshot.state.is_cancelled(),
            "workflow should be in cancelled state"
        );

        let WorkflowSnapshotState::Cancelled {
            reason,
            cancelled_by,
            interrupted_at_task,
            ..
        } = &snapshot.state
        else {
            panic!("Expected Cancelled state");
        };
        assert_eq!(reason, &Some("Timeout".to_string()));
        assert_eq!(cancelled_by, &Some("system".to_string()));
        assert_eq!(interrupted_at_task, &Some("task-1".to_string()));

        assert!(
            backend
                .get_cancellation_request("test-123")
                .await
                .unwrap()
                .is_none(),
            "cancellation request should be cleared after check_and_cancel"
        );
    }

    #[tokio::test]
    async fn test_check_and_cancel_no_request() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(snapshot).await.unwrap();

        let result = backend.check_and_cancel("test-123", None).await.unwrap();
        assert!(
            !result,
            "check_and_cancel should return false when no cancellation pending"
        );

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        assert!(
            snapshot.state.is_in_progress(),
            "workflow should still be in progress"
        );
    }

    #[tokio::test]
    async fn test_check_and_cancel_not_in_progress() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_completed(bytes::Bytes::from("done"));
        backend.save_snapshot(snapshot).await.unwrap();

        // Add a cancellation request directly (bypassing state check)
        {
            let mut requests = backend.cancellation_requests.write().unwrap();
            requests.insert("test-123".to_string(), CancellationRequest::new(None, None));
        }

        let result = backend.check_and_cancel("test-123", None).await.unwrap();
        assert!(
            !result,
            "check_and_cancel should return false for non-in-progress workflow"
        );

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        assert!(
            snapshot.state.is_completed(),
            "workflow should still be completed"
        );
    }

    #[tokio::test]
    async fn test_find_available_tasks_skips_cancelled_workflows() {
        let backend = InMemoryBackend::new();

        let mut snapshot1 = WorkflowSnapshot::new("workflow-1".to_string(), "hash-abc".to_string());
        snapshot1.update_position(ExecutionPosition::AtTask {
            task_id: "task-1".to_string(),
        });
        backend.save_snapshot(snapshot1).await.unwrap();

        let mut snapshot2 = WorkflowSnapshot::new("workflow-2".to_string(), "hash-abc".to_string());
        snapshot2.update_position(ExecutionPosition::AtTask {
            task_id: "task-2".to_string(),
        });
        backend.save_snapshot(snapshot2).await.unwrap();

        backend
            .request_cancellation("workflow-1", CancellationRequest::new(None, None))
            .await
            .unwrap();

        let tasks = backend.find_available_tasks("worker-1", 10).await.unwrap();

        assert!(
            !tasks.iter().any(|t| t.instance_id == "workflow-1"),
            "workflow with pending cancellation should be skipped"
        );
    }
}
