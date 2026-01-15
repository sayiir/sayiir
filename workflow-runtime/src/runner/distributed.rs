//! Checkpointing workflow runner for single-process execution with persistence.
//!
//! This runner executes an entire workflow within a single process while saving
//! snapshots after each task completion. This enables crash recovery and resumption
//! without requiring multiple workers.
//!
//! **Use this when**: You want to run a workflow reliably on a single node with
//! the ability to resume after crashes.
//!
//! **Use [`PooledWorker`](crate::worker::PooledWorker) instead when**: You need
//! horizontal scaling with multiple workers collaborating on tasks.

use bytes::Bytes;
use futures::future;
use std::sync::Arc;
use workflow_core::codec::Codec;
use workflow_core::codec::sealed;
use workflow_core::context::with_context;
use workflow_core::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};
use workflow_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};
use workflow_persistence::PersistentBackend;

/// A single-process workflow runner with checkpointing for crash recovery.
///
/// `CheckpointingRunner` executes an entire workflow within one process,
/// saving snapshots after each task. Fork branches run concurrently as tokio tasks.
/// If the process crashes, the workflow can be resumed from the last checkpoint.
///
/// # When to Use
///
/// - **Single-node execution**: One process runs the entire workflow
/// - **Crash recovery**: Resume from the last completed task after restart
/// - **Simple deployment**: No coordination between workers needed
///
/// For horizontal scaling with multiple workers, use [`PooledWorker`](crate::worker::PooledWorker).
///
/// # Example
///
/// ```rust,ignore
/// use workflow_runtime::CheckpointingRunner;
/// use workflow_persistence::InMemoryBackend;
/// use workflow_core::workflow::WorkflowBuilder;
/// use workflow_core::context::WorkflowContext;
///
/// let backend = InMemoryBackend::new();
/// let runner = CheckpointingRunner::new(backend);
///
/// let ctx = WorkflowContext::new("my-workflow", codec, metadata);
/// let workflow = WorkflowBuilder::new(ctx)
///     .then("step1", |i: u32| async move { Ok(i + 1) })
///     .build()?;
///
/// // Run workflow - snapshots are saved automatically
/// let status = runner.run(&workflow, "instance-123", 1).await?;
///
/// // Resume from checkpoint if needed (e.g., after crash)
/// let status = runner.resume(&workflow, "instance-123").await?;
/// ```
pub struct CheckpointingRunner<B> {
    backend: Arc<B>,
}

impl<B> CheckpointingRunner<B>
where
    B: PersistentBackend,
{
    /// Create a new checkpointing runner with the given backend.
    pub fn new(backend: B) -> Self {
        Self {
            backend: Arc::new(backend),
        }
    }
}

