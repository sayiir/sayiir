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
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time;
use workflow_core::codec::Codec;
use workflow_core::codec::sealed;
use workflow_core::context::with_context;
use workflow_core::registry::TaskRegistry;
use workflow_core::snapshot::{ExecutionPosition, WorkflowSnapshot};
use workflow_core::task_claim::AvailableTask;
use workflow_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};
use workflow_persistence::PersistentBackend;

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
/// use workflow_runtime::worker::PooledWorker;
/// use workflow_persistence::InMemoryBackend;
/// use workflow_core::registry::TaskRegistry;
///
/// let backend = InMemoryBackend::new();
/// let registry = TaskRegistry::new(); // Must contain all task implementations
/// let worker = PooledWorker::new("worker-1", backend, registry);
///
/// // Start polling for work
/// worker.start_polling(Duration::from_secs(1)).await?;
/// ```
#[derive(Clone)]
pub struct PooledWorker<B> {
    worker_id: String,
    backend: Arc<B>,
    #[allow(unused)]
    registry: Arc<TaskRegistry>,
    claim_ttl: Option<Duration>,
    heartbeat_interval: Option<Duration>,
    batch_size: NonZeroUsize,
    max_concurrency: NonZeroUsize,
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
    /// - `claim_ttl`: Optional TTL for task claims (default: 5 minutes)
    ///
    /// # Heartbeat
    ///
    /// By default, the worker will send heartbeats every 2 minutes to extend
    /// claims for long-running tasks. This prevents claim expiration while
    /// still allowing failed workers to be detected within the TTL window.
    pub fn new(worker_id: impl Into<String>, backend: B, registry: TaskRegistry) -> Self {
        Self {
            worker_id: worker_id.into(),
            backend: Arc::new(backend),
            registry: Arc::new(registry),
            claim_ttl: Some(Duration::from_secs(5 * 60)), // Default 5 minutes
            heartbeat_interval: Some(Duration::from_secs(2 * 60)), // Default 2 minutes (before TTL)
            batch_size: NonZeroUsize::new(1).unwrap(), // Default: fetch one task at a time
            max_concurrency: NonZeroUsize::new(1).unwrap(), // Default: sequential execution
        }
    }

