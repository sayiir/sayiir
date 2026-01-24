//! Workflow snapshot structures for checkpoint/restore functionality.
//!
//! Snapshots capture the complete execution state of a workflow, including
//! which tasks have completed and their outputs. This enables resuming
//! workflows from any checkpoint.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents the position in workflow execution.
///
/// This tracks which tasks have been completed and where execution should resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionPosition {
    /// Workflow has not started yet.
    NotStarted,
    /// Execution is at the start of a task.
    /// The task ID indicates which task should be executed next.
    AtTask { task_id: String },
    /// Execution is waiting for fork branches to complete.
    /// The map tracks which branches have completed (by branch ID).
    AtFork {
        branch_id: String,
        completed_branches: HashMap<String, TaskResult>,
    },
    /// Execution is at a join task, waiting for all branches.
    AtJoin {
        join_id: String,
        completed_branches: HashMap<String, TaskResult>,
    },
}

/// Result of a completed task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// The task ID that produced this result.
    pub task_id: String,
    /// The serialized output of the task.
    pub output: Bytes,
}

/// A request to cancel a workflow.
///
/// This is stored separately from the workflow state and checked by workers
/// at task boundaries. The actual `Cancelled` state is set after processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancellationRequest {
    /// Optional reason for the cancellation.
    pub reason: Option<String>,
    /// Optional identifier of who requested the cancellation.
    pub requested_by: Option<String>,
    /// Timestamp when the cancellation was requested.
    pub requested_at: DateTime<Utc>,
}

/// State of a workflow snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowSnapshotState {
    /// Workflow is in progress.
    InProgress {
        /// Current execution position.
        position: ExecutionPosition,
        /// Results from completed tasks (by task ID).
        completed_tasks: HashMap<String, TaskResult>,
        /// ID of the last completed task (for deterministic resume).
        /// This is needed because `HashMap` iteration order is not guaranteed.
        last_completed_task_id: Option<String>,
    },
    /// Workflow completed successfully.
    Completed {
        /// Final output of the workflow.
        final_output: Bytes,
    },
    /// Workflow failed with an error.
    Failed {
        /// Error message.
        error: String,
    },
    /// Workflow was cancelled.
    Cancelled {
        /// Optional reason for the cancellation.
        reason: Option<String>,
        /// Optional identifier of who cancelled the workflow.
        cancelled_by: Option<String>,
        /// Timestamp when the workflow was cancelled.
        cancelled_at: DateTime<Utc>,
        /// Results from tasks that completed before cancellation.
        completed_tasks: HashMap<String, TaskResult>,
        /// The task ID that was interrupted (if any).
        interrupted_at_task: Option<String>,
    },
}

impl WorkflowSnapshotState {
    /// Check if the workflow is completed.
    pub fn is_completed(&self) -> bool {
        matches!(self, WorkflowSnapshotState::Completed { .. })
    }

    /// Check if the workflow has failed.
    pub fn is_failed(&self) -> bool {
        matches!(self, WorkflowSnapshotState::Failed { .. })
    }

    /// Check if the workflow is still in progress.
    pub fn is_in_progress(&self) -> bool {
        matches!(self, WorkflowSnapshotState::InProgress { .. })
    }

    /// Check if the workflow was cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, WorkflowSnapshotState::Cancelled { .. })
    }

    /// Check if the workflow is in a terminal state (completed, failed, or cancelled).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkflowSnapshotState::Completed { .. }
                | WorkflowSnapshotState::Failed { .. }
                | WorkflowSnapshotState::Cancelled { .. }
        )
    }
}

/// A complete snapshot of workflow execution state.
///
/// This captures everything needed to resume a workflow from a checkpoint:
/// - The workflow instance ID
/// - The workflow definition hash (for validation)
/// - The current execution state
/// - All completed task results
/// - The initial input (for resuming from the beginning)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSnapshot {
    /// Unique identifier for this workflow instance.
    pub instance_id: String,
    /// Hash of the workflow definition (for validation).
    pub definition_hash: String,
    /// Current state of execution.
    pub state: WorkflowSnapshotState,
    /// Timestamp when this snapshot was created (Unix timestamp).
    pub created_at: u64,
    /// Timestamp when this snapshot was last updated (Unix timestamp).
    pub updated_at: u64,
    /// Initial input to the workflow (for resuming from start).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_input: Option<Bytes>,
}

impl WorkflowSnapshot {
    /// Get the current Unix timestamp.
    #[allow(clippy::cast_sign_loss)] // Timestamps are always positive
    fn current_timestamp() -> u64 {
        Utc::now().timestamp() as u64
    }

