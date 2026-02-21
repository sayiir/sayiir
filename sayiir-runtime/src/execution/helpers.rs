//! Guards, retry, deadline, and task execution primitives.

use bytes::Bytes;
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};
use sayiir_core::workflow::{WorkflowContinuation, WorkflowStatus};
use sayiir_persistence::{SignalStore, SnapshotStore};
use std::future::Future;

use crate::error::RuntimeError;
use sayiir_core::error::WorkflowError;

// ── Guards ──────────────────────────────────────────────────────────────

/// Check for cancellation and pause before or after a task boundary.
///
/// Combines the cancel + pause check into a single call. Returns `Ok(())` if
/// execution should proceed, or an error if the workflow was cancelled or paused.
pub(crate) async fn check_guards<B: SignalStore>(
    backend: &B,
    instance_id: &str,
    cancel_scope: Option<&str>,
) -> Result<(), RuntimeError> {
    if backend.check_and_cancel(instance_id, cancel_scope).await? {
        return Err(WorkflowError::cancelled().into());
    }
    if backend.check_and_pause(instance_id).await? {
        return Err(WorkflowError::paused().into());
    }
    Ok(())
}

/// Result of checking a parked position (delay or fork) on resume.
pub(crate) enum ParkedCheckResult {
    /// Workflow was cancelled while parked.
    Cancelled(WorkflowStatus),
    /// Workflow was paused while parked.
    Paused(WorkflowStatus),
    /// The wake time has not yet arrived.
    StillWaiting {
        wake_at: chrono::DateTime<chrono::Utc>,
        node_id: String,
    },
    /// The wake time has passed; execution can continue.
    Expired,
}

impl ParkedCheckResult {
    /// If the position is not expired, return the status for early return.
    pub(crate) fn into_status(self) -> Option<WorkflowStatus> {
        match self {
            Self::Cancelled(status) | Self::Paused(status) => Some(status),
            Self::StillWaiting { wake_at, node_id } => Some(WorkflowStatus::Waiting {
                wake_at,
                delay_id: node_id,
            }),
            Self::Expired => None,
        }
    }
}

/// Check cancel / pause / wake-time for a parked position (delay or fork).
pub(crate) async fn check_parked_position<B: SignalStore>(
    backend: &B,
    instance_id: &str,
    node_id: &str,
    wake_at: chrono::DateTime<chrono::Utc>,
) -> Result<ParkedCheckResult, RuntimeError> {
    if backend.check_and_cancel(instance_id, Some(node_id)).await? {
        let snapshot = backend.load_snapshot(instance_id).await?;
        let (reason, cancelled_by) = snapshot
            .state
            .cancellation_details()
            .unwrap_or((None, None));
        return Ok(ParkedCheckResult::Cancelled(WorkflowStatus::Cancelled {
            reason,
            cancelled_by,
        }));
    }
    if backend.check_and_pause(instance_id).await? {
        let snapshot = backend.load_snapshot(instance_id).await?;
        let (reason, paused_by) = snapshot.state.pause_details().unwrap_or((None, None));
        return Ok(ParkedCheckResult::Paused(WorkflowStatus::Paused {
            reason,
            paused_by,
        }));
    }
    if chrono::Utc::now() < wake_at {
        return Ok(ParkedCheckResult::StillWaiting {
            wake_at,
            node_id: node_id.to_string(),
        });
    }
    Ok(ParkedCheckResult::Expired)
}

/// Describes the parked position a snapshot was in when `resume` loaded it.
///
/// Extracted as owned data from the snapshot so the snapshot can be mutated
/// afterwards without borrow conflicts.
pub(crate) enum ResumeParkedPosition {
    /// Workflow was parked at a durable delay.
    Delay {
        wake_at: chrono::DateTime<chrono::Utc>,
        delay_id: String,
        next_task_id: Option<String>,
    },
    /// Workflow was parked at a fork (one or more branches waiting).
    Fork {
        wake_at: chrono::DateTime<chrono::Utc>,
        fork_id: String,
    },
    /// Workflow was parked waiting for an external signal.
    Signal {
        signal_id: String,
        signal_name: String,
        wake_at: Option<chrono::DateTime<chrono::Utc>>,
        next_task_id: Option<String>,
    },
    /// Snapshot is not in a parked position.
    NotParked,
}

