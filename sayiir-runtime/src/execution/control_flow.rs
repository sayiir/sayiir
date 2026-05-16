//! ControlFlow-based step results for checkpointing executors.
//!
//! These types replace the pattern of using `return Err(park_error)` to
//! represent workflow parking (delays, signals, forks). Parking is now an
//! explicit `StepOutcome::Park` variant, clearly separated from real errors.

use std::ops::ControlFlow;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use sayiir_core::error::WorkflowError;
use sayiir_core::snapshot::{ExecutionPosition, TaskHint};
use sayiir_persistence::SnapshotStore;

use crate::error::RuntimeError;

/// Why a workflow step needs to park.
pub(crate) enum ParkReason {
    /// A durable delay that hasn't expired yet.
    Delay {
        delay_id: String,
        wake_at: DateTime<Utc>,
        /// Pre-computed hint for the next task (priority + tags for persistence-layer advancement).
        next_task: Option<TaskHint>,
        /// Pass-through value stored as the delay's "result".
        passthrough: Bytes,
    },
    /// Waiting for an external signal that hasn't arrived yet.
    AwaitingSignal {
        signal_id: String,
        signal_name: String,
        timeout: Option<DateTime<Utc>>,
        /// Pre-computed hint for the next task (priority + tags for persistence-layer advancement).
        next_task: Option<TaskHint>,
    },
}

/// Terminal outcome of a step that doesn't continue the chain.
pub(crate) enum StepOutcome {
    /// Workflow produced its final output (e.g. fork with no join).
    Done(Bytes),
    /// Workflow needs to park and checkpoint.
    Park(ParkReason),
}

/// Result of executing one step in a checkpointing executor.
///
/// - `Ok(Continue(output))` — step succeeded, advance to the next node.
/// - `Ok(Break(Done(output)))` — step produced the workflow's final result.
/// - `Ok(Break(Park(reason)))` — step needs to park; save checkpoint and return.
/// - `Err(e)` — a real runtime error occurred.
pub(crate) type StepResult = Result<ControlFlow<StepOutcome, Bytes>, RuntimeError>;

/// Compute `wake_at` from a duration, returning a `RuntimeError` on overflow.
pub(crate) fn compute_wake_at(
    duration: &std::time::Duration,
) -> Result<DateTime<Utc>, RuntimeError> {
    let now = Utc::now();
    chrono::Duration::from_std(*duration)
        .map(|d| now + d)
        .map_err(|e| RuntimeError::from(WorkflowError::ResumeError(e.to_string())))
}

/// Compute an optional signal timeout deadline.
pub(crate) fn compute_signal_timeout(
    timeout: Option<&std::time::Duration>,
) -> Option<DateTime<Utc>> {
    timeout.and_then(|d| {
        chrono::Duration::from_std(*d)
            .ok()
            .map(|cd| Utc::now() + cd)
    })
}

/// Persist a park checkpoint for the main (non-branch) executors.
///
/// Maps each [`ParkReason`] to the appropriate snapshot position update,
/// saves the snapshot, and returns the corresponding `RuntimeError` that
/// the caller will propagate to `finalize_execution`.
pub(crate) async fn save_park_checkpoint<B: SnapshotStore>(
    reason: ParkReason,
    snapshot: &mut sayiir_core::snapshot::WorkflowSnapshot,
    backend: &B,
) -> RuntimeError {
    match reason {
        ParkReason::Delay {
            delay_id,
            wake_at,
            next_task,
            passthrough,
        } => {
            let next_task_id = next_task.as_ref().map(|h| h.id.clone());
            snapshot.set_task_hint(next_task.as_ref().unwrap_or(&TaskHint::default()));
            let now = Utc::now();
            snapshot.update_position(ExecutionPosition::AtDelay {
                delay_id: delay_id.clone(),
                entered_at: now,
                wake_at,
                next_task_id,
            });
            snapshot.mark_task_completed(delay_id, passthrough);
            if let Err(e) = backend.save_snapshot(snapshot).await {
                return RuntimeError::from(e);
            }
            WorkflowError::Waiting { wake_at }.into()
        }
        ParkReason::AwaitingSignal {
            signal_id,
            signal_name,
            timeout,
            next_task,
        } => {
            let next_task_id = next_task.as_ref().map(|h| h.id.clone());
            snapshot.set_task_hint(next_task.as_ref().unwrap_or(&TaskHint::default()));
            snapshot.update_position(ExecutionPosition::AtSignal {
                signal_id: signal_id.clone(),
                signal_name: signal_name.clone(),
                wake_at: timeout,
                next_task_id,
            });
            if let Err(e) = backend.save_snapshot(snapshot).await {
                return RuntimeError::from(e);
            }
            WorkflowError::AwaitingSignal {
                signal_id,
                signal_name,
                wake_at: timeout,
            }
            .into()
        }
    }
}

/// Persist a park checkpoint for branch executors.
///
/// Branch executors save individual task results (not full snapshots)
/// and return `WorkflowError::Waiting` or `WorkflowError::AwaitingSignal`.
pub(crate) async fn save_branch_park_checkpoint<B: SnapshotStore>(
    reason: ParkReason,
    instance_id: &str,
    backend: &B,
) -> RuntimeError {
    match reason {
        ParkReason::Delay {
            delay_id,
            wake_at,
            passthrough,
            ..
        } => {
            tracing::info!(delay_id = %delay_id, "parking branch at delay");
            if let Err(e) = backend
                .save_task_result(instance_id, &delay_id, passthrough)
                .await
            {
                return RuntimeError::from(e);
            }
            WorkflowError::Waiting { wake_at }.into()
        }
        ParkReason::AwaitingSignal {
            signal_id,
            signal_name,
            timeout,
            ..
        } => {
            tracing::info!(signal_id = %signal_id, %signal_name, "parking branch at signal");
            WorkflowError::AwaitingSignal {
                signal_id,
                signal_name,
                wake_at: timeout,
            }
            .into()
        }
    }
}
