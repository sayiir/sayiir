//! Workflow snapshot structures for checkpoint/restore functionality.
//!
//! Snapshots capture the complete execution state of a workflow, including
//! which tasks have completed and their outputs. This enables resuming
//! workflows from any checkpoint.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A persisted deadline for a running task.
///
/// When a task with a timeout starts, the absolute wall-clock deadline is
/// computed and stored in the snapshot. On crash recovery, an expired deadline
/// causes immediate `TaskTimedOut` without re-executing the task.
///
/// # Cancellation behaviour
///
/// When a deadline expires, the workflow is marked as `Failed`, which prevents
/// other workers from picking up further tasks for that instance.
///
/// All runners use `tokio::select!` to race the task future against a sleep
/// until the deadline. When the deadline fires first, the task future is
/// **dropped** (active cancellation). In the distributed worker, the heartbeat
/// loop provides an additional check during the tick interval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDeadline {
    /// The task this deadline applies to.
    pub task_id: String,
    /// Absolute wall-clock deadline.
    pub deadline: DateTime<Utc>,
    /// Original configured timeout in milliseconds (for error reporting).
    pub timeout_ms: u64,
}

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
    /// Execution is parked at a fork because one or more branches hit a delay.
    /// Completed branches have their results cached; delayed branches will
    /// re-execute (skipping cached sub-tasks) once `wake_at` passes.
    AtFork {
        fork_id: String,
        completed_branches: HashMap<String, TaskResult>,
        wake_at: DateTime<Utc>,
    },
    /// Execution is at a join task, waiting for all branches.
    AtJoin {
        join_id: String,
        completed_branches: HashMap<String, TaskResult>,
    },
    /// Execution is parked at a delay node, waiting for `wake_at`.
    AtDelay {
        delay_id: String,
        entered_at: DateTime<Utc>,
        wake_at: DateTime<Utc>,
        /// First task ID after the delay (so backends can advance without traversing the workflow tree).
        next_task_id: Option<String>,
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

impl CancellationRequest {
    /// Create a new cancellation request with the current timestamp.
    #[must_use]
    pub fn new(reason: Option<String>, requested_by: Option<String>) -> Self {
        Self {
            reason,
            requested_by,
            requested_at: Utc::now(),
        }
    }
}

/// A request to pause a workflow.
///
/// This is stored separately from the workflow state and checked by workers
/// at task boundaries. The actual `Paused` state is set after processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseRequest {
    /// Optional reason for the pause.
    pub reason: Option<String>,
    /// Optional identifier of who requested the pause.
    pub requested_by: Option<String>,
    /// Timestamp when the pause was requested.
    pub requested_at: DateTime<Utc>,
}

impl PauseRequest {
    /// Create a new pause request with the current timestamp.
    #[must_use]
    pub fn new(reason: Option<String>, requested_by: Option<String>) -> Self {
        Self {
            reason,
            requested_by,
            requested_at: Utc::now(),
        }
    }
}

/// Kind of signal that can be sent to a running workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalKind {
    /// Request cancellation.
    Cancel,
    /// Request pause.
    Pause,
}

/// A unified signal request that covers both cancel and pause.
///
/// Old `CancellationRequest`/`PauseRequest` types remain for snapshot state
/// serialization. `SignalRequest` is the type used by the `SignalStore` trait.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalRequest {
    /// Optional reason for the signal.
    pub reason: Option<String>,
    /// Optional identifier of who sent the signal.
    pub requested_by: Option<String>,
    /// Timestamp when the signal was sent.
    pub requested_at: DateTime<Utc>,
}

impl SignalRequest {
    /// Create a new signal request with the current timestamp.
    #[must_use]
    pub fn new(reason: Option<String>, requested_by: Option<String>) -> Self {
        Self {
            reason,
            requested_by,
            requested_at: Utc::now(),
        }
    }
}

impl From<CancellationRequest> for SignalRequest {
    fn from(r: CancellationRequest) -> Self {
        Self {
            reason: r.reason,
            requested_by: r.requested_by,
            requested_at: r.requested_at,
        }
    }
}

