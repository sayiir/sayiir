//! Fork/join/branch execution.

use std::collections::HashMap;
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::Arc;

use bytes::Bytes;
use sayiir_core::codec::EnvelopeCodec;
use sayiir_core::error::{BoxError, WorkflowError};
use sayiir_core::snapshot::{ExecutionPosition, TaskResult, WorkflowSnapshot};
use sayiir_core::workflow::WorkflowContinuation;
use sayiir_persistence::{SignalStore, SnapshotStore};

use crate::error::RuntimeError;

use super::control_flow::{
    ParkReason, StepOutcome, StepResult, compute_wake_at, save_branch_park_checkpoint,
};
use super::helpers::{
    branch_execute_or_skip_task, check_guards, resolve_branch, retry_with_checkpoint,
};
use super::loop_runner::{CheckpointingLoopHooks, LoopConfig, run_loop_async};

// ── Branch helpers ──────────────────────────────────────────────────────

/// Build a `HashMap<String, TaskResult>` from branch results.
///
/// Pure function that eliminates the repeated pattern of converting
/// `&[(String, Bytes)]` to a `HashMap` for snapshot position updates.
pub(crate) fn build_completed_branches(
    branch_results: &[(String, Bytes)],
) -> HashMap<String, TaskResult> {
    branch_results
        .iter()
        .map(|(id, output)| {
            (
                id.clone(),
                TaskResult {
                    task_id: id.clone(),
                    output: output.clone(),
                },
            )
        })
        .collect()
}

/// Returns `Some(results)` if every branch is cached, `None` otherwise.
pub(crate) fn collect_cached_branches(
    branches: &[Arc<WorkflowContinuation>],
    snapshot: &WorkflowSnapshot,
) -> Option<Vec<(String, Bytes)>> {
    let mut results = Vec::with_capacity(branches.len());
    for branch in branches {
        let branch_id = branch.id().to_string();
        if let Some(result) = snapshot.get_task_result(&branch_id) {
            results.push((branch_id, result.output.clone()));
        } else {
            return None;
        }
    }
    Some(results)
}

// ── Fork parking ────────────────────────────────────────────────────────

/// Park a workflow at a fork because one or more branches are waiting.
///
/// Saves completed branch results to the backend, reloads the snapshot
/// (to pick up sub-task results from branch execution), sets the `AtFork`
/// position, saves, and returns `WorkflowError::Waiting`.
///
/// Returns `RuntimeError` (not `Result`) — caller uses
/// `return Err(park_at_fork(...).await)`.
pub(crate) async fn park_at_fork<B: SnapshotStore>(
    fork_id: &str,
    branch_results: &[(String, Bytes)],
    wake_at: chrono::DateTime<chrono::Utc>,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
) -> RuntimeError {
    for (branch_id, output) in branch_results {
        if let Err(e) = backend
            .save_task_result(&snapshot.instance_id, branch_id, output.clone())
            .await
        {
            return RuntimeError::from(e);
        }
    }

    let mut updated_snapshot = match backend.load_snapshot(&snapshot.instance_id).await {
        Ok(s) => s,
        Err(e) => return RuntimeError::from(e),
    };

    updated_snapshot.update_position(ExecutionPosition::AtFork {
        fork_id: fork_id.to_string(),
        completed_branches: build_completed_branches(branch_results),
        wake_at,
    });
    if let Err(e) = backend.save_snapshot(&updated_snapshot).await {
        return RuntimeError::from(e);
    }
    *snapshot = updated_snapshot;

    WorkflowError::Waiting { wake_at }.into()
}

/// Sync branch results into the local snapshot and save the join position.
///
/// Marks each branch as completed in the snapshot, sets the `AtJoin` position
/// (if there is a join continuation), and saves to the backend.
pub(crate) async fn save_join_position<B: SnapshotStore>(
    branch_results: &[(String, Bytes)],
    join: Option<&WorkflowContinuation>,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
) -> Result<(), RuntimeError> {
    for (branch_id, output) in branch_results {
        snapshot.mark_task_completed(branch_id.clone(), output.clone());
    }

    if let Some(join_cont) = join {
        snapshot.update_position(ExecutionPosition::AtJoin {
            join_id: join_cont.first_task_id().to_string(),
            completed_branches: build_completed_branches(branch_results),
        });
    }

    backend.save_snapshot(snapshot).await?;
    Ok(())
}

