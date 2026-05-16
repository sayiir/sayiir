//! In-memory implementation of the persistence traits.
//!
//! This is a simple implementation that stores snapshots in a HashMap.
//! Useful for testing and as a reference implementation.

use crate::backend::{BackendError, SignalStore, SnapshotStore, TaskClaimStore, TaskResultStore};
use chrono::{Duration, Utc};
use sayiir_core::snapshot::{
    ExecutionPosition, PauseRequest, SignalKind, SignalRequest, TaskResult, WorkflowSnapshot,
    WorkflowSnapshotState,
};
use sayiir_core::task_claim::{AvailableTask, TaskClaim};
use std::collections::{HashMap, VecDeque};
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
    signals: Arc<RwLock<HashMap<String, HashMap<SignalKind, SignalRequest>>>>,
    /// Buffered external events per `(instance_id, signal_name)`, FIFO order.
    #[allow(clippy::type_complexity)]
    events: Arc<RwLock<HashMap<(String, String), VecDeque<bytes::Bytes>>>>,
    /// Cached task results for workflows that have transitioned to terminal states
    /// (Completed/Failed), where `completed_tasks` is no longer in the snapshot.
    task_results_cache: Arc<RwLock<HashMap<String, HashMap<String, TaskResult>>>>,
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

        // When transitioning to Completed/Failed, cache the previous snapshot's
        // completed_tasks so they can still be retrieved via TaskResultStore.
        if (snapshot.state.is_completed() || snapshot.state.is_failed())
            && let Some(prev) = snapshots.get(&snapshot.instance_id)
            && let Some(tasks) = prev.get_all_task_results()
            && !tasks.is_empty()
        {
            let mut cache = self.task_results_cache.write().map_err(Self::lock_error)?;
            cache.insert(snapshot.instance_id.clone(), tasks.clone());
        }

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
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;
        // Clean up the task results cache for this instance.
        if let Ok(mut cache) = self.task_results_cache.write() {
            cache.remove(instance_id);
        }
        Ok(())
    }

    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError> {
        let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
        Ok(snapshots.keys().cloned().collect())
    }
}

// ---------------------------------------------------------------------------
// TaskResultStore
// ---------------------------------------------------------------------------

