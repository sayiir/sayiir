//! Sync, async, and checkpointing execution loops.

use std::future::Future;
use std::ops::ControlFlow;
use std::sync::Arc;

use backon::{BlockingRetryable, Retryable};
use bytes::Bytes;
use sayiir_core::codec::EnvelopeCodec;
use sayiir_core::error::{BoxError, WorkflowError};
use sayiir_core::snapshot::ExecutionPosition;
use sayiir_core::workflow::WorkflowContinuation;
use sayiir_persistence::SignalStore;

use crate::error::RuntimeError;

use super::control_flow::{
    ParkReason, StepOutcome, StepResult, compute_signal_timeout, compute_wake_at,
    save_park_checkpoint,
};
use super::fork::{
    JoinResolution, collect_cached_branches, execute_fork_branches_sequential, resolve_join,
    settle_fork_outcome,
};
use super::helpers::{
    TaskStepParams, check_guards, execute_task_step, policy_to_backoff, resolve_branch,
};
use super::loop_runner::{
    LoopConfig, LoopExit, LoopNext, NoHooks, resolve_loop_iteration, run_loop_async,
};

// ── Sync ────────────────────────────────────────────────────────────────

/// Execute a workflow continuation synchronously.
///
/// This is useful for environments that don't support async (like Python with GIL).
/// Branches are executed sequentially.
///
/// # Arguments
/// * `continuation` - The workflow continuation to execute
/// * `input` - Input bytes for the first task
/// * `execute_task` - Callback to execute a task: (`task_id`, input) -> `Result<output>`
///
/// # Errors
/// Returns an error if task execution fails.
#[allow(clippy::too_many_lines)]
pub fn execute_continuation_sync<F, E>(
    continuation: &WorkflowContinuation,
    input: Bytes,
    execute_task: &F,
    envelope_codec: &E,
) -> Result<Bytes, RuntimeError>
where
    F: Fn(&str, Bytes) -> Result<Bytes, BoxError>,
    E: EnvelopeCodec,
{
    let mut current = continuation;
    let mut current_input = input;

    loop {
        match current {
            WorkflowContinuation::Task {
                id,
                retry_policy,
                next,
                ..
            } => {
                let output = (|| execute_task(id, current_input.clone()))
                    .retry(policy_to_backoff(retry_policy.as_ref()))
                    .sleep(std::thread::sleep)
                    .notify(|e, dur: std::time::Duration| {
                        tracing::info!(
                            task_id = %id,
                            delay_ms = dur.as_millis(),
                            error = %e,
                            "Retrying task (sync)"
                        );
                    })
                    .call()
                    .map_err(RuntimeError::from)?;

                match next {
                    Some(next_cont) => {
                        current = next_cont;
                        current_input = output;
                    }
                    None => return Ok(output),
                }
            }
            WorkflowContinuation::Fork { branches, join, .. } => {
                // Execute branches sequentially
                let mut branch_results = Vec::with_capacity(branches.len());

                for branch in branches {
                    let branch_id = branch.id().to_string();
                    let output = execute_continuation_sync(
                        branch,
                        current_input.clone(),
                        execute_task,
                        envelope_codec,
                    )?;
                    branch_results.push((branch_id, output));
                }

                match resolve_join(join.as_deref(), &branch_results, envelope_codec)? {
                    JoinResolution::Continue { next, input } => {
                        current = next;
                        current_input = input;
                    }
                    JoinResolution::Done(output) => return Ok(output),
                }
            }
            WorkflowContinuation::Delay { duration, next, .. } => {
                std::thread::sleep(*duration);
                match next {
                    Some(next_cont) => {
                        current = next_cont;
                    }
                    None => return Ok(current_input),
                }
            }
            WorkflowContinuation::AwaitSignal { id, .. } => {
                // Sync executor cannot wait for external signals
                return Err(WorkflowError::ResumeError(format!(
                    "AwaitSignal '{id}' not supported in sync executor"
                ))
                .into());
            }
            WorkflowContinuation::Branch {
                id,
                branches,
                default,
                next,
                ..
            } => {
                let key_bytes =
                    execute_task(&sayiir_core::workflow::key_fn_id(id), current_input.clone())?;
                let (key, chosen) =
                    resolve_branch(id, &key_bytes, branches, default.as_deref(), envelope_codec)?;
                let branch_output = execute_continuation_sync(
                    chosen,
                    current_input.clone(),
                    execute_task,
                    envelope_codec,
                )?;
                let envelope_bytes = envelope_codec
                    .encode_branch_envelope(&key, &branch_output)
                    .map_err(RuntimeError::from)?;

                match next {
                    Some(next_cont) => {
                        current = next_cont;
                        current_input = envelope_bytes;
                    }
                    None => return Ok(envelope_bytes),
                }
            }
            WorkflowContinuation::Loop {
                id,
                body,
                max_iterations,
                on_max,
                next,
            } => {
                let cfg = LoopConfig {
                    id,
                    body,
                    max_iterations: *max_iterations,
                    on_max: *on_max,
                    start_iteration: 0,
                };
                let mut loop_input = current_input.clone();
                for iteration in 0..cfg.max_iterations {
                    let output = execute_continuation_sync(
                        body,
                        loop_input.clone(),
                        execute_task,
                        envelope_codec,
                    )?;
                    match resolve_loop_iteration(&output, iteration, &cfg, envelope_codec)? {
                        ControlFlow::Break(LoopExit(inner)) => match next {
                            Some(next_cont) => {
                                current = next_cont;
                                current_input = inner;
                                break;
                            }
                            None => return Ok(inner),
                        },
                        ControlFlow::Continue(LoopNext(inner)) => {
                            loop_input = inner;
                        }
                    }
                }
            }
        }
    }
}

