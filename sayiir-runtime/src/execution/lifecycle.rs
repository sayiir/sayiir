//! Workflow lifecycle: prepare, resume, and finalize.

use bytes::Bytes;
use sayiir_core::error::WorkflowError;
use sayiir_core::snapshot::{
    ExecutionPosition, SignalKind, TaskHint, WorkflowSnapshot, WorkflowSnapshotState,
};
use sayiir_core::workflow::{ConflictPolicy, WorkflowStatus};
use sayiir_persistence::{SignalStore, SnapshotStore};

use super::helpers::ResumeParkedPosition;
use crate::error::RuntimeError;

/// Outcome of [`prepare_run`] when the conflict policy allows early return.
#[derive(Debug)]
pub enum PrepareRunOutcome {
    /// A fresh snapshot was created — proceed with execution.
    Fresh(Box<WorkflowSnapshot>),
    /// The instance already exists and the policy says to reuse it.
    ExistingStatus(WorkflowStatus, Option<Bytes>),
}

/// Check for an existing instance before encoding input.
///
/// For **`Fail`** and **`UseExisting`** policies, this avoids unnecessary codec
/// work by checking the backend before the caller serialises the workflow input.
///
/// Returns `Ok(Some((status, output)))` when the caller should return early
/// (instance exists and the policy says to reuse it, or the policy rejects the
/// duplicate).  Returns `Ok(None)` when the caller should proceed to encode
/// input and call [`prepare_run`].
///
/// **`TerminateExisting`** always returns `Ok(None)` — the actual cleanup is
/// deferred to [`prepare_run`].
///
/// # Errors
/// Returns [`RuntimeError::InstanceAlreadyExists`] for `Fail` when the
/// instance already exists, [`WorkflowError::DefinitionMismatch`] when the
/// existing snapshot has a different definition hash, or propagates backend
/// I/O errors.
pub async fn check_existing_instance<B>(
    instance_id: &str,
    definition_hash: &str,
    backend: &B,
    conflict_policy: ConflictPolicy,
) -> Result<Option<(WorkflowStatus, Option<Bytes>)>, RuntimeError>
where
    B: SnapshotStore,
{
    if matches!(conflict_policy, ConflictPolicy::TerminateExisting) {
        return Ok(None);
    }
    match backend.load_snapshot(instance_id).await {
        Ok(existing) => {
            if existing.definition_hash != definition_hash {
                return Err(WorkflowError::DefinitionMismatch {
                    expected: definition_hash.to_string(),
                    found: existing.definition_hash.clone(),
                }
                .into());
            }
            match conflict_policy {
                ConflictPolicy::Fail => {
                    Err(RuntimeError::InstanceAlreadyExists(instance_id.to_string()))
                }
                ConflictPolicy::UseExisting => {
                    let output = existing.state.completed_output().cloned();
                    let status = existing.state.as_status();
                    Ok(Some((status, output)))
                }
                ConflictPolicy::TerminateExisting => unreachable!(),
            }
        }
        Err(sayiir_persistence::BackendError::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Prepare a fresh workflow run: create initial snapshot and save it.
///
/// When called after [`check_existing_instance`] (the recommended path), set
/// `prechecked` to `true` to skip the redundant `load_snapshot`.  The only
/// policy that still needs a backend read is **`TerminateExisting`** (cleanup
/// of the old snapshot), which is handled regardless of the flag.
///
/// When `prechecked` is `false` the function performs the full existence check
/// internally as a safety net — this is useful for callers that cannot
/// guarantee a prior call to `check_existing_instance`.
///
/// # Errors
/// Returns an error if saving the initial snapshot fails or the conflict policy
/// rejects the duplicate.
#[tracing::instrument(
    name = "lifecycle.prepare_run",
    skip(input_bytes, backend),
    fields(%instance_id),
)]
pub async fn prepare_run<B>(
    instance_id: String,
    definition_hash: String,
    input_bytes: Bytes,
    first_task: TaskHint,
    backend: &B,
    conflict_policy: ConflictPolicy,
    prechecked: bool,
) -> Result<PrepareRunOutcome, RuntimeError>
where
    B: SnapshotStore + SignalStore,
{
    tracing::debug!("preparing fresh workflow run");

    // When prechecked == true the caller already verified via
    // check_existing_instance that the instance doesn't exist (Fail /
    // UseExisting) or that cleanup is needed (TerminateExisting).  We only
    // need the backend round-trip for TerminateExisting cleanup.
    if prechecked {
        if matches!(conflict_policy, ConflictPolicy::TerminateExisting) {
            // Best-effort cleanup — if nothing exists the deletes are no-ops.
            match backend.load_snapshot(&instance_id).await {
                Ok(_existing) => {
                    tracing::info!("terminating existing instance before restart");
                    backend.delete_snapshot(&instance_id).await?;
                    backend
                        .clear_signal(&instance_id, SignalKind::Cancel)
                        .await?;
                    backend
                        .clear_signal(&instance_id, SignalKind::Pause)
                        .await?;
                }
                Err(sayiir_persistence::BackendError::NotFound(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
    } else {
        // Full safety-net path — check for an existing snapshot.
        match backend.load_snapshot(&instance_id).await {
            Ok(existing) => {
                if existing.definition_hash != definition_hash {
                    return Err(WorkflowError::DefinitionMismatch {
                        expected: definition_hash,
                        found: existing.definition_hash.clone(),
                    }
                    .into());
                }
                match conflict_policy {
                    ConflictPolicy::Fail => {
                        return Err(RuntimeError::InstanceAlreadyExists(instance_id));
                    }
                    ConflictPolicy::UseExisting => {
                        let output = existing.state.completed_output().cloned();
                        let status = existing.state.as_status();
                        return Ok(PrepareRunOutcome::ExistingStatus(status, output));
                    }
                    ConflictPolicy::TerminateExisting => {
                        tracing::info!("terminating existing instance before restart");
                        backend.delete_snapshot(&instance_id).await?;
                        backend
                            .clear_signal(&instance_id, SignalKind::Cancel)
                            .await?;
                        backend
                            .clear_signal(&instance_id, SignalKind::Pause)
                            .await?;
                    }
                }
            }
            Err(sayiir_persistence::BackendError::NotFound(_)) => {
                // No existing snapshot — proceed normally
            }
            Err(e) => return Err(e.into()),
        }
    }

    let mut snapshot =
        WorkflowSnapshot::with_initial_input(instance_id, definition_hash, input_bytes);
    #[cfg(feature = "otel")]
    {
        snapshot.trace_parent = crate::trace_context::current_trace_parent();
    }
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: first_task.id.clone(),
    });
    snapshot.set_task_hint(&first_task);
    backend.save_snapshot(&snapshot).await?;
    Ok(PrepareRunOutcome::Fresh(Box::new(snapshot)))
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
#[tracing::instrument(
    name = "lifecycle.prepare_resume",
    skip(backend),
    fields(%instance_id),
)]
pub async fn prepare_resume<B>(
    instance_id: &str,
    definition_hash: &str,
    backend: &B,
) -> Result<ResumeOutcome, RuntimeError>
where
    B: SignalStore,
{
    tracing::debug!("preparing workflow resume");
    let mut snapshot = backend.load_snapshot(instance_id).await?;

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

    // Resolve any parked position (delay / signal / fork) before resuming.
    // This consumes buffered signals, checks delay expiry, etc. and updates
    // the snapshot so get_resume_input picks up the correct value.
    let parked = ResumeParkedPosition::extract(&snapshot);
    if let Some(status) = parked.resolve(&mut snapshot, instance_id, backend).await? {
        return Ok(ResumeOutcome::NotReady(status));
    }

    // Determine resume input (after resolve, so signal payloads are reflected)
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
    /// Parked position not yet ready (delay not expired, signal not arrived, etc.).
    NotReady(WorkflowStatus),
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
#[tracing::instrument(
    name = "lifecycle.finalize",
    skip_all,
    fields(instance_id = %snapshot.instance_id),
)]
pub async fn finalize_execution<B>(
    result: Result<Bytes, RuntimeError>,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
) -> Result<(WorkflowStatus, Option<Bytes>), RuntimeError>
where
    B: SnapshotStore,
{
    tracing::debug!("finalizing workflow execution");
    match result {
        Ok(output) => {
            tracing::info!(instance_id = %snapshot.instance_id, "workflow completed");
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
        Err(RuntimeError::Workflow(WorkflowError::AwaitingSignal {
            signal_id,
            signal_name,
            wake_at,
        })) => {
            tracing::info!(
                instance_id = %snapshot.instance_id,
                %signal_id,
                %signal_name,
                ?wake_at,
                "workflow parked at signal"
            );
            Ok((
                WorkflowStatus::AwaitingSignal {
                    signal_id,
                    signal_name,
                    wake_at,
                },
                None,
            ))
        }
        Err(RuntimeError::Workflow(WorkflowError::Cancelled { .. })) => {
            tracing::info!(instance_id = %snapshot.instance_id, "workflow cancelled");
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
            tracing::info!(instance_id = %snapshot.instance_id, "workflow paused");
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
            tracing::error!(instance_id = %snapshot.instance_id, error = %e, "workflow failed");
            snapshot.mark_failed(e.to_string());
            let _ = backend.save_snapshot(snapshot).await;
            Ok((WorkflowStatus::Failed(e.to_string()), None))
        }
    }
}