impl TaskResultStore for InMemoryBackend {
    async fn load_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
    ) -> Result<Option<bytes::Bytes>, BackendError> {
        let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
        let snapshot = snapshots
            .get(instance_id)
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        // Try the current snapshot first (works for InProgress/Cancelled/Paused).
        if let Some(result) = snapshot.get_task_result_bytes(task_id) {
            return Ok(Some(result));
        }

        // For terminal states (Completed/Failed), fall back to the cache.
        if snapshot.state.is_completed() || snapshot.state.is_failed() {
            let cache = self.task_results_cache.read().map_err(Self::lock_error)?;
            if let Some(tasks) = cache.get(instance_id) {
                return Ok(tasks.get(task_id).map(|r| r.output.clone()));
            }
        }

        Ok(None)
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
        signals
            .entry(instance_id.to_string())
            .or_default()
            .insert(kind, request);
        Ok(())
    }

    async fn get_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> Result<Option<SignalRequest>, BackendError> {
        let signals = self.signals.read().map_err(Self::lock_error)?;
        Ok(signals.get(instance_id).and_then(|m| m.get(&kind)).cloned())
    }

    async fn clear_signal(&self, instance_id: &str, kind: SignalKind) -> Result<(), BackendError> {
        let mut signals = self.signals.write().map_err(Self::lock_error)?;
        if let Some(inner) = signals.get_mut(instance_id) {
            inner.remove(&kind);
            if inner.is_empty() {
                signals.remove(instance_id);
            }
        }
        Ok(())
    }

    async fn send_event(
        &self,
        instance_id: &str,
        signal_name: &str,
        payload: bytes::Bytes,
    ) -> Result<(), BackendError> {
        let mut events = self.events.write().map_err(Self::lock_error)?;
        events
            .entry((instance_id.to_string(), signal_name.to_string()))
            .or_default()
            .push_back(payload);
        Ok(())
    }

    async fn consume_event(
        &self,
        instance_id: &str,
        signal_name: &str,
    ) -> Result<Option<bytes::Bytes>, BackendError> {
        let mut events = self.events.write().map_err(Self::lock_error)?;
        let key = (instance_id.to_string(), signal_name.to_string());
        let payload = events.get_mut(&key).and_then(VecDeque::pop_front);
        // Clean up empty queues
        if events.get(&key).is_some_and(VecDeque::is_empty) {
            events.remove(&key);
        }
        Ok(payload)
    }

    // Override check_and_cancel for more efficient locking (avoids load+save round-trip).
    async fn check_and_cancel(
        &self,
        instance_id: &str,
        interrupted_at_task: Option<&str>,
    ) -> Result<bool, BackendError> {
        let request = {
            let signals = self.signals.read().map_err(Self::lock_error)?;
            match signals
                .get(instance_id)
                .and_then(|m| m.get(&SignalKind::Cancel))
            {
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
            if let Some(inner) = signals.get_mut(instance_id) {
                inner.remove(&SignalKind::Cancel);
                if inner.is_empty() {
                    signals.remove(instance_id);
                }
            }
        }

        Ok(true)
    }

    // Override check_and_pause for more efficient locking.
    async fn check_and_pause(&self, instance_id: &str) -> Result<bool, BackendError> {
        let request = {
            let signals = self.signals.read().map_err(Self::lock_error)?;
            match signals
                .get(instance_id)
                .and_then(|m| m.get(&SignalKind::Pause))
            {
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
            if let Some(inner) = signals.get_mut(instance_id) {
                inner.remove(&SignalKind::Pause);
                if inner.is_empty() {
                    signals.remove(instance_id);
                }
            }
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
        worker_id: &str,
        limit: usize,
        aging_interval: chrono::Duration,
        worker_tags: &[String],
    ) -> Result<Vec<AvailableTask>, BackendError> {
        // Clean up expired claims first
        {
            let mut claims = self.claims.write().map_err(Self::lock_error)?;
            claims.retain(|_, claim| !claim.is_expired());
        }

        // Collect delay-expired workflows that need position advancement
        let mut delay_advances: Vec<(String, String)> = Vec::new();
        let mut delay_completions: Vec<(String, String)> = Vec::new();
        // Signal-related advancements: (instance_id, signal_name, next_task_id_or_none)
        let mut signal_advances: Vec<(String, String, Option<String>)> = Vec::new();
        // Signal timeout expirations: (instance_id, signal_id, next_task_id_or_none)
        let mut signal_timeout_advances: Vec<(String, String, Option<String>)> = Vec::new();

        {
            let snapshots = self.snapshots.read().map_err(Self::lock_error)?;
            let signals = self.signals.read().map_err(Self::lock_error)?;
            let events = self.events.read().map_err(Self::lock_error)?;

            for (instance_id, snapshot) in snapshots.iter() {
                if !snapshot.state.is_in_progress() {
                    continue;
                }
                if signals
                    .get(instance_id.as_str())
                    .is_some_and(|m| m.contains_key(&SignalKind::Cancel))
                {
                    continue;
                }
                if signals
                    .get(instance_id.as_str())
                    .is_some_and(|m| m.contains_key(&SignalKind::Pause))
                {
                    continue;
                }
                match &snapshot.state {
                    WorkflowSnapshotState::InProgress {
                        position:
                            ExecutionPosition::AtDelay {
                                wake_at,
                                delay_id,
                                next_task_id,
                                ..
                            },
                        ..
                    } if Utc::now() >= *wake_at => {
                        if let Some(next_id) = next_task_id {
                            delay_advances.push((instance_id.clone(), next_id.clone()));
                        } else {
                            delay_completions.push((instance_id.clone(), delay_id.clone()));
                        }
                    }
                    WorkflowSnapshotState::InProgress {
                        position:
                            ExecutionPosition::AtSignal {
                                signal_name,
                                wake_at,
                                next_task_id,
                                ..
                            },
                        ..
                    } => {
                        let key = (instance_id.clone(), signal_name.clone());
                        if events.get(&key).is_some_and(|q| !q.is_empty()) {
                            // Signal arrived — advance
                            signal_advances.push((
                                instance_id.clone(),
                                signal_name.clone(),
                                next_task_id.clone(),
                            ));
                        } else if wake_at.is_some_and(|wt| Utc::now() >= wt) {
                            // Timeout expired — advance with None payload
                            signal_timeout_advances.push((
                                instance_id.clone(),
                                signal_name.clone(),
                                next_task_id.clone(),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }

        // Apply delay advancements with write lock
        if !delay_advances.is_empty()
            || !delay_completions.is_empty()
            || !signal_advances.is_empty()
            || !signal_timeout_advances.is_empty()
        {
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
            // Consume signal events and advance position
            {
                let mut events = self.events.write().map_err(Self::lock_error)?;
                for (instance_id, signal_name, next_task_id) in &signal_advances {
                    let key = (instance_id.clone(), signal_name.clone());
                    let payload = events
                        .get_mut(&key)
                        .and_then(VecDeque::pop_front)
                        .unwrap_or_default();
                    // Clean up empty queues
                    if events.get(&key).is_some_and(VecDeque::is_empty) {
                        events.remove(&key);
                    }
                    if let Some(snapshot) = snapshots.get_mut(instance_id) {
                        // Store signal payload as a task result so the next step can use it
                        snapshot.mark_task_completed(signal_name.clone(), payload);
                        if let Some(next_id) = next_task_id {
                            snapshot.update_position(ExecutionPosition::AtTask {
                                task_id: next_id.clone(),
                            });
                        } else {
                            let output = snapshot
                                .get_task_result_bytes(signal_name)
                                .unwrap_or_default();
                            snapshot.mark_completed(output);
                        }
                    }
                }
            }
            // Handle signal timeouts (advance with empty payload)
            for (instance_id, signal_name, next_task_id) in &signal_timeout_advances {
                if let Some(snapshot) = snapshots.get_mut(instance_id) {
                    snapshot.mark_task_completed(signal_name.clone(), bytes::Bytes::new());
                    if let Some(next_id) = next_task_id {
                        snapshot.update_position(ExecutionPosition::AtTask {
                            task_id: next_id.clone(),
                        });
                    } else {
                        snapshot.mark_completed(bytes::Bytes::new());
                    }
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
            if let Some(instance_signals) = signals.get(instance_id.as_str())
                && (instance_signals.contains_key(&SignalKind::Cancel)
                    || instance_signals.contains_key(&SignalKind::Pause))
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
                    // Skip tasks whose retry backoff has not elapsed yet
                    if let Some(rs) = snapshot.task_retries.get(task_id)
                        && Utc::now() < rs.next_retry_at
                    {
                        continue;
                    }

                    // Tag-based filtering: if worker has tags, only accept tasks
                    // whose tags are a subset of the worker's tags (or untagged).
                    if !worker_tags.is_empty() {
                        let task_tags = snapshot.current_task_tags();
                        if !task_tags.is_empty()
                            && !task_tags.iter().all(|t| worker_tags.contains(t))
                        {
                            continue;
                        }
                    }

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
                            trace_parent: None,
                        });

                        if available.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        // Sort by (worker_bias, effective_priority). Worker bias is primary
        // (avoid re-executing on the same worker that failed), then by effective
        // priority with aging. Lower effective priority = higher urgency.
        // Clamp to a minimum of 1s to prevent division by zero.
        #[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let aging_secs = (aging_interval.num_milliseconds() as f64 / 1000.0).max(1.0);
        #[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let now_ts = Utc::now().timestamp() as f64;
        available.sort_by(|a, b| {
            let worker_bias = |t: &AvailableTask| -> bool {
                snapshots
                    .get(&t.instance_id)
                    .is_some_and(|s| s.has_failed_on_worker(&t.task_id, worker_id))
            };
            let eff_priority = |t: &AvailableTask| -> f64 {
                let snap = snapshots.get(&t.instance_id);
                let base = snap.map_or(3.0, |s| f64::from(s.current_task_priority()));
                let updated = snap.map_or(now_ts, |s| s.updated_at as f64);
                let wait = now_ts - updated;
                base - wait / aging_secs
            };
            let ba = worker_bias(a);
            let bb = worker_bias(b);
            ba.cmp(&bb).then_with(|| {
                let ea = eff_priority(a);
                let eb = eff_priority(b);
                ea.partial_cmp(&eb).unwrap_or(std::cmp::Ordering::Equal)
            })
        });

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
            signals
                .entry("test-123".to_string())
                .or_default()
                .insert(SignalKind::Cancel, SignalRequest::new(None, None));
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

        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
            .await
            .unwrap();

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
            signals
                .entry("test-123".to_string())
                .or_default()
                .insert(SignalKind::Pause, SignalRequest::new(None, None));
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

        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
            .await
            .unwrap();

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

        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
            .await
            .unwrap();
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

        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
            .await
            .unwrap();

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

        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
            .await
            .unwrap();

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

    // ── Priority ordering & aging ────────────────────────────────────────

    #[tokio::test]
    async fn test_find_available_tasks_returns_higher_priority_first() {
        let backend = InMemoryBackend::new();
        let input = bytes::Bytes::from("input");

        // Create three workflows with different priorities:
        // wf-low (priority 5), wf-normal (priority 3), wf-high (priority 1)
        for (id, priority) in [("wf-low", 5u8), ("wf-normal", 3), ("wf-high", 1)] {
            let mut snapshot = WorkflowSnapshot::with_initial_input(
                id.to_string(),
                "hash".to_string(),
                input.clone(),
            );
            snapshot.task_priority = Some(priority);
            snapshot.update_position(ExecutionPosition::AtTask {
                task_id: "task-a".to_string(),
            });
            backend.save_snapshot(&snapshot).await.unwrap();
        }

        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
            .await
            .unwrap();

        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].instance_id, "wf-high");
        assert_eq!(tasks[1].instance_id, "wf-normal");
        assert_eq!(tasks[2].instance_id, "wf-low");
    }

    #[tokio::test]
    async fn test_find_available_tasks_aging_promotes_low_priority() {
        let backend = InMemoryBackend::new();
        let input = bytes::Bytes::from("input");

        // Fresh high-priority workflow (priority 1).
        let mut high = WorkflowSnapshot::with_initial_input(
            "wf-high".to_string(),
            "hash".to_string(),
            input.clone(),
        );
        high.task_priority = Some(1);
        high.update_position(ExecutionPosition::AtTask {
            task_id: "task-a".to_string(),
        });
        // updated_at stays at "now"
        backend.save_snapshot(&high).await.unwrap();

        // Low-priority workflow (priority 5) that has been waiting a long time.
        let mut low = WorkflowSnapshot::with_initial_input(
            "wf-low".to_string(),
            "hash".to_string(),
            input.clone(),
        );
        low.task_priority = Some(5);
        low.update_position(ExecutionPosition::AtTask {
            task_id: "task-a".to_string(),
        });
        // Simulate long wait: push updated_at 10 minutes into the past.
        low.updated_at = (chrono::Utc::now().timestamp() - 600) as u64;
        backend.save_snapshot(&low).await.unwrap();

        // With a 60s aging interval, the low-priority task's effective priority:
        //   5 - (600 / 60) = 5 - 10 = -5
        // The high-priority task's effective priority:
        //   1 - (~0 / 60) ≈ 1
        // So the aged low-priority task should come first.
        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::seconds(60), &[])
            .await
            .unwrap();

        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks[0].instance_id, "wf-low",
            "aged low-priority task should be promoted ahead of fresh high-priority task"
        );
        assert_eq!(tasks[1].instance_id, "wf-high");
    }

    #[tokio::test]
    async fn test_find_available_tasks_zero_aging_interval_no_panic() {
        let backend = InMemoryBackend::new();
        let input = bytes::Bytes::from("input");

        let mut snapshot =
            WorkflowSnapshot::with_initial_input("wf-1".to_string(), "hash".to_string(), input);
        snapshot.task_priority = Some(3);
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "task-a".to_string(),
        });
        backend.save_snapshot(&snapshot).await.unwrap();

        // Zero aging interval should not panic or divide by zero.
        let tasks = backend
            .find_available_tasks("worker-1", 10, chrono::Duration::zero(), &[])
            .await
            .unwrap();

        assert_eq!(tasks.len(), 1);
    }

    // ── Worker tag filtering ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_find_available_tasks_filters_by_worker_tags() {
        let backend = InMemoryBackend::new();

        // Task tagged ["gpu"]
        let mut snap1 = WorkflowSnapshot::with_initial_input(
            "wf-gpu".into(),
            "h1".into(),
            bytes::Bytes::from("1"),
        );
        snap1.update_position(ExecutionPosition::AtTask {
            task_id: "t1".into(),
        });
        snap1.task_tags = vec!["gpu".into()];
        backend.save_snapshot(&snap1).await.unwrap();

        // Task tagged ["cpu"]
        let mut snap2 = WorkflowSnapshot::with_initial_input(
            "wf-cpu".into(),
            "h1".into(),
            bytes::Bytes::from("2"),
        );
        snap2.update_position(ExecutionPosition::AtTask {
            task_id: "t2".into(),
        });
        snap2.task_tags = vec!["cpu".into()];
        backend.save_snapshot(&snap2).await.unwrap();

        // Worker with ["gpu"] should only see the gpu task
        let tasks = backend
            .find_available_tasks("w1", 10, chrono::Duration::seconds(300), &["gpu".into()])
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].instance_id, "wf-gpu");
    }

    #[tokio::test]
    async fn test_find_available_tasks_untagged_worker_accepts_all() {
        let backend = InMemoryBackend::new();

        let mut snap1 = WorkflowSnapshot::with_initial_input(
            "wf-tagged".into(),
            "h1".into(),
            bytes::Bytes::from("1"),
        );
        snap1.update_position(ExecutionPosition::AtTask {
            task_id: "t1".into(),
        });
        snap1.task_tags = vec!["gpu".into()];
        backend.save_snapshot(&snap1).await.unwrap();

        let mut snap2 = WorkflowSnapshot::with_initial_input(
            "wf-plain".into(),
            "h1".into(),
            bytes::Bytes::from("2"),
        );
        snap2.update_position(ExecutionPosition::AtTask {
            task_id: "t2".into(),
        });
        backend.save_snapshot(&snap2).await.unwrap();

        // Untagged worker should see both
        let tasks = backend
            .find_available_tasks("w1", 10, chrono::Duration::seconds(300), &[])
            .await
            .unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn test_find_available_tasks_untagged_tasks_accepted_by_tagged_worker() {
        let backend = InMemoryBackend::new();

        // Untagged task
        let mut snap = WorkflowSnapshot::with_initial_input(
            "wf-plain".into(),
            "h1".into(),
            bytes::Bytes::from("1"),
        );
        snap.update_position(ExecutionPosition::AtTask {
            task_id: "t1".into(),
        });
        backend.save_snapshot(&snap).await.unwrap();

        // Tagged worker should still pick up untagged tasks
        let tasks = backend
            .find_available_tasks("w1", 10, chrono::Duration::seconds(300), &["gpu".into()])
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].instance_id, "wf-plain");
    }

    // ── Event queue (send_event / consume_event) ────────────────────────

    #[tokio::test]
    async fn test_send_and_consume_event_fifo() {
        let backend = InMemoryBackend::new();

        // Send two events for the same signal
        backend
            .send_event("wf-1", "approval", bytes::Bytes::from("first"))
            .await
            .unwrap();
        backend
            .send_event("wf-1", "approval", bytes::Bytes::from("second"))
            .await
            .unwrap();

        // Consume should return FIFO order
        let first = backend.consume_event("wf-1", "approval").await.unwrap();
        assert_eq!(first.as_deref(), Some(b"first".as_slice()));

        let second = backend.consume_event("wf-1", "approval").await.unwrap();
        assert_eq!(second.as_deref(), Some(b"second".as_slice()));

        // Queue is now empty
        let none = backend.consume_event("wf-1", "approval").await.unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn test_consume_event_empty_returns_none() {
        let backend = InMemoryBackend::new();
        let result = backend.consume_event("wf-1", "nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_events_are_isolated_by_signal_name() {
        let backend = InMemoryBackend::new();

        backend
            .send_event("wf-1", "sig_a", bytes::Bytes::from("a_payload"))
            .await
            .unwrap();
        backend
            .send_event("wf-1", "sig_b", bytes::Bytes::from("b_payload"))
            .await
            .unwrap();

        // Consuming sig_a should not affect sig_b
        let a = backend.consume_event("wf-1", "sig_a").await.unwrap();
        assert_eq!(a.as_deref(), Some(b"a_payload".as_slice()));

        let b = backend.consume_event("wf-1", "sig_b").await.unwrap();
        assert_eq!(b.as_deref(), Some(b"b_payload".as_slice()));
    }

    // ─── TaskResultStore ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_load_task_result_in_progress() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash".into());
        snapshot.mark_task_completed("task-1".into(), bytes::Bytes::from("out1"));
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend.load_task_result("wf-1", "task-1").await.unwrap();
        assert_eq!(result, Some(bytes::Bytes::from("out1")));
    }

    #[tokio::test]
    async fn test_load_task_result_not_found() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash".into());
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend
            .load_task_result("wf-1", "no-such-task")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_load_task_result_nonexistent_instance() {
        let backend = InMemoryBackend::new();
        let result = backend.load_task_result("no-such-wf", "task-1").await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_load_task_result_after_completion() {
        let backend = InMemoryBackend::new();

        // Create workflow with a completed task
        let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash".into());
        snapshot.mark_task_completed("task-1".into(), bytes::Bytes::from("out1"));
        backend.save_snapshot(&snapshot).await.unwrap();

        // Complete the workflow — save_snapshot caches the task results
        snapshot.mark_completed(bytes::Bytes::from("final"));
        backend.save_snapshot(&snapshot).await.unwrap();

        // Task result should still be accessible from cache
        let result = backend.load_task_result("wf-1", "task-1").await.unwrap();
        assert_eq!(result, Some(bytes::Bytes::from("out1")));
    }

    #[tokio::test]
    async fn test_load_task_result_after_failure() {
        let backend = InMemoryBackend::new();

        let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash".into());
        snapshot.mark_task_completed("task-1".into(), bytes::Bytes::from("out1"));
        backend.save_snapshot(&snapshot).await.unwrap();

        snapshot.mark_failed("boom".into());
        backend.save_snapshot(&snapshot).await.unwrap();

        let result = backend.load_task_result("wf-1", "task-1").await.unwrap();
        assert_eq!(result, Some(bytes::Bytes::from("out1")));
    }

    #[tokio::test]
    async fn test_delete_cleans_task_results_cache() {
        let backend = InMemoryBackend::new();

        let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash".into());
        snapshot.mark_task_completed("task-1".into(), bytes::Bytes::from("out1"));
        backend.save_snapshot(&snapshot).await.unwrap();

        snapshot.mark_completed(bytes::Bytes::from("final"));
        backend.save_snapshot(&snapshot).await.unwrap();

        // Delete should clean both snapshot and cache
        backend.delete_snapshot("wf-1").await.unwrap();

        let result = backend.load_task_result("wf-1", "task-1").await;
        assert!(matches!(result, Err(BackendError::NotFound(_))));
    }
}
