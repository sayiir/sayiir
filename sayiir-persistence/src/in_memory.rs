//! In-memory implementation of the persistence traits.
//!
//! This is a simple implementation that stores snapshots in a HashMap.
//! Useful for testing and as a reference implementation.

use crate::backend::{BackendError, SignalStore, SnapshotStore, TaskClaimStore};
use chrono::{Duration, Utc};
use sayiir_core::snapshot::{
    ExecutionPosition, PauseRequest, SignalKind, SignalRequest, WorkflowSnapshot,
    WorkflowSnapshotState,
};
use sayiir_core::task_claim::{AvailableTask, TaskClaim};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// In-memory backend that stores snapshots in a HashMap.
///
/// This implementation is thread-safe and suitable for testing.
/// For production use, consider implementing the persistence traits for
/// a more durable storage backend (Redis, PostgreSQL, etc.).
#[derive(Clone, Default)]
pub struct InMemoryBackend {
    snapshots: Arc<RwLock<HashMap<String, WorkflowSnapshot>>>,
    claims: Arc<RwLock<HashMap<String, TaskClaim>>>, // Key: "{instance_id}:{task_id}"
    signals: Arc<RwLock<HashMap<(String, SignalKind), SignalRequest>>>,
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

// ---------------------------------------------------------------------------
// SnapshotStore
// ---------------------------------------------------------------------------

impl SnapshotStore for InMemoryBackend {
    async fn save_snapshot(&self, snapshot: &WorkflowSnapshot) -> Result<(), BackendError> {
        let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;
        snapshots.insert(snapshot.instance_id.clone(), snapshot.clone());
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
}

// ---------------------------------------------------------------------------
// SignalStore (overrides default composites for lock efficiency)
// ---------------------------------------------------------------------------

impl SignalStore for InMemoryBackend {
    async fn store_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
        request: SignalRequest,
    ) -> Result<(), BackendError> {
        // Validate that the workflow exists and is in a signalable state
        {
            let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
            let snapshot = snapshots
                .get(instance_id)
                .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

            match kind {
                SignalKind::Cancel => {
                    if snapshot.state.is_completed() {
                        return Err(BackendError::CannotCancel("Completed".to_string()));
                    }
                    if snapshot.state.is_failed() {
                        return Err(BackendError::CannotCancel("Failed".to_string()));
                    }
                    if snapshot.state.is_cancelled() {
                        return Ok(()); // idempotent
                    }
                }
                SignalKind::Pause => {
                    if snapshot.state.is_completed() {
                        return Err(BackendError::CannotPause("Completed".to_string()));
                    }
                    if snapshot.state.is_failed() {
                        return Err(BackendError::CannotPause("Failed".to_string()));
                    }
                    if snapshot.state.is_cancelled() {
                        return Err(BackendError::CannotPause("Cancelled".to_string()));
                    }
                    if snapshot.state.is_paused() {
                        return Ok(()); // idempotent
                    }
                }
            }
        }

        let mut signals = self.signals.write().map_err(Self::lock_error)?;
        signals.insert((instance_id.to_string(), kind), request);
        Ok(())
    }

    async fn get_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> Result<Option<SignalRequest>, BackendError> {
        let signals = self.signals.read().map_err(Self::lock_error)?;
        Ok(signals.get(&(instance_id.to_string(), kind)).cloned())
    }

    async fn clear_signal(&self, instance_id: &str, kind: SignalKind) -> Result<(), BackendError> {
        let mut signals = self.signals.write().map_err(Self::lock_error)?;
        signals.remove(&(instance_id.to_string(), kind));
        Ok(())
    }