impl From<PauseRequest> for SignalRequest {
    fn from(r: PauseRequest) -> Self {
        Self {
            reason: r.reason,
            requested_by: r.requested_by,
            requested_at: r.requested_at,
        }
    }
}

impl From<SignalRequest> for CancellationRequest {
    fn from(r: SignalRequest) -> Self {
        Self {
            reason: r.reason,
            requested_by: r.requested_by,
            requested_at: r.requested_at,
        }
    }
}

impl From<SignalRequest> for PauseRequest {
    fn from(r: SignalRequest) -> Self {
        Self {
            reason: r.reason,
            requested_by: r.requested_by,
            requested_at: r.requested_at,
        }
    }
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
    /// Workflow was paused. Unlike Cancelled, this preserves position and
    /// `last_completed_task_id` so that the workflow can resume from exactly
    /// where it stopped.
    Paused {
        /// Optional reason for the pause.
        reason: Option<String>,
        /// Optional identifier of who paused the workflow.
        paused_by: Option<String>,
        /// Timestamp when the workflow was paused.
        paused_at: DateTime<Utc>,
        /// Results from tasks that completed before pausing.
        completed_tasks: HashMap<String, TaskResult>,
        /// Execution position at the time of pause (for exact resume).
        position: ExecutionPosition,
        /// ID of the last completed task (for deterministic resume).
        last_completed_task_id: Option<String>,
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

    /// Check if the workflow is paused.
    pub fn is_paused(&self) -> bool {
        matches!(self, WorkflowSnapshotState::Paused { .. })
    }

    /// Check if the workflow is in a terminal state (completed, failed, or cancelled).
    /// Note: Paused is NOT terminal — the workflow can be resumed.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkflowSnapshotState::Completed { .. }
                | WorkflowSnapshotState::Failed { .. }
                | WorkflowSnapshotState::Cancelled { .. }
        )
    }

    /// Convert a terminal state to a [`WorkflowStatus`](crate::workflow::WorkflowStatus).
    ///
    /// Returns `Some(status)` for `Completed`, `Failed`, and `Cancelled` states,
    /// or `None` if still `InProgress`.
    #[must_use]
    pub fn as_terminal_status(&self) -> Option<crate::workflow::WorkflowStatus> {
        use crate::workflow::WorkflowStatus;
        match self {
            Self::Completed { .. } => Some(WorkflowStatus::Completed),
            Self::Failed { error } => Some(WorkflowStatus::Failed(error.clone())),
            Self::Cancelled {
                reason,
                cancelled_by,
                ..
            } => Some(WorkflowStatus::Cancelled {
                reason: reason.clone(),
                cancelled_by: cancelled_by.clone(),
            }),
            Self::Paused {
                reason, paused_by, ..
            } => Some(WorkflowStatus::Paused {
                reason: reason.clone(),
                paused_by: paused_by.clone(),
            }),
            Self::InProgress { .. } => None,
        }
    }

    /// Extract the final output if in `Completed` state.
    #[must_use]
    pub fn completed_output(&self) -> Option<&Bytes> {
        if let WorkflowSnapshotState::Completed { final_output } = self {
            Some(final_output)
        } else {
            None
        }
    }

    /// Extract cancellation details if in `Cancelled` state.
    ///
    /// Returns `Some((reason, cancelled_by))` if cancelled, `None` otherwise.
    #[must_use]
    pub fn cancellation_details(&self) -> Option<(Option<String>, Option<String>)> {
        if let WorkflowSnapshotState::Cancelled {
            reason,
            cancelled_by,
            ..
        } = self
        {
            Some((reason.clone(), cancelled_by.clone()))
        } else {
            None
        }
    }

    /// Extract pause details if in `Paused` state.
    ///
    /// Returns `Some((reason, paused_by))` if paused, `None` otherwise.
    #[must_use]
    pub fn pause_details(&self) -> Option<(Option<String>, Option<String>)> {
        if let WorkflowSnapshotState::Paused {
            reason, paused_by, ..
        } = self
        {
            Some((reason.clone(), paused_by.clone()))
        } else {
            None
        }
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
    /// Active task deadline (set when a task with a timeout starts executing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_deadline: Option<TaskDeadline>,
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
            task_deadline: None,
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
            task_deadline: None,
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
            }
            | WorkflowSnapshotState::Paused {
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

    /// Set a task deadline from a timeout duration.
    ///
    /// Computes `deadline = Utc::now() + timeout` and stores it in the snapshot.
    #[allow(clippy::cast_possible_truncation)]
    pub fn set_task_deadline(&mut self, task_id: String, timeout: std::time::Duration) {
        let deadline =
            Utc::now() + chrono::Duration::from_std(timeout).unwrap_or(chrono::Duration::MAX);
        self.task_deadline = Some(TaskDeadline {
            task_id,
            deadline,
            timeout_ms: timeout.as_millis() as u64,
        });
    }

    /// Recompute the deadline to `Utc::now() + timeout_ms`.
    ///
    /// Call this right before actual task execution so the deadline reflects
    /// the true execution start, not the earlier moment when the deadline was
    /// first persisted (which includes snapshot-save I/O overhead).
    #[allow(clippy::cast_possible_wrap)]
    pub fn refresh_task_deadline(&mut self) {
        if let Some(d) = &mut self.task_deadline {
            let timeout = chrono::Duration::milliseconds(d.timeout_ms as i64);
            d.deadline = Utc::now() + timeout;
        }
    }

    /// Clear the active task deadline.
    pub fn clear_task_deadline(&mut self) {
        self.task_deadline = None;
    }

    /// If the persisted deadline has expired, return `(task_id, timeout)`.
    ///
    /// Returns `None` if no deadline is set or it hasn't expired yet.
    pub fn expired_task_deadline(&self) -> Option<(&str, std::time::Duration)> {
        self.task_deadline.as_ref().and_then(|d| {
            if Utc::now() >= d.deadline {
                Some((
                    d.task_id.as_str(),
                    std::time::Duration::from_millis(d.timeout_ms),
                ))
            } else {
                None
            }
        })
    }

    /// Mark the workflow as completed with a final output.
    pub fn mark_completed(&mut self, final_output: Bytes) {
        self.task_deadline = None;
        self.state = WorkflowSnapshotState::Completed { final_output };
        self.updated_at = Self::current_timestamp();
    }

    /// Mark the workflow as failed with an error.
    pub fn mark_failed(&mut self, error: String) {
        self.task_deadline = None;
        self.state = WorkflowSnapshotState::Failed { error };
        self.updated_at = Self::current_timestamp();
    }

    /// Mark the workflow as paused.
    ///
    /// This preserves the completed tasks, position, and `last_completed_task_id`
    /// from the current `InProgress` state so that the workflow can resume from
    /// exactly where it stopped.
    pub fn mark_paused(&mut self, request: &PauseRequest) {
        if let WorkflowSnapshotState::InProgress {
            position,
            completed_tasks,
            last_completed_task_id,
        } = &self.state
        {
            self.task_deadline = None;
            self.state = WorkflowSnapshotState::Paused {
                reason: request.reason.clone(),
                paused_by: request.requested_by.clone(),
                paused_at: Utc::now(),
                completed_tasks: completed_tasks.clone(),
                position: position.clone(),
                last_completed_task_id: last_completed_task_id.clone(),
            };
            self.updated_at = Self::current_timestamp();
        }
    }

    /// Transition from Paused back to `InProgress`, restoring position and
    /// `completed_tasks` so execution can continue.
    pub fn mark_unpaused(&mut self) {
        if let WorkflowSnapshotState::Paused {
            completed_tasks,
            position,
            last_completed_task_id,
            ..
        } = &self.state
        {
            self.state = WorkflowSnapshotState::InProgress {
                position: position.clone(),
                completed_tasks: completed_tasks.clone(),
                last_completed_task_id: last_completed_task_id.clone(),
            };
            self.updated_at = Self::current_timestamp();
        }
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

        self.task_deadline = None;
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_new_snapshot_in_progress() {
        let snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        assert_eq!(snapshot.instance_id, "inst-1");
        assert_eq!(snapshot.definition_hash, "hash-1");
        assert!(snapshot.state.is_in_progress());
        assert!(!snapshot.state.is_terminal());
        assert!(snapshot.initial_input.is_none());
    }

    #[test]
    fn test_snapshot_with_initial_input() {
        let snapshot = WorkflowSnapshot::with_initial_input(
            "inst-1".into(),
            "hash-1".into(),
            Bytes::from("hello"),
        );
        assert_eq!(snapshot.initial_input_bytes(), Some(Bytes::from("hello")));
        assert!(snapshot.state.is_in_progress());
    }

    #[test]
    fn test_mark_task_completed_and_retrieve() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_task_completed("task-1".into(), Bytes::from("output1"));

        let result = snapshot.get_task_result("task-1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().output, Bytes::from("output1"));
        assert_eq!(result.unwrap().task_id, "task-1");
    }

    #[test]
    fn test_get_last_task_output() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        assert!(snapshot.get_last_task_output().is_none());

        snapshot.mark_task_completed("task-1".into(), Bytes::from("out1"));
        assert_eq!(snapshot.get_last_task_output(), Some(Bytes::from("out1")));

        snapshot.mark_task_completed("task-2".into(), Bytes::from("out2"));
        assert_eq!(snapshot.get_last_task_output(), Some(Bytes::from("out2")));
    }

    #[test]
    fn test_get_task_result_bytes() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_task_completed("task-1".into(), Bytes::from("data"));
        assert_eq!(
            snapshot.get_task_result_bytes("task-1"),
            Some(Bytes::from("data"))
        );
        assert!(snapshot.get_task_result_bytes("nonexistent").is_none());
    }

    #[test]
    fn test_mark_completed() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_completed(Bytes::from("final"));
        assert!(snapshot.state.is_completed());
        assert!(snapshot.state.is_terminal());
        assert!(!snapshot.state.is_in_progress());
        assert_eq!(snapshot.final_output_bytes(), Some(Bytes::from("final")));
    }

    #[test]
    fn test_mark_failed() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_failed("something went wrong".into());
        assert!(snapshot.state.is_failed());
        assert!(snapshot.state.is_terminal());
        assert!(!snapshot.state.is_in_progress());
        assert!(snapshot.final_output_bytes().is_none());
    }

    #[test]
    fn test_mark_cancelled_preserves_completed_tasks() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_task_completed("task-1".into(), Bytes::from("output1"));
        snapshot.mark_cancelled(
            Some("user request".into()),
            Some("admin".into()),
            Some("task-2".into()),
        );

        assert!(snapshot.state.is_cancelled());
        assert!(snapshot.state.is_terminal());
        // Completed tasks should be preserved in cancelled state
        assert!(snapshot.get_task_result("task-1").is_some());
    }

    #[test]
    fn test_cancellation_details() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        assert!(snapshot.state.cancellation_details().is_none());

        snapshot.mark_cancelled(Some("timeout".into()), Some("system".into()), None);
        let details = snapshot.state.cancellation_details();
        assert!(details.is_some());
        let (reason, by) = details.unwrap();
        assert_eq!(reason, Some("timeout".into()));
        assert_eq!(by, Some("system".into()));
    }

    #[test]
    fn test_update_position() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "task-1".into(),
        });
        match &snapshot.state {
            WorkflowSnapshotState::InProgress { position, .. } => match position {
                ExecutionPosition::AtTask { task_id } => assert_eq!(task_id, "task-1"),
                _ => panic!("Expected AtTask"),
            },
            _ => panic!("Expected InProgress"),
        }
    }

    #[test]
    fn test_update_position_noop_on_terminal() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_completed(Bytes::from("done"));
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "task-1".into(),
        });
        // Position update should be a no-op on completed state
        assert!(snapshot.state.is_completed());
    }

    #[test]
    fn test_mark_task_completed_noop_on_terminal() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_completed(Bytes::from("done"));
        snapshot.mark_task_completed("task-1".into(), Bytes::from("output"));
        // Should not crash, just be a no-op
        assert!(snapshot.state.is_completed());
    }

    #[test]
    fn test_get_task_result_on_completed_state() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_completed(Bytes::from("done"));
        assert!(snapshot.get_task_result("task-1").is_none());
    }

    #[test]
    fn test_get_task_result_on_failed_state() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_failed("err".into());
        assert!(snapshot.get_task_result("task-1").is_none());
    }

    #[test]
    fn test_state_transitions() {
        let state = WorkflowSnapshotState::InProgress {
            position: ExecutionPosition::NotStarted,
            completed_tasks: HashMap::new(),
            last_completed_task_id: None,
        };
        assert!(state.is_in_progress());
        assert!(!state.is_completed());
        assert!(!state.is_failed());
        assert!(!state.is_cancelled());
        assert!(!state.is_terminal());

        let state = WorkflowSnapshotState::Completed {
            final_output: Bytes::new(),
        };
        assert!(!state.is_in_progress());
        assert!(state.is_completed());
        assert!(state.is_terminal());

        let state = WorkflowSnapshotState::Failed {
            error: "err".into(),
        };
        assert!(state.is_failed());
        assert!(state.is_terminal());

        let state = WorkflowSnapshotState::Cancelled {
            reason: None,
            cancelled_by: None,
            cancelled_at: Utc::now(),
            completed_tasks: HashMap::new(),
            interrupted_at_task: None,
        };
        assert!(state.is_cancelled());
        assert!(state.is_terminal());
    }

    #[test]
    fn test_timestamps_are_set() {
        let snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        assert!(snapshot.created_at > 0);
        assert!(snapshot.updated_at > 0);
        assert_eq!(snapshot.created_at, snapshot.updated_at);
    }

    // ========================================================================
    // From conversion tests
    // ========================================================================

    #[test]
    fn test_cancellation_request_to_signal_request() {
        let cr = CancellationRequest::new(Some("timeout".into()), Some("admin".into()));
        let ts = cr.requested_at;
        let sr: SignalRequest = cr.into();
        assert_eq!(sr.reason, Some("timeout".into()));
        assert_eq!(sr.requested_by, Some("admin".into()));
        assert_eq!(sr.requested_at, ts);
    }

    #[test]
    fn test_pause_request_to_signal_request() {
        let pr = PauseRequest::new(Some("maintenance".into()), Some("ops".into()));
        let ts = pr.requested_at;
        let sr: SignalRequest = pr.into();
        assert_eq!(sr.reason, Some("maintenance".into()));
        assert_eq!(sr.requested_by, Some("ops".into()));
        assert_eq!(sr.requested_at, ts);
    }

    #[test]
    fn test_signal_request_to_cancellation_request() {
        let sr = SignalRequest::new(Some("done".into()), Some("user".into()));
        let ts = sr.requested_at;
        let cr: CancellationRequest = sr.into();
        assert_eq!(cr.reason, Some("done".into()));
        assert_eq!(cr.requested_by, Some("user".into()));
        assert_eq!(cr.requested_at, ts);
    }

    #[test]
    fn test_signal_request_to_pause_request() {
        let sr = SignalRequest::new(Some("scaling".into()), Some("system".into()));
        let ts = sr.requested_at;
        let pr: PauseRequest = sr.into();
        assert_eq!(pr.reason, Some("scaling".into()));
        assert_eq!(pr.requested_by, Some("system".into()));
        assert_eq!(pr.requested_at, ts);
    }

    #[test]
    fn test_cancellation_request_roundtrip() {
        let original = CancellationRequest::new(Some("reason".into()), Some("who".into()));
        let ts = original.requested_at;
        let signal: SignalRequest = original.into();
        let back: CancellationRequest = signal.into();
        assert_eq!(back.reason, Some("reason".into()));
        assert_eq!(back.requested_by, Some("who".into()));
        assert_eq!(back.requested_at, ts);
    }

    #[test]
    fn test_pause_request_roundtrip() {
        let original = PauseRequest::new(Some("reason".into()), Some("who".into()));
        let ts = original.requested_at;
        let signal: SignalRequest = original.into();
        let back: PauseRequest = signal.into();
        assert_eq!(back.reason, Some("reason".into()));
        assert_eq!(back.requested_by, Some("who".into()));
        assert_eq!(back.requested_at, ts);
    }

    #[test]
    fn test_from_conversion_with_none_fields() {
        let sr = SignalRequest::new(None, None);
        let cr: CancellationRequest = sr.into();
        assert!(cr.reason.is_none());
        assert!(cr.requested_by.is_none());
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for arbitrary `TaskResult`.
    fn arb_task_result() -> impl Strategy<Value = TaskResult> {
        (
            "[a-z0-9]{1,8}",
            proptest::collection::vec(any::<u8>(), 0..32),
        )
            .prop_map(|(task_id, data)| TaskResult {
                task_id,
                output: Bytes::from(data),
            })
    }

    /// Strategy for arbitrary `HashMap<String, TaskResult>`.
    fn arb_completed_tasks() -> impl Strategy<Value = HashMap<String, TaskResult>> {
        proptest::collection::hash_map("[a-z0-9]{1,8}", arb_task_result(), 0..4)
    }

    /// Strategy for arbitrary `WorkflowSnapshotState`.
    fn arb_state() -> impl Strategy<Value = WorkflowSnapshotState> {
        prop_oneof![
            // InProgress
            arb_completed_tasks().prop_map(|tasks| {
                WorkflowSnapshotState::InProgress {
                    position: ExecutionPosition::NotStarted,
                    completed_tasks: tasks,
                    last_completed_task_id: None,
                }
            }),
            // Completed
            proptest::collection::vec(any::<u8>(), 0..32).prop_map(|data| {
                WorkflowSnapshotState::Completed {
                    final_output: Bytes::from(data),
                }
            }),
            // Failed
            "[a-zA-Z0-9 ]{0,32}".prop_map(|error| WorkflowSnapshotState::Failed { error }),
            // Cancelled
            (
                prop::option::of("[a-zA-Z0-9 ]{0,32}"),
                prop::option::of("[a-zA-Z0-9 ]{0,32}"),
                arb_completed_tasks(),
                prop::option::of("[a-z0-9]{1,8}"),
            )
                .prop_map(
                    |(reason, cancelled_by, completed_tasks, interrupted_at_task)| {
                        WorkflowSnapshotState::Cancelled {
                            reason,
                            cancelled_by,
                            cancelled_at: Utc::now(),
                            completed_tasks,
                            interrupted_at_task,
                        }
                    }
                ),
            // Paused
            (
                prop::option::of("[a-zA-Z0-9 ]{0,32}"),
                prop::option::of("[a-zA-Z0-9 ]{0,32}"),
                arb_completed_tasks(),
                prop::option::of("[a-z0-9]{1,8}"),
            )
                .prop_map(
                    |(reason, paused_by, completed_tasks, last_completed_task_id)| {
                        WorkflowSnapshotState::Paused {
                            reason,
                            paused_by,
                            paused_at: Utc::now(),
                            completed_tasks,
                            position: ExecutionPosition::NotStarted,
                            last_completed_task_id,
                        }
                    }
                ),
        ]
    }

    proptest! {
        // Property 8: Exactly one predicate is true for any state.
        #[test]
        fn exactly_one_predicate_true(state in arb_state()) {
            let flags = [
                state.is_in_progress(),
                state.is_completed(),
                state.is_failed(),
                state.is_cancelled(),
                state.is_paused(),
            ];
            let count = flags.iter().filter(|&&f| f).count();
            prop_assert!(count == 1, "Expected exactly 1 true predicate, got {}: {:?}", count, flags);
        }

        // Property 9: `is_terminal()` is equivalent to completed, failed, or cancelled.
        #[test]
        fn terminal_consistency(state in arb_state()) {
            prop_assert_eq!(
                state.is_terminal(),
                state.is_completed() || state.is_failed() || state.is_cancelled(),
            );
        }

        // Property 10: `as_terminal_status().is_some() == !is_in_progress()`.
        #[test]
        fn as_terminal_status_consistency(state in arb_state()) {
            prop_assert_eq!(
                state.as_terminal_status().is_some(),
                !state.is_in_progress(),
            );
        }
    }
}
