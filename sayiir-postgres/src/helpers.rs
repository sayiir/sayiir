//! Internal helpers for snapshot ↔ database field extraction.

use chrono::{DateTime, Utc};
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};

/// Extract the status string from a [`WorkflowSnapshotState`].
pub(crate) fn status_str(state: &WorkflowSnapshotState) -> &str {
    state.as_ref()
}

/// Extract the current task ID from a snapshot, if at a task position.
pub(crate) fn current_task_id(snapshot: &WorkflowSnapshot) -> Option<&str> {
    match &snapshot.state {
        WorkflowSnapshotState::InProgress {
            position: ExecutionPosition::AtTask { task_id },
            ..
        } => Some(task_id.as_str()),
        _ => None,
    }
}

/// Extract the count of completed tasks from a snapshot.
pub(crate) fn completed_task_count(snapshot: &WorkflowSnapshot) -> i32 {
    match &snapshot.state {
        WorkflowSnapshotState::InProgress {
            completed_tasks, ..
        }
        | WorkflowSnapshotState::Cancelled {
            completed_tasks, ..
        }
        | WorkflowSnapshotState::Paused {
            completed_tasks, ..
        } => completed_tasks.len().try_into().unwrap_or(i32::MAX),
        _ => 0,
    }
}

/// Extract the error message from a snapshot, if in failed state.
pub(crate) fn error_message(snapshot: &WorkflowSnapshot) -> Option<&str> {
    match &snapshot.state {
        WorkflowSnapshotState::Failed { error } => Some(error.as_str()),
        _ => None,
    }
}

/// Whether the snapshot is in a terminal state (needs `completed_at` set).
pub(crate) fn is_terminal(snapshot: &WorkflowSnapshot) -> bool {
    snapshot.state.is_terminal()
}

/// Extract the position kind string from a snapshot.
///
/// Returns `None` for terminal states (Completed, Failed, Cancelled)
/// since those don't have a meaningful position.
pub(crate) fn position_kind(snapshot: &WorkflowSnapshot) -> Option<&str> {
    match &snapshot.state {
        WorkflowSnapshotState::InProgress { position, .. }
        | WorkflowSnapshotState::Paused { position, .. } => Some(position.as_ref()),
        _ => None,
    }
}

/// Extract the delay `wake_at` time, if the workflow is parked at a delay.
pub(crate) fn delay_wake_at(snapshot: &WorkflowSnapshot) -> Option<DateTime<Utc>> {
    match &snapshot.state {
        WorkflowSnapshotState::InProgress {
            position: ExecutionPosition::AtDelay { wake_at, .. },
            ..
        }
        | WorkflowSnapshotState::InProgress {
            position:
                ExecutionPosition::AtSignal {
                    wake_at: Some(wake_at),
                    ..
                },
            ..
        } => Some(*wake_at),
        _ => None,
    }
}