impl ResumeParkedPosition {
    /// Extract the parked position from a snapshot, cloning the necessary fields.
    pub(crate) fn extract(snapshot: &WorkflowSnapshot) -> Self {
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
            } => Self::Delay {
                wake_at: *wake_at,
                delay_id: delay_id.clone(),
                next_task_id: next_task_id.clone(),
            },
            WorkflowSnapshotState::InProgress {
                position:
                    ExecutionPosition::AtFork {
                        fork_id, wake_at, ..
                    },
                ..
            } => Self::Fork {
                wake_at: *wake_at,
                fork_id: fork_id.clone(),
            },
            WorkflowSnapshotState::InProgress {
                position:
                    ExecutionPosition::AtSignal {
                        signal_id,
                        signal_name,
                        wake_at,
                        next_task_id,
                    },
                ..
            } => Self::Signal {
                signal_id: signal_id.clone(),
                signal_name: signal_name.clone(),
                wake_at: *wake_at,
                next_task_id: next_task_id.clone(),
            },
            _ => Self::NotParked,
        }
    }

    /// Check the parked position and advance the snapshot if the park has expired.
    ///
    /// Returns `Ok(Some(status))` when the workflow should not continue (cancelled,
    /// paused, or still waiting). Returns `Ok(None)` when the parked position has
    /// expired and the snapshot has been advanced — the caller should proceed with
    /// normal execution.
    pub(crate) async fn resolve<B: SignalStore>(
        self,
        snapshot: &mut WorkflowSnapshot,
        instance_id: &str,
        backend: &B,
    ) -> Result<Option<WorkflowStatus>, RuntimeError> {
        match self {
            Self::NotParked => Ok(None),
            Self::Delay {
                wake_at,
                delay_id,
                next_task_id,
            } => {
                let result =
                    check_parked_position(backend, instance_id, &delay_id, wake_at).await?;
                if let Some(status) = result.into_status() {
                    return Ok(Some(status));
                }
                tracing::info!(instance_id, %delay_id, "delay expired, advancing execution");
                if let Some(next_id) = next_task_id {
                    snapshot.update_position(ExecutionPosition::AtTask { task_id: next_id });
                } else {
                    tracing::info!(instance_id, %delay_id, "delay was last node, completing workflow");
                    let output = snapshot
                        .get_task_result_bytes(&delay_id)
                        .unwrap_or_default();
                    snapshot.mark_completed(output);
                    backend.save_snapshot(snapshot).await?;
                    return Ok(Some(WorkflowStatus::Completed));
                }
                backend.save_snapshot(snapshot).await?;
                Ok(None)
            }
            Self::Fork { wake_at, fork_id } => {
                let result = check_parked_position(backend, instance_id, &fork_id, wake_at).await?;
                if let Some(status) = result.into_status() {
                    return Ok(Some(status));
                }
                tracing::info!(instance_id, %fork_id, "fork delays expired, resuming execution");
                Ok(None)
            }
            Self::Signal {
                signal_id,
                signal_name,
                wake_at,
                next_task_id,
            } => {
                // Check cancel/pause first
                if backend
                    .check_and_cancel(instance_id, Some(&signal_id))
                    .await?
                {
                    let snap = backend.load_snapshot(instance_id).await?;
                    let (reason, cancelled_by) =
                        snap.state.cancellation_details().unwrap_or((None, None));
                    return Ok(Some(WorkflowStatus::Cancelled {
                        reason,
                        cancelled_by,
                    }));
                }
                if backend.check_and_pause(instance_id).await? {
                    let snap = backend.load_snapshot(instance_id).await?;
                    let (reason, paused_by) = snap.state.pause_details().unwrap_or((None, None));
                    return Ok(Some(WorkflowStatus::Paused { reason, paused_by }));
                }

                // Try to consume a buffered signal
                if let Some(payload) = backend.consume_event(instance_id, &signal_name).await? {
                    tracing::info!(instance_id, %signal_id, %signal_name, "signal received, advancing");
                    // Store under signal_id (node ID) so the executor's skip logic finds it
                    snapshot.mark_task_completed(signal_id.clone(), payload);
                    if let Some(next_id) = next_task_id {
                        snapshot.update_position(ExecutionPosition::AtTask { task_id: next_id });
                    } else {
                        let output = snapshot
                            .get_task_result_bytes(&signal_id)
                            .unwrap_or_default();
                        snapshot.mark_completed(output);
                        backend.save_snapshot(snapshot).await?;
                        return Ok(Some(WorkflowStatus::Completed));
                    }
                    backend.save_snapshot(snapshot).await?;
                    return Ok(None);
                }

                // Check timeout
                if let Some(wt) = wake_at
                    && chrono::Utc::now() >= wt
                {
                    tracing::info!(instance_id, %signal_id, %signal_name, "signal timed out, advancing with empty payload");
                    snapshot.mark_task_completed(signal_id, Bytes::new());
                    if let Some(next_id) = next_task_id {
                        snapshot.update_position(ExecutionPosition::AtTask { task_id: next_id });
                    } else {
                        snapshot.mark_completed(Bytes::new());
                        backend.save_snapshot(snapshot).await?;
                        return Ok(Some(WorkflowStatus::Completed));
                    }
                    backend.save_snapshot(snapshot).await?;
                    return Ok(None);
                }

                // Still waiting
                Ok(Some(WorkflowStatus::AwaitingSignal {
                    signal_id,
                    signal_name,
                    wake_at,
                }))
            }
        }
    }
}

