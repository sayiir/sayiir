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

use std::ops::ControlFlow;
use std::sync::Arc;

use bytes::Bytes;
use sayiir_core::codec::sealed;
use sayiir_core::codec::{Codec, EnvelopeCodec};
use sayiir_core::context::{WorkflowContext, with_context};
use sayiir_core::error::WorkflowError;
use sayiir_core::snapshot::{ExecutionPosition, SignalKind, SignalRequest, WorkflowSnapshot};
use sayiir_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};
use sayiir_persistence::PersistentBackend;

use crate::error::RuntimeError;
use crate::execution::control_flow::{
    ParkReason, StepOutcome, StepResult, compute_signal_timeout, compute_wake_at,
    save_branch_park_checkpoint, save_park_checkpoint,
};
use crate::execution::{
    ForkBranchOutcome, JoinResolution, ResumeParkedPosition, branch_execute_or_skip_task,
    check_guards, collect_cached_branches, execute_or_skip_task, finalize_execution,
    get_resume_input, resolve_join, retry_with_checkpoint, set_deadline_if_needed,
    settle_fork_outcome,
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
/// ```rust,no_run
/// # use sayiir_runtime::prelude::*;
/// # use std::sync::Arc;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let backend = InMemoryBackend::new();
/// let runner = CheckpointingRunner::new(backend);
///
/// let ctx = WorkflowContext::new("my-workflow", Arc::new(JsonCodec), Arc::new(()));
/// let workflow = WorkflowBuilder::new(ctx)
///     .then("step1", |i: u32| async move { Ok(i + 1) })
///     .build()?;
///
/// // Run workflow - snapshots are saved automatically
/// let status = runner.run(&workflow, "instance-123", 1u32).await?;
///
/// // Resume from checkpoint if needed (e.g., after crash)
/// let status = runner.resume(&workflow, "instance-123").await?;
/// # Ok(())
/// # }
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
            .store_signal(
                instance_id,
                SignalKind::Cancel,
                SignalRequest::new(reason, cancelled_by),
            )
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
            .store_signal(
                instance_id,
                SignalKind::Pause,
                SignalRequest::new(reason, paused_by),
            )
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
        C: Codec
            + EnvelopeCodec
            + sealed::EncodeValue<Input>
            + sealed::DecodeValue<Input>
            + 'static,
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
        C: Codec
            + EnvelopeCodec
            + sealed::DecodeValue<Input>
            + sealed::EncodeValue<Input>
            + 'static,
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

        // Resolve any parked position (delay / fork) before resuming.
        let parked = ResumeParkedPosition::extract(&snapshot);
        if let Some(status) = parked
            .resolve(&mut snapshot, instance_id, self.backend.as_ref())
            .await?
        {
            return Ok(status);
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
        C: Codec + EnvelopeCodec + 'static,
        M: Send + Sync + 'static,
    {
        let mut current = continuation;
        let mut current_input = input;

        loop {
            let step: StepResult = match current {
                WorkflowContinuation::Task {
                    id,
                    func: Some(func),
                    timeout,
                    retry_policy,
                    ..
                } => {
                    check_guards(backend.as_ref(), &snapshot.instance_id, Some(id)).await?;
                    set_deadline_if_needed(id, timeout.as_ref(), snapshot, backend.as_ref())
                        .await?;

                    let output = retry_with_checkpoint(
                        id,
                        retry_policy.as_ref(),
                        timeout.as_ref(),
                        snapshot,
                        Some(backend.as_ref()),
                        async |snap| {
                            execute_or_skip_task(id, current_input.clone(), |i| func.run(i), snap)
                                .await
                        },
                    )
                    .await?;

                    if let Some(next_cont) = current.get_next() {
                        snapshot.update_position(ExecutionPosition::AtTask {
                            task_id: next_cont.first_task_id().to_string(),
                        });
                    }
                    backend.save_snapshot(snapshot).await?;
                    check_guards(backend.as_ref(), &snapshot.instance_id, None).await?;

                    Ok(ControlFlow::Continue(output))
                }
                WorkflowContinuation::Task { func: None, id, .. } => {
                    return Err(WorkflowError::TaskNotImplemented(id.clone()).into());
                }
                WorkflowContinuation::Delay { id, duration, next } => {
                    check_guards(backend.as_ref(), &snapshot.instance_id, Some(id)).await?;

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
                    check_guards(backend.as_ref(), &snapshot.instance_id, Some(id)).await?;

                    if snapshot.get_task_result(id).is_some() {
                        let payload = snapshot
                            .get_task_result_bytes(id)
                            .unwrap_or(current_input.clone());
                        Ok(ControlFlow::Continue(payload))
                    } else {
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
                            settle_fork_outcome(
                                fork_id,
                                outcome,
                                join.as_deref(),
                                snapshot,
                                backend.as_ref(),
                            )
                            .await?
                        };

                    match resolve_join(join.as_deref(), &branch_results, context.codec.as_ref())? {
                        JoinResolution::Continue { input, .. } => Ok(ControlFlow::Continue(input)),
                        JoinResolution::Done(output) => {
                            Ok(ControlFlow::Break(StepOutcome::Done(output)))
                        }
                    }
                }
                WorkflowContinuation::Branch {
                    id,
                    key_fn: Some(key_fn),
                    branches,
                    default,
                    ..
                } => {
                    check_guards(backend.as_ref(), &snapshot.instance_id, Some(id)).await?;

                    if let Some(result) = snapshot.get_task_result(id) {
                        Ok(ControlFlow::Continue(result.output.clone()))
                    } else {
                        let key_bytes = key_fn
                            .run(current_input.clone())
                            .await
                            .map_err(RuntimeError::from)?;
                        let key: String = context
                            .codec
                            .decode_string(&key_bytes)
                            .map_err(RuntimeError::from)?;

                        let chosen = branches.get(&key).or(default.as_ref()).ok_or_else(|| {
                            WorkflowError::BranchKeyNotFound {
                                branch_id: id.clone(),
                                key: key.clone(),
                            }
                        })?;

                        let branch_output = Self::execute_branch_with_checkpoint(
                            chosen,
                            current_input.clone(),
                            Arc::clone(&backend),
                            snapshot.instance_id.clone(),
                            context.clone(),
                        )
                        .await?;

                        let envelope_bytes = context
                            .codec
                            .encode_branch_envelope(&key, &branch_output)
                            .map_err(RuntimeError::from)?;

                        snapshot.mark_task_completed(id.clone(), envelope_bytes.clone());
                        backend.save_snapshot(snapshot).await?;

                        Ok(ControlFlow::Continue(envelope_bytes))
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
                    return Err(save_park_checkpoint(reason, snapshot, backend.as_ref()).await);
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
        C: Codec + EnvelopeCodec + 'static,
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

    /// Execute nested fork branches in parallel within a branch.
    ///
    /// Spawns each branch as a tokio task, collects all results, and propagates
    /// errors (including `JoinError`).
    async fn execute_nested_fork_branches<C, M>(
        branches: &[Arc<WorkflowContinuation>],
        input: &Bytes,
        backend: &Arc<B>,
        instance_id: &str,
        context: &WorkflowContext<C, M>,
    ) -> Result<Vec<(String, Bytes)>, RuntimeError>
    where
        B: 'static,
        C: Codec + EnvelopeCodec + 'static,
        M: Send + Sync + 'static,
    {
        let mut set: tokio::task::JoinSet<Result<(String, Bytes), RuntimeError>> =
            tokio::task::JoinSet::new();
        for branch in branches {
            let id = branch.id().to_string();
            let branch = Arc::clone(branch);
            let branch_input = input.clone();
            let branch_backend = Arc::clone(backend);
            let branch_instance_id = instance_id.to_string();
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

        let mut branch_results: Vec<(String, Bytes)> = Vec::with_capacity(set.len());
        while let Some(res) = set.join_next().await {
            branch_results.push(res??);
        }
        Ok(branch_results)
    }

    /// Execute branch continuation with per-task checkpointing (iterative, no boxing).
    ///
    /// Unlike `execute_with_checkpointing`, this doesn't update position tracking
    /// (branches run independently). It saves each task result directly to the backend.
    ///
    /// On resume after `AtFork`, the backend snapshot contains sub-task results from
    /// the previous execution. This function loads the snapshot to skip cached tasks
    /// and parks at delays instead of sleeping through them.
    #[allow(clippy::manual_async_fn, clippy::too_many_lines)]
    fn execute_branch_with_checkpoint<C, M>(
        continuation: &WorkflowContinuation,
        input: Bytes,
        backend: Arc<B>,
        instance_id: String,
        context: WorkflowContext<C, M>,
    ) -> impl std::future::Future<Output = Result<Bytes, RuntimeError>> + Send + '_
    where
        B: 'static,
        C: Codec + EnvelopeCodec + 'static,
        M: Send + Sync + 'static,
    {
        async move {
            let mut snapshot = backend.load_snapshot(&instance_id).await?;

            let mut current = continuation;
            let mut current_input = input;

            loop {
                let step: StepResult = match current {
                    WorkflowContinuation::Task {
                        id,
                        func,
                        timeout,
                        retry_policy,
                        ..
                    } => {
                        let func = func
                            .as_ref()
                            .ok_or_else(|| WorkflowError::TaskNotImplemented(id.clone()))?;

                        let output = loop {
                            match branch_execute_or_skip_task(
                                id,
                                current_input.clone(),
                                |i| func.run(i),
                                timeout.as_ref(),
                                &mut snapshot,
                                &instance_id,
                                backend.as_ref(),
                            )
                            .await
                            {
                                Ok(output) => {
                                    snapshot.clear_retry_state(id);
                                    break output;
                                }
                                Err(e) => {
                                    if let Some(rp) = retry_policy
                                        && !snapshot.retries_exhausted(id)
                                    {
                                        let next_retry_at =
                                            snapshot.record_retry(id, rp, &e.to_string(), None);
                                        snapshot.clear_task_deadline();
                                        tracing::info!(
                                            task_id = %id,
                                            attempt = snapshot.get_retry_state(id).map_or(0, |rs| rs.attempts),
                                            max_retries = rp.max_retries,
                                            %next_retry_at,
                                            error = %e,
                                            "Retrying task (branch)"
                                        );
                                        let delay = (next_retry_at - chrono::Utc::now())
                                            .to_std()
                                            .unwrap_or_default();
                                        tokio::time::sleep(delay).await;
                                        continue;
                                    }
                                    return Err(e);
                                }
                            }
                        };
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
                            let wake_at = compute_signal_timeout(timeout.as_ref());
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
                        let branch_results = Self::execute_nested_fork_branches(
                            branches,
                            &current_input,
                            &backend,
                            &instance_id,
                            &context,
                        )
                        .await?;

                        match resolve_join(
                            join.as_deref(),
                            &branch_results,
                            context.codec.as_ref(),
                        )? {
                            JoinResolution::Continue { input, .. } => {
                                Ok(ControlFlow::Continue(input))
                            }
                            JoinResolution::Done(output) => {
                                Ok(ControlFlow::Break(StepOutcome::Done(output)))
                            }
                        }
                    }
                    WorkflowContinuation::Branch {
                        id,
                        key_fn: Some(key_fn),
                        branches,
                        default,
                        ..
                    } => {
                        if let Some(result) = snapshot.get_task_result(id) {
                            Ok(ControlFlow::Continue(result.output.clone()))
                        } else {
                            let key_bytes = key_fn
                                .run(current_input.clone())
                                .await
                                .map_err(RuntimeError::from)?;
                            let key: String = context
                                .codec
                                .decode_string(&key_bytes)
                                .map_err(RuntimeError::from)?;

                            let chosen =
                                branches.get(&key).or(default.as_ref()).ok_or_else(|| {
                                    WorkflowError::BranchKeyNotFound {
                                        branch_id: id.clone(),
                                        key: key.clone(),
                                    }
                                })?;

                            let branch_output = Box::pin(Self::execute_branch_with_checkpoint(
                                chosen,
                                current_input.clone(),
                                Arc::clone(&backend),
                                instance_id.clone(),
                                context.clone(),
                            ))
                            .await?;

                            let envelope_bytes = context
                                .codec
                                .encode_branch_envelope(&key, &branch_output)
                                .map_err(RuntimeError::from)?;

                            snapshot.mark_task_completed(id.clone(), envelope_bytes.clone());
                            backend.save_snapshot(&snapshot).await?;

                            Ok(ControlFlow::Continue(envelope_bytes))
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
                        return Err(save_branch_park_checkpoint(
                            reason,
                            &instance_id,
                            backend.as_ref(),
                        )
                        .await);
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
    use sayiir_core::snapshot::WorkflowSnapshotState;
    use sayiir_core::task::BranchOutputs;
    use sayiir_core::workflow::WorkflowBuilder;
    use sayiir_macros::BranchKey;
    use sayiir_persistence::InMemoryBackend;
    use sayiir_persistence::{SignalStore, SnapshotStore};

    #[derive(BranchKey)]
    enum RouteKey {
        Billing,
        Tech,
    }

    #[derive(BranchKey)]
    enum AbKey {
        A,
        B,
    }

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
                assert!(e.contains("intentional failure"));
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
            .get_signal("inst-cancel", SignalKind::Cancel)
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
                assert!(e.contains("mid-chain failure"));
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

    // ========================================================================
    // Timeout tests
    // ========================================================================

    #[tokio::test]
    async fn test_run_task_timeout_fails_workflow() {
        use sayiir_core::task::TaskMetadata;

        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .with_registry()
            .then("slow_task", |i: u32| async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok(i)
            })
            .with_metadata(TaskMetadata {
                timeout: Some(std::time::Duration::from_millis(5)),
                ..Default::default()
            })
            .build()
            .unwrap();

        let status = runner
            .run(workflow.workflow(), "inst-timeout", 5u32)
            .await
            .unwrap();
        match status {
            WorkflowStatus::Failed(msg) => {
                assert!(
                    msg.contains("timed out"),
                    "Expected timeout error, got: {msg}"
                );
                assert!(
                    msg.contains("slow_task"),
                    "Expected task id in error, got: {msg}"
                );
            }
            other => panic!("Expected Failed status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_run_task_within_timeout_succeeds() {
        use sayiir_core::task::TaskMetadata;

        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend);

        let workflow = WorkflowBuilder::new(ctx())
            .with_registry()
            .then("fast_task", |i: u32| async move { Ok(i + 1) })
            .with_metadata(TaskMetadata {
                timeout: Some(std::time::Duration::from_secs(5)),
                ..Default::default()
            })
            .build()
            .unwrap();

        let status = runner
            .run(workflow.workflow(), "inst-fast", 5u32)
            .await
            .unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));
    }

    #[tokio::test]
    async fn test_route_selects_correct_branch() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend.clone());

        let workflow = WorkflowBuilder::new(ctx())
            .then("classify", |input: String| async move {
                Ok(serde_json::json!({ "intent": input }))
            })
            .route::<u32, RouteKey, _, _>(|data: serde_json::Value| async move {
                match data["intent"].as_str().unwrap_or("unknown") {
                    "billing" => Ok(RouteKey::Billing),
                    "tech" => Ok(RouteKey::Tech),
                    other => Err(format!("unknown intent: {other}").into()),
                }
            })
            .branch(RouteKey::Billing, |sub| {
                sub.then("handle_billing", |_data: serde_json::Value| async move {
                    Ok(100u32)
                })
            })
            .unwrap()
            .branch(RouteKey::Tech, |sub| {
                sub.then("handle_tech", |_data: serde_json::Value| async move {
                    Ok(200u32)
                })
            })
            .unwrap()
            .done()
            .unwrap()
            .build()
            .unwrap();

        // Route to "billing"
        let status = runner
            .run(&workflow, "inst-branch-1", "billing".to_string())
            .await
            .unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));

        let snapshot = backend.load_snapshot("inst-branch-1").await.unwrap();
        // Workflow completed — check the final output (which is the branch envelope
        // since route is the last step)
        match &snapshot.state {
            WorkflowSnapshotState::Completed { final_output } => {
                let envelope: serde_json::Value = serde_json::from_slice(final_output).unwrap();
                assert_eq!(envelope["branch"], "billing");
                assert_eq!(envelope["result"], 100);
            }
            other => panic!("Expected Completed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_route_with_default() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend.clone());

        // With typed keys the default branch catches enum variants that
        // don't have an explicit `.branch()` call.  Route "b" has no
        // branch, so the default fires.
        let workflow = WorkflowBuilder::new(ctx())
            .route::<String, AbKey, _, _>(|input: String| async move {
                match input.as_str() {
                    "a" => Ok(AbKey::A),
                    "b" => Ok(AbKey::B),
                    other => Err(format!("unknown: {other}").into()),
                }
            })
            .branch(AbKey::A, |sub| {
                sub.then("handle_a", |_data: String| async move {
                    Ok("matched".to_string())
                })
            })
            .unwrap()
            .default_branch(|sub| {
                sub.then("handle_fallback", |_data: String| async move {
                    Ok("fallback".to_string())
                })
            })
            .unwrap()
            .done()
            .unwrap()
            .build()
            .unwrap();

        // Send "b" — not explicitly branched, so the default fires
        let status = runner
            .run(&workflow, "inst-branch-default", "b".to_string())
            .await
            .unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));

        let snapshot = backend.load_snapshot("inst-branch-default").await.unwrap();
        match &snapshot.state {
            WorkflowSnapshotState::Completed { final_output } => {
                let envelope: serde_json::Value = serde_json::from_slice(final_output).unwrap();
                assert_eq!(envelope["branch"], "b");
                assert_eq!(envelope["result"], "fallback");
            }
            other => panic!("Expected Completed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_route_missing_branches_detected() {
        // With typed keys, missing branches are caught at build time.
        // RouteKey has {billing, tech} but we only branch on billing → MissingBranches.
        let result = WorkflowBuilder::new(ctx())
            .route::<String, RouteKey, _, _>(|input: String| async move {
                match input.as_str() {
                    "billing" => Ok(RouteKey::Billing),
                    _ => Ok(RouteKey::Tech),
                }
            })
            .branch(RouteKey::Billing, |sub| {
                sub.then("handle_billing", |_data: String| async move {
                    Ok("ok".to_string())
                })
            })
            .unwrap()
            .done();

        match result {
            Err(sayiir_core::error::BuildError::MissingBranches {
                branch_id,
                missing_keys,
            }) => {
                assert_eq!(branch_id, "branch_1");
                assert!(missing_keys.contains(&"tech".to_string()));
            }
            Err(other) => panic!("Expected MissingBranches, got: {other}"),
            Ok(_) => panic!("Expected MissingBranches error, got Ok"),
        }
    }

    #[tokio::test]
    async fn test_route_then_next_step() {
        let backend = InMemoryBackend::new();
        let runner = CheckpointingRunner::new(backend.clone());

        use sayiir_core::task::BranchEnvelope;

        let workflow = WorkflowBuilder::new(ctx())
            .route::<u32, AbKey, _, _>(|input: String| async move {
                match input.as_str() {
                    "a" => Ok(AbKey::A),
                    "b" => Ok(AbKey::B),
                    other => Err(format!("unknown: {other}").into()),
                }
            })
            .branch(AbKey::A, |sub| {
                sub.then("handle_a", |_data: String| async move { Ok(10u32) })
            })
            .unwrap()
            .branch(AbKey::B, |sub| {
                sub.then("handle_b", |_data: String| async move { Ok(20u32) })
            })
            .unwrap()
            .done()
            .unwrap()
            .then("finalize", |env: BranchEnvelope<u32>| async move {
                Ok(env.result + 1)
            })
            .build()
            .unwrap();

        let status = runner
            .run(&workflow, "inst-branch-next", "a".to_string())
            .await
            .unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));

        let snapshot = backend.load_snapshot("inst-branch-next").await.unwrap();
        match &snapshot.state {
            WorkflowSnapshotState::Completed { final_output } => {
                let val: u32 = serde_json::from_slice(final_output).unwrap();
                assert_eq!(val, 11); // branch "a" returned 10, finalize adds 1
            }
            other => panic!("Expected Completed, got: {other:?}"),
        }
    }
}