// ── Async ───────────────────────────────────────────────────────────────

/// Execute a workflow continuation asynchronously with parallel branch execution.
///
/// Uses the `func` from each task in the continuation for execution, and spawns
/// branches in parallel using tokio tasks.
///
/// # Arguments
/// * `continuation` - The workflow continuation to execute
/// * `input` - Input bytes for the first task
///
/// # Errors
/// Returns an error if task execution fails.
pub async fn execute_continuation_async<E: EnvelopeCodec + Clone + 'static>(
    continuation: &WorkflowContinuation,
    input: Bytes,
    envelope_codec: &E,
) -> Result<Bytes, RuntimeError> {
    execute_async_inner(continuation, input, true, envelope_codec).await
}

/// Execute a task function with optional timeout wrapping and retry backoff.
///
/// Wraps the task's `func.run(input)` in an optional `tokio::time::timeout`,
/// then retries the whole closure according to `retry_policy` (via `backon`).
async fn run_task_with_retry(
    id: &str,
    input: Bytes,
    func: &dyn sayiir_core::task::CoreTask<
        Input = Bytes,
        Output = Bytes,
        Future = sayiir_core::task::BytesFuture,
    >,
    timeout: Option<&std::time::Duration>,
    retry_policy: Option<&sayiir_core::task::RetryPolicy>,
) -> Result<Bytes, RuntimeError> {
    (|| async {
        let task_input = input.clone();
        if let Some(d) = timeout {
            match tokio::time::timeout(*d, func.run(task_input)).await {
                Ok(result) => result.map_err(RuntimeError::from),
                Err(_) => Err(WorkflowError::TaskTimedOut {
                    task_id: id.to_string(),
                    timeout: *d,
                }
                .into()),
            }
        } else {
            func.run(task_input).await.map_err(RuntimeError::from)
        }
    })
    .retry(policy_to_backoff(retry_policy))
    .notify(|e, dur: std::time::Duration| {
        tracing::info!(
            task_id = %id,
            delay_ms = dur.as_millis(),
            error = %e,
            "Retrying task"
        );
    })
    .await
}