    // Override check_and_cancel for more efficient locking (avoids load+save round-trip).
    async fn check_and_cancel(
        &self,
        instance_id: &str,
        interrupted_at_task: Option<&str>,
    ) -> Result<bool, BackendError> {
        let request = {
            let signals = self.signals.read().map_err(Self::lock_error)?;
            match signals.get(&(instance_id.to_string(), SignalKind::Cancel)) {
                Some(req) => req.clone(),
                None => return Ok(false),
            }
        };

        {
            let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;
            let snapshot = snapshots
                .get_mut(instance_id)
                .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;
            if !snapshot.state.is_in_progress() {
                return Ok(false);
            }
            snapshot.mark_cancelled(
                request.reason,
                request.requested_by,
                interrupted_at_task.map(String::from),
            );
        }

        {
            let mut signals = self.signals.write().map_err(Self::lock_error)?;
            signals.remove(&(instance_id.to_string(), SignalKind::Cancel));
        }

        Ok(true)
    }

    // Override check_and_pause for more efficient locking.
    async fn check_and_pause(&self, instance_id: &str) -> Result<bool, BackendError> {
        let request = {
            let signals = self.signals.read().map_err(Self::lock_error)?;
            match signals.get(&(instance_id.to_string(), SignalKind::Pause)) {
                Some(req) => req.clone(),
                None => return Ok(false),
            }
        };

        {
            let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;
            let snapshot = snapshots
                .get_mut(instance_id)
                .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;
            if !snapshot.state.is_in_progress() {
                return Ok(false);
            }
            let pause_request: PauseRequest = request.into();
            snapshot.mark_paused(&pause_request);
        }

        {
            let mut signals = self.signals.write().map_err(Self::lock_error)?;
            signals.remove(&(instance_id.to_string(), SignalKind::Pause));
        }

        Ok(true)
    }

    // Override unpause for more efficient locking.
    async fn unpause(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;

        let snapshot = snapshots
            .get_mut(instance_id)
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        if !snapshot.state.is_paused() {
            return Err(BackendError::CannotPause(format!(
                "Workflow is not paused (current state: {:?})",
                if snapshot.state.is_in_progress() {
                    "InProgress"
                } else if snapshot.state.is_completed() {
                    "Completed"
                } else if snapshot.state.is_failed() {
                    "Failed"
                } else if snapshot.state.is_cancelled() {
                    "Cancelled"
                } else {
                    "Unknown"
                }
            )));
        }

        snapshot.mark_unpaused();
        Ok(snapshot.clone())
    }
}

// ---------------------------------------------------------------------------
// TaskClaimStore
// ---------------------------------------------------------------------------

impl TaskClaimStore for InMemoryBackend {
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

        // Collect delay-expired workflows that need position advancement
        let mut delay_advances: Vec<(String, String)> = Vec::new();
        let mut delay_completions: Vec<(String, String)> = Vec::new();

        {
            let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
            let signals = self.signals.read().map_err(Self::lock_error)?;

            for (instance_id, snapshot) in snapshots.iter() {
                if !snapshot.state.is_in_progress() {
                    continue;
                }
                if signals.contains_key(&(instance_id.clone(), SignalKind::Cancel)) {
                    continue;
                }
                if signals.contains_key(&(instance_id.clone(), SignalKind::Pause)) {
                    continue;
                }
                if let WorkflowSnapshotState::InProgress {
                    position:
                        ExecutionPosition::AtDelay {
                            wake_at,
                            delay_id,
                            next_task_id,
                            ..
                        },
                    ..
                } = &snapshot.state
                    && Utc::now() >= *wake_at
                {
                    if let Some(next_id) = next_task_id {
                        delay_advances.push((instance_id.clone(), next_id.clone()));
                    } else {
                        delay_completions.push((instance_id.clone(), delay_id.clone()));
                    }
                }
            }
        }

        // Apply delay advancements with write lock
        if !delay_advances.is_empty() || !delay_completions.is_empty() {
            let mut snapshots = self.snapshots.write().map_err(Self::lock_error)?;
            for (instance_id, next_task_id) in &delay_advances {
                if let Some(snapshot) = snapshots.get_mut(instance_id) {
                    snapshot.update_position(ExecutionPosition::AtTask {
                        task_id: next_task_id.clone(),
                    });
                }
            }
            for (instance_id, delay_id) in &delay_completions {
                if let Some(snapshot) = snapshots.get_mut(instance_id) {
                    let output = snapshot.get_task_result_bytes(delay_id).unwrap_or_default();
                    snapshot.mark_completed(output);
                }
            }
        }

