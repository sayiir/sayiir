//! Pooled worker for distributed, multi-worker workflow execution.
//!
//! A pooled worker is part of a worker pool that collaboratively executes workflows.
//! Each worker polls the backend for available tasks, claims them (to prevent duplicates),
//! executes them, and updates the snapshot. Multiple workers can process tasks from
//! the same workflow instance in parallel.
//!
//! **Use this when**: You need horizontal scaling with multiple workers processing
//! tasks concurrently across machines or processes.
//!
//! **Use [`CheckpointingRunner`](crate::runner::distributed::CheckpointingRunner) instead when**:
//! You want a single process to run an entire workflow with crash recovery.

use bytes::Bytes;
use chrono;
use futures::FutureExt;
use sayiir_core::codec::Codec;
use sayiir_core::codec::sealed;
use sayiir_core::context::with_context;
use sayiir_core::error::WorkflowError;
use sayiir_core::registry::TaskRegistry;
use sayiir_core::snapshot::{
    CancellationRequest, ExecutionPosition, PauseRequest, WorkflowSnapshot,
};
use sayiir_core::task_claim::AvailableTask;
use sayiir_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};
use sayiir_persistence::PersistentBackend;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
/// Owns a claimed task and provides explicit release methods.
///
/// No `Drop` impl — callers must explicitly call `release()` or `release_quietly()`.
struct ActiveTaskClaim<'a, B> {
    backend: &'a B,
    instance_id: String,
    task_id: String,
    worker_id: String,
}

impl<B: PersistentBackend> ActiveTaskClaim<'_, B> {
    /// Release the claim, propagating backend errors.
    async fn release(self) -> Result<(), crate::error::RuntimeError> {
        self.backend
            .release_task_claim(&self.instance_id, &self.task_id, &self.worker_id)
            .await?;
        Ok(())
    }

    /// Release the claim, silently ignoring errors. Use for error/panic paths.
    async fn release_quietly(self) {
        let _ = self
            .backend
            .release_task_claim(&self.instance_id, &self.task_id, &self.worker_id)
            .await;
    }
}

/// A pooled worker that claims and executes tasks from a shared backend.
///
/// `PooledWorker` is designed for horizontal scaling: multiple workers can run
/// across different machines/processes, all polling the same backend for tasks.
/// Task claiming with TTL prevents duplicate execution while allowing automatic
/// recovery when workers crash.
///
/// # When to Use
///
/// - **Horizontal scaling**: Multiple workers process tasks concurrently
/// - **Fault tolerance**: Failed workers' tasks are automatically reclaimed
/// - **Load balancing**: Tasks distributed across available workers
///
/// For single-process execution with checkpointing, use
/// [`CheckpointingRunner`](crate::runner::distributed::CheckpointingRunner).
///
/// # Example
///
/// ```rust,ignore
/// use sayiir_runtime::worker::PooledWorker;
/// use sayiir_persistence::InMemoryBackend;
/// use sayiir_core::registry::TaskRegistry;
///
/// let backend = InMemoryBackend::new();
/// let registry = TaskRegistry::new(); // Must contain all task implementations
/// let worker = PooledWorker::new("worker-1", backend, registry);
///
/// // Start polling for work
/// worker.start_polling(Duration::from_secs(1)).await?;
/// ```
pub struct PooledWorker<B> {
    worker_id: String,
    backend: Arc<B>,
    #[allow(unused)]
    registry: Arc<TaskRegistry>,
    claim_ttl: Option<Duration>,
    batch_size: NonZeroUsize,
}