// ── Fork execution ──────────────────────────────────────────────────────

/// Outcome of executing fork branches (before settling).
pub(crate) struct ForkBranchOutcome {
    /// Branch results collected so far (including cached ones).
    pub results: Vec<(String, Bytes)>,
    /// If any branch returned `Waiting`, the latest wake time.
    pub max_wake_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Execute fork branches sequentially, collecting results and tracking delays.
pub(crate) async fn execute_fork_branches_sequential<F, Fut, B, E>(
    branches: &[Arc<WorkflowContinuation>],
    input: &Bytes,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
    execute_task: &F,
    envelope_codec: &E,
) -> Result<ForkBranchOutcome, RuntimeError>
where
    B: SnapshotStore,
    F: Fn(&str, Bytes) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Bytes, BoxError>> + Send,
    E: EnvelopeCodec,
{
    let mut branch_results = Vec::with_capacity(branches.len());
    let mut max_wake_at: Option<chrono::DateTime<chrono::Utc>> = None;
    let instance_id = snapshot.instance_id.clone();

    for branch in branches {
        let branch_id = branch.id().to_string();

        if let Some(result) = snapshot.get_task_result(&branch_id) {
            branch_results.push((branch_id, result.output.clone()));
            continue;
        }

        match execute_branch_with_checkpointing(
            branch,
            input.clone(),
            &instance_id,
            backend,
            execute_task,
            envelope_codec,
        )
        .await
        {
            Ok(output) => {
                snapshot.mark_task_completed(branch_id.clone(), output.clone());
                backend
                    .save_task_result(&instance_id, &branch_id, output.clone())
                    .await?;
                branch_results.push((branch_id, output));
            }
            Err(RuntimeError::Workflow(WorkflowError::Waiting { wake_at })) => {
                max_wake_at = Some(match max_wake_at {
                    Some(existing) => existing.max(wake_at),
                    None => wake_at,
                });
            }
            Err(e) => return Err(e),
        }
    }

    Ok(ForkBranchOutcome {
        results: branch_results,
        max_wake_at,
    })
}

/// Settle a fork after all branches have been attempted.
///
/// If any branch is still waiting (`max_wake_at` is `Some`), parks the workflow
/// at the fork and returns an error. Otherwise, runs cancel/pause guards, saves
/// the join position, and returns the branch results.
pub(crate) async fn settle_fork_outcome<B: SignalStore>(
    fork_id: &str,
    outcome: ForkBranchOutcome,
    join: Option<&WorkflowContinuation>,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
) -> Result<Vec<(String, Bytes)>, RuntimeError> {
    if let Some(wake_at) = outcome.max_wake_at {
        return Err(park_at_fork(fork_id, &outcome.results, wake_at, snapshot, backend).await);
    }
    check_guards(backend, &snapshot.instance_id, None).await?;
    save_join_position(&outcome.results, join, snapshot, backend).await?;
    Ok(outcome.results)
}

// ── Join ────────────────────────────────────────────────────────────────

/// Outcome of resolving a fork's join.
pub(crate) enum JoinResolution<'a> {
    /// Continue with the join continuation and serialized input.
    Continue {
        next: &'a WorkflowContinuation,
        input: Bytes,
    },
    /// No join — return the last branch result directly.
    Done(Bytes),
}

/// Resolve the join after all fork branches complete.
///
/// Serializes branch results for the join task, or returns the last branch
/// result when there is no join. Returns an error if there are no branch
/// results and no join (A1: empty branches guard).
pub(crate) fn resolve_join<'a, E: EnvelopeCodec>(
    join: Option<&'a WorkflowContinuation>,
    branch_results: &[(String, Bytes)],
    codec: &E,
) -> Result<JoinResolution<'a>, RuntimeError> {
    if let Some(join_cont) = join {
        let input = serialize_branch_results(branch_results, codec)?;
        Ok(JoinResolution::Continue {
            next: join_cont,
            input,
        })
    } else {
        let output = branch_results
            .last()
            .map(|(_, b)| b.clone())
            .ok_or(WorkflowError::EmptyFork)?;
        Ok(JoinResolution::Done(output))
    }
}