        let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
        let claims = self.claims.read().map_err(Self::lock_error)?;
        let signals = self.signals.read().map_err(Self::lock_error)?;

        let mut available = Vec::new();

        for (instance_id, snapshot) in snapshots.iter() {
            if !snapshot.state.is_in_progress() {
                continue;
            }

            // Skip workflows with pending cancellation or pause requests
            if signals.contains_key(&(instance_id.clone(), SignalKind::Cancel))
                || signals.contains_key(&(instance_id.clone(), SignalKind::Pause))
            {
                continue;
            }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SignalStore;
    use sayiir_core::snapshot::SignalKind;

    #[tokio::test]
    async fn test_save_and_load() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());

        backend.save_snapshot(&snapshot).await.unwrap();
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

        backend.save_snapshot(&snapshot).await.unwrap();
        backend.delete_snapshot("test-123").await.unwrap();

        let result = backend.load_snapshot("test-123").await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_list_snapshots() {
        let backend = InMemoryBackend::new();

        backend
            .save_snapshot(&WorkflowSnapshot::new(
                "test-1".to_string(),
                "hash-1".to_string(),
            ))
            .await
            .unwrap();
        backend
            .save_snapshot(&WorkflowSnapshot::new(
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
    async fn test_store_signal_cancel_success() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(
                    Some("User requested".to_string()),
                    Some("admin".to_string()),
                ),
            )
            .await;
        assert!(result.is_ok(), "store_signal should succeed");

        let stored = backend
            .get_signal("test-123", SignalKind::Cancel)
            .await
            .unwrap();
        assert!(stored.is_some(), "cancel signal should be stored");
        let stored = stored.unwrap();
        assert_eq!(stored.reason, Some("User requested".to_string()));
        assert_eq!(stored.requested_by, Some("admin".to_string()));
    }

    #[tokio::test]
    async fn test_store_signal_cancel_not_found() {
        let backend = InMemoryBackend::new();

        let result = backend
            .store_signal(
                "nonexistent",
                SignalKind::Cancel,
                SignalRequest::new(None, None),
            )
            .await;
        assert!(
            matches!(result, Err(BackendError::NotFound(_))),
            "should return NotFound for non-existent workflow"
        );
    }

    #[tokio::test]
    async fn test_store_signal_cancel_completed_workflow() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_completed(bytes::Bytes::from("result"));
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(None, None),
            )
            .await;
        assert!(
            matches!(result, Err(BackendError::CannotCancel(_))),
            "should return CannotCancel for completed workflow"
        );
    }