    /// Create a new snapshot for a workflow instance.
    #[must_use]
    pub fn new(instance_id: String, definition_hash: String) -> Self {
        let now = Self::current_timestamp();
        Self {
            instance_id,
            definition_hash,
            state: WorkflowSnapshotState::InProgress {
                position: ExecutionPosition::NotStarted,
                completed_tasks: HashMap::new(),
                last_completed_task_id: None,
            },
            created_at: now,
            updated_at: now,
            initial_input: None,
        }
    }

    /// Create a new snapshot with initial input.
    pub fn with_initial_input(
        instance_id: String,
        definition_hash: String,
        initial_input: Bytes,
    ) -> Self {
        let now = Self::current_timestamp();
        Self {
            instance_id,
            definition_hash,
            state: WorkflowSnapshotState::InProgress {
                position: ExecutionPosition::NotStarted,
                completed_tasks: HashMap::new(),
                last_completed_task_id: None,
            },
            created_at: now,
            updated_at: now,
            initial_input: Some(initial_input),
        }
    }

    /// Get the initial input as Bytes.
    pub fn initial_input_bytes(&self) -> Option<Bytes> {
        self.initial_input.clone()
    }

    /// Get the result of a completed task, if available.
    pub fn get_task_result(&self, task_id: &str) -> Option<&TaskResult> {
        match &self.state {
            WorkflowSnapshotState::InProgress {
                completed_tasks, ..
            }
            | WorkflowSnapshotState::Cancelled {
                completed_tasks, ..
            } => completed_tasks.get(task_id),
            _ => None,
        }
    }

    /// Get the result of a completed task as Bytes, if available.
    pub fn get_task_result_bytes(&self, task_id: &str) -> Option<Bytes> {
        self.get_task_result(task_id).map(|r| r.output.clone())
    }

    /// Mark a task as completed and store its result.
    pub fn mark_task_completed(&mut self, task_id: String, output: Bytes) {
        if let WorkflowSnapshotState::InProgress {
            completed_tasks,
            last_completed_task_id,
            ..
        } = &mut self.state
        {
            completed_tasks.insert(
                task_id.clone(),
                TaskResult {
                    task_id: task_id.clone(),
                    output,
                },
            );
            *last_completed_task_id = Some(task_id);
            self.updated_at = Self::current_timestamp();
        }
    }

    /// Get the last completed task's output, if any.
    pub fn get_last_task_output(&self) -> Option<Bytes> {
        match &self.state {
            WorkflowSnapshotState::InProgress {
                completed_tasks,
                last_completed_task_id,
                ..
            } => last_completed_task_id
                .as_ref()
                .and_then(|id| completed_tasks.get(id))
                .map(|r| r.output.clone()),
            _ => None,
        }
    }

    /// Get the final output as Bytes if completed.
    pub fn final_output_bytes(&self) -> Option<Bytes> {
        match &self.state {
            WorkflowSnapshotState::Completed { final_output } => Some(final_output.clone()),
            _ => None,
        }
    }

    /// Update the execution position.
    pub fn update_position(&mut self, position: ExecutionPosition) {
        if let WorkflowSnapshotState::InProgress { position: pos, .. } = &mut self.state {
            *pos = position;
            self.updated_at = Self::current_timestamp();
        }
    }

    /// Mark the workflow as completed with a final output.
    pub fn mark_completed(&mut self, final_output: Bytes) {
        self.state = WorkflowSnapshotState::Completed { final_output };
        self.updated_at = Self::current_timestamp();
    }

    /// Mark the workflow as failed with an error.
    pub fn mark_failed(&mut self, error: String) {
        self.state = WorkflowSnapshotState::Failed { error };
        self.updated_at = Self::current_timestamp();
    }

    /// Mark the workflow as cancelled.
    ///
    /// This preserves the completed tasks from the current state.
    pub fn mark_cancelled(
        &mut self,
        reason: Option<String>,
        cancelled_by: Option<String>,
        interrupted_at_task: Option<String>,
    ) {
        let completed_tasks = match &self.state {
            WorkflowSnapshotState::InProgress {
                completed_tasks, ..
            } => completed_tasks.clone(),
            _ => HashMap::new(),
        };

        self.state = WorkflowSnapshotState::Cancelled {
            reason,
            cancelled_by,
            cancelled_at: Utc::now(),
            completed_tasks,
            interrupted_at_task,
        };
        self.updated_at = Self::current_timestamp();
    }
}