/// Serialize named branch results into a format that can be passed to the join task.
///
/// Uses the provided [`EnvelopeCodec`] to serialize the results.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn serialize_branch_results<E: EnvelopeCodec>(
    branch_results: &[(String, Bytes)],
    codec: &E,
) -> Result<Bytes, RuntimeError> {
    Ok(codec.encode_named_results(branch_results)?)
}

// ── Branch checkpointing ────────────────────────────────────────────────

/// Execute nested fork branches within a parent branch, sequentially and recursively.
///
/// Each branch is executed via [`execute_branch_with_checkpointing`] (boxed-pinned
/// to support recursion) and its output collected into a `(branch_id, output)` vec.
async fn execute_nested_branches<F, Fut, B, E>(
    branches: &[Arc<WorkflowContinuation>],
    input: Bytes,
    instance_id: &str,
    backend: &B,
    execute_task: &F,
    envelope_codec: &E,
) -> Result<Vec<(String, Bytes)>, RuntimeError>
where
    B: SnapshotStore,
    F: Fn(&str, Bytes) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Bytes, BoxError>> + Send,
    E: EnvelopeCodec,
{
    let mut results = Vec::with_capacity(branches.len());
    for branch in branches {
        let branch_id = branch.id().to_string();
        let output = Box::pin(execute_branch_with_checkpointing(
            branch,
            input.clone(),
            instance_id,
            backend,
            execute_task,
            envelope_codec,
        ))
        .await?;
        results.push((branch_id, output));
    }
    Ok(results)
}