    /// Set the TTL for task claims.
    #[must_use]
    pub fn with_claim_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.claim_ttl = ttl;
        self
    }

    /// Set the heartbeat interval for claim refreshing.
    ///
    /// The worker will periodically extend task claims to prevent expiration
    /// for long-running tasks. Set to `None` to disable heartbeats.
    ///
    /// # Recommendation
    ///
    /// Should be less than the claim TTL (e.g., if TTL is 5 minutes, use 2 minutes).
    #[must_use]
    pub fn with_heartbeat_interval(mut self, interval: Option<Duration>) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Set the number of tasks to fetch per poll (default: 1).
    ///
    /// With batch_size=1, the worker fetches one task, executes it, then polls again.
    /// Other workers can pick up remaining tasks immediately.
    ///
    /// Higher values reduce polling overhead but may cause workers to hold task IDs
    /// they won't process immediately (though other workers can still claim them).
    #[must_use]
    pub fn with_batch_size(mut self, size: NonZeroUsize) -> Self {
        self.batch_size = size;
        self
    }

    /// Set the maximum number of concurrent task executions (default: 1).
    ///
    /// With max_concurrency=1 (default), tasks execute sequentially.
    /// Higher values allow parallel task execution within this worker,
    /// useful for I/O-bound tasks.
    ///
    /// Note: Each concurrent task maintains its own heartbeat.
    #[must_use]
    pub fn with_max_concurrency(mut self, max: NonZeroUsize) -> Self {
        self.max_concurrency = max;
        self
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
    ) -> anyhow::Result<WorkflowStatus>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<Input> + 'static,
    {
        if available_task.workflow_definition_hash != workflow.definition_hash() {
            return Err(anyhow::anyhow!(
                "Workflow definition hash mismatch: expected {}, found {}",
                workflow.definition_hash(),
                available_task.workflow_definition_hash
            ));
        }

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

        match claim {
            Some(_) => {
                tracing::debug!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    "Claim successful"
                );
            }
            None => {
                tracing::debug!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    "Task was already claimed by another worker"
                );
                return Ok(WorkflowStatus::InProgress);
            }
        }

        let mut snapshot = self
            .backend
            .load_snapshot(&available_task.instance_id)
            .await?;

        if snapshot.get_task_result(&available_task.task_id).is_some() {
            tracing::debug!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Task already completed, releasing claim"
            );
            let _ = self
                .backend
                .release_task_claim(
                    &available_task.instance_id,
                    &available_task.task_id,
                    &self.worker_id,
                )
                .await;
            return Ok(WorkflowStatus::InProgress);
        }

        if !Self::find_task_id_in_continuation(workflow.continuation(), &available_task.task_id) {
            tracing::error!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Task does not exist in workflow, releasing claim"
            );
            let _ = self
                .backend
                .release_task_claim(
                    &available_task.instance_id,
                    &available_task.task_id,
                    &self.worker_id,
                )
                .await;
            return Err(anyhow::anyhow!(
                "Task {} not found in workflow",
                available_task.task_id
            ));
        }

        // Start heartbeat task to periodically extend the claim
        let heartbeat_handle = if let Some(interval) = self.heartbeat_interval {
            let backend = self.backend.clone();
            let instance_id = available_task.instance_id.clone();
            let task_id = available_task.task_id.clone();
            let worker_id = self.worker_id.clone();
            let claim_ttl = self.claim_ttl;

            let handle = tokio::spawn(async move {
                let mut interval_timer = time::interval(interval);
                interval_timer.tick().await; // Skip first immediate tick

                loop {
                    interval_timer.tick().await;

                    tracing::trace!(
                        instance_id = %instance_id,
                        task_id = %task_id,
                        "Extending task claim via heartbeat"
                    );

                    if let Some(ttl) = claim_ttl {
                        let chrono_ttl = chrono::Duration::from_std(ttl).ok();
                        if let Some(ttl) = chrono_ttl {
                            let result = backend
                                .extend_task_claim(&instance_id, &task_id, &worker_id, ttl)
                                .await;

                            if let Err(e) = result {
                                tracing::warn!(
                                    instance_id = %instance_id,
                                    task_id = %task_id,
                                    error = %e,
                                    "Failed to extend task claim during heartbeat"
                                );
                                // Continue anyway - the task execution should handle expiration
                            } else {
                                tracing::trace!(
                                    instance_id = %instance_id,
                                    task_id = %task_id,
                                    "Extended task claim via heartbeat"
                                );
                            }
                        }
                    }
                }
            });
            Some(handle)
        } else {
            None
        };

        tracing::debug!(
            instance_id = %available_task.instance_id,
            task_id = %available_task.task_id,
            "Executing task"
        );

        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let task_id = available_task.task_id.clone();
        let input = available_task.input.clone();

        // We need to execute the task directly from the continuation
        // Since we can't clone the continuation, we'll traverse it to find and execute the task
        let result = with_context(context, || async move {
            Self::execute_task_by_id(continuation, &task_id, input).await
        })
        .await;

        if let Some(handle) = heartbeat_handle {
            handle.abort();
            tracing::debug!(
                instance_id = %available_task.instance_id,
                task_id = %available_task.task_id,
                "Stopped heartbeat for task"
            );
        }

        match result {
            Ok(output) => {
                snapshot.mark_task_completed(available_task.task_id.clone(), output.clone());
                tracing::debug!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    "Task completed"
                );

                Self::update_position_after_task(
                    workflow.continuation(),
                    &available_task.task_id,
                    &mut snapshot,
                );

                self.backend.save_snapshot(snapshot.clone()).await?;

                self.backend
                    .release_task_claim(
                        &available_task.instance_id,
                        &available_task.task_id,
                        &self.worker_id,
                    )
                    .await?;

                if Self::is_workflow_complete(workflow.continuation(), &snapshot) {
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
            Err(e) => {
                tracing::error!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    error = %e,
                    "Task execution failed"
                );
                let _ = self
                    .backend
                    .release_task_claim(
                        &available_task.instance_id,
                        &available_task.task_id,
                        &self.worker_id,
                    )
                    .await;
                Err(e)
            }
        }
    }

    /// Poll for available tasks and execute them.
    ///
    /// This continuously polls the backend for available tasks and executes them.
    /// Task execution is gated by a semaphore based on `max_concurrency`.
    /// Returns when an error occurs or the future is cancelled.
    ///
    /// # Parameters
    ///
    /// - `poll_interval`: How often to poll for new tasks
    /// - `workflows`: Map of workflow definition hash to workflow (for task execution)
    #[allow(clippy::type_complexity)]
    pub async fn start_polling<C, Input, M>(
        self,
        poll_interval: Duration,
        workflows: Vec<(String, Arc<Workflow<C, Input, M>>)>,
    ) -> anyhow::Result<()>
    where
        Input: Send + Sync + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<Input> + Send + Sync + 'static,
    {
        let semaphore = Arc::new(Semaphore::new(self.max_concurrency.get()));
        let worker = Arc::new(self);
        let workflows = Arc::new(workflows);
        let mut interval = time::interval(poll_interval);

        loop {
            // Wait for available capacity
            let permit = semaphore.clone().acquire_owned().await?;

            // Fetch tasks (up to batch_size, but we process one at a time)
            let available_tasks = worker
                .backend
                .find_available_tasks(&worker.worker_id, worker.batch_size.get())
                .await?;

            // Find first task with a matching workflow
            let task_with_workflow = available_tasks.into_iter().find_map(|task| {
                workflows
                    .iter()
                    .find(|(hash, _)| *hash == task.workflow_definition_hash)
                    .map(|(_, workflow)| (task, Arc::clone(workflow)))
            });

            match task_with_workflow {
                Some((task, workflow)) => {
                    // Spawn task with the permit
                    let worker = Arc::clone(&worker);

                    tokio::spawn(async move {
                        let _permit = permit; // Hold permit until task completes

                        match worker.execute_task(workflow.as_ref(), task).await {
                            Ok(_) => {
                                tracing::info!("Worker {} completed a task", worker.worker_id);
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Worker {} task execution failed: {}",
                                    worker.worker_id,
                                    e
                                );
                            }
                        }
                    });

                    // Immediately loop back to fill more capacity (no wait)
                }
                None => {
                    // No tasks available, release permit and wait for poll interval
                    drop(permit);
                    interval.tick().await;
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
            WorkflowContinuation::Task { id, .. } => id == task_id,
            WorkflowContinuation::Fork { branches, join } => {
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

    /// Execute a task by ID from the workflow continuation.
    fn execute_task_by_id<'a>(
        continuation: &'a WorkflowContinuation,
        task_id: &'a str,
        input: Bytes,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Bytes>> + Send + 'a>>
    {
        Box::pin(async move {
            match continuation {
                WorkflowContinuation::Task { id, func, next } => {
                    if id == task_id {
                        func.run(input).await
                    } else if let Some(next_cont) = next {
                        Self::execute_task_by_id(next_cont, task_id, input).await
                    } else {
                        Err(anyhow::anyhow!("Task {task_id} not found"))
                    }
                }
                WorkflowContinuation::Fork { branches, join } => {
                    // Check branches
                    for branch in branches {
                        if Self::find_task_id_in_continuation(branch, task_id) {
                            return Self::execute_task_by_id(branch, task_id, input).await;
                        }
                    }
                    // Check join
                    if let Some(join_cont) = join {
                        Self::execute_task_by_id(join_cont, task_id, input).await
                    } else {
                        Err(anyhow::anyhow!("Task {task_id} not found"))
                    }
                }
            }
        })
    }

    /// Update execution position after a task completes.
    fn update_position_after_task(
        continuation: &WorkflowContinuation,
        completed_task_id: &str,
        snapshot: &mut WorkflowSnapshot,
    ) {
        match continuation {
            WorkflowContinuation::Task { id, next, .. } => {
                if id == completed_task_id {
                    if let Some(next_cont) = next {
                        snapshot.update_position(ExecutionPosition::AtTask {
                            task_id: Self::get_first_task_id(next_cont),
                        });
                    }
                } else if let Some(next_cont) = next {
                    Self::update_position_after_task(next_cont, completed_task_id, snapshot);
                }
            }
            WorkflowContinuation::Fork { branches, join } => {
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

    /// Get the first task ID from a continuation.
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
            WorkflowContinuation::Fork { branches, join } => {
                // All branches must be completed
                for branch in branches.iter() {
                    if let WorkflowContinuation::Task { id, .. } = branch.as_ref()
                        && snapshot.get_task_result(id).is_none()
                    {
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