impl<B> PooledWorker<B>
where
    B: PersistentBackend + 'static,
{
    /// Create a new worker node.
    ///
    /// # Parameters
    ///
    /// - `worker_id`: Unique identifier for this worker node
    /// - `backend`: The persistent backend to use
    /// - `registry`: Task registry containing all task implementations
    ///
    /// # Heartbeat
    ///
    /// Heartbeats are derived automatically from `claim_ttl` (TTL / 2).
    /// With the default 5-minute TTL, heartbeats fire every 2.5 minutes.
    ///
    pub fn new(worker_id: impl Into<String>, backend: B, registry: TaskRegistry) -> Self {
        Self {
            worker_id: worker_id.into(),
            backend: Arc::new(backend),
            registry: Arc::new(registry),
            claim_ttl: Some(Duration::from_secs(5 * 60)), // Default 5 minutes
            batch_size: NonZeroUsize::MIN,                // Default: fetch one task at a time (1)
        }
    }

    /// Set the TTL for task claims.
    #[must_use]
    pub fn with_claim_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.claim_ttl = ttl;
        self
    }

    /// Set the number of tasks to fetch per poll (default: 1).
    ///
    /// With `batch_size=1`, the worker fetches one task, executes it, then polls again.
    /// Other workers can pick up remaining tasks immediately.
    ///
    /// Higher values reduce polling overhead but may cause workers to hold task IDs
    /// they won't process immediately (though other workers can still claim them).
    #[must_use]
    pub fn with_batch_size(mut self, size: NonZeroUsize) -> Self {
        self.batch_size = size;
        self
    }

    /// Request cancellation of a workflow.
    ///
    /// This requests cancellation of the specified workflow instance.
    /// Running tasks will complete, but no new tasks will be started.
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
    pub async fn cancel_workflow(
        &self,
        instance_id: &str,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> Result<(), crate::error::RuntimeError> {
        self.backend
            .request_cancellation(instance_id, CancellationRequest::new(reason, cancelled_by))
            .await?;

        Ok(())
    }

    /// Request pausing of a workflow.
    ///
    /// This requests pausing of the specified workflow instance.
    /// Running tasks will complete, but no new tasks will be started.
    ///
    /// # Parameters
    ///
    /// - `instance_id`: The workflow instance ID to pause
    /// - `reason`: Optional reason for the pause
    /// - `paused_by`: Optional identifier of who requested the pause
    ///
    /// # Errors
    ///
    /// Returns an error if the workflow cannot be paused (not found or in terminal/paused state).
    pub async fn pause_workflow(
        &self,
        instance_id: &str,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> Result<(), crate::error::RuntimeError> {
        self.backend
            .request_pause(instance_id, PauseRequest::new(reason, paused_by))
            .await?;

        Ok(())
    }

    /// Get a reference to the backend.
    #[must_use]
    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }

    /// Load cancellation status from a snapshot.
    ///
    /// Attempts to load the snapshot and extract cancellation details.
    /// Returns `WorkflowStatus::Cancelled` with either the extracted details or defaults.
    async fn load_cancelled_status(&self, instance_id: &str) -> WorkflowStatus {
        if let Ok(snapshot) = self.backend.load_snapshot(instance_id).await
            && let Some((reason, cancelled_by)) = snapshot.state.cancellation_details()
        {
            return WorkflowStatus::Cancelled {
                reason,
                cancelled_by,
            };
        }
        WorkflowStatus::Cancelled {
            reason: None,
            cancelled_by: None,
        }
    }

    /// Load paused status from a snapshot.
    ///
    /// Attempts to load the snapshot and extract pause details.
    /// Returns `WorkflowStatus::Paused` with either the extracted details or defaults.
    async fn load_paused_status(&self, instance_id: &str) -> WorkflowStatus {
        if let Ok(snapshot) = self.backend.load_snapshot(instance_id).await
            && let Some((reason, paused_by)) = snapshot.state.pause_details()
        {
            return WorkflowStatus::Paused { reason, paused_by };
        }
        WorkflowStatus::Paused {
            reason: None,
            paused_by: None,
        }
    }

    /// Execute a single task from an available task.
    ///
    /// This claims the task, executes it, updates the snapshot, and releases the claim.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The task cannot be claimed
    /// - The workflow definition hash doesn't match
    /// - Task execution fails
    /// - Snapshot update fails
    pub async fn execute_task<C, Input, M>(
        &self,
        workflow: &Workflow<C, Input, M>,
        available_task: AvailableTask,
    ) -> Result<WorkflowStatus, crate::error::RuntimeError>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<Input> + 'static,
    {
        // 1. Load snapshot + pure validation
        let mut snapshot = self
            .backend
            .load_snapshot(&available_task.instance_id)
            .await?;
        let already_completed = Self::validate_task_preconditions(
            workflow.definition_hash(),
            workflow.continuation(),
            &available_task,
            &snapshot,
        )?;
        if already_completed {
            return Ok(WorkflowStatus::InProgress);
        }

        let Some(claim) = self.claim_task(&available_task).await? else {
            return Ok(WorkflowStatus::InProgress);
        };

        // 3. Post-claim guards (cancel/pause)
        if let Some(status) = self.check_post_claim_guards(&available_task).await? {
            claim.release_quietly().await;
            return Ok(status);
        }

        tracing::debug!(
            instance_id = %available_task.instance_id,
            task_id = %available_task.task_id,
            "Executing task"
        );

        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let task_id = available_task.task_id.clone();
        let input = available_task.input.clone();

        // 4. Execute task with panic safety and inline heartbeat
        let execution_future = with_context(context, || async move {
            Self::execute_task_by_id(continuation, &task_id, input).await
        });
        let panic_result = self
            .run_with_heartbeat(&claim, AssertUnwindSafe(execution_future).catch_unwind())
            .await;

        match panic_result {
            Err(panic_payload) => {
                let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Task panicked with unknown payload".to_string()
                };

                tracing::error!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    panic = %panic_msg,
                    "Task panicked - releasing claim"
                );

                claim.release_quietly().await;
                Err(WorkflowError::TaskPanicked(panic_msg).into())
            }
            Ok(Err(e)) => {
                tracing::error!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    error = %e,
                    "Task execution failed"
                );
                claim.release_quietly().await;
                Err(e)
            }
            Ok(Ok(output)) => {
                self.commit_task_result(
                    workflow.continuation(),
                    &available_task,
                    &mut snapshot,
                    output.clone(),
                    claim,
                )
                .await?;
                self.determine_post_task_status(
                    workflow.continuation(),
                    &available_task,
                    &mut snapshot,
                    output,
                )
                .await
            }
        }
    }

    /// Validate task preconditions without side effects.
    ///
    /// Checks definition hash match, task existence in continuation,
    /// and that the task is not already completed in the snapshot.
    /// Returns `Ok(true)` if the task should be skipped (already completed).
    fn validate_task_preconditions(
        definition_hash: &str,
        continuation: &WorkflowContinuation,
        available_task: &AvailableTask,
        snapshot: &WorkflowSnapshot,
    ) -> Result<bool, crate::error::RuntimeError> {
        if available_task.workflow_definition_hash != definition_hash {
            return Err(WorkflowError::DefinitionMismatch {
                expected: definition_hash.to_string(),
                found: available_task.workflow_definition_hash.clone(),
            }
            .into());
        }

        if !Self::find_task_id_in_continuation(continuation, &available_task.task_id) {
            tracing::error!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Task does not exist in workflow"
            );
            return Err(WorkflowError::TaskNotFound(available_task.task_id.clone()).into());
        }

        if snapshot.get_task_result(&available_task.task_id).is_some() {
            tracing::debug!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Task already completed, skipping"
            );
            return Ok(true);
        }

        Ok(false)
    }

    /// Acquire a claim on the task, returning an `ActiveTaskClaim`.
    ///
    /// Returns `None` if already claimed by another worker.
    async fn claim_task(
        &self,
        available_task: &AvailableTask,
    ) -> Result<Option<ActiveTaskClaim<'_, B>>, crate::error::RuntimeError> {
        let claim = self
            .backend
            .claim_task(
                &available_task.instance_id,
                &available_task.task_id,
                &self.worker_id,
                self.claim_ttl
                    .and_then(|d| chrono::Duration::from_std(d).ok()),
            )
            .await?;

        if claim.is_some() {
            tracing::debug!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Claim successful"
            );
            Ok(Some(ActiveTaskClaim {
                backend: &self.backend,
                instance_id: available_task.instance_id.clone(),
                task_id: available_task.task_id.clone(),
                worker_id: self.worker_id.clone(),
            }))
        } else {
            tracing::debug!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Task was already claimed by another worker"
            );
            Ok(None)
        }
    }

    /// Check cancel/pause guards after claiming.
    ///
    /// Returns `Some(status)` if the workflow is cancelled or paused
    /// (caller should release claim and return status).
    /// Returns `None` if execution should proceed.
    async fn check_post_claim_guards(
        &self,
        available_task: &AvailableTask,
    ) -> Result<Option<WorkflowStatus>, crate::error::RuntimeError> {
        if self
            .backend
            .check_and_cancel(&available_task.instance_id, Some(&available_task.task_id))
            .await?
        {
            tracing::info!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Workflow was cancelled, releasing claim"
            );
            return Ok(Some(
                self.load_cancelled_status(&available_task.instance_id)
                    .await,
            ));
        }

        if self
            .backend
            .check_and_pause(&available_task.instance_id)
            .await?
        {
            tracing::info!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Workflow was paused, releasing claim"
            );
            return Ok(Some(
                self.load_paused_status(&available_task.instance_id).await,
            ));
        }

        Ok(None)
    }

    /// Execute a future while periodically extending the task claim.
    ///
    /// Uses `tokio::select!` to race execution against a heartbeat timer — no
    /// `tokio::spawn`, no background tasks. When heartbeats are disabled
    /// (`claim_ttl` is `None`), awaits the future directly.
    async fn run_with_heartbeat<F, T>(&self, claim: &ActiveTaskClaim<'_, B>, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let Some(ttl) = self.claim_ttl else {
            return future.await;
        };
        let Some(chrono_ttl) = chrono::Duration::from_std(ttl).ok() else {
            return future.await;
        };

        let interval_duration = ttl / 2;
        let mut heartbeat_timer = time::interval(interval_duration);
        heartbeat_timer.tick().await; // skip first immediate tick

        tokio::pin!(future);

        loop {
            tokio::select! {
                result = &mut future => break result,
                _ = heartbeat_timer.tick() => {
                    tracing::trace!(
                        instance_id = %claim.instance_id,
                        task_id = %claim.task_id,
                        "Extending task claim via heartbeat"
                    );
                    if let Err(e) = self.backend
                        .extend_task_claim(
                            &claim.instance_id,
                            &claim.task_id,
                            &claim.worker_id,
                            chrono_ttl,
                        )
                        .await
                    {
                        tracing::warn!(
                            instance_id = %claim.instance_id,
                            task_id = %claim.task_id,
                            error = %e,
                            "Failed to extend task claim"
                        );
                    }
                }
            }
        }
    }

    /// Persist task result and release the claim.
    async fn commit_task_result(
        &self,
        continuation: &WorkflowContinuation,
        available_task: &AvailableTask,
        snapshot: &mut WorkflowSnapshot,
        output: Bytes,
        claim: ActiveTaskClaim<'_, B>,
    ) -> Result<(), crate::error::RuntimeError> {
        snapshot.mark_task_completed(available_task.task_id.clone(), output);
        tracing::debug!(
            instance_id = %available_task.instance_id,
            task_id = %available_task.task_id,
            "Task completed"
        );

        Self::update_position_after_task(continuation, &available_task.task_id, snapshot);
        self.backend.save_snapshot(snapshot).await?;
        claim.release().await?;
        Ok(())
    }

    /// Determine workflow status after a task completes.
    ///
    /// Checks cancel/pause guards and workflow completion.
    async fn determine_post_task_status(
        &self,
        continuation: &WorkflowContinuation,
        available_task: &AvailableTask,
        snapshot: &mut WorkflowSnapshot,
        output: Bytes,
    ) -> Result<WorkflowStatus, crate::error::RuntimeError> {
        // Check for cancellation after task completion
        if self
            .backend
            .check_and_cancel(&available_task.instance_id, None)
            .await?
        {
            tracing::info!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Workflow was cancelled after task completion"
            );
            return Ok(self
                .load_cancelled_status(&available_task.instance_id)
                .await);
        }

        // Check for pause after task completion
        if self
            .backend
            .check_and_pause(&available_task.instance_id)
            .await?
        {
            tracing::info!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Workflow was paused after task completion"
            );
            return Ok(self.load_paused_status(&available_task.instance_id).await);
        }

        if Self::is_workflow_complete(continuation, snapshot) {
            tracing::info!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Workflow complete"
            );
            snapshot.mark_completed(output);
            self.backend.save_snapshot(snapshot).await?;
            Ok(WorkflowStatus::Completed)
        } else {
            tracing::debug!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Task completed, workflow continues"
            );
            Ok(WorkflowStatus::InProgress)
        }
    }

    /// Poll for available tasks and execute them.
    ///
    /// This continuously polls the backend for available tasks and executes them.
    /// Returns when an error occurs or the future is cancelled.
    ///
    /// # Parameters
    ///
    /// - `poll_interval`: How often to poll for new tasks
    /// - `workflows`: Map of workflow definition hash to workflow (for task execution)
    ///
    /// # Errors
    ///
    /// Returns an error if polling the backend fails.
    #[allow(clippy::type_complexity)]
    pub async fn start_polling<C, Input, M>(
        &self,
        poll_interval: Duration,
        workflows: Vec<(String, Arc<Workflow<C, Input, M>>)>,
    ) -> Result<(), crate::error::RuntimeError>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<Input> + 'static,
    {
        let mut interval = time::interval(poll_interval);

        loop {
            interval.tick().await;

            // Find available tasks
            let available_tasks = self
                .backend
                .find_available_tasks(&self.worker_id, self.batch_size.get())
                .await?;

            for task in available_tasks {
                // Find matching workflow
                if let Some((_, workflow)) = workflows
                    .iter()
                    .find(|(hash, _)| *hash == task.workflow_definition_hash)
                {
                    // Execute task
                    match self.execute_task(workflow.as_ref(), task).await {
                        Ok(_) => {
                            tracing::info!("Worker {} completed a task", self.worker_id);
                        }
                        Err(e) => {
                            tracing::error!(
                                "Worker {} task execution failed: {}",
                                self.worker_id,
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    /// Find a task function in the workflow continuation and return a reference.
    ///
    /// Note: We can't clone `UntypedCoreTask`, so we need to execute it directly
    /// from the continuation structure. This method returns the task ID if found.
    fn find_task_id_in_continuation(continuation: &WorkflowContinuation, task_id: &str) -> bool {
        match continuation {
            WorkflowContinuation::Task { id, next, .. }
            | WorkflowContinuation::Delay { id, next, .. } => {
                if id == task_id {
                    return true;
                }
                next.as_ref()
                    .is_some_and(|n| Self::find_task_id_in_continuation(n, task_id))
            }
            WorkflowContinuation::Fork { branches, join, .. } => {
                // Check branches
                for branch in branches {
                    if Self::find_task_id_in_continuation(branch, task_id) {
                        return true;
                    }
                }
                // Check join
                if let Some(join_cont) = join {
                    Self::find_task_id_in_continuation(join_cont, task_id)
                } else {
                    false
                }
            }
        }
    }

    /// Execute a task by ID from the workflow continuation (iterative, no boxing).
    #[allow(clippy::manual_async_fn)]
    fn execute_task_by_id<'a>(
        continuation: &'a WorkflowContinuation,
        task_id: &'a str,
        input: Bytes,
    ) -> impl std::future::Future<Output = Result<Bytes, crate::error::RuntimeError>> + Send + 'a
    {
        async move {
            let mut current = continuation;

            loop {
                match current {
                    WorkflowContinuation::Task { id, func, next } => {
                        if id == task_id {
                            let func = func
                                .as_ref()
                                .ok_or_else(|| WorkflowError::TaskNotImplemented(id.clone()))?;
                            return Ok(func.run(input).await?);
                        } else if let Some(next_cont) = next {
                            current = next_cont;
                        } else {
                            return Err(WorkflowError::TaskNotFound(task_id.to_string()).into());
                        }
                    }
                    WorkflowContinuation::Delay { next, .. } => {
                        // Skip over delay nodes when searching for a task
                        if let Some(next_cont) = next {
                            current = next_cont;
                        } else {
                            return Err(WorkflowError::TaskNotFound(task_id.to_string()).into());
                        }
                    }
                    WorkflowContinuation::Fork { branches, join, .. } => {
                        // Check branches
                        let mut found_in_branch = false;
                        for branch in branches {
                            if Self::find_task_id_in_continuation(branch, task_id) {
                                current = branch;
                                found_in_branch = true;
                                break;
                            }
                        }
                        if found_in_branch {
                            continue;
                        }
                        // Check join
                        if let Some(join_cont) = join {
                            current = join_cont;
                        } else {
                            return Err(WorkflowError::TaskNotFound(task_id.to_string()).into());
                        }
                    }
                }
            }
        }
    }

    /// Update execution position after a task completes.
    fn update_position_after_task(
        continuation: &WorkflowContinuation,
        completed_task_id: &str,
        snapshot: &mut WorkflowSnapshot,
    ) {
        match continuation {
            WorkflowContinuation::Task { id, next, .. }
            | WorkflowContinuation::Delay { id, next, .. } => {
                if id == completed_task_id {
                    if let Some(next_cont) = next {
                        snapshot.update_position(ExecutionPosition::AtTask {
                            task_id: next_cont.first_task_id().to_string(),
                        });
                    }
                } else if let Some(next_cont) = next {
                    Self::update_position_after_task(next_cont, completed_task_id, snapshot);
                }
            }
            WorkflowContinuation::Fork { branches, join, .. } => {
                // Check if any branch task completed
                for branch in branches {
                    Self::update_position_after_task(branch, completed_task_id, snapshot);
                }
                // Check join
                if let Some(join_cont) = join {
                    Self::update_position_after_task(join_cont, completed_task_id, snapshot);
                }
            }
        }
    }

    /// Check if the workflow is complete based on the snapshot.
    fn is_workflow_complete(
        continuation: &WorkflowContinuation,
        snapshot: &WorkflowSnapshot,
    ) -> bool {
        // Check if all tasks in the continuation are completed
        match continuation {
            WorkflowContinuation::Task { id, next, .. } => {
                if snapshot.get_task_result(id).is_none() {
                    return false;
                }
                if let Some(next_cont) = next {
                    Self::is_workflow_complete(next_cont, snapshot)
                } else {
                    true // Last task completed
                }
            }
            WorkflowContinuation::Delay { id, next, .. } => {
                if snapshot.get_task_result(id).is_none() {
                    return false;
                }
                next.as_ref()
                    .is_none_or(|n| Self::is_workflow_complete(n, snapshot))
            }
            WorkflowContinuation::Fork { branches, join, .. } => {
                // All branches must be completed (recursively check entire branch chain)
                for branch in branches {
                    if !Self::is_workflow_complete(branch, snapshot) {
                        return false;
                    }
                }
                // Join must be completed if it exists
                if let Some(join_cont) = join {
                    Self::is_workflow_complete(join_cont, snapshot)
                } else {
                    true
                }
            }
        }
    }
}
