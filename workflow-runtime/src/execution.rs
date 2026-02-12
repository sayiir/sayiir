//! Shared workflow execution logic.
//!
//! Provides generic execution functions that can be used by different runners
//! (in-process, Python bindings, etc.) by supplying task execution callbacks.

use bytes::Bytes;
use std::future::Future;
use std::sync::Arc;
use workflow_core::error::WorkflowError;
use workflow_core::snapshot::{
    ExecutionPosition, TaskResult, WorkflowSnapshot, WorkflowSnapshotState,
};
use workflow_core::workflow::{WorkflowContinuation, WorkflowStatus};
use workflow_persistence::PersistentBackend;

/// Outcome of resolving a fork's join.
enum JoinResolution<'a> {
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
fn resolve_join<'a>(
    join: Option<&'a WorkflowContinuation>,
    branch_results: &[(String, Bytes)],
) -> anyhow::Result<JoinResolution<'a>> {
    if let Some(join_cont) = join {
        let input = serialize_branch_results(branch_results)?;
        Ok(JoinResolution::Continue {
            next: join_cont,
            input,
        })
    } else {
        let output = branch_results
            .last()
            .map(|(_, b)| b.clone())
            .ok_or_else(|| anyhow::anyhow!("Fork with no branches and no join task"))?;
        Ok(JoinResolution::Done(output))
    }
}

/// Serialize named branch results into a format that can be passed to the join task.
///
/// Uses a length-prefixed format with names:
/// - 4 bytes: number of branches (u32, little-endian)
/// - For each branch:
///   - 4 bytes: name length (u32, little-endian)
///   - N bytes: name (UTF-8)
///   - 4 bytes: data length (u32, little-endian)
///   - M bytes: data
///
/// # Errors
///
/// Returns an error if writing to the buffer fails.
#[allow(clippy::cast_possible_truncation)]
pub fn serialize_branch_results(branch_results: &[(String, Bytes)]) -> anyhow::Result<Bytes> {
    use std::io::Write;

    let mut buffer = Vec::new();

    // Safe: we never have more than u32::MAX branches in practice
    buffer.write_all(&(branch_results.len() as u32).to_le_bytes())?;

    // Write each branch result with name and length prefix
    for (name, data) in branch_results {
        // Write name length and name
        let name_bytes = name.as_bytes();
        buffer.write_all(&(name_bytes.len() as u32).to_le_bytes())?;
        buffer.write_all(name_bytes)?;

        // Write data length and data
        buffer.write_all(&(data.len() as u32).to_le_bytes())?;
        buffer.write_all(data.as_ref())?;
    }

    Ok(Bytes::from(buffer))
}