/// Execute a branch continuation with checkpointing (sequential, callback-based).
///
/// Used internally by [`execute_continuation_with_checkpointing`](super::executors::execute_continuation_with_checkpointing) for fork branches.
/// Saves each task result to the backend individually (like `CheckpointingRunner::execute_branch_with_checkpoint`).
///
/// On resume after `AtFork`, the backend snapshot contains sub-task results from
/// the previous execution. This function loads the snapshot to skip cached tasks
/// and parks at delays instead of sleeping through them.
#[allow(clippy::too_many_lines)]
pub(super) async fn execute_branch_with_checkpointing<F, Fut, B, E>(
    continuation: &WorkflowContinuation,
    input: Bytes,
    instance_id: &str,
    backend: &B,
    execute_task: &F,
    envelope_codec: &E,
) -> Result<Bytes, RuntimeError>
where
    B: SnapshotStore,
    F: Fn(&str, Bytes) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Bytes, BoxError>> + Send,
    E: EnvelopeCodec,
{
    // Load snapshot for checking cached results (populated on resume after AtFork)
    let mut snapshot = backend.load_snapshot(instance_id).await?;

    let mut current = continuation;
    let mut current_input = input;

    loop {
        let step: StepResult = match current {
            WorkflowContinuation::Task {
                id,
                timeout,
                retry_policy,
                ..
            } => {
                let output = retry_with_checkpoint(
                    id,
                    retry_policy.as_ref(),
                    timeout.as_ref(),
                    &mut snapshot,
                    None::<&B>,
                    async |snap| {
                        branch_execute_or_skip_task(
                            id,
                            current_input.clone(),
                            |i| execute_task(id, i),
                            timeout.as_ref(),
                            snap,
                            instance_id,
                            backend,
                        )
                        .await
                    },
                )
                .await?;
                Ok(ControlFlow::Continue(output))
            }
            WorkflowContinuation::Delay { id, duration, .. } => {
                if let Some(result) = snapshot.get_task_result(id) {
                    tracing::debug!(delay_id = %id, "delay already completed in branch, skipping");
                    Ok(ControlFlow::Continue(result.output.clone()))
                } else {
                    let wake_at = compute_wake_at(duration)?;
                    Ok(ControlFlow::Break(StepOutcome::Park(ParkReason::Delay {
                        delay_id: id.clone(),
                        wake_at,
                        next_task_id: None,
                        passthrough: current_input.clone(),
                    })))
                }
            }
            WorkflowContinuation::AwaitSignal {
                id,
                signal_name,
                timeout,
                ..
            } => {
                if let Some(result) = snapshot.get_task_result(id) {
                    tracing::debug!(signal_id = %id, %signal_name, "signal already consumed in branch, skipping");
                    Ok(ControlFlow::Continue(result.output.clone()))
                } else {
                    let wake_at = super::control_flow::compute_signal_timeout(timeout.as_ref());
                    Ok(ControlFlow::Break(StepOutcome::Park(
                        ParkReason::AwaitingSignal {
                            signal_id: id.clone(),
                            signal_name: signal_name.clone(),
                            timeout: wake_at,
                            next_task_id: None,
                        },
                    )))
                }
            }
            WorkflowContinuation::Fork { branches, join, .. } => {
                let branch_results = execute_nested_branches(
                    branches,
                    current_input.clone(),
                    instance_id,
                    backend,
                    execute_task,
                    envelope_codec,
                )
                .await?;

                match resolve_join(join.as_deref(), &branch_results, envelope_codec)? {
                    JoinResolution::Continue { input, .. } => Ok(ControlFlow::Continue(input)),
                    JoinResolution::Done(output) => {
                        Ok(ControlFlow::Break(StepOutcome::Done(output)))
                    }
                }
            }
            WorkflowContinuation::Branch {
                id,
                branches,
                default,
                ..
            } => {
                if let Some(result) = snapshot.get_task_result(id) {
                    Ok(ControlFlow::Continue(result.output.clone()))
                } else {
                    let key_bytes =
                        execute_task(&sayiir_core::workflow::key_fn_id(id), current_input.clone())
                            .await
                            .map_err(RuntimeError::from)?;
                    let (key, chosen) = resolve_branch(
                        id,
                        &key_bytes,
                        branches,
                        default.as_deref(),
                        envelope_codec,
                    )?;

                    let branch_output = Box::pin(execute_branch_with_checkpointing(
                        chosen,
                        current_input.clone(),
                        instance_id,
                        backend,
                        execute_task,
                        envelope_codec,
                    ))
                    .await?;

                    let envelope_bytes = envelope_codec
                        .encode_branch_envelope(&key, &branch_output)
                        .map_err(RuntimeError::from)?;

                    backend
                        .save_task_result(instance_id, id, envelope_bytes.clone())
                        .await?;

                    Ok(ControlFlow::Continue(envelope_bytes))
                }
            }
            WorkflowContinuation::Loop {
                id,
                body,
                max_iterations,
                on_max,
                ..
            } => {
                if let Some(result) = snapshot.get_task_result(id) {
                    Ok(ControlFlow::Continue(result.output.clone()))
                } else {
                    let cfg = LoopConfig {
                        id,
                        body,
                        max_iterations: *max_iterations,
                        on_max: *on_max,
                        start_iteration: snapshot.loop_iteration(id),
                    };
                    let mut hooks = CheckpointingLoopHooks {
                        snapshot: &mut snapshot,
                        backend,
                        track_position: false,
                    };
                    let output = run_loop_async(
                        &cfg,
                        current_input.clone(),
                        |input| {
                            Box::pin(execute_branch_with_checkpointing(
                                body,
                                input,
                                instance_id,
                                backend,
                                execute_task,
                                envelope_codec,
                            ))
                        },
                        &mut hooks,
                    )
                    .await?;
                    Ok(ControlFlow::Continue(output))
                }
            }
            WorkflowContinuation::ChildWorkflow { id, child, .. } => {
                if let Some(result) = snapshot.get_task_result(id) {
                    Ok(ControlFlow::Continue(result.output.clone()))
                } else {
                    let output = Box::pin(execute_branch_with_checkpointing(
                        child,
                        current_input.clone(),
                        instance_id,
                        backend,
                        execute_task,
                        envelope_codec,
                    ))
                    .await?;

                    snapshot.mark_task_completed(id.clone(), output.clone());
                    backend.save_snapshot(&snapshot).await?;

                    Ok(ControlFlow::Continue(output))
                }
            }
        };

        match step? {
            ControlFlow::Continue(output) => match current.get_next() {
                Some(next) => {
                    current = next;
                    current_input = output;
                }
                None => return Ok(output),
            },
            ControlFlow::Break(StepOutcome::Done(output)) => return Ok(output),
            ControlFlow::Break(StepOutcome::Park(reason)) => {
                return Err(save_branch_park_checkpoint(reason, instance_id, backend).await);
            }
        }
    }
}