/// Shared implementation for async continuation execution.
///
/// When `parallel_branches` is `true`, top-level fork branches are spawned as
/// parallel tokio tasks. When `false` (used inside branches to avoid unbounded
/// spawning), branches run sequentially.
///
/// Returns a boxed future so the recursive call is provably `Send` for `tokio::spawn`.
#[allow(clippy::too_many_lines)]
fn execute_async_inner<'a, E: EnvelopeCodec + Clone + 'static>(
    continuation: &'a WorkflowContinuation,
    input: Bytes,
    parallel_branches: bool,
    envelope_codec: &'a E,
) -> std::pin::Pin<Box<dyn Future<Output = Result<Bytes, RuntimeError>> + Send + 'a>> {
    Box::pin(async move {
        let mut current = continuation;
        let mut current_input = input;

        loop {
            match current {
                WorkflowContinuation::Task {
                    id,
                    func: Some(func),
                    timeout,
                    retry_policy,
                    next,
                } => {
                    let output = run_task_with_retry(
                        id,
                        current_input.clone(),
                        func.as_ref(),
                        timeout.as_ref(),
                        retry_policy.as_ref(),
                    )
                    .await?;

                    match next {
                        Some(next_cont) => {
                            current = next_cont;
                            current_input = output;
                        }
                        None => return Ok(output),
                    }
                }
                WorkflowContinuation::Task { func: None, id, .. } => {
                    return Err(WorkflowError::TaskNotImplemented(id.clone()).into());
                }
                WorkflowContinuation::Delay { duration, next, .. } => {
                    tokio::time::sleep(*duration).await;
                    match next {
                        Some(next_cont) => {
                            current = next_cont;
                        }
                        None => return Ok(current_input),
                    }
                }
                WorkflowContinuation::AwaitSignal { id, .. } => {
                    // Async executor (non-durable) cannot wait for external signals
                    return Err(WorkflowError::ResumeError(format!(
                        "AwaitSignal '{id}' not supported in non-durable async executor"
                    ))
                    .into());
                }
                WorkflowContinuation::Branch {
                    id,
                    key_fn: Some(key_fn),
                    branches,
                    default,
                    next,
                } => {
                    let key_bytes = key_fn
                        .run(current_input.clone())
                        .await
                        .map_err(RuntimeError::from)?;
                    let (key, chosen) = resolve_branch(
                        id,
                        &key_bytes,
                        branches,
                        default.as_deref(),
                        envelope_codec,
                    )?;
                    let branch_output =
                        execute_async_inner(chosen, current_input.clone(), false, envelope_codec)
                            .await?;
                    let envelope_bytes = envelope_codec
                        .encode_branch_envelope(&key, &branch_output)
                        .map_err(RuntimeError::from)?;

                    match next {
                        Some(next_cont) => {
                            current = next_cont;
                            current_input = envelope_bytes;
                        }
                        None => return Ok(envelope_bytes),
                    }
                }
                WorkflowContinuation::Branch {
                    key_fn: None, id, ..
                } => {
                    return Err(WorkflowError::TaskNotImplemented(
                        sayiir_core::workflow::key_fn_id(id),
                    )
                    .into());
                }
                WorkflowContinuation::Loop {
                    id,
                    body,
                    max_iterations,
                    on_max,
                    next,
                } => {
                    let cfg = LoopConfig {
                        id,
                        body,
                        max_iterations: *max_iterations,
                        on_max: *on_max,
                        start_iteration: 0,
                    };
                    let output = run_loop_async(
                        &cfg,
                        current_input.clone(),
                        envelope_codec,
                        |input| execute_async_inner(body, input, false, envelope_codec),
                        &mut NoHooks,
                    )
                    .await?;
                    match next {
                        Some(next_cont) => {
                            current = next_cont;
                            current_input = output;
                        }
                        None => return Ok(output),
                    }
                }
                WorkflowContinuation::Fork { branches, join, .. } => {
                    let branch_results = if parallel_branches && branches.len() > 1 {
                        // Multiple branches: spawn each as a tokio task for parallelism.
                        let mut set = tokio::task::JoinSet::new();
                        for branch in branches {
                            let branch_id = branch.id().to_string();
                            let branch = Arc::clone(branch);
                            let branch_input = current_input.clone();
                            let branch_codec = envelope_codec.clone();
                            set.spawn(async move {
                                execute_async_inner(&branch, branch_input, false, &branch_codec)
                                    .await
                                    .map(|output| (branch_id, output))
                            });
                        }

                        let mut results = Vec::with_capacity(set.len());
                        while let Some(res) = set.join_next().await {
                            results.push(res??);
                        }
                        results
                    } else {
                        // Single branch or non-parallel: run inline (no spawn overhead)
                        let mut results = Vec::with_capacity(branches.len());
                        for branch in branches {
                            let branch_id = branch.id().to_string();
                            let output = execute_async_inner(
                                branch,
                                current_input.clone(),
                                false,
                                envelope_codec,
                            )
                            .await?;
                            results.push((branch_id, output));
                        }
                        results
                    };

                    match resolve_join(join.as_deref(), &branch_results, envelope_codec)? {
                        JoinResolution::Continue { next, input } => {
                            current = next;
                            current_input = input;
                        }
                        JoinResolution::Done(output) => return Ok(output),
                    }
                }
            }
        }
    })
}

