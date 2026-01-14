//! In-memory implementation of PersistentBackend.
//!
//! This is a simple implementation that stores snapshots in a HashMap.
//! Useful for testing and as a reference implementation.

use crate::persistence::backend::{BackendError, PersistentBackend};
use crate::persistence::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};
use async_trait::async_trait;
use chrono::Duration;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use workflow_core::task_claim::AvailableTask;
use workflow_core::task_claim::TaskClaim;

/// In-memory backend that stores snapshots in a HashMap.
///
/// This implementation is thread-safe and suitable for testing.
/// For production use, consider implementing PersistentBackend for
/// a more durable storage backend (Redis, PostgreSQL, etc.).
///
/// # Example
///
/// ```rust,ignore
/// use workflow_runtime::persistence::{InMemoryBackend, PersistentBackend};
///
/// let backend = InMemoryBackend::new();
/// let snapshot = WorkflowSnapshot::new("instance-123", "hash-abc".to_string());
/// backend.save_snapshot(snapshot).await?;
/// ```
#[derive(Clone)]
pub struct InMemoryBackend {
    snapshots: Arc<RwLock<HashMap<String, WorkflowSnapshot>>>,
    claims: Arc<RwLock<HashMap<String, TaskClaim>>>, // Key: "{instance_id}:{task_id}"
}

impl InMemoryBackend {
    /// Create a new in-memory backend.
    pub fn new() -> Self {
        Self {
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            claims: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn claim_key(instance_id: &str, task_id: &str) -> String {
        format!("{}:{}", instance_id, task_id)
    }
}

impl Default for InMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PersistentBackend for InMemoryBackend {
    async fn save_snapshot(&self, snapshot: WorkflowSnapshot) -> Result<(), BackendError> {
        let instance_id = snapshot.instance_id.clone();
        let mut snapshots = self
            .snapshots
            .write()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;
        snapshots.insert(instance_id, snapshot);
        Ok(())
    }

    async fn load_snapshot(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        let snapshots = self
            .snapshots
            .read()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;
        snapshots
            .get(instance_id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))
    }

    async fn delete_snapshot(&self, instance_id: &str) -> Result<(), BackendError> {
        let mut snapshots = self
            .snapshots
            .write()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;
        snapshots
            .remove(instance_id)
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))
            .map(|_| ())
    }

    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError> {
        let snapshots = self
            .snapshots
            .read()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;
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
        let mut claims = self
            .claims
            .write()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;

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
        let mut claims = self
            .claims
            .write()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;

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
        let mut claims = self
            .claims
            .write()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;

        if let Some(claim) = claims.get_mut(&key) {
            if claim.worker_id != worker_id {
                return Err(BackendError::Backend(format!(
                    "Claim owned by different worker: {}",
                    claim.worker_id
                )));
            }
            if let Some(expires_at) = claim.expires_at {
                // Use chrono's DateTime for robust arithmetic
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
            let mut claims = self
                .claims
                .write()
                .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;
            claims.retain(|_, claim| !claim.is_expired());
        }

        let snapshots = self
            .snapshots
            .read()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;
        let claims = self
            .claims
            .read()
            .map_err(|e| BackendError::Backend(format!("Lock error: {}", e)))?;

        let mut available = Vec::new();

        for (instance_id, snapshot) in snapshots.iter() {
            if !snapshot.is_in_progress() {
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
            .claim_task("workflow-1", "task-1", "worker-1", Some(Duration::seconds(300)))
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
            .claim_task("workflow-1", "task-1", "worker-1", Some(Duration::seconds(300)))
            .await
            .unwrap();
        assert!(claim1.is_some());

        // Second claim by different worker fails
        let claim2 = backend
            .claim_task("workflow-1", "task-1", "worker-2", Some(Duration::seconds(300)))
            .await
            .unwrap();
        assert!(claim2.is_none());
    }

    #[tokio::test]
    async fn test_claim_task_expired_claim_replaced() {
        let backend = InMemoryBackend::new();

        // Create a claim with 0 TTL (immediately expired)
        let claim1 = backend
            .claim_task("workflow-1", "task-1", "worker-1", Some(Duration::seconds(0)))
            .await
            .unwrap();
        assert!(claim1.is_some());

        // Wait a moment to ensure expiration
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Second claim should succeed because first is expired
        let claim2 = backend
            .claim_task("workflow-1", "task-1", "worker-2", Some(Duration::seconds(300)))
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
            .claim_task("workflow-1", "task-1", "worker-1", Some(Duration::seconds(300)))
            .await
            .unwrap();

        // Release it
        let result = backend
            .release_task_claim("workflow-1", "task-1", "worker-1")
            .await;
        assert!(result.is_ok());

        // Can claim again
        let claim = backend
            .claim_task("workflow-1", "task-1", "worker-2", Some(Duration::seconds(300)))
            .await
            .unwrap();
        assert!(claim.is_some());
    }

    #[tokio::test]
    async fn test_release_task_claim_wrong_worker() {
        let backend = InMemoryBackend::new();

        // Claim a task as worker-1
        backend
            .claim_task("workflow-1", "task-1", "worker-1", Some(Duration::seconds(300)))
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
            .claim_task("workflow-1", "task-1", "worker-1", Some(Duration::seconds(10)))
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
            .claim_task("workflow-1", "task-1", "worker-1", Some(Duration::seconds(300)))
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
}