// ── Branch helpers ──────────────────────────────────────────────────────

/// Decode the routing key and look up the matching branch continuation.
///
/// Combines `EnvelopeCodec::decode_string` with the `HashMap` lookup and
/// `BranchKeyNotFound` error, eliminating the 7-line pattern repeated in
/// every executor.
pub(crate) fn resolve_branch<'a, E: sayiir_core::codec::EnvelopeCodec>(
    branch_id: &str,
    key_bytes: &[u8],
    branches: &'a std::collections::HashMap<String, Box<WorkflowContinuation>>,
    default: Option<&'a WorkflowContinuation>,
    envelope_codec: &E,
) -> Result<(String, &'a WorkflowContinuation), RuntimeError> {
    let key: String = envelope_codec
        .decode_string(key_bytes)
        .map_err(RuntimeError::from)?;
    let chosen = branches
        .get(&key)
        .map(AsRef::as_ref)
        .or(default)
        .ok_or_else(|| WorkflowError::BranchKeyNotFound {
            branch_id: branch_id.to_string(),
            key: key.clone(),
        })?;
    Ok((key, chosen))
}

// ── Retry ───────────────────────────────────────────────────────────────

/// Convert a `RetryPolicy` into a `backon::ExponentialBuilder` for in-process retries.
///
/// When `policy` is `None`, the builder allows zero retries (i.e. no retry).
pub(super) fn policy_to_backoff(
    policy: Option<&sayiir_core::task::RetryPolicy>,
) -> backon::ExponentialBuilder {
    match policy {
        Some(rp) => {
            let builder = backon::ExponentialBuilder::default()
                .with_min_delay(rp.initial_delay)
                .with_factor(rp.backoff_multiplier)
                .with_max_times(rp.max_retries as usize);
            match rp.max_delay {
                Some(max) => builder.with_max_delay(max),
                None => builder.without_max_delay(),
            }
        }
        None => backon::ExponentialBuilder::default().with_max_times(0),
    }
}

