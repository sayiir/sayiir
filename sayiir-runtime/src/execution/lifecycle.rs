//! Workflow lifecycle: prepare, resume, and finalize.

use bytes::Bytes;
use sayiir_core::error::WorkflowError;
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};
use sayiir_core::workflow::WorkflowStatus;
use sayiir_persistence::SnapshotStore;

use crate::error::RuntimeError;

/// Prepare a fresh workflow run: create initial snapshot and save it.
///
/// Returns the snapshot and encoded input ready for execution.
///
/// # Errors
/// Returns an error if saving the initial snapshot fails.
pub async fn prepare_run<B>(
    instance_id: String,
    definition_hash: String,
    input_bytes: Bytes,
    first_task_id: String,
    backend: &B,
) -> Result<WorkflowSnapshot, RuntimeError>
where
    B: SnapshotStore,
{
    let mut snapshot =
        WorkflowSnapshot::with_initial_input(instance_id, definition_hash, input_bytes);
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: first_task_id,
    });
    backend.save_snapshot(&snapshot).await?;
    Ok(snapshot)
}

/// Prepare to resume a workflow from a saved snapshot.
///
/// Loads the snapshot, validates the definition hash, checks for terminal states,
/// and determines the correct resume input.
///
/// Returns `Ok(Some((snapshot, input)))` if the workflow can be resumed,
/// or `Ok(None)` with the terminal status if the workflow is already done.
///
/// # Errors
/// Returns an error if the snapshot cannot be loaded or the definition hash mismatches.
pub async fn prepare_resume<B>(
    instance_id: &str,
    definition_hash: &str,
    backend: &B,
) -> Result<ResumeOutcome, RuntimeError>
where
    B: SnapshotStore,
{
    let snapshot = backend.load_snapshot(instance_id).await?;

    // Validate definition hash
    if snapshot.definition_hash != definition_hash {
        return Err(WorkflowError::DefinitionMismatch {
            expected: definition_hash.to_string(),
            found: snapshot.definition_hash.clone(),
        }
        .into());
    }

    // Check if already in terminal state
    if let Some(status) = snapshot.state.as_terminal_status() {
        if snapshot.state.is_paused() {
            return Ok(ResumeOutcome::Paused(status));
        }
        return Ok(ResumeOutcome::AlreadyTerminal(status));
    }

    // Determine resume input
    let input_bytes = get_resume_input(&snapshot)?;
    Ok(ResumeOutcome::Ready {
        snapshot: Box::new(snapshot),
        input_bytes,
    })
}

/// Outcome of [`prepare_resume`].
#[derive(Debug)]
pub enum ResumeOutcome {
    /// Workflow can be resumed with this snapshot and input.
    Ready {
        /// The loaded snapshot (in-progress state).
        snapshot: Box<WorkflowSnapshot>,
        /// The input bytes for the next task.
        input_bytes: Bytes,
    },
    /// Workflow is already in a terminal state.
    AlreadyTerminal(WorkflowStatus),
    /// Workflow is paused (not terminal, but cannot execute until unpaused).
    Paused(WorkflowStatus),
}

/// Get the input for resuming execution from a snapshot.
///
/// Uses the last completed task's output, or the initial input if no tasks
/// have completed yet.
///
/// # Errors
/// Returns an error if no resume input can be determined.
pub fn get_resume_input(snapshot: &WorkflowSnapshot) -> Result<Bytes, RuntimeError> {
    match &snapshot.state {
        WorkflowSnapshotState::InProgress {
            completed_tasks, ..
        } => {
            if completed_tasks.is_empty() {
                snapshot.initial_input_bytes().ok_or_else(|| {
                    WorkflowError::ResumeError(
                        "no completed tasks and initial input not stored".into(),
                    )
                    .into()
                })
            } else {
                snapshot.get_last_task_output().ok_or_else(|| {
                    WorkflowError::ResumeError("no task results available".into()).into()
                })
            }
        }
        _ => Err(WorkflowError::ResumeError("workflow not in progress".into()).into()),
    }
}

/// Finalize a workflow execution, converting the result to a [`WorkflowStatus`].
///
/// On success, marks the workflow as completed in the snapshot and returns the
/// output bytes alongside the status.
/// On cancellation error, returns `Cancelled` status with details from the backend.
/// On other errors, marks the workflow as failed.
///
/// This mirrors `CheckpointingRunner::handle_execution_result`.
///
/// # Errors
/// Returns an error if saving the snapshot to the backend fails.
pub async fn finalize_execution<B>(
    result: Result<Bytes, RuntimeError>,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
) -> Result<(WorkflowStatus, Option<Bytes>), RuntimeError>
where
    B: SnapshotStore,
{
    match result {
        Ok(output) => {
            snapshot.mark_completed(output.clone());
            backend.save_snapshot(snapshot).await?;
            Ok((WorkflowStatus::Completed, Some(output)))
        }
        Err(RuntimeError::Workflow(WorkflowError::Waiting { wake_at })) => {
            let delay_id = match &snapshot.state {
                WorkflowSnapshotState::InProgress {
                    position: ExecutionPosition::AtDelay { delay_id, .. },
                    ..
                } => delay_id.clone(),
                WorkflowSnapshotState::InProgress {
                    position: ExecutionPosition::AtFork { fork_id, .. },
                    ..
                } => fork_id.clone(),
                _ => String::new(),
            };
            tracing::info!(
                instance_id = %snapshot.instance_id,
                %delay_id,
                %wake_at,
                "workflow parked at delay"
            );
            Ok((WorkflowStatus::Waiting { wake_at, delay_id }, None))
        }
        Err(RuntimeError::Workflow(WorkflowError::Cancelled { .. })) => {
            // Reload snapshot to get cancellation details (set by check_and_cancel)
            if let Ok(cancelled_snapshot) = backend.load_snapshot(&snapshot.instance_id).await
                && let Some((reason, cancelled_by)) =
                    cancelled_snapshot.state.cancellation_details()
            {
                return Ok((
                    WorkflowStatus::Cancelled {
                        reason,
                        cancelled_by,
                    },
                    None,
                ));
            }
            // Fallback if we couldn't get details
            Ok((
                WorkflowStatus::Cancelled {
                    reason: None,
                    cancelled_by: None,
                },
                None,
            ))
        }
        Err(RuntimeError::Workflow(WorkflowError::Paused { .. })) => {
            // Reload snapshot to get pause details (set by check_and_pause)
            if let Ok(paused_snapshot) = backend.load_snapshot(&snapshot.instance_id).await
                && let Some((reason, paused_by)) = paused_snapshot.state.pause_details()
            {
                return Ok((WorkflowStatus::Paused { reason, paused_by }, None));
            }
            Ok((
                WorkflowStatus::Paused {
                    reason: None,
                    paused_by: None,
                },
                None,
            ))
        }
        Err(e) => {
            snapshot.mark_failed(e.to_string());
            let _ = backend.save_snapshot(snapshot).await;
            Ok((WorkflowStatus::Failed(e.to_string()), None))
        }
    }
}
