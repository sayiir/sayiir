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
use sayiir_core::codec::Codec;
use sayiir_core::codec::sealed;
use sayiir_core::context::{WorkflowContext, with_context};
use sayiir_core::error::WorkflowError;
use sayiir_core::snapshot::{
    CancellationRequest, ExecutionPosition, PauseRequest, WorkflowSnapshot, WorkflowSnapshotState,
};
use sayiir_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};
use sayiir_persistence::PersistentBackend;
use std::sync::Arc;

use crate::error::RuntimeError;
use crate::execution::{
    ForkBranchOutcome, check_guards, check_parked_position, collect_cached_branches,
    execute_or_skip_task, finalize_execution, get_resume_input, park_at_delay, park_at_fork,
    save_join_position, serialize_branch_results,
};

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
/// use sayiir_runtime::CheckpointingRunner;
/// use sayiir_persistence::InMemoryBackend;
/// use sayiir_core::workflow::WorkflowBuilder;
/// use sayiir_core::context::WorkflowContext;
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
    ) -> Result<(), RuntimeError> {
        self.backend
            .request_cancellation(instance_id, CancellationRequest::new(reason, cancelled_by))
            .await?;

        Ok(())
    }

    /// Request pausing of a workflow.
    ///
    /// The workflow will be paused at the next task boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails to store the pause request.
    pub async fn pause(
        &self,
        instance_id: &str,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.backend
            .request_pause(instance_id, PauseRequest::new(reason, paused_by))
            .await?;
        Ok(())
    }

    /// Unpause a paused workflow and return the updated snapshot.
    ///
    /// Transitions the workflow from Paused back to `InProgress`.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails to unpause the workflow.
    pub async fn unpause(&self, instance_id: &str) -> Result<WorkflowSnapshot, RuntimeError> {
        let snapshot = self.backend.unpause(instance_id).await?;
        Ok(snapshot)
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
    ) -> Result<WorkflowStatus, RuntimeError>
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
            task_id: workflow.continuation().first_task_id().to_string(),
        });

        // Save initial snapshot
        self.backend.save_snapshot(&snapshot).await?;

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

            let (status, _output) =
                finalize_execution(result, &mut snapshot, backend.as_ref()).await?;
            Ok(status)
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
    ) -> Result<WorkflowStatus, RuntimeError>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<Input> + 'static,
    {
        // Load snapshot
        let mut snapshot = self.backend.load_snapshot(instance_id).await?;

        // Validate definition hash
        if snapshot.definition_hash != workflow.definition_hash() {
            return Err(WorkflowError::DefinitionMismatch {
                expected: workflow.definition_hash().to_string(),
                found: snapshot.definition_hash.clone(),
            }
            .into());
        }

        // Check if already in terminal state
        if let Some(status) = snapshot.state.as_terminal_status() {
            return Ok(status);
        }

        // Extract delay/fork fields into owned locals before any mutation.
        let delay_info = if let WorkflowSnapshotState::InProgress {
            position:
                ExecutionPosition::AtDelay {
                    wake_at,
                    delay_id,
                    next_task_id,
                    ..
                },
            ..
        } = &snapshot.state
        {
            Some((*wake_at, delay_id.clone(), next_task_id.clone()))
        } else {
            None
        };

        if let Some((wake_at, delay_id, next_task_id)) = delay_info {
            let result =
                check_parked_position(self.backend.as_ref(), instance_id, &delay_id, wake_at)
                    .await?;
            if let Some(status) = result.into_status() {
                return Ok(status);
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
                self.backend.save_snapshot(&snapshot).await?;
                return Ok(WorkflowStatus::Completed);
            }
            self.backend.save_snapshot(&snapshot).await?;
        }

        // Handle AtFork: branches hit delays and the fork was parked.
        let fork_info = if let WorkflowSnapshotState::InProgress {
            position: ExecutionPosition::AtFork {
                fork_id, wake_at, ..
            },
            ..
        } = &snapshot.state
        {
            Some((*wake_at, fork_id.clone()))
        } else {
            None
        };

        if let Some((wake_at, fork_id)) = fork_info {
            let result =
                check_parked_position(self.backend.as_ref(), instance_id, &fork_id, wake_at)
                    .await?;
            if let Some(status) = result.into_status() {
                return Ok(status);
            }
            tracing::info!(instance_id, %fork_id, "fork delays expired, resuming execution");
        }

        // Resume execution
        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let backend = Arc::clone(&self.backend);

        with_context(context.clone(), || async move {
            // Get the last completed task's output or initial input
            let input_bytes = get_resume_input(&snapshot)?;

            let result = Self::execute_with_checkpointing(
                continuation,
                input_bytes,
                &mut snapshot,
                Arc::clone(&backend),
                context,
            )
            .await;

            let (status, _output) =
                finalize_execution(result, &mut snapshot, backend.as_ref()).await?;
            Ok(status)
        })
        .await
    }

    /// Execute continuation with checkpointing after each task (iterative, no boxing).
    #[allow(clippy::manual_async_fn, clippy::too_many_lines)]
    async fn execute_with_checkpointing<'a, C, M>(
        continuation: &'a WorkflowContinuation,
        input: Bytes,
        snapshot: &'a mut WorkflowSnapshot,
        backend: Arc<B>,
        context: WorkflowContext<C, M>,
    ) -> Result<Bytes, RuntimeError>
    where
        B: 'static,
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        let mut current = continuation;
        let mut current_input = input;

        loop {
            match current {
                WorkflowContinuation::Task {
                    id,
                    func: Some(func),
                    next,
                } => {
                    check_guards(backend.as_ref(), &snapshot.instance_id, Some(id)).await?;

                    let output =
                        execute_or_skip_task(id, current_input, |i| func.run(i), snapshot).await?;

                    if let Some(next_cont) = next {
                        snapshot.update_position(ExecutionPosition::AtTask {
                            task_id: next_cont.first_task_id().to_string(),
                        });
                    }
                    backend.save_snapshot(snapshot).await?;
                    check_guards(backend.as_ref(), &snapshot.instance_id, None).await?;

                    match next {
                        Some(next_continuation) => {
                            current = next_continuation;
                            current_input = output;
                        }
                        None => return Ok(output),
                    }
                }
                WorkflowContinuation::Task { func: None, id, .. } => {
                    return Err(WorkflowError::TaskNotImplemented(id.clone()).into());
                }
                WorkflowContinuation::Delay { id, duration, next } => {
                    check_guards(backend.as_ref(), &snapshot.instance_id, Some(id)).await?;

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
                        backend.as_ref(),
                    )
                    .await);
                }
                WorkflowContinuation::Fork {
                    id: fork_id,
                    branches,
                    join,
                } => {
                    check_guards(backend.as_ref(), &snapshot.instance_id, None).await?;

                    let branch_results =
                        if let Some(cached) = collect_cached_branches(branches, snapshot) {
                            cached
                        } else {
                            let outcome = Self::execute_fork_branches_parallel(
                                branches,
                                &current_input,
                                snapshot,
                                &backend,
                                &context,
                            )
                            .await?;
                            {
                                if let Some(wake_at) = outcome.max_wake_at {
                                    return Err(park_at_fork(
                                        fork_id,
                                        &outcome.results,
                                        wake_at,
                                        snapshot,
                                        backend.as_ref(),
                                    )
                                    .await);
                                }
                                check_guards(backend.as_ref(), &snapshot.instance_id, None).await?;
                                save_join_position(
                                    &outcome.results,
                                    join.as_deref(),
                                    snapshot,
                                    backend.as_ref(),
                                )
                                .await?;
                                outcome.results
                            }
                        };

                    // Proceed to join or return
                    match join {
                        Some(join_continuation) => {
                            let join_input = serialize_branch_results(&branch_results)?;
                            current = join_continuation;
                            current_input = join_input;
                        }
                        None => {
                            return branch_results
                                .last()
                                .map(|(_, b)| b.clone())
                                .ok_or_else(|| WorkflowError::EmptyFork.into());
                        }
                    }
                }
            }
        }
    }

    /// Execute fork branches in parallel using tokio tasks.
    async fn execute_fork_branches_parallel<C, M>(
        branches: &[Arc<WorkflowContinuation>],
        input: &Bytes,
        snapshot: &WorkflowSnapshot,
        backend: &Arc<B>,
        context: &WorkflowContext<C, M>,
    ) -> Result<ForkBranchOutcome, RuntimeError>
    where
        B: 'static,
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        let mut branch_results = Vec::with_capacity(branches.len());
        let mut set = tokio::task::JoinSet::new();
        let instance_id = snapshot.instance_id.clone();

        for branch in branches {
            let branch_id = branch.id().to_string();

            if let Some(result) = snapshot.get_task_result(&branch_id) {
                branch_results.push((branch_id, result.output.clone()));
            } else {
                let branch = Arc::clone(branch);
                let branch_input = input.clone();
                let branch_backend = Arc::clone(backend);
                let branch_instance_id = instance_id.clone();
                let ctx_for_work = context.clone();

                set.spawn(with_context(context.clone(), || async move {
                    let result = Self::execute_branch_with_checkpoint(
                        &branch,
                        branch_input,
                        branch_backend,
                        branch_instance_id,
                        ctx_for_work,
                    )
                    .await?;
                    Ok((branch_id, result))
                }));
            }
        }

        let mut max_wake_at: Option<chrono::DateTime<chrono::Utc>> = None;

        while let Some(result) = set.join_next().await {
            match result {
                Ok(Ok((branch_id, output))) => {
                    branch_results.push((branch_id, output));
                }
                Ok(Err(RuntimeError::Workflow(WorkflowError::Waiting { wake_at }))) => {
                    max_wake_at = Some(match max_wake_at {
                        Some(existing) => existing.max(wake_at),
                        None => wake_at,
                    });
                }
                Ok(Err(e)) => return Err(e),
                Err(join_err) => return Err(RuntimeError::from(join_err)),
            }
        }

        Ok(ForkBranchOutcome {
            results: branch_results,
            max_wake_at,
        })
    }

    /// Execute branch continuation with per-task checkpointing (iterative, no boxing).
    ///
    /// Unlike `execute_with_checkpointing`, this doesn't update position tracking
    /// (branches run independently). It saves each task result directly to the backend.
    ///
    /// On resume after `AtFork`, the backend snapshot contains sub-task results from
    /// the previous execution. This function loads the snapshot to skip cached tasks
    /// and parks at delays instead of sleeping through them.
    #[allow(clippy::manual_async_fn)]
    fn execute_branch_with_checkpoint<C, M>(
        continuation: &WorkflowContinuation,
        input: Bytes,
        backend: Arc<B>,
        instance_id: String,
        context: WorkflowContext<C, M>,
    ) -> impl std::future::Future<Output = Result<Bytes, RuntimeError>> + Send + '_
    where
        B: 'static,
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        async move {
            // Load snapshot for checking cached results (populated on resume after AtFork)
            let snapshot = backend.load_snapshot(&instance_id).await?;

            let mut current = continuation;
            let mut current_input = input;

            loop {
                match current {
                    WorkflowContinuation::Task { id, func, next } => {
                        // Skip if already cached in snapshot (resume case)
                        let output = if let Some(result) = snapshot.get_task_result(id) {
                            result.output.clone()
                        } else {
                            let func = func
                                .as_ref()
                                .ok_or_else(|| WorkflowError::TaskNotImplemented(id.clone()))?;
                            let output = func.run(current_input).await?;
                            // Checkpoint: save task result directly to backend
                            backend
                                .save_task_result(&instance_id, id, output.clone())
                                .await?;
                            output
                        };

                        match next {
                            Some(next_continuation) => {
                                current = next_continuation;
                                current_input = output;
                            }
                            None => return Ok(output),
                        }
                    }
                    WorkflowContinuation::Delay { id, duration, next } => {
                        // Skip if pass-through was already saved (resume case)
                        if let Some(result) = snapshot.get_task_result(id) {
                            tracing::debug!(delay_id = %id, "delay already completed in branch, skipping");
                            match next {
                                Some(next_cont) => {
                                    current = next_cont;
                                    current_input = result.output.clone();
                                    continue;
                                }
                                None => return Ok(result.output.clone()),
                            }
                        }

                        // Park at delay: save pass-through and return Waiting
                        tracing::info!(delay_id = %id, ?duration, "parking branch at delay");
                        let now = chrono::Utc::now();
                        let wake_at = match chrono::Duration::from_std(*duration) {
                            Ok(d) => now + d,
                            Err(e) => return Err(WorkflowError::ResumeError(e.to_string()).into()),
                        };
                        backend
                            .save_task_result(&instance_id, id, current_input)
                            .await?;
                        return Err(WorkflowError::Waiting { wake_at }.into());
                    }
                    WorkflowContinuation::Fork { branches, join, .. } => {
                        // Nested fork within a branch
                        let mut set: tokio::task::JoinSet<Result<(String, Bytes), RuntimeError>> =
                            tokio::task::JoinSet::new();
                        for branch in branches {
                            let id = branch.id().to_string();
                            let branch = Arc::clone(branch);
                            let branch_input = current_input.clone();
                            let branch_backend = Arc::clone(&backend);
                            let branch_instance_id = instance_id.clone();
                            let ctx_for_work = context.clone();

                            set.spawn(with_context(context.clone(), || async move {
                                let result = Self::execute_branch_with_checkpoint(
                                    &branch,
                                    branch_input,
                                    branch_backend,
                                    branch_instance_id,
                                    ctx_for_work,
                                )
                                .await?;
                                Ok((id, result))
                            }));
                        }

                        let mut branch_results: Vec<(String, Bytes)> =
                            Vec::with_capacity(set.len());
                        while let Some(res) = set.join_next().await {
                            branch_results.push(res??);
                        }

                        match join {
                            Some(join_continuation) => {
                                let join_input = serialize_branch_results(&branch_results)?;
                                current = join_continuation;
                                current_input = join_input;
                            }
                            None => {
                                return branch_results
                                    .last()
                                    .map(|(_, b)| b.clone())
                                    .ok_or_else(|| WorkflowError::EmptyFork.into());
                            }
                        }
                    }
                }
            }
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
    use sayiir_core::codec::Encoder;
    use sayiir_core::context::WorkflowContext;
    use sayiir_core::error::BoxError;
    use sayiir_core::task::BranchOutputs;
    use sayiir_core::workflow::WorkflowBuilder;
    use sayiir_persistence::InMemoryBackend;

    fn ctx() -> WorkflowContext<JsonCodec, ()> {
        WorkflowContext::new("test-workflow", Arc::new(JsonCodec), Arc::new(()))
    }

    // ========================================================================
    // Run (fresh execution)
    // ========================================================================

    #[tokio::test]
    async fn test_run_single_task() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("add_one", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 5u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));

        // Verify snapshot was saved as completed
        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_completed());
    }

    #[tokio::test]
    async fn test_run_chained_tasks() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("add_one", |i: u32| async move { Ok(i + 1) })
            .then("double", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));

        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_completed());
    }

    #[tokio::test]
    async fn test_run_three_task_chain() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 3) })
            .then("step3", |i: u32| async move { Ok(i - 2) })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 5u32).await.unwrap();
        // 5+1=6, 6*3=18, 18-2=16
        assert!(matches!(status, WorkflowStatus::Completed));
    }

    #[tokio::test]
    async fn test_run_task_failure() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("fail", |_i: u32| async move {
                Err::<u32, BoxError>("intentional failure".into())
            })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 1u32).await.unwrap();
        match status {
            WorkflowStatus::Failed(e) => {
                assert!(e.to_string().contains("intentional failure"));
            }
            _ => panic!("Expected Failed status"),
        }

        // Snapshot should be marked as failed
        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_failed());
    }

    #[tokio::test]
    async fn test_run_fork_join() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                b.add("double", |i: u32| async move { Ok(i * 2) });
                b.add("add_ten", |i: u32| async move { Ok(i + 10) });
            })
            .join("combine", |outputs: BranchOutputs<JsonCodec>| async move {
                let doubled: u32 = outputs.get("double")?;
                let added: u32 = outputs.get("add_ten")?;
                Ok(doubled + added)
            })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 5u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));

        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_completed());
    }

    #[tokio::test]
    async fn test_run_checkpoints_intermediate_tasks() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));

        // The final snapshot should be completed, but we can verify the
        // instance was tracked throughout
        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_completed());
    }

    // ========================================================================
    // Resume
    // ========================================================================

    #[tokio::test]
    async fn test_resume_completed_workflow() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Run to completion
        runner.run(&workflow, "inst-1", 5u32).await.unwrap();

        // Resume should return Completed immediately
        let status = runner.resume(&workflow, "inst-1").await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));
    }

    #[tokio::test]
    async fn test_resume_failed_workflow() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("fail", |_i: u32| async move {
                Err::<u32, BoxError>("failure".into())
            })
            .build()
            .unwrap();

        runner.run(&workflow, "inst-1", 1u32).await.unwrap();

        let status = runner.resume(&workflow, "inst-1").await.unwrap();
        match status {
            WorkflowStatus::Failed(_) => {}
            _ => panic!("Expected Failed status"),
        }
    }

    #[tokio::test]
    async fn test_resume_definition_hash_mismatch() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow1 = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Run with workflow1
        runner.run(&workflow1, "inst-1", 5u32).await.unwrap();

        // Manually create in-progress snapshot with workflow1's hash
        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "inst-2".into(),
            workflow1.definition_hash().to_string(),
            Bytes::from(serde_json::to_vec(&5u32).unwrap()),
        );
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "step1".into(),
        });
        runner.backend().save_snapshot(&snapshot).await.unwrap();

        // Build a different workflow
        let workflow2 = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Resume with different workflow definition should fail
        let result = runner.resume(&workflow2, "inst-2").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mismatch"));
    }

    // ========================================================================
    // Cancellation
    // ========================================================================

    #[tokio::test]
    async fn test_cancel_running_workflow() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        // Create a workflow with a slow task
        let workflow = WorkflowBuilder::new(ctx())
            .then("slow_task", |i: u32| async move {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                Ok(i)
            })
            .build()
            .unwrap();

        // Set up a snapshot as if it's in progress
        let input_bytes = Arc::new(JsonCodec).encode(&1u32).unwrap();
        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "inst-cancel".into(),
            workflow.definition_hash().to_string(),
            input_bytes,
        );
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "slow_task".into(),
        });
        runner.backend().save_snapshot(&snapshot).await.unwrap();

        // Request cancellation
        runner
            .cancel(
                "inst-cancel",
                Some("testing".into()),
                Some("test-suite".into()),
            )
            .await
            .unwrap();

        // Verify cancellation request was stored
        let req = runner
            .backend()
            .get_cancellation_request("inst-cancel")
            .await
            .unwrap();
        assert!(req.is_some());
        assert_eq!(req.unwrap().reason, Some("testing".into()));
    }

    #[tokio::test]
    async fn test_run_with_pre_cancellation() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("task1", |i: u32| async move { Ok(i + 1) })
            .then("task2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Save initial snapshot and request cancellation before running
        let input_bytes = Arc::new(JsonCodec).encode(&1u32).unwrap();
        let mut snapshot = WorkflowSnapshot::with_initial_input(
            "inst-precancel".into(),
            workflow.definition_hash().to_string(),
            input_bytes,
        );
        snapshot.update_position(ExecutionPosition::AtTask {
            task_id: "task1".into(),
        });
        runner.backend().save_snapshot(&snapshot).await.unwrap();

        runner
            .cancel("inst-precancel", Some("pre-cancel".into()), None)
            .await
            .unwrap();

        // Resume should detect cancellation
        let status = runner.resume(&workflow, "inst-precancel").await.unwrap();
        match status {
            WorkflowStatus::Cancelled { reason, .. } => {
                assert_eq!(reason, Some("pre-cancel".into()));
            }
            _ => panic!("Expected Cancelled status, got: {status:?}"),
        }
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[tokio::test]
    async fn test_resume_nonexistent_instance() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("task", |i: u32| async move { Ok(i) })
            .build()
            .unwrap();

        let result = runner.resume(&workflow, "nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_failure_in_chain_saves_snapshot() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("fail_step", |_i: u32| async move {
                Err::<u32, BoxError>("mid-chain failure".into())
            })
            .then("step3", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
        match status {
            WorkflowStatus::Failed(e) => {
                assert!(e.to_string().contains("mid-chain failure"));
            }
            _ => panic!("Expected Failed"),
        }

        // Snapshot should be saved as failed
        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_failed());
    }

    // ========================================================================
    // Delay tests
    // ========================================================================

    #[tokio::test]
    async fn test_run_workflow_with_delay_returns_waiting() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_1h", std::time::Duration::from_secs(3600))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();

        // Should return Waiting (delay is 1 hour in the future)
        match &status {
            WorkflowStatus::Waiting { delay_id, .. } => {
                assert_eq!(delay_id, "wait_1h");
            }
            _ => panic!("Expected Waiting status, got {status:?}"),
        }

        // Snapshot should be in-progress at AtDelay position
        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_in_progress());
        match &snapshot.state {
            WorkflowSnapshotState::InProgress { position, .. } => match position {
                ExecutionPosition::AtDelay {
                    delay_id,
                    next_task_id,
                    ..
                } => {
                    assert_eq!(delay_id, "wait_1h");
                    assert_eq!(next_task_id.as_deref(), Some("step2"));
                }
                other => panic!("Expected AtDelay, got {other:?}"),
            },
            _ => panic!("Expected InProgress"),
        }

        // step1 should have been completed
        assert!(snapshot.get_task_result("step1").is_some());
        // delay pass-through should be stored
        assert!(snapshot.get_task_result("wait_1h").is_some());
    }

    #[tokio::test]
    async fn test_resume_before_delay_expires_returns_waiting() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_1h", std::time::Duration::from_secs(3600))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Run to delay
        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Waiting { .. }));

        // Resume immediately (delay hasn't expired)
        let status = runner.resume(&workflow, "inst-1").await.unwrap();
        match &status {
            WorkflowStatus::Waiting { delay_id, .. } => {
                assert_eq!(delay_id, "wait_1h");
            }
            _ => panic!("Expected Waiting on resume, got {status:?}"),
        }
    }

    #[tokio::test]
    async fn test_resume_after_delay_expires_completes() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        // Use a very short delay so it expires immediately
        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_short", std::time::Duration::from_millis(1))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Run — delay is so short it should still park (snapshot is saved before checking time)
        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Waiting { .. }));

        // Wait a bit for the delay to definitely expire
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Resume — delay should have expired, execution continues
        let status = runner.resume(&workflow, "inst-1").await.unwrap();
        assert!(
            matches!(status, WorkflowStatus::Completed),
            "Expected Completed after delay expired, got {status:?}"
        );

        // Verify final state
        let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
        assert!(snapshot.state.is_completed());
    }

    #[tokio::test]
    async fn test_cancel_during_delay() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_1h", std::time::Duration::from_secs(3600))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Run to delay
        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Waiting { .. }));

        // Cancel during delay
        runner
            .cancel(
                "inst-1",
                Some("no longer needed".into()),
                Some("admin".into()),
            )
            .await
            .unwrap();

        // Resume should detect cancellation
        let status = runner.resume(&workflow, "inst-1").await.unwrap();
        match status {
            WorkflowStatus::Cancelled {
                reason,
                cancelled_by,
            } => {
                assert_eq!(reason, Some("no longer needed".into()));
                assert_eq!(cancelled_by, Some("admin".into()));
            }
            _ => panic!("Expected Cancelled status, got {status:?}"),
        }
    }

    #[tokio::test]
    async fn test_delay_as_last_node() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("final_wait", std::time::Duration::from_millis(1))
            .build()
            .unwrap();

        // Run to delay
        let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Waiting { .. }));

        // Wait for delay to expire
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Resume — delay was the last node, should complete
        let status = runner.resume(&workflow, "inst-1").await.unwrap();
        assert!(
            matches!(status, WorkflowStatus::Completed),
            "Expected Completed when delay is last node, got {status:?}"
        );
    }

    #[tokio::test]
    async fn test_delay_data_passthrough() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        // step1 produces 11, delay passes it through, step2 receives 11 and doubles
        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait", std::time::Duration::from_millis(1))
            .then("step2", |i: u32| async move {
                // Verify input is the passthrough value from step1
                assert_eq!(i, 11);
                Ok(i * 2)
            })
            .build()
            .unwrap();

        // Run to delay
        runner.run(&workflow, "inst-1", 10u32).await.unwrap();

        // Wait and resume
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let status = runner.resume(&workflow, "inst-1").await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));
    }
}