/// Get the ID of a continuation (for branch identification).
#[must_use]
pub fn continuation_id(continuation: &WorkflowContinuation) -> String {
    match continuation {
        WorkflowContinuation::Task { id, .. } => id.clone(),
        WorkflowContinuation::Fork { .. } => String::from("unnamed"),
    }
}

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
) -> anyhow::Result<Bytes>
where
    F: Fn(&str, Bytes) -> anyhow::Result<Bytes>,
{
    let mut current = continuation;
    let mut current_input = input;

    loop {
        match current {
            WorkflowContinuation::Task { id, next, .. } => {
                let output = execute_task(id, current_input)?;

                match next {
                    Some(next_cont) => {
                        current = next_cont;
                        current_input = output;
                    }
                    None => return Ok(output),
                }
            }
            WorkflowContinuation::Fork { branches, join } => {
                // Execute branches sequentially
                let mut branch_results = Vec::new();

                for branch in branches {
                    let branch_id = continuation_id(branch);
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
        }
    }
}

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
) -> anyhow::Result<Bytes> {
    let mut current = continuation;
    let mut current_input = input;

    loop {
        match current {
            WorkflowContinuation::Task {
                func: Some(func),
                next,
                ..
            } => {
                let output = func.run(current_input).await?;

                match next {
                    Some(next_cont) => {
                        current = next_cont;
                        current_input = output;
                    }
                    None => return Ok(output),
                }
            }
            WorkflowContinuation::Task { func: None, id, .. } => {
                return Err(anyhow::anyhow!("Task '{id}' has no implementation"));
            }
            WorkflowContinuation::Fork { branches, join } => {
                let branch_handles: Vec<_> = branches
                    .iter()
                    .map(|branch| {
                        let branch_id = continuation_id(branch);
                        let branch = Arc::clone(branch);
                        let branch_input = current_input.clone();
                        tokio::task::spawn(async move {
                            execute_branch_async(&branch, branch_input)
                                .await
                                .map(|output| (branch_id, output))
                        })
                    })
                    .collect();

                let branch_results: Vec<(String, Bytes)> =
                    futures::future::try_join_all(branch_handles)
                        .await?
                        .into_iter()
                        .collect::<anyhow::Result<Vec<_>>>()?;

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

/// Execute a branch asynchronously.
async fn execute_branch_async(
    continuation: &WorkflowContinuation,
    input: Bytes,
) -> anyhow::Result<Bytes> {
    let mut current = continuation;
    let mut current_input = input;

    loop {
        match current {
            WorkflowContinuation::Task {
                func: Some(func),
                next,
                ..
            } => {
                let output = func.run(current_input).await?;

                match next {
                    Some(next_cont) => {
                        current = next_cont;
                        current_input = output;
                    }
                    None => return Ok(output),
                }
            }
            WorkflowContinuation::Task { func: None, id, .. } => {
                return Err(anyhow::anyhow!("Task '{id}' has no implementation"));
            }
            WorkflowContinuation::Fork { branches, join } => {
                // Nested forks in branches: execute sequentially to avoid unbounded spawning
                let mut branch_results = Vec::new();

                for branch in branches {
                    let branch_id = continuation_id(branch);
                    let output =
                        Box::pin(execute_branch_async(branch, current_input.clone())).await?;
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
        }
    }
}

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
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
    execute_task: &F,
) -> anyhow::Result<Bytes>
where
    B: PersistentBackend,
    F: Fn(&str, Bytes) -> Fut + Send + Sync,
    Fut: Future<Output = anyhow::Result<Bytes>> + Send,
{
    let mut current = continuation;
    let mut current_input = input;

    loop {
        match current {
            WorkflowContinuation::Task { id, next, .. } => {
                // Check for cancellation before executing task
                if backend
                    .check_and_cancel(&snapshot.instance_id, Some(id))
                    .await?
                {
                    return Err(WorkflowError::cancelled().into());
                }

                // Check if this task was already completed (resume case)
                let output = if let Some(task_result) = snapshot.get_task_result(id) {
                    task_result.output.clone()
                } else {
                    let output = execute_task(id, current_input).await?;
                    snapshot.mark_task_completed(id.clone(), output.clone());
                    output
                };

                if let Some(next_cont) = next {
                    snapshot.update_position(ExecutionPosition::AtTask {
                        task_id: next_cont.first_task_id(),
                    });
                }

                backend.save_snapshot(snapshot.clone()).await?;

                // Check for cancellation after task completion
                if backend
                    .check_and_cancel(&snapshot.instance_id, None)
                    .await?
                {
                    return Err(WorkflowError::cancelled().into());
                }

                match next {
                    Some(next_continuation) => {
                        current = next_continuation;
                        current_input = output;
                    }
                    None => return Ok(output),
                }
            }
            WorkflowContinuation::Fork { branches, join } => {
                // Check for cancellation before starting fork
                if backend
                    .check_and_cancel(&snapshot.instance_id, None)
                    .await?
                {
                    return Err(WorkflowError::cancelled().into());
                }

                // Check if all branches already completed (resume case)
                let mut all_branches_completed = true;
                let mut branch_results = Vec::new();

                for branch in branches {
                    let branch_id = continuation_id(branch);

                    if let Some(result) = snapshot.get_task_result(&branch_id) {
                        branch_results.push((branch_id, result.output.clone()));
                    } else {
                        all_branches_completed = false;
                        break;
                    }
                }

                if !all_branches_completed {
                    // Execute branches sequentially (GIL-friendly)
                    branch_results.clear();

                    for branch in branches {
                        let branch_id = continuation_id(branch);

                        // Check if this specific branch was already completed
                        let output = if let Some(result) = snapshot.get_task_result(&branch_id) {
                            result.output.clone()
                        } else {
                            let output = execute_branch_with_checkpointing(
                                branch,
                                current_input.clone(),
                                &snapshot.instance_id,
                                backend,
                                execute_task,
                            )
                            .await?;

                            // Save branch result to snapshot
                            snapshot.mark_task_completed(branch_id.clone(), output.clone());
                            backend
                                .save_task_result(&snapshot.instance_id, &branch_id, output.clone())
                                .await?;
                            output
                        };

                        branch_results.push((branch_id, output));
                    }

                    // Check for cancellation after fork
                    if backend
                        .check_and_cancel(&snapshot.instance_id, None)
                        .await?
                    {
                        return Err(WorkflowError::cancelled().into());
                    }

                    // Update position for join
                    if let Some(join_cont) = join {
                        let completed_branches: std::collections::HashMap<String, TaskResult> =
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
                                .collect();
                        snapshot.update_position(ExecutionPosition::AtJoin {
                            join_id: join_cont.first_task_id(),
                            completed_branches,
                        });
                    }

                    backend.save_snapshot(snapshot.clone()).await?;
                }

                // Proceed to join or return
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

/// Execute a branch continuation with checkpointing (sequential, callback-based).
///
/// Used internally by [`execute_continuation_with_checkpointing`] for fork branches.
/// Saves each task result to the backend individually (like `CheckpointingRunner::execute_branch_with_checkpoint`).
async fn execute_branch_with_checkpointing<F, Fut, B>(
    continuation: &WorkflowContinuation,
    input: Bytes,
    instance_id: &str,
    backend: &B,
    execute_task: &F,
) -> anyhow::Result<Bytes>
where
    B: PersistentBackend,
    F: Fn(&str, Bytes) -> Fut + Send + Sync,
    Fut: Future<Output = anyhow::Result<Bytes>> + Send,
{
    let mut current = continuation;
    let mut current_input = input;

    loop {
        match current {
            WorkflowContinuation::Task { id, next, .. } => {
                let output = execute_task(id, current_input).await?;

                // Checkpoint: save task result directly to backend
                backend
                    .save_task_result(instance_id, id, output.clone())
                    .await?;

                match next {
                    Some(next_continuation) => {
                        current = next_continuation;
                        current_input = output;
                    }
                    None => return Ok(output),
                }
            }
            WorkflowContinuation::Fork { branches, join } => {
                // Nested fork within a branch: execute sequentially
                let mut branch_results = Vec::new();

                for branch in branches {
                    let branch_id = continuation_id(branch);
                    let output = Box::pin(execute_branch_with_checkpointing(
                        branch,
                        current_input.clone(),
                        instance_id,
                        backend,
                        execute_task,
                    ))
                    .await?;
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
        }
    }
}

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
) -> anyhow::Result<WorkflowSnapshot>
where
    B: PersistentBackend,
{
    let mut snapshot =
        WorkflowSnapshot::with_initial_input(instance_id, definition_hash, input_bytes);
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: first_task_id,
    });
    backend.save_snapshot(snapshot.clone()).await?;
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
) -> anyhow::Result<ResumeOutcome>
where
    B: PersistentBackend,
{
    let snapshot = backend.load_snapshot(instance_id).await?;

    // Validate definition hash
    if snapshot.definition_hash != definition_hash {
        return Err(anyhow::anyhow!(
            "Workflow definition hash mismatch: expected {}, found {}",
            definition_hash,
            snapshot.definition_hash
        ));
    }

    // Check if already in terminal state
    if snapshot.state.is_completed() {
        return Ok(ResumeOutcome::AlreadyTerminal(WorkflowStatus::Completed));
    }
    if let WorkflowSnapshotState::Failed { ref error } = snapshot.state {
        return Ok(ResumeOutcome::AlreadyTerminal(WorkflowStatus::Failed(
            anyhow::anyhow!("{error}"),
        )));
    }
    if let WorkflowSnapshotState::Cancelled {
        ref reason,
        ref cancelled_by,
        ..
    } = snapshot.state
    {
        return Ok(ResumeOutcome::AlreadyTerminal(WorkflowStatus::Cancelled {
            reason: reason.clone(),
            cancelled_by: cancelled_by.clone(),
        }));
    }

    // Determine resume input
    let input_bytes = get_resume_input(&snapshot)?;
    Ok(ResumeOutcome::Ready {
        snapshot: Box::new(snapshot),
        input_bytes,
    })
}

/// Outcome of [`prepare_resume`].
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
}

/// Get the input for resuming execution from a snapshot.
///
/// Uses the last completed task's output, or the initial input if no tasks
/// have completed yet.
///
/// # Errors
/// Returns an error if no resume input can be determined.
pub fn get_resume_input(snapshot: &WorkflowSnapshot) -> anyhow::Result<Bytes> {
    match &snapshot.state {
        WorkflowSnapshotState::InProgress {
            completed_tasks, ..
        } => {
            if completed_tasks.is_empty() {
                snapshot.initial_input_bytes().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Cannot resume: no completed tasks and initial input not stored"
                    )
                })
            } else {
                snapshot
                    .get_last_task_output()
                    .ok_or_else(|| anyhow::anyhow!("Cannot resume: no task results available"))
            }
        }
        _ => Err(anyhow::anyhow!("Cannot resume: workflow not in progress")),
    }
}

/// Finalize a workflow execution, converting the result to a [`WorkflowStatus`].
///
/// On success, marks the workflow as completed in the snapshot.
/// On cancellation error, returns `Cancelled` status with details from the backend.
/// On other errors, marks the workflow as failed.
///
/// This mirrors `CheckpointingRunner::handle_execution_result`.
///
/// # Errors
/// Returns an error if saving the snapshot to the backend fails.
pub async fn finalize_execution<B>(
    result: anyhow::Result<Bytes>,
    snapshot: &mut WorkflowSnapshot,
    backend: &B,
) -> anyhow::Result<WorkflowStatus>
where
    B: PersistentBackend,
{
    match result {
        Ok(output) => {
            snapshot.mark_completed(output);
            backend.save_snapshot(snapshot.clone()).await?;
            Ok(WorkflowStatus::Completed)
        }
        Err(e) => {
            // Check if this was a cancellation using downcasting
            if let Some(WorkflowError::Cancelled { .. }) = e.downcast_ref::<WorkflowError>() {
                // Reload snapshot to get cancellation details (set by check_and_cancel)
                if let Ok(cancelled_snapshot) = backend.load_snapshot(&snapshot.instance_id).await
                    && let Some((reason, cancelled_by)) =
                        cancelled_snapshot.state.cancellation_details()
                {
                    return Ok(WorkflowStatus::Cancelled {
                        reason,
                        cancelled_by,
                    });
                }
                // Fallback if we couldn't get details
                return Ok(WorkflowStatus::Cancelled {
                    reason: None,
                    cancelled_by: None,
                });
            }
            snapshot.mark_failed(e.to_string());
            let _ = backend.save_snapshot(snapshot.clone()).await;
            Ok(WorkflowStatus::Failed(e))
        }
    }
}
