//! Task claiming mechanism for multi-node collaboration.
//!
//! This module provides structures for claiming and tracking task execution
//! across multiple nodes, preventing duplicate execution.

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

/// A claim on a task by a worker node.
///
/// When a worker claims a task, it has exclusive rights to execute it
/// until the claim expires or is released.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskClaim {
    /// The workflow instance ID (not to be confused with the workflow ID)
    pub instance_id: String,
    /// The task ID being claimed.
    pub task_id: String,
    /// The worker node ID that claimed this task.
    pub worker_id: String,
    /// When the claim was created (Unix timestamp).
    pub claimed_at: u64,
    /// When the claim expires (Unix timestamp).
    /// If None, the claim never expires.
    pub expires_at: Option<u64>,
}

#[allow(clippy::cast_sign_loss)] // Timestamps are always positive
impl TaskClaim {
    /// Create a new task claim.
    #[must_use]
    pub fn new(
        instance_id: String,
        task_id: String,
        worker_id: String,
        ttl: Option<Duration>,
    ) -> Self {
        let now = Utc::now();
        let claimed_at = now.timestamp() as u64;
        let expires_at = ttl.and_then(|duration| {
            now.checked_add_signed(duration)
                .map(|expiry| expiry.timestamp() as u64)
        });

        Self {
            instance_id,
            task_id,
            worker_id,
            claimed_at,
            expires_at,
        }
    }

    /// Check if this claim has expired.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            let now = Utc::now().timestamp() as u64;
            now >= expires_at
        } else {
            false
        }
    }

    /// Check if this claim belongs to the given worker.
    #[must_use]
    pub fn is_owned_by(&self, worker_id: &str) -> bool {
        self.worker_id == worker_id
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_sign_loss
)]
mod tests {
    use super::*;

    fn claim(worker: &str, expires_at: Option<u64>) -> TaskClaim {
        TaskClaim {
            instance_id: "inst-1".into(),
            task_id: "task-1".into(),
            worker_id: worker.into(),
            claimed_at: 1_000_000,
            expires_at,
        }
    }

    #[test]
    fn no_ttl_never_expires() {
        assert!(!claim("w1", None).is_expired());
    }

    #[test]
    fn future_expiry_is_not_expired() {
        let far_future = Utc::now().timestamp() as u64 + 3600;
        assert!(!claim("w1", Some(far_future)).is_expired());
    }

    #[test]
    fn past_expiry_is_expired() {
        assert!(claim("w1", Some(0)).is_expired());
    }

    #[test]
    fn boundary_expiry_is_expired() {
        // expires_at == now should be expired (now >= expires_at)
        let now = Utc::now().timestamp() as u64;
        assert!(claim("w1", Some(now)).is_expired());
    }

    #[test]
    fn is_owned_by_matching_worker() {
        assert!(claim("worker-a", None).is_owned_by("worker-a"));
    }

    #[test]
    fn is_not_owned_by_different_worker() {
        assert!(!claim("worker-a", None).is_owned_by("worker-b"));
    }

    #[test]
    fn new_with_ttl_sets_expiry() {
        let c = TaskClaim::new(
            "i".into(),
            "t".into(),
            "w".into(),
            Some(Duration::seconds(60)),
        );
        assert!(c.expires_at.is_some());
        assert!(c.expires_at.unwrap() > c.claimed_at);
    }

    #[test]
    fn new_without_ttl_has_no_expiry() {
        let c = TaskClaim::new("i".into(), "t".into(), "w".into(), None);
        assert!(c.expires_at.is_none());
    }
}

/// Information about an available task ready for execution.
#[derive(Debug, Clone)]
pub struct AvailableTask {
    /// The workflow instance ID.
    pub instance_id: String,
    /// The task ID.
    pub task_id: String,
    /// The input data for the task (serialized).
    pub input: bytes::Bytes,
    /// The workflow definition hash.
    pub workflow_definition_hash: String,
    /// W3C `traceparent` header for distributed trace context propagation.
    pub trace_parent: Option<String>,
}
