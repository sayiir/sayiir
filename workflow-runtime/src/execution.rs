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
            .ok_or(WorkflowError::EmptyFork)?;
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
    execute_async_inner(continuation, input, true).await
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
) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Bytes>> + Send + 'a>> {
    Box::pin(async move {
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
                    return Err(WorkflowError::TaskNotImplemented(id.clone()).into());
                }
                WorkflowContinuation::Fork { branches, join } => {
                    let branch_results = if parallel_branches {
                        let branch_handles: Vec<_> = branches
                            .iter()
                            .map(|branch| {
                                let branch_id = continuation_id(branch);
                                let branch = Arc::clone(branch);
                                let branch_input = current_input.clone();
                                tokio::task::spawn(async move {
                                    execute_async_inner(&branch, branch_input, false)
                                        .await
                                        .map(|output| (branch_id, output))
                                })
                            })
                            .collect();

                        futures::future::try_join_all(branch_handles)
                            .await?
                            .into_iter()
                            .collect::<anyhow::Result<Vec<_>>>()?
                    } else {
                        let mut results = Vec::new();
                        for branch in branches {
                            let branch_id = continuation_id(branch);
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
        return Err(WorkflowError::DefinitionMismatch {
            expected: definition_hash.to_string(),
            found: snapshot.definition_hash.clone(),
        }
        .into());
    }

    // Check if already in terminal state
    if let Some(status) = snapshot.state.as_terminal_status() {
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]
mod tests {
    use super::*;
    use crate::serialization::JsonCodec;
    use std::sync::Arc;
    use workflow_core::task::{deserialize_named_branch_results, to_core_task};
    use workflow_persistence::InMemoryBackend;

    fn codec() -> Arc<JsonCodec> {
        Arc::new(JsonCodec)
    }

    fn encode_u32(val: u32) -> Bytes {
        Bytes::from(serde_json::to_vec(&val).unwrap())
    }

    fn decode_u32(bytes: &Bytes) -> u32 {
        serde_json::from_slice(bytes).unwrap()
    }

    /// Build a WorkflowContinuation::Task with a real func.
    fn task_node<F, Fut>(
        id: &str,
        f: F,
        next: Option<Box<WorkflowContinuation>>,
    ) -> WorkflowContinuation
    where
        F: Fn(u32) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<u32>> + Send + 'static,
    {
        let c = codec();
        WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(to_core_task(f, c)),
            next,
        }
    }

    /// Build a WorkflowContinuation::Task with no func (for callback-based tests).
    fn stub_node(id: &str, next: Option<Box<WorkflowContinuation>>) -> WorkflowContinuation {
        WorkflowContinuation::Task {
            id: id.to_string(),
            func: None,
            next,
        }
    }

    // ========================================================================
    // serialize_branch_results
    // ========================================================================

    #[test]
    fn test_serialize_branch_results_roundtrip() {
        let results = vec![
            ("branch_a".to_string(), Bytes::from(vec![1, 2, 3])),
            ("branch_b".to_string(), Bytes::from(vec![4, 5])),
        ];

        let serialized = serialize_branch_results(&results).unwrap();
        let deserialized = deserialize_named_branch_results(&serialized).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized["branch_a"], Bytes::from(vec![1, 2, 3]));
        assert_eq!(deserialized["branch_b"], Bytes::from(vec![4, 5]));
    }

    #[test]
    fn test_serialize_branch_results_empty() {
        let results: Vec<(String, Bytes)> = vec![];
        let serialized = serialize_branch_results(&results).unwrap();
        let deserialized = deserialize_named_branch_results(&serialized).unwrap();
        assert!(deserialized.is_empty());
    }

    #[test]
    fn test_serialize_branch_results_single() {
        let results = vec![("only".to_string(), Bytes::from("data"))];
        let serialized = serialize_branch_results(&results).unwrap();
        let deserialized = deserialize_named_branch_results(&serialized).unwrap();
        assert_eq!(deserialized.len(), 1);
        assert_eq!(deserialized["only"], Bytes::from("data"));
    }

    // ========================================================================
    // continuation_id
    // ========================================================================

    #[test]
    fn test_continuation_id_task() {
        let cont = WorkflowContinuation::Task {
            id: "my_task".into(),
            func: None,
            next: None,
        };
        assert_eq!(continuation_id(&cont), "my_task");
    }

    #[test]
    fn test_continuation_id_fork() {
        let cont = WorkflowContinuation::Fork {
            branches: vec![].into_boxed_slice(),
            join: None,
        };
        assert_eq!(continuation_id(&cont), "unnamed");
    }

    // ========================================================================
    // execute_continuation_sync
    // ========================================================================

    #[test]
    fn test_sync_single_task() {
        let input = encode_u32(5);
        let cont = stub_node("add_one", None);

        let callback = |_id: &str, input: Bytes| -> anyhow::Result<Bytes> {
            let val = decode_u32(&input);
            Ok(encode_u32(val + 1))
        };

        let result = execute_continuation_sync(&cont, input, &callback).unwrap();
        assert_eq!(decode_u32(&result), 6);
    }

    #[test]
    fn test_sync_chained_tasks() {
        let double = stub_node("double", None);
        let add_one = stub_node("add_one", Some(Box::new(double)));
        let input = encode_u32(10);

        let callback = |id: &str, input: Bytes| -> anyhow::Result<Bytes> {
            let val = decode_u32(&input);
            match id {
                "add_one" => Ok(encode_u32(val + 1)),
                "double" => Ok(encode_u32(val * 2)),
                _ => anyhow::bail!("Unknown task: {id}"),
            }
        };

        let result = execute_continuation_sync(&add_one, input, &callback).unwrap();
        // 10 + 1 = 11, 11 * 2 = 22
        assert_eq!(decode_u32(&result), 22);
    }

    #[test]
    fn test_sync_fork_with_join() {
        let branch_a = Arc::new(stub_node("branch_a", None));
        let branch_b = Arc::new(stub_node("branch_b", None));
        let join_task = stub_node("join", None);

        let fork = WorkflowContinuation::Fork {
            branches: vec![branch_a, branch_b].into_boxed_slice(),
            join: Some(Box::new(join_task)),
        };

        let input = encode_u32(10);

        let callback = |id: &str, input: Bytes| -> anyhow::Result<Bytes> {
            let val: u32 = serde_json::from_slice(&input).unwrap_or(0);
            match id {
                "branch_a" => Ok(encode_u32(val * 2)),
                "branch_b" => Ok(encode_u32(val + 5)),
                "join" => {
                    let branches = deserialize_named_branch_results(&input).unwrap();
                    let a = decode_u32(&branches["branch_a"]);
                    let b = decode_u32(&branches["branch_b"]);
                    Ok(encode_u32(a + b))
                }
                _ => anyhow::bail!("Unknown task: {id}"),
            }
        };

        let result = execute_continuation_sync(&fork, input, &callback).unwrap();
        // branch_a: 10*2=20, branch_b: 10+5=15, join: 20+15=35
        assert_eq!(decode_u32(&result), 35);
    }

    #[test]
    fn test_sync_fork_without_join() {
        let branch_a = Arc::new(stub_node("branch_a", None));
        let branch_b = Arc::new(stub_node("branch_b", None));

        let fork = WorkflowContinuation::Fork {
            branches: vec![branch_a, branch_b].into_boxed_slice(),
            join: None,
        };

        let input = encode_u32(10);

        let callback = |id: &str, input: Bytes| -> anyhow::Result<Bytes> {
            let val = decode_u32(&input);
            match id {
                "branch_a" => Ok(encode_u32(val * 2)),
                "branch_b" => Ok(encode_u32(val + 5)),
                _ => anyhow::bail!("Unknown"),
            }
        };

        // Without join, returns last branch result
        let result = execute_continuation_sync(&fork, input, &callback).unwrap();
        assert_eq!(decode_u32(&result), 15); // branch_b: 10+5
    }

    #[test]
    fn test_sync_task_failure_propagates() {
        let cont = stub_node("fail_task", None);
        let input = encode_u32(1);

        let callback =
            |_id: &str, _input: Bytes| -> anyhow::Result<Bytes> { anyhow::bail!("task exploded") };

        let result = execute_continuation_sync(&cont, input, &callback);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("task exploded"));
    }

    // ========================================================================
    // execute_continuation_async
    // ========================================================================

    #[tokio::test]
    async fn test_async_single_task() {
        let input = encode_u32(5);
        let cont = task_node("add_one", |i: u32| async move { Ok(i + 1) }, None);

        let result = execute_continuation_async(&cont, input).await.unwrap();
        assert_eq!(decode_u32(&result), 6);
    }

    #[tokio::test]
    async fn test_async_chained_tasks() {
        let double = task_node("double", |i: u32| async move { Ok(i * 2) }, None);
        let add_one = task_node(
            "add_one",
            |i: u32| async move { Ok(i + 1) },
            Some(Box::new(double)),
        );

        let input = encode_u32(10);
        let result = execute_continuation_async(&add_one, input).await.unwrap();
        assert_eq!(decode_u32(&result), 22);
    }

    #[tokio::test]
    async fn test_async_fork_with_parallel_branches() {
        let branch_a = Arc::new(task_node(
            "branch_a",
            |i: u32| async move { Ok(i * 2) },
            None,
        ));
        let branch_b = Arc::new(task_node(
            "branch_b",
            |i: u32| async move { Ok(i + 5) },
            None,
        ));

        // No join - returns last branch result
        let fork = WorkflowContinuation::Fork {
            branches: vec![branch_a, branch_b].into_boxed_slice(),
            join: None,
        };

        let input = encode_u32(10);
        let result = execute_continuation_async(&fork, input).await.unwrap();
        assert_eq!(decode_u32(&result), 15); // branch_b: 10+5
    }

    #[tokio::test]
    async fn test_async_task_no_implementation() {
        let cont = WorkflowContinuation::Task {
            id: "missing".into(),
            func: None,
            next: None,
        };

        let result = execute_continuation_async(&cont, Bytes::new()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no implementation")
        );
    }

    #[tokio::test]
    async fn test_async_task_failure_propagates() {
        let cont = task_node(
            "fail",
            |_i: u32| async move { anyhow::bail!("async task failed") },
            None,
        );

        let input = encode_u32(1);
        let result = execute_continuation_async(&cont, input).await;
        assert!(result.is_err());
    }

    // ========================================================================
    // prepare_run / prepare_resume / finalize_execution
    // ========================================================================

    #[tokio::test]
    async fn test_prepare_run_creates_snapshot() {
        let backend = InMemoryBackend::new();
        let snapshot = prepare_run(
            "inst-1".into(),
            "hash-1".into(),
            Bytes::from("input"),
            "task-1".into(),
            &backend,
        )
        .await
        .unwrap();

        assert_eq!(snapshot.instance_id, "inst-1");
        assert_eq!(snapshot.definition_hash, "hash-1");
        assert!(snapshot.state.is_in_progress());

        // Verify it was saved to backend
        let loaded = backend.load_snapshot("inst-1").await.unwrap();
        assert_eq!(loaded.instance_id, "inst-1");
    }

    #[tokio::test]
    async fn test_prepare_resume_ready() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::with_initial_input(
            "inst-1".into(),
            "hash-1".into(),
            Bytes::from("input"),
        );
        backend.save_snapshot(snapshot).await.unwrap();

        let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
        match outcome {
            ResumeOutcome::Ready {
                snapshot,
                input_bytes,
            } => {
                assert_eq!(snapshot.instance_id, "inst-1");
                assert_eq!(input_bytes, Bytes::from("input"));
            }
            _ => panic!("Expected Ready outcome"),
        }
    }

    #[tokio::test]
    async fn test_prepare_resume_with_completed_tasks() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "inst-1".into(),
            "hash-1".into(),
            Bytes::from("initial"),
        );
        snapshot.mark_task_completed("task-1".into(), Bytes::from("task1_output"));
        backend.save_snapshot(snapshot).await.unwrap();

        let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
        match outcome {
            ResumeOutcome::Ready { input_bytes, .. } => {
                // Should use last task output, not initial input
                assert_eq!(input_bytes, Bytes::from("task1_output"));
            }
            _ => panic!("Expected Ready outcome"),
        }
    }

    #[tokio::test]
    async fn test_prepare_resume_already_completed() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_completed(Bytes::from("result"));
        backend.save_snapshot(snapshot).await.unwrap();

        let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
        match outcome {
            ResumeOutcome::AlreadyTerminal(WorkflowStatus::Completed) => {}
            _ => panic!("Expected AlreadyTerminal(Completed)"),
        }
    }

    #[tokio::test]
    async fn test_prepare_resume_already_failed() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_failed("err".into());
        backend.save_snapshot(snapshot).await.unwrap();

        let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
        match outcome {
            ResumeOutcome::AlreadyTerminal(WorkflowStatus::Failed(_)) => {}
            _ => panic!("Expected AlreadyTerminal(Failed)"),
        }
    }

    #[tokio::test]
    async fn test_prepare_resume_already_cancelled() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_cancelled(Some("reason".into()), Some("admin".into()), None);
        backend.save_snapshot(snapshot).await.unwrap();

        let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
        match outcome {
            ResumeOutcome::AlreadyTerminal(WorkflowStatus::Cancelled { reason, .. }) => {
                assert_eq!(reason, Some("reason".into()));
            }
            _ => panic!("Expected AlreadyTerminal(Cancelled)"),
        }
    }

    #[tokio::test]
    async fn test_prepare_resume_hash_mismatch() {
        let backend = InMemoryBackend::new();
        let snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        backend.save_snapshot(snapshot).await.unwrap();

        let result = prepare_resume("inst-1", "wrong-hash", &backend).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mismatch"));
    }

    #[tokio::test]
    async fn test_finalize_execution_success() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        backend.save_snapshot(snapshot.clone()).await.unwrap();

        let status = finalize_execution(Ok(Bytes::from("output")), &mut snapshot, &backend)
            .await
            .unwrap();

        match status {
            WorkflowStatus::Completed => {}
            _ => panic!("Expected Completed"),
        }

        let saved = backend.load_snapshot("inst-1").await.unwrap();
        assert!(saved.state.is_completed());
    }

    #[tokio::test]
    async fn test_finalize_execution_failure() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        backend.save_snapshot(snapshot.clone()).await.unwrap();

        let status =
            finalize_execution(Err(anyhow::anyhow!("task failed")), &mut snapshot, &backend)
                .await
                .unwrap();

        match status {
            WorkflowStatus::Failed(e) => {
                assert!(e.to_string().contains("task failed"));
            }
            _ => panic!("Expected Failed"),
        }

        let saved = backend.load_snapshot("inst-1").await.unwrap();
        assert!(saved.state.is_failed());
    }

    #[tokio::test]
    async fn test_finalize_execution_cancellation() {
        let backend = InMemoryBackend::new();
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        // Mark as cancelled in backend so finalize can reload details
        snapshot.mark_cancelled(Some("timeout".into()), Some("system".into()), None);
        backend.save_snapshot(snapshot).await.unwrap();

        // Reset local snapshot to in-progress for finalize logic
        let mut local_snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());

        let status = finalize_execution(
            Err(WorkflowError::cancelled().into()),
            &mut local_snapshot,
            &backend,
        )
        .await
        .unwrap();

        match status {
            WorkflowStatus::Cancelled {
                reason,
                cancelled_by,
            } => {
                assert_eq!(reason, Some("timeout".into()));
                assert_eq!(cancelled_by, Some("system".into()));
            }
            _ => panic!("Expected Cancelled"),
        }
    }

    // ========================================================================
    // execute_continuation_with_checkpointing
    // ========================================================================

    #[tokio::test]
    async fn test_checkpointing_single_task() {
        let backend = InMemoryBackend::new();
        let input = encode_u32(5);

        let mut snapshot =
            WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
        backend.save_snapshot(snapshot.clone()).await.unwrap();

        let cont = stub_node("add_one", None);

        let callback = |id: &str, input: Bytes| {
            let id = id.to_string();
            async move {
                let val: u32 = serde_json::from_slice(&input)?;
                match id.as_str() {
                    "add_one" => Ok(Bytes::from(serde_json::to_vec(&(val + 1))?)),
                    _ => anyhow::bail!("Unknown: {id}"),
                }
            }
        };

        let result = execute_continuation_with_checkpointing(
            &cont,
            input,
            &mut snapshot,
            &backend,
            &callback,
        )
        .await
        .unwrap();

        assert_eq!(decode_u32(&result), 6);
        assert!(snapshot.get_task_result("add_one").is_some());
    }

    #[tokio::test]
    async fn test_checkpointing_chain() {
        let backend = InMemoryBackend::new();
        let input = encode_u32(10);

        let mut snapshot =
            WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
        backend.save_snapshot(snapshot.clone()).await.unwrap();

        let double = stub_node("double", None);
        let add_one = stub_node("add_one", Some(Box::new(double)));

        let callback = |id: &str, input: Bytes| {
            let id = id.to_string();
            async move {
                let val: u32 = serde_json::from_slice(&input)?;
                match id.as_str() {
                    "add_one" => Ok(Bytes::from(serde_json::to_vec(&(val + 1))?)),
                    "double" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                    _ => anyhow::bail!("Unknown: {id}"),
                }
            }
        };

        let result = execute_continuation_with_checkpointing(
            &add_one,
            input,
            &mut snapshot,
            &backend,
            &callback,
        )
        .await
        .unwrap();

        assert_eq!(decode_u32(&result), 22); // (10+1)*2
        assert!(snapshot.get_task_result("add_one").is_some());
        assert!(snapshot.get_task_result("double").is_some());
    }

    #[tokio::test]
    async fn test_checkpointing_skips_completed_tasks() {
        let backend = InMemoryBackend::new();
        let input = encode_u32(10);

        let mut snapshot =
            WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
        // Pre-mark task as completed (simulates resume)
        snapshot.mark_task_completed("add_one".into(), encode_u32(11));
        backend.save_snapshot(snapshot.clone()).await.unwrap();

        let double = stub_node("double", None);
        let add_one = stub_node("add_one", Some(Box::new(double)));

        let was_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let was_called_clone = was_called.clone();

        let callback = move |id: &str, input: Bytes| {
            let id = id.to_string();
            let was_called_inner = was_called_clone.clone();
            async move {
                let val: u32 = serde_json::from_slice(&input)?;
                match id.as_str() {
                    "add_one" => {
                        was_called_inner.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(Bytes::from(serde_json::to_vec(&(val + 1))?))
                    }
                    "double" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                    _ => anyhow::bail!("Unknown: {id}"),
                }
            }
        };

        let result = execute_continuation_with_checkpointing(
            &add_one,
            input,
            &mut snapshot,
            &backend,
            &callback,
        )
        .await
        .unwrap();

        // add_one should NOT have been called - it was already completed
        assert!(!was_called.load(std::sync::atomic::Ordering::SeqCst));
        // cached output 11 * 2 = 22
        assert_eq!(decode_u32(&result), 22);
    }

    #[tokio::test]
    async fn test_checkpointing_fork_sequential() {
        let backend = InMemoryBackend::new();
        let input = encode_u32(10);

        let mut snapshot =
            WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
        backend.save_snapshot(snapshot.clone()).await.unwrap();

        let branch_a = Arc::new(stub_node("branch_a", None));
        let branch_b = Arc::new(stub_node("branch_b", None));
        let join_task = stub_node("join", None);

        let fork = WorkflowContinuation::Fork {
            branches: vec![branch_a, branch_b].into_boxed_slice(),
            join: Some(Box::new(join_task)),
        };

        let callback = |id: &str, input: Bytes| {
            let id = id.to_string();
            async move {
                let val: u32 = serde_json::from_slice(&input).unwrap_or(0);
                match id.as_str() {
                    "branch_a" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                    "branch_b" => Ok(Bytes::from(serde_json::to_vec(&(val + 5))?)),
                    "join" => {
                        let branches = deserialize_named_branch_results(&input)?;
                        let a: u32 = serde_json::from_slice(&branches["branch_a"])?;
                        let b: u32 = serde_json::from_slice(&branches["branch_b"])?;
                        Ok(Bytes::from(serde_json::to_vec(&(a + b))?))
                    }
                    _ => anyhow::bail!("Unknown: {id}"),
                }
            }
        };

        let result = execute_continuation_with_checkpointing(
            &fork,
            input,
            &mut snapshot,
            &backend,
            &callback,
        )
        .await
        .unwrap();

        // branch_a: 10*2=20, branch_b: 10+5=15, join: 20+15=35
        assert_eq!(decode_u32(&result), 35);
    }

    #[tokio::test]
    async fn test_checkpointing_cancellation() {
        let backend = InMemoryBackend::new();
        let input = encode_u32(5);

        let mut snapshot =
            WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
        backend.save_snapshot(snapshot.clone()).await.unwrap();

        // Request cancellation before execution
        backend
            .request_cancellation(
                "inst-1",
                workflow_core::snapshot::CancellationRequest::new(
                    Some("test cancel".into()),
                    Some("tester".into()),
                ),
            )
            .await
            .unwrap();

        let cont = stub_node("task1", None);

        let callback = |_id: &str, _input: Bytes| async { anyhow::bail!("Should not be called") };

        let result = execute_continuation_with_checkpointing(
            &cont,
            input,
            &mut snapshot,
            &backend,
            &callback,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.downcast_ref::<WorkflowError>().is_some());
    }

    // ========================================================================
    // get_resume_input
    // ========================================================================

    #[test]
    fn test_get_resume_input_no_completed_tasks() {
        let snapshot = WorkflowSnapshot::with_initial_input(
            "inst-1".into(),
            "hash-1".into(),
            Bytes::from("initial"),
        );
        let input = get_resume_input(&snapshot).unwrap();
        assert_eq!(input, Bytes::from("initial"));
    }

    #[test]
    fn test_get_resume_input_with_completed_tasks() {
        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "inst-1".into(),
            "hash-1".into(),
            Bytes::from("initial"),
        );
        snapshot.mark_task_completed("task-1".into(), Bytes::from("task1_out"));
        let input = get_resume_input(&snapshot).unwrap();
        assert_eq!(input, Bytes::from("task1_out"));
    }

    #[test]
    fn test_get_resume_input_not_in_progress() {
        let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        snapshot.mark_completed(Bytes::from("done"));
        let result = get_resume_input(&snapshot);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_resume_input_no_initial_input() {
        let snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
        let result = get_resume_input(&snapshot);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("initial input not stored")
        );
    }
}
