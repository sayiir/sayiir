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
use chrono::Utc;
use futures::future;
use std::sync::Arc;
use workflow_core::codec::Codec;
use workflow_core::codec::sealed;
use workflow_core::context::{WorkflowContext, with_context};
use workflow_core::error::WorkflowError;
use workflow_core::snapshot::{
    CancellationRequest, ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState,
};
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

    /// Request cancellation of a workflow.
    ///
    /// This requests cancellation of the specified workflow instance.
    /// The workflow will be cancelled at the next task boundary.
    ///
    /// # Parameters
    ///
    /// - `instance_id`: The workflow instance ID to cancel
    /// - `reason`: Optional reason for the cancellation
    /// - `cancelled_by`: Optional identifier of who requested the cancellation
    ///
    /// # Errors
    ///
    /// Returns an error if the workflow cannot be cancelled (not found or in terminal state).
    pub async fn cancel(
        &self,
        instance_id: &str,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> anyhow::Result<()> {
        let request = CancellationRequest {
            reason,
            requested_by: cancelled_by,
            requested_at: Utc::now(),
        };

        self.backend
            .request_cancellation(instance_id, request)
            .await?;

        Ok(())
    }

    /// Get a reference to the backend.
    #[must_use]
    pub fn backend(&self) -> &Arc<B> {
        &self.backend
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
            task_id: workflow.continuation().first_task_id(),
        });

        // Save initial snapshot
        self.backend.save_snapshot(snapshot.clone()).await?;

        // Execute workflow with checkpointing
        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let backend = Arc::clone(&self.backend);

        with_context(context.clone(), || async move {
            let result = Self::execute_with_checkpointing(
                continuation,
                input_bytes,
                &mut snapshot,
                Arc::clone(&backend),
                context,
            )
            .await;

            Self::handle_execution_result(result, &mut snapshot, &backend).await
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

        // Check if already completed, failed, or cancelled
        if snapshot.state.is_completed() {
            return Ok(WorkflowStatus::Completed);
        }
        if snapshot.state.is_failed()
            && let WorkflowSnapshotState::Failed { error } = &snapshot.state
        {
            return Ok(WorkflowStatus::Failed(anyhow::anyhow!("{error}")));
        }
        if let WorkflowSnapshotState::Cancelled {
            reason,
            cancelled_by,
            ..
        } = &snapshot.state
        {
            return Ok(WorkflowStatus::Cancelled {
                reason: reason.clone(),
                cancelled_by: cancelled_by.clone(),
            });
        }

        // Resume execution
        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let backend = Arc::clone(&self.backend);

        with_context(context.clone(), || async move {
            // Get the last completed task's output or initial input
            let input_bytes = Self::get_resume_input(&snapshot, continuation)?;

            let result = Self::execute_with_checkpointing(
                continuation,
                input_bytes,
                &mut snapshot,
                Arc::clone(&backend),
                context,
            )
            .await;

            Self::handle_execution_result(result, &mut snapshot, &backend).await
        })
        .await
    }

    /// Handle execution result, converting to `WorkflowStatus`.
    ///
    /// On success, marks the workflow as completed.
    /// On cancellation error, returns Cancelled status with details from snapshot.
    /// On other errors, marks the workflow as failed.
    async fn handle_execution_result(
        result: anyhow::Result<Bytes>,
        snapshot: &mut WorkflowSnapshot,
        backend: &Arc<B>,
    ) -> anyhow::Result<WorkflowStatus> {
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
                    if let Ok(cancelled_snapshot) =
                        backend.load_snapshot(&snapshot.instance_id).await
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

    /// Execute continuation with checkpointing after each task (iterative, no boxing).
    #[allow(clippy::too_many_lines, clippy::manual_async_fn)]
    async fn execute_with_checkpointing<'a, C, M>(
        continuation: &'a WorkflowContinuation,
        input: Bytes,
        snapshot: &'a mut WorkflowSnapshot,
        backend: Arc<B>,
        context: WorkflowContext<C, M>,
    ) -> anyhow::Result<Bytes>
    where
        B: 'static,
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        let mut current = continuation;
        let mut current_input = input;

        loop {
            match current {
                WorkflowContinuation::Task { id, func, next } => {
                    // Check for cancellation before executing task
                    if backend
                        .check_and_cancel(&snapshot.instance_id, Some(id))
                        .await?
                    {
                        return Err(WorkflowError::cancelled().into());
                    }

                    // Check if this task was already completed
                    let output = if let Some(task_result) = snapshot.get_task_result(id) {
                        task_result.output.clone()
                    } else {
                        let output = func.run(current_input).await?;
                        snapshot.mark_task_completed(id.clone(), output.clone());
                        output
                    };

                    if let Some(next_cont) = next {
                        snapshot.update_position(ExecutionPosition::AtTask {
                            task_id: next_cont.first_task_id(),
                        });
                    }

                    backend.save_snapshot(snapshot.clone()).await?;

                    if backend
                        .check_and_cancel(&snapshot.instance_id, None)
                        .await?
                    {
                        return Err(WorkflowError::cancelled().into());
                    }

                    // Continue to next or return
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

                    // Check if all branches already completed
                    let mut all_branches_completed = true;
                    let mut branch_results = Vec::new();

                    for branch in branches {
                        let branch_id = match branch.as_ref() {
                            WorkflowContinuation::Task { id, .. } => id.clone(),
                            WorkflowContinuation::Fork { .. } => String::from("unnamed"),
                        };

                        if let Some(result) = snapshot.get_task_result(&branch_id) {
                            branch_results.push((branch_id, result.output.clone()));
                        } else {
                            all_branches_completed = false;
                            break;
                        }
                    }

                    if !all_branches_completed {
                        // Execute branches in parallel
                        let branch_info: Vec<(String, Arc<WorkflowContinuation>)> = branches
                            .iter()
                            .map(|branch| {
                                let branch_id = match branch.as_ref() {
                                    WorkflowContinuation::Task { id, .. } => id.clone(),
                                    WorkflowContinuation::Fork { .. } => String::from("unnamed"),
                                };
                                (branch_id, Arc::clone(branch))
                            })
                            .collect();

                        let instance_id = snapshot.instance_id.clone();

                        let branch_handles: Vec<_> = branch_info
                            .into_iter()
                            .map(|(branch_id, branch)| {
                                let cached_result = snapshot
                                    .get_task_result(&branch_id)
                                    .map(|r| r.output.clone());

                                if let Some(result) = cached_result {
                                    return tokio::task::spawn(async move {
                                        Ok::<_, anyhow::Error>((branch_id, result))
                                    });
                                }

                                let branch_input = current_input.clone();
                                let branch_backend = Arc::clone(&backend);
                                let branch_instance_id = instance_id.clone();
                                let branch_context = context.clone();

                                tokio::task::spawn(Self::execute_branch_task(
                                    branch,
                                    branch_input,
                                    branch_backend,
                                    branch_instance_id,
                                    branch_context,
                                    branch_id,
                                ))
                            })
                            .collect();

                        branch_results = future::try_join_all(branch_handles)
                            .await?
                            .into_iter()
                            .collect::<anyhow::Result<Vec<_>>>()?;

                        // Check for cancellation after fork
                        if backend
                            .check_and_cancel(&snapshot.instance_id, None)
                            .await?
                        {
                            return Err(WorkflowError::cancelled().into());
                        }

                        // Sync local snapshot
                        for (branch_id, output) in &branch_results {
                            snapshot.mark_task_completed(branch_id.clone(), output.clone());
                        }

                        tracing::debug!("Updating position for join");
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
                                join_id: join_cont.first_task_id(),
                                completed_branches,
                            });
                        }

                        backend.save_snapshot(snapshot.clone()).await?;
                    }

                    // Proceed to join or return
                    match join {
                        Some(join_continuation) => {
                            let join_input = Self::serialize_named_branch_results(&branch_results)?;
                            current = join_continuation;
                            current_input = join_input;
                        }
                        None => {
                            return Ok(branch_results
                                .last()
                                .map(|(_, b)| b.clone())
                                .unwrap_or_default());
                        }
                    }
                }
            }
        }
    }

    /// Execute a branch in a spawned task (takes ownership for Send).
    #[allow(clippy::manual_async_fn)]
    fn execute_branch_task<C, M>(
        branch: Arc<WorkflowContinuation>,
        input: Bytes,
        backend: Arc<B>,
        instance_id: String,
        context: WorkflowContext<C, M>,
        branch_id: String,
    ) -> impl std::future::Future<Output = anyhow::Result<(String, Bytes)>> + Send
    where
        B: 'static,
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        async move {
            with_context(context.clone(), || async {
                let result = Self::execute_branch_with_checkpoint(
                    &branch,
                    input,
                    backend,
                    instance_id,
                    context,
                )
                .await?;
                Ok((branch_id, result))
            })
            .await
        }
    }

    /// Execute branch continuation with per-task checkpointing (iterative, no boxing).
    ///
    /// Unlike `execute_with_checkpointing`, this doesn't update position tracking
    /// (branches run independently). It saves each task result directly to the backend.
    #[allow(clippy::manual_async_fn)]
    fn execute_branch_with_checkpoint<C, M>(
        continuation: &WorkflowContinuation,
        input: Bytes,
        backend: Arc<B>,
        instance_id: String,
        context: WorkflowContext<C, M>,
    ) -> impl std::future::Future<Output = anyhow::Result<Bytes>> + Send + '_
    where
        B: 'static,
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        async move {
            let mut current = continuation;
            let mut current_input = input;

            loop {
                match current {
                    WorkflowContinuation::Task { id, func, next } => {
                        let output = func.run(current_input).await?;

                        // Checkpoint: save task result directly to backend
                        backend
                            .save_task_result(&instance_id, id, output.clone())
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
                        // Nested fork within a branch
                        let branch_handles: Vec<_> = branches
                            .iter()
                            .map(|branch| {
                                let id = match branch.as_ref() {
                                    WorkflowContinuation::Task { id, .. } => id.clone(),
                                    WorkflowContinuation::Fork { .. } => String::from("unnamed"),
                                };
                                let branch = Arc::clone(branch);
                                let branch_input = current_input.clone();
                                let branch_backend = Arc::clone(&backend);
                                let branch_instance_id = instance_id.clone();
                                let branch_context = context.clone();

                                tokio::task::spawn(Self::execute_nested_branch(
                                    branch,
                                    branch_input,
                                    branch_backend,
                                    branch_instance_id,
                                    branch_context,
                                    id,
                                ))
                            })
                            .collect();

                        let branch_results: Vec<(String, Bytes)> =
                            future::try_join_all(branch_handles)
                                .await?
                                .into_iter()
                                .collect::<anyhow::Result<Vec<_>>>()?;

                        match join {
                            Some(join_continuation) => {
                                let join_input =
                                    Self::serialize_named_branch_results(&branch_results)?;
                                current = join_continuation;
                                current_input = join_input;
                            }
                            None => {
                                return Ok(branch_results
                                    .last()
                                    .map(|(_, b)| b.clone())
                                    .unwrap_or_default());
                            }
                        }
                    }
                }
            }
        }
    }

    /// Execute a nested branch in a spawned task (takes ownership for Send).
    #[allow(clippy::manual_async_fn)]
    fn execute_nested_branch<C, M>(
        branch: Arc<WorkflowContinuation>,
        input: Bytes,
        backend: Arc<B>,
        instance_id: String,
        context: WorkflowContext<C, M>,
        branch_id: String,
    ) -> impl std::future::Future<Output = anyhow::Result<(String, Bytes)>> + Send
    where
        B: 'static,
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        async move {
            with_context(context.clone(), || async {
                let result = Self::execute_branch_with_checkpoint(
                    &branch,
                    input,
                    backend,
                    instance_id,
                    context,
                )
                .await?;
                Ok((branch_id, result))
            })
            .await
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
    #[allow(clippy::cast_possible_truncation)]
    fn serialize_named_branch_results(branch_results: &[(String, Bytes)]) -> anyhow::Result<Bytes> {
        use std::io::Write;

        let mut buffer = Vec::new();

        // Safe: we never have more than u32::MAX branches in practice
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