    #[tokio::test]
    async fn test_store_signal_cancel_failed_workflow() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_failed("Some error".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(None, None),
            )
            .await;
        assert!(
            matches!(result, Err(BackendError::CannotCancel(_))),
            "should return CannotCancel for failed workflow"
        );
    }

    #[tokio::test]
    async fn test_store_signal_cancel_already_cancelled_idempotent() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_cancelled(Some("First cancel".to_string()), None, None);
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("Second cancel".to_string()), None),
            )
            .await;
        assert!(
            result.is_ok(),
            "cancelling already-cancelled workflow should be idempotent"
        );
    }

    #[tokio::test]
    async fn test_get_signal_cancel_none() {
        let backend = InMemoryBackend::new();

        let result = backend
            .get_signal("test-123", SignalKind::Cancel)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "should return None when no cancellation signal exists"
        );
    }

    #[tokio::test]
    async fn test_clear_signal_cancel() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("Test".to_string()), None),
            )
            .await
            .unwrap();

        assert!(
            backend
                .get_signal("test-123", SignalKind::Cancel)
                .await
                .unwrap()
                .is_some(),
            "cancel signal should exist before clearing"
        );

        backend
            .clear_signal("test-123", SignalKind::Cancel)
            .await
            .unwrap();

        assert!(
            backend
                .get_signal("test-123", SignalKind::Cancel)
                .await
                .unwrap()
                .is_none(),
            "cancel signal should be gone after clearing"
        );
    }

    #[tokio::test]
    async fn test_store_signal_pause_completed_workflow() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_completed(bytes::Bytes::from("result"));
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(None, None),
            )
            .await;
        assert!(
            matches!(result, Err(BackendError::CannotPause(_))),
            "should return CannotPause for completed workflow"
        );
    }

    #[tokio::test]
    async fn test_store_signal_pause_failed_workflow() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_failed("Some error".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(None, None),
            )
            .await;
        assert!(
            matches!(result, Err(BackendError::CannotPause(_))),
            "should return CannotPause for failed workflow"
        );
    }

    #[tokio::test]
    async fn test_store_signal_pause_cancelled_workflow() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_cancelled(Some("done".to_string()), None, None);
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(None, None),
            )
            .await;
        assert!(
            matches!(result, Err(BackendError::CannotPause(_))),
            "should return CannotPause for cancelled workflow"
        );
    }

    #[tokio::test]
    async fn test_store_signal_pause_already_paused_idempotent() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_paused(&PauseRequest::new(Some("first".to_string()), None));
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(Some("second".to_string()), None),
            )
            .await;
        assert!(
            result.is_ok(),
            "pausing already-paused workflow should be idempotent"
        );
    }

    #[tokio::test]
    async fn test_store_signal_pause_not_found() {
        let backend = InMemoryBackend::new();
        let result = backend
            .store_signal(
                "nonexistent",
                SignalKind::Pause,
                SignalRequest::new(None, None),
            )
            .await;
        assert!(
            matches!(result, Err(BackendError::NotFound(_))),
            "should return NotFound for non-existent workflow"
        );
    }

    #[tokio::test]
    async fn test_check_and_cancel_success() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("Timeout".to_string()), Some("system".to_string())),
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
                .get_signal("test-123", SignalKind::Cancel)
                .await
                .unwrap()
                .is_none(),
            "cancel signal should be cleared after check_and_cancel"
        );
    }

    #[tokio::test]
    async fn test_check_and_cancel_no_request() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

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
        backend.save_snapshot(&snapshot).await.unwrap();

        // Add a cancel signal directly (bypassing state check)
        {
            let mut signals = backend.signals.write().unwrap();
            signals.insert(
                ("test-123".to_string(), SignalKind::Cancel),
                SignalRequest::new(None, None),
            );
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
        backend.save_snapshot(&snapshot1).await.unwrap();

        let mut snapshot2 = WorkflowSnapshot::new("workflow-2".to_string(), "hash-abc".to_string());
        snapshot2.update_position(ExecutionPosition::AtTask {
            task_id: "task-2".to_string(),
        });
        backend.save_snapshot(&snapshot2).await.unwrap();

        backend
            .store_signal(
                "workflow-1",
                SignalKind::Cancel,
                SignalRequest::new(None, None),
            )
            .await
            .unwrap();

        let tasks = backend.find_available_tasks("worker-1", 10).await.unwrap();

        assert!(
            !tasks.iter().any(|t| t.instance_id == "workflow-1"),
            "workflow with pending cancellation should be skipped"
        );
    }

    // ========================================================================
    // check_and_pause tests
    // ========================================================================

    #[tokio::test]
    async fn test_check_and_pause_success() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(Some("maintenance".to_string()), Some("ops".to_string())),
            )
            .await
            .unwrap();

        let result = backend.check_and_pause("test-123").await.unwrap();
        assert!(
            result,
            "check_and_pause should return true when pause pending"
        );

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        assert!(snapshot.state.is_paused(), "workflow should be paused");

        let WorkflowSnapshotState::Paused {
            reason, paused_by, ..
        } = &snapshot.state
        else {
            panic!("Expected Paused state");
        };
        assert_eq!(reason, &Some("maintenance".to_string()));
        assert_eq!(paused_by, &Some("ops".to_string()));

        assert!(
            backend
                .get_signal("test-123", SignalKind::Pause)
                .await
                .unwrap()
                .is_none(),
            "pause signal should be cleared after check_and_pause"
        );
    }

    #[tokio::test]
    async fn test_check_and_pause_no_request() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend.check_and_pause("test-123").await.unwrap();
        assert!(
            !result,
            "check_and_pause should return false when no pause pending"
        );

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        assert!(
            snapshot.state.is_in_progress(),
            "workflow should still be in progress"
        );
    }

    #[tokio::test]
    async fn test_check_and_pause_not_in_progress() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_completed(bytes::Bytes::from("done"));
        backend.save_snapshot(&snapshot).await.unwrap();

        // Add a pause signal directly (bypassing state check)
        {
            let mut signals = backend.signals.write().unwrap();
            signals.insert(
                ("test-123".to_string(), SignalKind::Pause),
                SignalRequest::new(None, None),
            );
        }

        let result = backend.check_and_pause("test-123").await.unwrap();
        assert!(
            !result,
            "check_and_pause should return false for non-in-progress workflow"
        );

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        assert!(
            snapshot.state.is_completed(),
            "workflow should still be completed"
        );
    }

    #[tokio::test]
    async fn test_check_and_pause_preserves_position() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "task-3".to_string(),
        });
        snapshot.mark_task_completed("task-1".to_string(), bytes::Bytes::from("out1"));
        snapshot.mark_task_completed("task-2".to_string(), bytes::Bytes::from("out2"));
        backend.save_snapshot(&snapshot).await.unwrap();

        backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(None, None),
            )
            .await
            .unwrap();

        backend.check_and_pause("test-123").await.unwrap();

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        let WorkflowSnapshotState::Paused {
            completed_tasks,
            position,
            last_completed_task_id,
            ..
        } = &snapshot.state
        else {
            panic!("Expected Paused state");
        };

        assert_eq!(completed_tasks.len(), 2);
        assert!(completed_tasks.contains_key("task-1"));
        assert!(completed_tasks.contains_key("task-2"));
        assert!(matches!(
            position,
            ExecutionPosition::AtTask { task_id } if task_id == "task-3"
        ));
        assert_eq!(last_completed_task_id, &Some("task-2".to_string()));
    }

    // ========================================================================
    // unpause tests
    // ========================================================================

    #[tokio::test]
    async fn test_unpause_success() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "task-2".to_string(),
        });
        snapshot.mark_task_completed("task-1".to_string(), bytes::Bytes::from("out1"));
        snapshot.mark_paused(&PauseRequest::new(
            Some("maintenance".to_string()),
            Some("ops".to_string()),
        ));
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend.unpause("test-123").await.unwrap();

        assert!(
            result.state.is_in_progress(),
            "unpaused workflow should be in progress"
        );

        // Verify position and tasks were restored
        let WorkflowSnapshotState::InProgress {
            position,
            completed_tasks,
            last_completed_task_id,
        } = &result.state
        else {
            panic!("Expected InProgress state");
        };
        assert!(matches!(
            position,
            ExecutionPosition::AtTask { task_id } if task_id == "task-2"
        ));
        assert!(completed_tasks.contains_key("task-1"));
        assert_eq!(last_completed_task_id, &Some("task-1".to_string()));

        // Verify persisted state matches
        let loaded = backend.load_snapshot("test-123").await.unwrap();
        assert!(loaded.state.is_in_progress());
    }

    #[tokio::test]
    async fn test_unpause_not_paused_errors() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend.unpause("test-123").await;
        assert!(
            matches!(result, Err(BackendError::CannotPause(_))),
            "unpause on in-progress workflow should error"
        );
    }

    #[tokio::test]
    async fn test_unpause_completed_errors() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        snapshot.mark_completed(bytes::Bytes::from("done"));
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend.unpause("test-123").await;
        assert!(
            matches!(result, Err(BackendError::CannotPause(_))),
            "unpause on completed workflow should error"
        );
    }

    #[tokio::test]
    async fn test_unpause_not_found() {
        let backend = InMemoryBackend::new();
        let result = backend.unpause("nonexistent").await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }

    // ========================================================================
    // Concurrent signals tests
    // ========================================================================

    #[tokio::test]
    async fn test_cancel_and_pause_simultaneously_cancel_wins() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        // Store both signals
        backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("cancel reason".to_string()), None),
            )
            .await
            .unwrap();
        backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(Some("pause reason".to_string()), None),
            )
            .await
            .unwrap();

        // check_and_cancel should process the cancel signal
        let cancelled = backend
            .check_and_cancel("test-123", Some("task-1"))
            .await
            .unwrap();
        assert!(cancelled, "cancel should succeed");

        // Now check_and_pause — workflow is already cancelled (not in progress)
        let paused = backend.check_and_pause("test-123").await.unwrap();
        assert!(
            !paused,
            "pause should return false since workflow is already cancelled"
        );

        let snapshot = backend.load_snapshot("test-123").await.unwrap();
        assert!(snapshot.state.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_signal_independent_of_pause_signal() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        // Store both signals
        backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("cancel".to_string()), None),
            )
            .await
            .unwrap();
        backend
            .store_signal(
                "test-123",
                SignalKind::Pause,
                SignalRequest::new(Some("pause".to_string()), None),
            )
            .await
            .unwrap();

        // Clear only cancel
        backend
            .clear_signal("test-123", SignalKind::Cancel)
            .await
            .unwrap();

        // Cancel should be gone, pause should remain
        assert!(
            backend
                .get_signal("test-123", SignalKind::Cancel)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            backend
                .get_signal("test-123", SignalKind::Pause)
                .await
                .unwrap()
                .is_some()
        );
    }

    // ========================================================================
    // find_available_tasks + pause signal
    // ========================================================================

    #[tokio::test]
    async fn test_find_available_tasks_skips_paused_workflows() {
        let backend = InMemoryBackend::new();

        let mut snapshot1 = WorkflowSnapshot::with_initial_input(
            "workflow-1".to_string(),
            "hash-abc".to_string(),
            bytes::Bytes::from(vec![1]),
        );
        snapshot1.update_position(ExecutionPosition::AtTask {
            task_id: "task-1".to_string(),
        });
        backend.save_snapshot(&snapshot1).await.unwrap();

        let mut snapshot2 = WorkflowSnapshot::with_initial_input(
            "workflow-2".to_string(),
            "hash-abc".to_string(),
            bytes::Bytes::from(vec![2]),
        );
        snapshot2.update_position(ExecutionPosition::AtTask {
            task_id: "task-2".to_string(),
        });
        backend.save_snapshot(&snapshot2).await.unwrap();

        // Pause workflow-1
        backend
            .store_signal(
                "workflow-1",
                SignalKind::Pause,
                SignalRequest::new(None, None),
            )
            .await
            .unwrap();

        let tasks = backend.find_available_tasks("worker-1", 10).await.unwrap();

        assert!(
            !tasks.iter().any(|t| t.instance_id == "workflow-1"),
            "workflow with pending pause should be skipped"
        );
        assert!(
            tasks.iter().any(|t| t.instance_id == "workflow-2"),
            "workflow without signals should be available"
        );
    }

    // ========================================================================
    // Orphaned signals
    // ========================================================================

    #[tokio::test]
    async fn test_delete_snapshot_leaves_orphaned_signals() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("reason".to_string()), None),
            )
            .await
            .unwrap();

        // Delete the snapshot
        backend.delete_snapshot("test-123").await.unwrap();

        // Signal is still there (orphaned) — this documents current behavior
        let signal = backend
            .get_signal("test-123", SignalKind::Cancel)
            .await
            .unwrap();
        assert!(
            signal.is_some(),
            "signal persists after snapshot deletion (orphaned)"
        );
    }

    #[tokio::test]
    async fn test_store_signal_overwrites_previous() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("test-123".to_string(), "hash-abc".to_string());
        backend.save_snapshot(&snapshot).await.unwrap();

        backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("first".to_string()), None),
            )
            .await
            .unwrap();
        backend
            .store_signal(
                "test-123",
                SignalKind::Cancel,
                SignalRequest::new(Some("second".to_string()), None),
            )
            .await
            .unwrap();

        let signal = backend
            .get_signal("test-123", SignalKind::Cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            signal.reason,
            Some("second".to_string()),
            "latest signal should overwrite previous"
        );
    }

    // ========================================================================
    // Delay tests
    // ========================================================================

    #[tokio::test]
    async fn test_find_available_tasks_skips_unexpired_delay() {
        let backend = InMemoryBackend::new();

        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "workflow-1".to_string(),
            "hash-abc".to_string(),
            bytes::Bytes::from(vec![42]),
        );
        // Park at a delay that expires in the future
        let wake_at = Utc::now() + chrono::Duration::hours(1);
        snapshot.update_position(ExecutionPosition::AtDelay {
            delay_id: "wait_1h".to_string(),
            entered_at: Utc::now(),
            wake_at,
            next_task_id: Some("next_step".to_string()),
        });
        snapshot.mark_task_completed("wait_1h".to_string(), bytes::Bytes::from(vec![42]));
        backend.save_snapshot(&snapshot).await.unwrap();

        let tasks = backend.find_available_tasks("worker-1", 10).await.unwrap();
        assert!(
            tasks.is_empty(),
            "workflow at unexpired delay should not appear in available tasks"
        );
    }

    #[tokio::test]
    async fn test_find_available_tasks_advances_expired_delay() {
        let backend = InMemoryBackend::new();

        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "workflow-1".to_string(),
            "hash-abc".to_string(),
            bytes::Bytes::from(vec![42]),
        );
        // Park at a delay that has already expired
        let wake_at = Utc::now() - chrono::Duration::seconds(1);
        snapshot.update_position(ExecutionPosition::AtDelay {
            delay_id: "wait_done".to_string(),
            entered_at: Utc::now() - chrono::Duration::seconds(2),
            wake_at,
            next_task_id: Some("process".to_string()),
        });
        snapshot.mark_task_completed("wait_done".to_string(), bytes::Bytes::from(vec![42]));
        backend.save_snapshot(&snapshot).await.unwrap();

        let tasks = backend.find_available_tasks("worker-1", 10).await.unwrap();

        // The delay has expired, so the position should have been advanced to "process"
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].instance_id, "workflow-1");
        assert_eq!(tasks[0].task_id, "process");

        // Verify position was advanced in the snapshot
        let loaded = backend.load_snapshot("workflow-1").await.unwrap();
        match &loaded.state {
            WorkflowSnapshotState::InProgress {
                position: ExecutionPosition::AtTask { task_id },
                ..
            } => {
                assert_eq!(task_id, "process");
            }
            other => panic!("Expected AtTask position, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_find_available_tasks_completes_expired_delay_last_node() {
        let backend = InMemoryBackend::new();

        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "workflow-1".to_string(),
            "hash-abc".to_string(),
            bytes::Bytes::from(vec![42]),
        );
        // Park at a delay that has expired AND has no next task (delay is last node)
        let wake_at = Utc::now() - chrono::Duration::seconds(1);
        snapshot.update_position(ExecutionPosition::AtDelay {
            delay_id: "final_wait".to_string(),
            entered_at: Utc::now() - chrono::Duration::seconds(2),
            wake_at,
            next_task_id: None,
        });
        snapshot.mark_task_completed("final_wait".to_string(), bytes::Bytes::from(vec![42]));
        backend.save_snapshot(&snapshot).await.unwrap();

        let tasks = backend.find_available_tasks("worker-1", 10).await.unwrap();

        // No available tasks — the workflow should have been marked completed
        assert!(
            tasks.is_empty(),
            "completed workflow should not appear in available tasks"
        );

        // Verify workflow was marked completed
        let loaded = backend.load_snapshot("workflow-1").await.unwrap();
        assert!(
            loaded.state.is_completed(),
            "workflow should be completed when delay is last node and expired"
        );
    }
}