/// Retry a checkpointing task with snapshot side-effects between attempts.
///
pub(crate) async fn retry_with_checkpoint<B>(
    task_id: &str,
    retry_policy: Option<&sayiir_core::task::RetryPolicy>,
    timeout: Option<&std::time::Duration>,
    snapshot: &mut WorkflowSnapshot,
    save_backend: Option<&B>,
    mut execute: impl AsyncFnMut(&mut WorkflowSnapshot) -> Result<Bytes, RuntimeError>,
) -> Result<Bytes, RuntimeError>
where
    B: SnapshotStore,
{
    loop {
        match execute(snapshot).await {
            Ok(output) => {
                snapshot.clear_retry_state(task_id);
                return Ok(output);
            }
            Err(e) => {
                if let Some(rp) = retry_policy
                    && !snapshot.retries_exhausted(task_id)
                {
                    let next_retry_at = snapshot.record_retry(task_id, rp, &e.to_string(), None);
                    snapshot.clear_task_deadline();

                    if let Some(backend) = save_backend {
                        backend.save_snapshot(snapshot).await?;
                    }

                    tracing::info!(
                        task_id = %task_id,
                        attempt = snapshot.get_retry_state(task_id).map_or(0, |rs| rs.attempts),
                        max_retries = rp.max_retries,
                        %next_retry_at,
                        error = %e,
                        "Retrying task (checkpointing)"
                    );

                    let delay = (next_retry_at - chrono::Utc::now())
                        .to_std()
                        .unwrap_or_default();
                    tokio::time::sleep(delay).await;

                    if let Some(backend) = save_backend {
                        set_deadline_if_needed(task_id, timeout, snapshot, backend).await?;
                    }
                    continue;
                }
                return Err(e);
            }
        }
    }
}

// ── Deadline ────────────────────────────────────────────────────────────

/// Maximum interval between periodic deadline checks (1 second).
const MAX_DEADLINE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Minimum interval between periodic deadline checks (1 millisecond).
const MIN_DEADLINE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);