// ── Checkpointing ───────────────────────────────────────────────────────

/// Execute a workflow continuation with checkpointing after each task.
///
/// This is the callback-based variant of `CheckpointingRunner::execute_with_checkpointing`.
/// Instead of calling `func.run(input)` directly (which requires real Rust task implementations),
/// it delegates task execution to a caller-supplied async callback. This enables environments
/// like Python bindings to provide task implementations while still getting full checkpointing,
/// cancellation, and resume support.
///
/// Fork branches are executed **sequentially** (correct for Python's GIL; parallel can come later).
///
/// # Arguments
/// * `continuation` - The workflow continuation to execute
/// * `input` - Input bytes for the first task
/// * `snapshot` - Mutable snapshot tracking execution progress
/// * `backend` - Persistent backend for saving checkpoints
/// * `execute_task` - Async callback: `(task_id, input) -> Result<output>`
///
/// # Errors
/// Returns an error if task execution, cancellation checking, or snapshot saving fails.
#[allow(clippy::too_many_lines)]
pub async fn execute_continuation_with_checkpointing<F, Fut, B, E>(
    continuation: &WorkflowContinuation,
    input: Bytes,
    snapshot: &mut sayiir_core::snapshot::WorkflowSnapshot,
    backend: &B,
    execute_task: &F,
    envelope_codec: &E,
) -> Result<Bytes, RuntimeError>
where
    B: SignalStore,
    F: Fn(&str, Bytes) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Bytes, BoxError>> + Send,
    E: EnvelopeCodec,
{
    let mut current = continuation;
    let mut current_input = input;

    loop {
        let step: StepResult = match current {
            WorkflowContinuation::Task {
                id,
                timeout,
                retry_policy,
                next,
                ..
            } => {
                let task_params = TaskStepParams {
                    id,
                    timeout: timeout.as_ref(),
                    retry_policy: retry_policy.as_ref(),
                    next: next.as_deref(),
                };
                let output = execute_task_step(
                    &task_params,
                    current_input.clone(),
                    snapshot,
                    backend,
                    |i| execute_task(id, i),
                )
                .await?;
                Ok(ControlFlow::Continue(output))
            }
            WorkflowContinuation::Delay { id, duration, next } => {
                check_guards(backend, &snapshot.instance_id, Some(id)).await?;

                if snapshot.get_task_result(id).is_some() {
                    Ok(ControlFlow::Continue(current_input.clone()))
                } else {
                    let wake_at = compute_wake_at(duration)?;
                    Ok(ControlFlow::Break(StepOutcome::Park(ParkReason::Delay {
                        delay_id: id.clone(),
                        wake_at,
                        next_task_id: next.as_deref().map(|n| n.first_task_id().to_string()),
                        passthrough: current_input.clone(),
                    })))
                }
            }
            WorkflowContinuation::AwaitSignal {
                id,
                signal_name,
                timeout,
                next,
            } => {
                check_guards(backend, &snapshot.instance_id, Some(id)).await?;

                if snapshot.get_task_result(id).is_some() {
                    let payload = snapshot
                        .get_task_result_bytes(id)
                        .unwrap_or(current_input.clone());
                    Ok(ControlFlow::Continue(payload))
                } else {
                    // Try to consume a buffered signal before parking
                    match backend
                        .consume_event(&snapshot.instance_id, signal_name)
                        .await
                    {
                        Ok(Some(payload)) => {
                            snapshot.mark_task_completed(id.clone(), payload);
                            if let Some(next_cont) = next.as_deref() {
                                snapshot.update_position(ExecutionPosition::AtTask {
                                    task_id: next_cont.first_task_id().to_string(),
                                });
                            }
                            backend.save_snapshot(snapshot).await?;
                            let output = snapshot
                                .get_task_result_bytes(id)
                                .unwrap_or(current_input.clone());
                            Ok(ControlFlow::Continue(output))
                        }
                        Ok(None) => Ok(ControlFlow::Break(StepOutcome::Park(
                            ParkReason::AwaitingSignal {
                                signal_id: id.clone(),
                                signal_name: signal_name.clone(),
                                timeout: compute_signal_timeout(timeout.as_ref()),
                                next_task_id: next
                                    .as_deref()
                                    .map(|n| n.first_task_id().to_string()),
                            },
                        ))),
                        Err(e) => Err(RuntimeError::from(e)),
                    }
                }
            }
            WorkflowContinuation::Fork {
                id: fork_id,
                branches,
                join,
            } => {
                check_guards(backend, &snapshot.instance_id, None).await?;

                let branch_results =
                    if let Some(cached) = collect_cached_branches(branches, snapshot) {
                        cached
                    } else {
                        let outcome = execute_fork_branches_sequential(
                            branches,
                            &current_input,
                            snapshot,
                            backend,
                            execute_task,
                            envelope_codec,
                        )
                        .await?;
                        settle_fork_outcome(fork_id, outcome, join.as_deref(), snapshot, backend)
                            .await?
                    };

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
                check_guards(backend, &snapshot.instance_id, Some(id)).await?;

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
                    let branch_output = super::fork::execute_branch_with_checkpointing(
                        chosen,
                        current_input.clone(),
                        &snapshot.instance_id,
                        backend,
                        execute_task,
                        envelope_codec,
                    )
                    .await?;

                    let envelope_bytes = envelope_codec
                        .encode_branch_envelope(&key, &branch_output)
                        .map_err(RuntimeError::from)?;

                    snapshot.mark_task_completed(id.clone(), envelope_bytes.clone());
                    backend.save_snapshot(snapshot).await?;

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
                check_guards(backend, &snapshot.instance_id, Some(id)).await?;

                let cfg = LoopConfig {
                    id,
                    body,
                    max_iterations: *max_iterations,
                    on_max: *on_max,
                    start_iteration: snapshot.loop_iteration(id),
                };
                let mut loop_input = current_input.clone();
                let mut final_output = None;

                for iteration in cfg.start_iteration..cfg.max_iterations {
                    let output = Box::pin(execute_continuation_with_checkpointing(
                        body,
                        loop_input.clone(),
                        snapshot,
                        backend,
                        execute_task,
                        envelope_codec,
                    ))
                    .await?;

                    let body_ser = body.to_serializable();
                    for tid in &body_ser.task_ids() {
                        snapshot.remove_task_result(tid);
                    }

                    match resolve_loop_iteration(&output, iteration, &cfg, envelope_codec)? {
                        ControlFlow::Break(LoopExit(inner)) => {
                            snapshot.clear_loop_iteration(id);
                            final_output = Some(inner);
                            break;
                        }
                        ControlFlow::Continue(LoopNext(inner)) => {
                            snapshot.set_loop_iteration(id, iteration + 1);
                            snapshot.update_position(ExecutionPosition::InLoop {
                                loop_id: id.clone(),
                                iteration: iteration + 1,
                                next_task_id: Some(body.first_task_id().to_string()),
                            });
                            backend.save_snapshot(snapshot).await?;
                            loop_input = inner;
                        }
                    }
                }

                match final_output {
                    Some(output) => Ok(ControlFlow::Continue(output)),
                    None => Err(RuntimeError::from(WorkflowError::MaxIterationsExceeded {
                        loop_id: id.clone(),
                        max_iterations: *max_iterations,
                    })),
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
                return Err(save_park_checkpoint(reason, snapshot, backend).await);
            }
        }
    }
}