impl<B> CheckpointingRunner<B>
where
    B: PersistentBackend + 'static,
{
    /// Run a workflow from the beginning, saving checkpoints after each task.
    ///
    /// The `instance_id` uniquely identifies this workflow execution instance.
    /// If a snapshot with this ID already exists, it will be overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error if the workflow cannot be executed or if snapshot
    /// operations fail.
    pub async fn run<C, Input, M>(
        &self,
        workflow: &Workflow<C, Input, M>,
        instance_id: impl Into<String>,
        input: Input,
    ) -> anyhow::Result<WorkflowStatus>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::EncodeValue<Input> + sealed::DecodeValue<Input> + 'static,
    {
        let instance_id = instance_id.into();
        let definition_hash = workflow.definition_hash().to_string();

        // Encode initial input
        let input_bytes = workflow.context().codec.encode(&input)?;

        // Create initial snapshot with input
        let mut snapshot = WorkflowSnapshot::with_initial_input(
            instance_id.clone(),
            definition_hash.clone(),
            input_bytes.clone(),
        );
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: Self::get_first_task_id(workflow.continuation()),
        });

        // Save initial snapshot
        self.backend.save_snapshot(snapshot.clone()).await?;

        // Execute workflow with checkpointing
        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let backend = Arc::clone(&self.backend);

        with_context(context, || async move {
            match Self::execute_with_checkpointing(
                continuation,
                input_bytes,
                &mut snapshot,
                Arc::clone(&backend),
            )
            .await
            {
                Ok(output) => {
                    snapshot.mark_completed(output.clone());
                    backend.save_snapshot(snapshot).await?;
                    Ok(WorkflowStatus::Completed)
                }
                Err(e) => {
                    snapshot.mark_failed(e.to_string());
                    let _ = backend.save_snapshot(snapshot).await;
                    Ok(WorkflowStatus::Failed(e))
                }
            }
        })
        .await
    }

    /// Resume a workflow from a saved snapshot.
    ///
    /// Loads the snapshot for the given instance ID and continues execution
    /// from the last checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The snapshot is not found
    /// - The workflow definition hash doesn't match (workflow definition changed)
    /// - The workflow cannot be resumed
    #[allow(clippy::needless_lifetimes)]
    pub async fn resume<'w, C, Input, M>(
        &self,
        workflow: &'w Workflow<C, Input, M>,
        instance_id: &str,
    ) -> anyhow::Result<WorkflowStatus>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<Input> + 'static,
    {
        // Load snapshot
        let mut snapshot = self.backend.load_snapshot(instance_id).await?;

        // Validate definition hash
        if snapshot.definition_hash != workflow.definition_hash() {
            return Err(anyhow::anyhow!(
                "Workflow definition hash mismatch: expected {}, found {}",
                workflow.definition_hash(),
                snapshot.definition_hash
            ));
        }

        // Check if already completed or failed
        if snapshot.is_completed() {
            return Ok(WorkflowStatus::Completed);
        }
        if snapshot.is_failed()
            && let WorkflowSnapshotState::Failed { error } = &snapshot.state
        {
            return Ok(WorkflowStatus::Failed(anyhow::anyhow!("{error}")));
        }

        // Resume execution
        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let backend = Arc::clone(&self.backend);

        with_context(context, || async move {
            // Get the last completed task's output or initial input
            let input_bytes = Self::get_resume_input(&snapshot, continuation)?;

            match Self::execute_with_checkpointing(
                continuation,
                input_bytes,
                &mut snapshot,
                Arc::clone(&backend),
            )
            .await
            {
                Ok(output) => {
                    snapshot.mark_completed(output.clone());
                    backend.save_snapshot(snapshot).await?;
                    Ok(WorkflowStatus::Completed)
                }
                Err(e) => {
                    snapshot.mark_failed(e.to_string());
                    let _ = backend.save_snapshot(snapshot).await;
                    Ok(WorkflowStatus::Failed(e))
                }
            }
        })
        .await
    }

    /// Execute continuation with checkpointing after each task.
    fn execute_with_checkpointing<'a>(
        continuation: &'a WorkflowContinuation,
        input: Bytes,
        snapshot: &'a mut WorkflowSnapshot,
        backend: Arc<B>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Bytes>> + Send + 'a>>
    where
        B: 'static,
    {
        Box::pin(async move {
            match continuation {
                WorkflowContinuation::Task { id, func, next } => {
                    // Check if this task was already completed
                    if let Some(task_result) = snapshot.get_task_result(id) {
                        // Task already completed, use cached result
                        let output = task_result.output.clone();

                        // Update position to next task
                        if let Some(next_cont) = next {
                            snapshot.update_position(ExecutionPosition::AtTask {
                                task_id: Self::get_first_task_id(next_cont),
                            });
                        } else {
                            // No next task, workflow complete
                            return Ok(output);
                        }

                        // Save checkpoint
                        backend.save_snapshot(snapshot.clone()).await?;

                        // Continue with next task
                        if let Some(next_continuation) = next {
                            Self::execute_with_checkpointing(
                                next_continuation,
                                output,
                                snapshot,
                                Arc::clone(&backend),
                            )
                            .await
                        } else {
                            Ok(output)
                        }
                    } else {
                        // Execute task
                        let output = func.run(input).await?;

                        // Mark task as completed
                        snapshot.mark_task_completed(id.clone(), output.clone());

                        // Update position
                        if let Some(next_cont) = next {
                            snapshot.update_position(ExecutionPosition::AtTask {
                                task_id: Self::get_first_task_id(next_cont),
                            });
                        }

                        // Save checkpoint
                        backend.save_snapshot(snapshot.clone()).await?;

                        // Continue with next task
                        if let Some(next_continuation) = next {
                            Self::execute_with_checkpointing(
                                next_continuation,
                                output,
                                snapshot,
                                Arc::clone(&backend),
                            )
                            .await
                        } else {
                            Ok(output)
                        }
                    }
                }
                WorkflowContinuation::Fork { branches, join } => {
                    // Check if fork was already completed
                    let mut all_branches_completed = true;
                    let mut branch_results = Vec::new();

                    for branch in branches.iter() {
                        let branch_id = match branch.as_ref() {
                            WorkflowContinuation::Task { id, .. } => id.clone(),
                            _ => String::from("unnamed"),
                        };

                        if let Some(result) = snapshot.get_task_result(&branch_id) {
                            branch_results.push((branch_id.clone(), result.output.clone()));
                        } else {
                            all_branches_completed = false;
                            break;
                        }
                    }

                    if all_branches_completed {
                        // All branches completed, proceed to join
                        if let Some(join_continuation) = join {
                            let join_input = Self::serialize_named_branch_results(&branch_results)?;
                            Self::execute_with_checkpointing(
                                join_continuation,
                                join_input,
                                snapshot,
                                Arc::clone(&backend),
                            )
                            .await
                        } else {
                            // No join task, return last branch result
                            Ok(branch_results
                                .last()
                                .map(|(_, b)| b.clone())
                                .unwrap_or_default())
                        }
                    } else {
                        // Collect branch IDs first to avoid borrow issues
                        let branch_info: Vec<(String, Arc<WorkflowContinuation>)> = branches
                            .iter()
                            .map(|branch| {
                                let branch_id = match branch.as_ref() {
                                    WorkflowContinuation::Task { id, .. } => id.clone(),
                                    _ => String::from("unnamed"),
                                };
                                (branch_id, Arc::clone(branch))
                            })
                            .collect();

                        let instance_id = snapshot.instance_id.clone();

                        // Execute branches in parallel with per-task checkpointing
                        let branch_handles: Vec<_> = branch_info
                            .into_iter()
                            .map(|(branch_id, branch)| {
                                // Check if already completed (clone the result to avoid borrow)
                                let cached_result = snapshot
                                    .get_task_result(&branch_id)
                                    .map(|r| r.output.clone());

                                if let Some(result) = cached_result {
                                    return tokio::task::spawn(async move {
                                        Ok::<_, anyhow::Error>((branch_id, result))
                                    });
                                }

                                let branch_input = input.clone();
                                let branch_backend = Arc::clone(&backend);
                                let branch_instance_id = instance_id.clone();
                                tokio::task::spawn(async move {
                                    // Use per-task checkpointing for branch execution
                                    let result = Self::execute_branch_with_checkpoint(
                                        &branch,
                                        branch_input,
                                        branch_backend,
                                        branch_instance_id,
                                    )
                                    .await?;
                                    Ok((branch_id, result))
                                })
                            })
                            .collect();

                        // Wait for all branches
                        let branch_results: Vec<(String, Bytes)> =
                            future::try_join_all(branch_handles)
                                .await?
                                .into_iter()
                                .collect::<anyhow::Result<Vec<_>>>()?;

                        // Sync local snapshot with results (already saved to backend per-task)
                        for (branch_id, output) in &branch_results {
                            snapshot.mark_task_completed(branch_id.clone(), output.clone());
                        }

                        // Update position for join
                        if let Some(join_cont) = join {
                            let completed_branches: std::collections::HashMap<
                                String,
                                workflow_core::snapshot::TaskResult,
                            > = branch_results
                                .iter()
                                .map(|(id, output)| {
                                    (
                                        id.clone(),
                                        workflow_core::snapshot::TaskResult {
                                            task_id: id.clone(),
                                            output: output.clone(),
                                        },
                                    )
                                })
                                .collect();
                            snapshot.update_position(ExecutionPosition::AtJoin {
                                join_id: Self::get_first_task_id(join_cont),
                                completed_branches,
                            });
                        }

                        // Save position update (task results already saved per-task)
                        backend.save_snapshot(snapshot.clone()).await?;

                        // Proceed to join
                        if let Some(join_continuation) = join {
                            let join_input = Self::serialize_named_branch_results(&branch_results)?;
                            Self::execute_with_checkpointing(
                                join_continuation,
                                join_input,
                                snapshot,
                                Arc::clone(&backend),
                            )
                            .await
                        } else {
                            Ok(branch_results
                                .last()
                                .map(|(_, b)| b.clone())
                                .unwrap_or_default())
                        }
                    }
                }
            }
        })
    }

    /// Execute branch continuation with per-task checkpointing.
    ///
    /// Unlike `execute_with_checkpointing`, this doesn't update position tracking
    /// (branches run independently). It saves each task result directly to the backend.
    fn execute_branch_with_checkpoint<'a>(
        continuation: &'a WorkflowContinuation,
        input: Bytes,
        backend: Arc<B>,
        instance_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Bytes>> + Send + 'a>>
    where
        B: 'static,
    {
        Box::pin(async move {
            match continuation {
                WorkflowContinuation::Task { id, func, next } => {
                    let output = func.run(input).await?;

                    // Checkpoint: save task result directly to backend
                    backend.save_task_result(&instance_id, id, output.clone()).await?;

                    match next {
                        Some(next_continuation) => {
                            Self::execute_branch_with_checkpoint(
                                next_continuation,
                                output,
                                backend,
                                instance_id,
                            )
                            .await
                        }
                        None => Ok(output),
                    }
                }
                WorkflowContinuation::Fork { branches, join } => {
                    // Nested fork within a branch
                    let branch_handles: Vec<_> = branches
                        .iter()
                        .map(|branch| {
                            let id = match branch.as_ref() {
                                WorkflowContinuation::Task { id, .. } => id.clone(),
                                _ => String::from("unnamed"),
                            };
                            let branch = Arc::clone(branch);
                            let branch_input = input.clone();
                            let backend = Arc::clone(&backend);
                            let instance_id = instance_id.clone();
                            tokio::task::spawn(async move {
                                let result = Self::execute_branch_with_checkpoint(
                                    &branch,
                                    branch_input,
                                    backend,
                                    instance_id,
                                )
                                .await?;
                                Ok::<_, anyhow::Error>((id, result))
                            })
                        })
                        .collect();

                    let branch_results: Vec<(String, Bytes)> = future::try_join_all(branch_handles)
                        .await?
                        .into_iter()
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    match join {
                        Some(join_continuation) => {
                            let join_input = Self::serialize_named_branch_results(&branch_results)?;
                            Self::execute_branch_with_checkpoint(
                                join_continuation,
                                join_input,
                                backend,
                                instance_id,
                            )
                            .await
                        }
                        None => Ok(branch_results
                            .last()
                            .map(|(_, b)| b.clone())
                            .unwrap_or_default()),
                    }
                }
            }
        })
    }

    /// Get the first task ID from a continuation (for position tracking).
    fn get_first_task_id(continuation: &WorkflowContinuation) -> String {
        match continuation {
            WorkflowContinuation::Task { id, .. } => id.clone(),
            WorkflowContinuation::Fork { branches, .. } => {
                if let Some(first_branch) = branches.first() {
                    Self::get_first_task_id(first_branch)
                } else {
                    String::from("unknown")
                }
            }
        }
    }

    /// Get the input for resuming execution.
    ///
    /// Uses the execution position to determine the correct input:
    /// - If no tasks completed, use initial input
    /// - Otherwise, use the output of the last completed task
    fn get_resume_input(
        snapshot: &WorkflowSnapshot,
        _continuation: &WorkflowContinuation,
    ) -> anyhow::Result<Bytes> {
        match &snapshot.state {
            WorkflowSnapshotState::InProgress {
                completed_tasks, ..
            } => {
                if completed_tasks.is_empty() {
                    // No tasks completed yet, use initial input
                    snapshot.initial_input_bytes().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Cannot resume: no completed tasks and initial input not stored"
                        )
                    })
                } else {
                    // Use output of last completed task (deterministic via last_completed_task_id)
                    snapshot
                        .get_last_task_output()
                        .ok_or_else(|| anyhow::anyhow!("Cannot resume: no task results available"))
                }
            }
            _ => Err(anyhow::anyhow!("Cannot resume: workflow not in progress")),
        }
    }

    /// Serialize named branch results for join task input.
    fn serialize_named_branch_results(branch_results: &[(String, Bytes)]) -> anyhow::Result<Bytes> {
        use std::io::Write;

        let mut buffer = Vec::new();

        buffer.write_all(&(branch_results.len() as u32).to_le_bytes())?;

        for (name, data) in branch_results {
            let name_bytes = name.as_bytes();
            buffer.write_all(&(name_bytes.len() as u32).to_le_bytes())?;
            buffer.write_all(name_bytes)?;
            buffer.write_all(&(data.len() as u32).to_le_bytes())?;
            buffer.write_all(data.as_ref())?;
        }

        Ok(Bytes::from(buffer))
    }
}