/// Compute the check interval for a deadline: `min(1s, remaining / 2)`,
/// clamped to at least 1ms.
fn deadline_check_interval(deadline: &sayiir_core::snapshot::TaskDeadline) -> std::time::Duration {
    let remaining = (deadline.deadline - chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    (remaining / 2)
        .max(MIN_DEADLINE_CHECK_INTERVAL)
        .min(MAX_DEADLINE_CHECK_INTERVAL)
}

/// Persist a task deadline on the snapshot if the task is uncached and has a timeout.
pub(crate) async fn set_deadline_if_needed<B: SnapshotStore>(
    id: &str,
    timeout: Option<&std::time::Duration>,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
) -> Result<(), RuntimeError> {
    if snapshot.get_task_result(id).is_none()
        && let Some(d) = timeout
    {
        snapshot.set_task_deadline(id.to_string(), *d);
        backend.save_snapshot(snapshot).await?;
    }
    Ok(())
}

// ── Task execution ──────────────────────────────────────────────────────

/// Execute a task or skip it if already cached in the snapshot.
///
/// Checks the snapshot cache first. If the task result exists, clears any
/// deadline and returns it. Otherwise, checks for an expired deadline (crash
/// recovery: previous attempt's deadline expired), then executes the task.
///
/// When a deadline is set on the snapshot, the task future is raced against a
/// periodic check of the **persisted** deadline (like the worker heartbeat).
/// If the deadline expires mid-execution, the future is dropped (active
/// cancellation). Without a deadline, the task runs normally.
///
/// The caller is responsible for setting the deadline on the snapshot *before*
/// calling this function.
///
/// Does NOT save the snapshot to the backend — the caller handles position
/// update + save after, since the position depends on context.
pub(crate) async fn execute_or_skip_task<F, Fut, E>(
    id: &str,
    input: Bytes,
    execute: F,
    snapshot: &mut WorkflowSnapshot,
) -> Result<Bytes, RuntimeError>
where
    F: FnOnce(Bytes) -> Fut,
    Fut: Future<Output = Result<Bytes, E>>,
    E: Into<RuntimeError>,
{
    if let Some(cached) = snapshot.get_task_result(id).map(|r| r.output.clone()) {
        snapshot.clear_task_deadline();
        return Ok(cached);
    }

    // Crash recovery: deadline from a previous attempt already expired
    if let Some((tid, timeout)) = snapshot.expired_task_deadline() {
        let err = WorkflowError::TaskTimedOut {
            task_id: tid.to_string(),
            timeout,
        };
        snapshot.clear_task_deadline();
        return Err(err.into());
    }

    // Refresh deadline to now + timeout so it measures actual execution time,
    // not time spent on prior snapshot-save I/O.
    snapshot.refresh_task_deadline();

    let output = if let Some(dl) = &snapshot.task_deadline {
        let task_future = execute(input);
        tokio::pin!(task_future);
        let mut interval = tokio::time::interval(deadline_check_interval(dl));
        interval.tick().await; // skip first immediate tick

        loop {
            tokio::select! {
                result = &mut task_future => break result.map_err(Into::into)?,
                _ = interval.tick() => {
                    if let Some((tid, timeout)) = snapshot.expired_task_deadline() {
                        let err = WorkflowError::TaskTimedOut {
                            task_id: tid.to_string(),
                            timeout,
                        };
                        snapshot.clear_task_deadline();
                        return Err(err.into());
                    }
                }
            }
        }
    } else {
        execute(input).await.map_err(Into::into)?
    };

    snapshot.clear_task_deadline();
    snapshot.mark_task_completed(id.to_string(), output.clone());
    Ok(output)
}

/// Execute a task within a branch, skipping if already cached.
///
/// Checks the snapshot cache first. If uncached, sets an in-memory deadline,
/// executes the task, checks for expiry, clears the deadline, and saves the
/// result directly to the backend via `save_task_result`.
///
/// Unlike [`execute_or_skip_task`], this function does NOT race against a
/// persisted deadline — branch tasks use a simpler post-execution check.
pub(crate) async fn branch_execute_or_skip_task<F, Fut, E, B>(
    id: &str,
    input: Bytes,
    execute: F,
    timeout: Option<&std::time::Duration>,
    snapshot: &mut WorkflowSnapshot,
    instance_id: &str,
    backend: &B,
) -> Result<Bytes, RuntimeError>
where
    F: FnOnce(Bytes) -> Fut,
    Fut: Future<Output = Result<Bytes, E>>,
    E: Into<RuntimeError>,
    B: SnapshotStore,
{
    if let Some(cached) = snapshot.get_task_result(id).map(|r| r.output.clone()) {
        return Ok(cached);
    }

    if let Some(d) = timeout {
        snapshot.set_task_deadline(id.to_string(), *d);
    }
    let output = execute(input).await.map_err(Into::into)?;

    if let Some((tid, t)) = snapshot.expired_task_deadline() {
        let err = WorkflowError::TaskTimedOut {
            task_id: tid.to_string(),
            timeout: t,
        };
        snapshot.clear_task_deadline();
        return Err(err.into());
    }
    snapshot.clear_task_deadline();

    backend
        .save_task_result(instance_id, id, output.clone())
        .await?;
    Ok(output)
}

/// Execute one checkpointed task with the full guard/deadline/retry/save lifecycle.
///
/// Task parameters extracted from [`WorkflowContinuation::Task`].
pub(crate) struct TaskStepParams<'a> {
    pub id: &'a str,
    pub timeout: Option<&'a std::time::Duration>,
    pub retry_policy: Option<&'a sayiir_core::task::RetryPolicy>,
    pub next: Option<&'a WorkflowContinuation>,
}

/// Runs pre-guards, sets the deadline, retries via [`retry_with_checkpoint`],
/// updates the snapshot position to the next task, saves the snapshot, and
/// runs post-guards.
pub(crate) async fn execute_task_step<B, ExecFn, ExecFut, E>(
    params: &TaskStepParams<'_>,
    current_input: Bytes,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
    execute: ExecFn,
) -> Result<Bytes, RuntimeError>
where
    B: SignalStore,
    ExecFn: Fn(Bytes) -> ExecFut,
    ExecFut: Future<Output = Result<Bytes, E>> + Send,
    E: Into<RuntimeError>,
{
    check_guards(backend, &snapshot.instance_id, Some(params.id)).await?;
    set_deadline_if_needed(params.id, params.timeout, snapshot, backend).await?;

    let output = retry_with_checkpoint(
        params.id,
        params.retry_policy,
        params.timeout,
        snapshot,
        Some(backend),
        async |snap| execute_or_skip_task(params.id, current_input.clone(), &execute, snap).await,
    )
    .await?;

    if let Some(next_cont) = params.next {
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: next_cont.first_task_id().to_string(),
        });
    }
    backend.save_snapshot(snapshot).await?;
    check_guards(backend, &snapshot.instance_id, None).await?;

    Ok(output)
}
