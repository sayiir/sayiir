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
    /// The workflow instance ID (not not confuse with the workflow ID)
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

impl TaskClaim {
    /// Create a new task claim.
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
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            let now = Utc::now().timestamp() as u64;
            now >= expires_at
        } else {
            false
        }
    }

    /// Check if this claim belongs to the given worker.
    pub fn is_owned_by(&self, worker_id: &str) -> bool {
        self.worker_id == worker_id
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
}
