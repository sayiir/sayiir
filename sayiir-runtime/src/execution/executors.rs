//! Sync, async, and checkpointing execution loops.

use backon::{BlockingRetryable, Retryable};
use bytes::Bytes;
use sayiir_core::error::{BoxError, WorkflowError};
use sayiir_core::workflow::WorkflowContinuation;
use sayiir_persistence::SignalStore;
use std::future::Future;
use std::sync::Arc;

use crate::error::RuntimeError;

use super::fork::{
    JoinResolution, collect_cached_branches, execute_fork_branches_sequential, resolve_join,
    settle_fork_outcome,
};
use super::helpers::{
    check_guards, execute_task_step, park_at_delay, park_at_signal, policy_to_backoff,
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
pub fn execute_continuation_sync<F>(
    continuation: &WorkflowContinuation,
    input: Bytes,
    execute_task: &F,
) -> Result<Bytes, RuntimeError>
where
    F: Fn(&str, Bytes) -> Result<Bytes, BoxError>,
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
                    let output =
                        execute_continuation_sync(branch, current_input.clone(), execute_task)?;
                    branch_results.push((branch_id, output));
                }

                match resolve_join(join.as_deref(), &branch_results)? {
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
pub async fn execute_continuation_async(
    continuation: &WorkflowContinuation,
    input: Bytes,
) -> Result<Bytes, RuntimeError> {
    execute_async_inner(continuation, input, true).await
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
fn execute_async_inner<'a>(
    continuation: &'a WorkflowContinuation,
    input: Bytes,
    parallel_branches: bool,
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
                WorkflowContinuation::Fork { branches, join, .. } => {
                    let branch_results = if parallel_branches && branches.len() > 1 {
                        // Multiple branches: spawn each as a tokio task for parallelism
                        let mut set = tokio::task::JoinSet::new();
                        for branch in branches {
                            let branch_id = branch.id().to_string();
                            let branch = Arc::clone(branch);
                            let branch_input = current_input.clone();
                            set.spawn(async move {
                                execute_async_inner(&branch, branch_input, false)
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
                            let output =
                                execute_async_inner(branch, current_input.clone(), false).await?;
                            results.push((branch_id, output));
                        }
                        results
                    };

                    match resolve_join(join.as_deref(), &branch_results)? {
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
pub async fn execute_continuation_with_checkpointing<F, Fut, B>(
    continuation: &WorkflowContinuation,
    input: Bytes,
    snapshot: &mut sayiir_core::snapshot::WorkflowSnapshot,
    backend: &B,
    execute_task: &F,
) -> Result<Bytes, RuntimeError>
where
    B: SignalStore,
    F: Fn(&str, Bytes) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Bytes, BoxError>> + Send,
{
    let mut current = continuation;
    let mut current_input = input;

    loop {
        match current {
            WorkflowContinuation::Task {
                id,
                timeout,
                retry_policy,
                next,
                ..
            } => {
                let output = execute_task_step(
                    id,
                    timeout.as_ref(),
                    retry_policy.as_ref(),
                    next.as_deref(),
                    current_input.clone(),
                    snapshot,
                    backend,
                    |i| execute_task(id, i),
                )
                .await?;

                match next {
                    Some(next_continuation) => {
                        current = next_continuation;
                        current_input = output;
                    }
                    None => return Ok(output),
                }
            }
            WorkflowContinuation::Delay { id, duration, next } => {
                check_guards(backend, &snapshot.instance_id, Some(id)).await?;

                if snapshot.get_task_result(id).is_some() {
                    match next {
                        Some(n) => {
                            current = n;
                            continue;
                        }
                        None => return Ok(current_input),
                    }
                }

                return Err(park_at_delay(
                    id,
                    duration,
                    next.as_deref(),
                    current_input,
                    snapshot,
                    backend,
                )
                .await);
            }
            WorkflowContinuation::AwaitSignal {
                id,
                signal_name,
                timeout,
                next,
            } => {
                check_guards(backend, &snapshot.instance_id, Some(id)).await?;

                // If we already consumed this signal (on resume), skip it
                if snapshot.get_task_result(id).is_some() {
                    match next {
                        Some(n) => {
                            current = n;
                            // Use the signal payload as input for the next step
                            current_input =
                                snapshot.get_task_result_bytes(id).unwrap_or(current_input);
                            continue;
                        }
                        None => return Ok(current_input),
                    }
                }

                let err = park_at_signal(
                    id,
                    signal_name,
                    timeout.as_ref(),
                    next.as_deref(),
                    snapshot,
                    backend,
                )
                .await;

                // If the signal was already buffered, park_at_signal consumed it
                // and updated the snapshot — continue execution
                if matches!(err, RuntimeError::Workflow(WorkflowError::SignalConsumed)) {
                    if let Some(n) = next {
                        current = n;
                        current_input = snapshot.get_task_result_bytes(id).unwrap_or(current_input);
                        continue;
                    }
                    let output = snapshot.get_task_result_bytes(id).unwrap_or(current_input);
                    return Ok(output);
                }

                return Err(err);
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
                        )
                        .await?;
                        settle_fork_outcome(fork_id, outcome, join.as_deref(), snapshot, backend)
                            .await?
                    };

                match resolve_join(join.as_deref(), &branch_results)? {
                    JoinResolution::Continue { next, input } => {
                        current = next;
                        current_input = input;
                    }
                    JoinResolution::Done(output) => return Ok(output),
                }
            }
        }
    }
}
