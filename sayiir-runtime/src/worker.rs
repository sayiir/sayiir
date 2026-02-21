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

use std::collections::HashMap;

use bytes::Bytes;
use chrono;
use futures::FutureExt;
use sayiir_core::codec::sealed;
use sayiir_core::codec::{Codec, EnvelopeCodec, LoopDecision};
use sayiir_core::context::with_context;
use sayiir_core::error::{BoxError, CodecError, WorkflowError};
use sayiir_core::registry::TaskRegistry;
use sayiir_core::snapshot::{
    ExecutionPosition, SignalKind, SignalRequest, TaskDeadline, WorkflowSnapshot,
};
use sayiir_core::task_claim::AvailableTask;
use sayiir_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};
use sayiir_persistence::{PersistentBackend, SignalStore, TaskClaimStore};
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;

/// A list of workflow definitions keyed by their definition hash.
pub type WorkflowRegistry<C, Input, M> = Vec<(String, Arc<Workflow<C, Input, M>>)>;

/// Workflow definition for binding-friendly worker API.
///
/// Contains only the structural information (definition hash + continuation tree)
/// needed by `PooledWorker` for position tracking, completion detection, retry
/// policies, and timeouts. Task execution is delegated to an external executor.
pub struct ExternalWorkflow {
    /// The continuation tree describing the workflow structure.
    pub continuation: Arc<WorkflowContinuation>,
}

/// Workflow index keyed by definition hash for O(1) lookup during task dispatch.
pub type WorkflowIndex = HashMap<String, ExternalWorkflow>;

/// External task executor function signature.
///
/// Receives the task ID and input bytes, returns the output bytes.
/// Used by language bindings (Python, Node.js) to delegate task execution
/// to the host language's runtime.
pub type ExternalTaskExecutor = Arc<
    dyn Fn(
            &str,
            Bytes,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<Bytes, BoxError>> + Send>>
        + Send
        + Sync,
>;

/// Internal command sent from [`WorkerHandle`] to the actor loop.
enum WorkerCommand {
    Shutdown,
}

struct WorkerHandleInner<B> {
    backend: Arc<B>,
    shutdown_tx: mpsc::Sender<WorkerCommand>,
    join_handle:
        tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), crate::error::RuntimeError>>>>,
}

/// A cloneable handle for interacting with a running [`PooledWorker`].
///
/// Obtained from [`PooledWorker::spawn`]. The handle is cheap to clone and can
/// be shared across tasks. Dropping **all** handles triggers a graceful
/// shutdown of the worker (equivalent to calling [`shutdown`](Self::shutdown)).
pub struct WorkerHandle<B> {
    inner: Arc<WorkerHandleInner<B>>,
}

impl<B> Clone for WorkerHandle<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<B> WorkerHandle<B> {
    /// Request a graceful shutdown of the worker.
    ///
    /// The worker will finish its current task (if any) and then exit.
    /// This is a non-async, fire-and-forget operation — errors are ignored
    /// (the actor may have already stopped).
    pub fn shutdown(&self) {
        let _ = self.inner.shutdown_tx.try_send(WorkerCommand::Shutdown);
    }

    /// Wait for the worker task to finish.
    ///
    /// The first caller gets the real result; subsequent callers get `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker task panicked or returned an error.
    pub async fn join(&self) -> Result<(), crate::error::RuntimeError> {
        let jh = self.inner.join_handle.lock().await.take();
        match jh {
            Some(jh) => Ok(jh.await??),
            None => Ok(()),
        }
    }

    /// Get a reference to the backend.
    #[must_use]
    pub fn backend(&self) -> &Arc<B> {
        &self.inner.backend
    }
}

impl<B: SignalStore> WorkerHandle<B> {
    /// Request cancellation of a workflow.
    ///
    /// This stores a cancel signal directly in the backend. The worker will
    /// pick it up at the next guard check (task boundary).
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be stored (workflow not found or in terminal state).
    pub async fn cancel_workflow(
        &self,
        instance_id: &str,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> Result<(), crate::error::RuntimeError> {
        self.inner
            .backend
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
    /// This stores a pause signal directly in the backend. The worker will
    /// pick it up at the next guard check (task boundary).
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be stored (workflow not found or in terminal/paused state).
    pub async fn pause_workflow(
        &self,
        instance_id: &str,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> Result<(), crate::error::RuntimeError> {
        self.inner
            .backend
            .store_signal(
                instance_id,
                SignalKind::Pause,
                SignalRequest::new(reason, paused_by),
            )
            .await?;
        Ok(())
    }
}

/// Owns a claimed task and provides explicit release methods.
///
/// No `Drop` impl — callers must explicitly call `release()` or `release_quietly()`.
struct ActiveTaskClaim<'a, B> {
    backend: &'a B,
    instance_id: String,
    task_id: String,
    worker_id: String,
}

impl<B: TaskClaimStore> ActiveTaskClaim<'_, B> {
    /// Release the claim, propagating backend errors.
    async fn release(self) -> Result<(), crate::error::RuntimeError> {
        self.backend
            .release_task_claim(&self.instance_id, &self.task_id, &self.worker_id)
            .await?;
        Ok(())
    }

    /// Release the claim, silently ignoring errors. Use for error/panic paths.
    async fn release_quietly(self) {
        let _ = self.release().await;
    }
}

/// Outcome of running a task through `execute_with_deadline`.
enum ExecutionOutcome {
    /// Task completed successfully.
    Success(Bytes),
    /// Task execution returned an error.
    TaskError(crate::error::RuntimeError),
    /// Task panicked.
    Panic(Box<dyn std::any::Any + Send>),
    /// Heartbeat detected an expired deadline (active cancellation).
    Timeout(crate::error::RuntimeError),
}

/// Extract a human-readable message from a panic payload.
fn extract_panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "Task panicked with unknown payload".to_string()
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
/// ```rust,no_run
/// # use sayiir_runtime::prelude::*;
/// # use std::sync::Arc;
/// # use std::time::Duration;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let backend = InMemoryBackend::new();
/// let registry = TaskRegistry::new();
/// let worker = PooledWorker::new("worker-1", backend, registry);
///
/// let ctx = WorkflowContext::new("my-wf", Arc::new(JsonCodec), Arc::new(()));
/// let workflow = WorkflowBuilder::new(ctx)
///     .then("step1", |i: u32| async move { Ok(i + 1) })
///     .build()?;
/// let workflows = vec![(workflow.definition_hash().to_string(), Arc::new(workflow))];
///
/// // Spawn the worker and get a handle for lifecycle control
/// let handle = worker.spawn(Duration::from_secs(1), workflows);
/// // ... later ...
/// handle.shutdown();
/// handle.join().await?;
/// # Ok(())
/// # }
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
    B: PersistentBackend + TaskClaimStore + 'static,
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
            .store_signal(
                instance_id,
                SignalKind::Pause,
                SignalRequest::new(reason, paused_by),
            )
            .await?;

        Ok(())
    }

    /// Get a reference to the backend.
    #[must_use]
    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }

    /// Spawn the worker as a background task and return a handle.
    ///
    /// Consumes `self`, creates an internal command channel, and spawns the
    /// actor loop on the Tokio runtime. Returns a cloneable [`WorkerHandle`]
    /// for lifecycle control — call [`WorkerHandle::shutdown`] to request
    /// graceful shutdown and [`WorkerHandle::join`] to await completion.
    ///
    /// The worker runs until:
    /// - [`WorkerHandle::shutdown`] is called, or
    /// - All clones of the handle are dropped, or
    /// - A fatal backend error occurs.
    ///
    /// # Parameters
    ///
    /// - `poll_interval`: How often to poll for new tasks
    /// - `workflows`: Map of workflow definition hash to workflow
    #[must_use]
    pub fn spawn<C, Input, M>(
        self,
        poll_interval: Duration,
        workflows: WorkflowRegistry<C, Input, M>,
    ) -> WorkerHandle<B>
    where
        Input: Send + Sync + 'static,
        M: Send + Sync + 'static,
        C: Codec
            + EnvelopeCodec
            + sealed::DecodeValue<Input>
            + sealed::EncodeValue<Input>
            + 'static,
    {
        let (tx, rx) = mpsc::channel(1);
        let backend = Arc::clone(&self.backend);
        let join_handle =
            tokio::spawn(async move { self.run_actor_loop(poll_interval, workflows, rx).await });
        WorkerHandle {
            inner: Arc::new(WorkerHandleInner {
                backend,
                shutdown_tx: tx,
                join_handle: tokio::sync::Mutex::new(Some(join_handle)),
            }),
        }
    }

    /// Spawn the worker with an external executor and return a handle.
    ///
    /// Like [`spawn`](Self::spawn) but instead of executing tasks via typed
    /// `Workflow` closures, delegates all task execution to the provided
    /// `executor`. This is used by language bindings (Python, Node.js) where
    /// task functions live in the host language.
    ///
    /// # Parameters
    ///
    /// - `poll_interval`: How often to poll for new tasks
    /// - `workflows`: Workflow definitions (hash + continuation tree)
    /// - `executor`: Closure that executes a task by ID given input bytes
    #[must_use]
    pub fn spawn_with_executor(
        self,
        poll_interval: Duration,
        workflows: WorkflowIndex,
        executor: ExternalTaskExecutor,
    ) -> WorkerHandle<B> {
        let (tx, rx) = mpsc::channel(1);
        let backend = Arc::clone(&self.backend);
        let join_handle = tokio::spawn(async move {
            self.run_external_actor_loop(poll_interval, workflows, executor, rx)
                .await
        });
        WorkerHandle {
            inner: Arc::new(WorkerHandleInner {
                backend,
                shutdown_tx: tx,
                join_handle: tokio::sync::Mutex::new(Some(join_handle)),
            }),
        }
    }

    /// Actor loop for external executor mode.
    async fn run_external_actor_loop(
        &self,
        poll_interval: Duration,
        workflows: WorkflowIndex,
        executor: ExternalTaskExecutor,
        mut cmd_rx: mpsc::Receiver<WorkerCommand>,
    ) -> Result<(), crate::error::RuntimeError> {
        let mut interval = time::interval(poll_interval);

        loop {
            tokio::select! {
                biased;

                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(WorkerCommand::Shutdown) | None => {
                            tracing::info!(worker_id = %self.worker_id, "Worker shutting down");
                            return Ok(());
                        }
                    }
                }

                _ = interval.tick() => {
                    tracing::trace!(worker_id = %self.worker_id, "Will poll for available tasks");
                }
            }

            let available_tasks = self
                .backend
                .find_available_tasks(&self.worker_id, self.batch_size.get())
                .await?;

            for task in available_tasks {
                if let Ok(WorkerCommand::Shutdown) | Err(mpsc::error::TryRecvError::Disconnected) =
                    cmd_rx.try_recv()
                {
                    tracing::info!(worker_id = %self.worker_id, "Worker shutting down mid-batch");
                    return Ok(());
                }

                if let Some(ext_wf) = workflows.get(&task.workflow_definition_hash) {
                    match self
                        .execute_external_task(
                            &ext_wf.continuation,
                            &task.workflow_definition_hash,
                            &executor,
                            &task,
                        )
                        .await
                    {
                        Err(ref e) if e.is_timeout() => {
                            tracing::error!(
                                worker_id = %self.worker_id,
                                error = %e,
                                "Task timed out — worker shutting down"
                            );
                            return Ok(());
                        }
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

    /// Execute a single task using an external executor.
    async fn execute_external_task(
        &self,
        continuation: &WorkflowContinuation,
        definition_hash: &str,
        executor: &ExternalTaskExecutor,
        available_task: &AvailableTask,
    ) -> Result<WorkflowStatus, crate::error::RuntimeError> {
        let mut snapshot = self
            .backend
            .load_snapshot(&available_task.instance_id)
            .await?;
        let already_completed = Self::validate_task_preconditions(
            definition_hash,
            continuation,
            available_task,
            &snapshot,
        )?;
        if already_completed {
            return Ok(WorkflowStatus::InProgress);
        }

        let Some(claim) = self.claim_task(available_task).await? else {
            return Ok(WorkflowStatus::InProgress);
        };

        if let Some(status) = self.check_post_claim_guards(available_task).await? {
            claim.release_quietly().await;
            return Ok(status);
        }

        tracing::debug!(
            instance_id = %available_task.instance_id,
            task_id = %available_task.task_id,
            "Executing task (external)"
        );

        let execution_result = self
            .execute_with_deadline_ext(
                continuation,
                executor,
                available_task,
                &mut snapshot,
                &claim,
            )
            .await;

        self.settle_execution_result_ext(
            execution_result,
            continuation,
            available_task,
            &mut snapshot,
            claim,
        )
        .await
    }

    /// Run the external executor with an optional deadline.
    async fn execute_with_deadline_ext(
        &self,
        continuation: &WorkflowContinuation,
        executor: &ExternalTaskExecutor,
        available_task: &AvailableTask,
        snapshot: &mut WorkflowSnapshot,
        claim: &ActiveTaskClaim<'_, B>,
    ) -> ExecutionOutcome {
        let task_id = available_task.task_id.clone();
        let input = available_task.input.clone();

        let deadline = if let Some(timeout) = continuation.get_task_timeout(&task_id) {
            snapshot.set_task_deadline(task_id.clone(), timeout);
            let _ = self.backend.save_snapshot(snapshot).await;
            snapshot.refresh_task_deadline();
            snapshot.task_deadline.clone()
        } else {
            None
        };

        let execution_future = executor(&task_id, input);

        let heartbeat_result = self
            .run_with_heartbeat(
                claim,
                deadline.as_ref(),
                AssertUnwindSafe(execution_future).catch_unwind(),
            )
            .await;

        snapshot.clear_task_deadline();

        match heartbeat_result {
            Err(timeout_err) => ExecutionOutcome::Timeout(timeout_err),
            Ok(Err(panic_payload)) => ExecutionOutcome::Panic(panic_payload),
            Ok(Ok(Err(e))) => ExecutionOutcome::TaskError(e.into()),
            Ok(Ok(Ok(output))) => ExecutionOutcome::Success(output),
        }
    }

    /// Settle execution result for external executor mode.
    async fn settle_execution_result_ext(
        &self,
        outcome: ExecutionOutcome,
        continuation: &WorkflowContinuation,
        available_task: &AvailableTask,
        snapshot: &mut WorkflowSnapshot,
        claim: ActiveTaskClaim<'_, B>,
    ) -> Result<WorkflowStatus, crate::error::RuntimeError> {
        match outcome {
            ExecutionOutcome::Timeout(err) => {
                if let Ok(Some(status)) = self
                    .try_schedule_retry(continuation, available_task, snapshot, &err.to_string())
                    .await
                {
                    claim.release_quietly().await;
                    return Ok(status);
                }

                tracing::warn!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    error = %err,
                    "Task timed out via heartbeat — marking workflow failed, shutting down"
                );
                snapshot.mark_failed(err.to_string());
                let _ = self.backend.save_snapshot(snapshot).await;
                claim.release_quietly().await;
                Err(err)
            }
            ExecutionOutcome::Panic(panic_payload) => {
                let panic_msg = extract_panic_message(&panic_payload);

                if let Ok(Some(status)) = self
                    .try_schedule_retry(continuation, available_task, snapshot, &panic_msg)
                    .await
                {
                    claim.release_quietly().await;
                    return Ok(status);
                }

                tracing::error!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    panic = %panic_msg,
                    "Task panicked - releasing claim"
                );
                claim.release_quietly().await;
                Err(WorkflowError::TaskPanicked(panic_msg).into())
            }
            ExecutionOutcome::TaskError(e) => {
                if let Ok(Some(status)) = self
                    .try_schedule_retry(continuation, available_task, snapshot, &e.to_string())
                    .await
                {
                    claim.release_quietly().await;
                    return Ok(status);
                }

                tracing::error!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    error = %e,
                    "Task execution failed"
                );
                claim.release_quietly().await;
                Err(e)
            }
            ExecutionOutcome::Success(output) => {
                snapshot.clear_retry_state(&available_task.task_id);
                self.commit_task_result(
                    continuation,
                    available_task,
                    snapshot,
                    output.clone(),
                    claim,
                )
                .await?;
                self.determine_post_task_status(continuation, available_task, snapshot, output)
                    .await
            }
        }
    }

    /// The actor loop: poll for tasks, execute them, respond to shutdown.
    ///
    async fn run_actor_loop<C, Input, M>(
        &self,
        poll_interval: Duration,
        workflows: WorkflowRegistry<C, Input, M>,
        mut cmd_rx: mpsc::Receiver<WorkerCommand>,
    ) -> Result<(), crate::error::RuntimeError>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec
            + EnvelopeCodec
            + sealed::DecodeValue<Input>
            + sealed::EncodeValue<Input>
            + 'static,
    {
        let mut interval = time::interval(poll_interval);

        loop {
            tokio::select! {
                biased;

                cmd = cmd_rx.recv() => {
                    // None (all handles dropped) or Some(Shutdown) → exit
                    match cmd {
                        Some(WorkerCommand::Shutdown) | None => {
                            tracing::info!(worker_id = %self.worker_id, "Worker shutting down");
                            return Ok(());
                        }
                    }
                }

                _ = interval.tick() => {
                    tracing::trace!(worker_id = %self.worker_id, "Will poll for available tasks");
                }
            }

            let available_tasks = self
                .backend
                .find_available_tasks(&self.worker_id, self.batch_size.get())
                .await?;

            for task in available_tasks {
                if let Ok(WorkerCommand::Shutdown) | Err(mpsc::error::TryRecvError::Disconnected) =
                    cmd_rx.try_recv()
                {
                    tracing::info!(worker_id = %self.worker_id, "Worker shutting down mid-batch");
                    return Ok(());
                }

                if let Some((_, workflow)) = workflows
                    .iter()
                    .find(|(hash, _)| *hash == task.workflow_definition_hash)
                {
                    match self.execute_task(workflow.as_ref(), task).await {
                        Err(ref e) if e.is_timeout() => {
                            tracing::error!(
                                worker_id = %self.worker_id,
                                error = %e,
                                "Task timed out — worker shutting down"
                            );
                            return Ok(());
                        }
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
        C: Codec
            + EnvelopeCodec
            + sealed::DecodeValue<Input>
            + sealed::EncodeValue<Input>
            + 'static,
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

        // 4. Execute with deadline + heartbeat, then settle the result
        let execution_result = self
            .execute_with_deadline(workflow, &available_task, &mut snapshot, &claim)
            .await;

        self.settle_execution_result(
            execution_result,
            workflow,
            &available_task,
            &mut snapshot,
            claim,
        )
        .await
    }

    /// Run the task future with an optional deadline, returning the panic-wrapped result.
    ///
    /// Sets a deadline on the snapshot (if the task has a timeout), persists it,
    /// then runs the future inside `run_with_heartbeat`. On heartbeat-level timeout
    /// the task future is dropped and an `Err` is returned.
    async fn execute_with_deadline<C, Input, M>(
        &self,
        workflow: &Workflow<C, Input, M>,
        available_task: &AvailableTask,
        snapshot: &mut WorkflowSnapshot,
        claim: &ActiveTaskClaim<'_, B>,
    ) -> ExecutionOutcome
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec
            + EnvelopeCodec
            + sealed::DecodeValue<Input>
            + sealed::EncodeValue<Input>
            + 'static,
    {
        let continuation = workflow.continuation();
        let task_id = available_task.task_id.clone();
        let input = available_task.input.clone();

        // Set deadline if task has a timeout configured
        let deadline = if let Some(timeout) = continuation.get_task_timeout(&task_id) {
            snapshot.set_task_deadline(task_id.clone(), timeout);
            let _ = self.backend.save_snapshot(snapshot).await;
            // Refresh deadline to now + timeout so it measures actual execution
            // time, not time spent on the snapshot save above.
            snapshot.refresh_task_deadline();
            snapshot.task_deadline.clone()
        } else {
            None
        };

        let context = workflow.context().clone();
        let execution_future = with_context(context, || async move {
            Self::execute_task_by_id(continuation, &task_id, input).await
        });

        let heartbeat_result = self
            .run_with_heartbeat(
                claim,
                deadline.as_ref(),
                AssertUnwindSafe(execution_future).catch_unwind(),
            )
            .await;

        snapshot.clear_task_deadline();

        match heartbeat_result {
            Err(timeout_err) => ExecutionOutcome::Timeout(timeout_err),
            Ok(Err(panic_payload)) => ExecutionOutcome::Panic(panic_payload),
            Ok(Ok(Err(e))) => ExecutionOutcome::TaskError(e),
            Ok(Ok(Ok(output))) => ExecutionOutcome::Success(output),
        }
    }

    /// Try to schedule a retry for a failed task.
    ///
    /// Looks up the retry policy on the continuation. If retries are available,
    /// records the retry state on the snapshot, clears the deadline, saves the
    /// snapshot, releases the claim, and returns `Ok(Some(InProgress))`.
    /// Otherwise returns `Ok(None)` (caller falls through to existing error handling).
    async fn try_schedule_retry(
        &self,
        continuation: &WorkflowContinuation,
        available_task: &AvailableTask,
        snapshot: &mut WorkflowSnapshot,
        error_msg: &str,
    ) -> Result<Option<WorkflowStatus>, crate::error::RuntimeError> {
        let Some(policy) = continuation.get_task_retry_policy(&available_task.task_id) else {
            return Ok(None);
        };

        if snapshot.retries_exhausted(&available_task.task_id) {
            return Ok(None);
        }

        let next_retry_at = snapshot.record_retry(
            &available_task.task_id,
            policy,
            error_msg,
            Some(&self.worker_id),
        );
        snapshot.clear_task_deadline();
        let _ = self.backend.save_snapshot(snapshot).await;

        tracing::info!(
            instance_id = %available_task.instance_id,
            task_id = %available_task.task_id,
            attempt = snapshot.get_retry_state(&available_task.task_id).map_or(0, |rs| rs.attempts),
            max_retries = policy.max_retries,
            %next_retry_at,
            "Scheduling retry"
        );

        Ok(Some(WorkflowStatus::InProgress))
    }

    /// Settle the outcome of task execution: persist results or errors, release claim.
    async fn settle_execution_result<C, Input, M>(
        &self,
        outcome: ExecutionOutcome,
        workflow: &Workflow<C, Input, M>,
        available_task: &AvailableTask,
        snapshot: &mut WorkflowSnapshot,
        claim: ActiveTaskClaim<'_, B>,
    ) -> Result<WorkflowStatus, crate::error::RuntimeError>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec
            + EnvelopeCodec
            + sealed::DecodeValue<Input>
            + sealed::EncodeValue<Input>
            + 'static,
    {
        match outcome {
            ExecutionOutcome::Timeout(err) => {
                if let Ok(Some(status)) = self
                    .try_schedule_retry(
                        workflow.continuation(),
                        available_task,
                        snapshot,
                        &err.to_string(),
                    )
                    .await
                {
                    claim.release_quietly().await;
                    return Ok(status);
                }

                tracing::warn!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    error = %err,
                    "Task timed out via heartbeat — marking workflow failed, shutting down"
                );
                snapshot.mark_failed(err.to_string());
                let _ = self.backend.save_snapshot(snapshot).await;
                claim.release_quietly().await;
                Err(err)
            }
            ExecutionOutcome::Panic(panic_payload) => {
                let panic_msg = extract_panic_message(&panic_payload);

                if let Ok(Some(status)) = self
                    .try_schedule_retry(
                        workflow.continuation(),
                        available_task,
                        snapshot,
                        &panic_msg,
                    )
                    .await
                {
                    claim.release_quietly().await;
                    return Ok(status);
                }

                tracing::error!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    panic = %panic_msg,
                    "Task panicked - releasing claim"
                );
                claim.release_quietly().await;
                Err(WorkflowError::TaskPanicked(panic_msg).into())
            }
            ExecutionOutcome::TaskError(e) => {
                if let Ok(Some(status)) = self
                    .try_schedule_retry(
                        workflow.continuation(),
                        available_task,
                        snapshot,
                        &e.to_string(),
                    )
                    .await
                {
                    claim.release_quietly().await;
                    return Ok(status);
                }

                tracing::error!(
                    instance_id = %available_task.instance_id,
                    task_id = %available_task.task_id,
                    error = %e,
                    "Task execution failed"
                );
                claim.release_quietly().await;
                Err(e)
            }
            ExecutionOutcome::Success(output) => {
                snapshot.clear_retry_state(&available_task.task_id);
                self.commit_task_result(
                    workflow.continuation(),
                    available_task,
                    snapshot,
                    output.clone(),
                    claim,
                )
                .await?;
                // After saving the body task result, resolve any parent loop
                // nodes so that is_workflow_complete() can detect loop completion.
                Self::resolve_loop_completions(
                    workflow.continuation(),
                    snapshot,
                    self.backend.as_ref(),
                )
                .await?;
                self.determine_post_task_status(
                    workflow.continuation(),
                    available_task,
                    snapshot,
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
    /// If a `deadline` is provided, the heartbeat tick also checks whether the
    /// deadline has expired. If it has, the task future is dropped (active
    /// cancellation) and a `TaskTimedOut` error is returned.
    async fn run_with_heartbeat<F, T>(
        &self,
        claim: &ActiveTaskClaim<'_, B>,
        deadline: Option<&TaskDeadline>,
        future: F,
    ) -> Result<T, crate::error::RuntimeError>
    where
        F: std::future::Future<Output = T>,
    {
        let Some(ttl) = self.claim_ttl else {
            return Ok(future.await);
        };
        let Some(chrono_ttl) = chrono::Duration::from_std(ttl).ok() else {
            return Ok(future.await);
        };

        let interval_duration = ttl / 2;
        let mut heartbeat_timer = time::interval(interval_duration);
        heartbeat_timer.tick().await; // skip first immediate tick

        tokio::pin!(future);

        loop {
            tokio::select! {
                result = &mut future => break Ok(result),
                _ = heartbeat_timer.tick() => {
                    // Check deadline during heartbeat
                    if let Some(dl) = deadline
                        && chrono::Utc::now() >= dl.deadline
                    {
                        tracing::warn!(
                            instance_id = %claim.instance_id,
                            task_id = %dl.task_id,
                            "Task deadline expired during heartbeat, cancelling"
                        );
                        return Err(WorkflowError::TaskTimedOut {
                            task_id: dl.task_id.clone(),
                            timeout: std::time::Duration::from_millis(dl.timeout_ms),
                        }
                        .into());
                    }

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

    /// Find a task function in the workflow continuation and return a reference.
    ///
    /// Note: We can't clone `UntypedCoreTask`, so we need to execute it directly
    /// from the continuation structure. This method returns the task ID if found.
    fn find_task_id_in_continuation(continuation: &WorkflowContinuation, task_id: &str) -> bool {
        match continuation {
            WorkflowContinuation::Task { id, next, .. }
            | WorkflowContinuation::Delay { id, next, .. }
            | WorkflowContinuation::AwaitSignal { id, next, .. } => {
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
            WorkflowContinuation::Branch {
                branches,
                default,
                next,
                ..
            } => {
                for branch_cont in branches.values() {
                    if Self::find_task_id_in_continuation(branch_cont, task_id) {
                        return true;
                    }
                }
                if let Some(def) = default
                    && Self::find_task_id_in_continuation(def, task_id)
                {
                    return true;
                }
                next.as_ref()
                    .is_some_and(|n| Self::find_task_id_in_continuation(n, task_id))
            }
            WorkflowContinuation::Loop { body, next, .. } => {
                if Self::find_task_id_in_continuation(body, task_id) {
                    return true;
                }
                next.as_ref()
                    .is_some_and(|n| Self::find_task_id_in_continuation(n, task_id))
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
                    WorkflowContinuation::Task { id, func, next, .. } => {
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
                    WorkflowContinuation::Delay { next, .. }
                    | WorkflowContinuation::AwaitSignal { next, .. } => {
                        // Skip over delay/signal nodes when searching for a task
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
                    WorkflowContinuation::Branch {
                        branches,
                        default,
                        next,
                        ..
                    } => {
                        // Search branch sub-continuations for the task
                        let mut found = false;
                        for branch_cont in branches.values() {
                            if Self::find_task_id_in_continuation(branch_cont, task_id) {
                                current = branch_cont;
                                found = true;
                                break;
                            }
                        }
                        if found {
                            continue;
                        }
                        if let Some(def) = default
                            && Self::find_task_id_in_continuation(def, task_id)
                        {
                            current = def;
                            continue;
                        }
                        if let Some(next_cont) = next {
                            current = next_cont;
                        } else {
                            return Err(WorkflowError::TaskNotFound(task_id.to_string()).into());
                        }
                    }
                    WorkflowContinuation::Loop { body, next, .. } => {
                        if Self::find_task_id_in_continuation(body, task_id) {
                            current = body;
                            continue;
                        }
                        if let Some(next_cont) = next {
                            current = next_cont;
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
            | WorkflowContinuation::Delay { id, next, .. }
            | WorkflowContinuation::AwaitSignal { id, next, .. } => {
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
            WorkflowContinuation::Branch {
                branches,
                default,
                next,
                ..
            } => {
                for branch_cont in branches.values() {
                    Self::update_position_after_task(branch_cont, completed_task_id, snapshot);
                }
                if let Some(def) = default {
                    Self::update_position_after_task(def, completed_task_id, snapshot);
                }
                if let Some(next_cont) = next {
                    Self::update_position_after_task(next_cont, completed_task_id, snapshot);
                }
            }
            WorkflowContinuation::Loop { body, next, .. } => {
                Self::update_position_after_task(body, completed_task_id, snapshot);
                if let Some(next_cont) = next {
                    Self::update_position_after_task(next_cont, completed_task_id, snapshot);
                }
            }
        }
    }

    /// Create a builder with sensible defaults.
    ///
    /// By default, the worker ID is derived from `{hostname}-{pid}`.
    /// Override with [`PooledWorkerBuilder::worker_id`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use sayiir_runtime::prelude::*;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // Auto-generated worker ID from hostname + PID
    /// let worker = PooledWorker::builder(InMemoryBackend::new(), TaskRegistry::new()).build();
    ///
    /// // Or override with explicit ID
    /// let worker = PooledWorker::builder(InMemoryBackend::new(), TaskRegistry::new())
    ///     .worker_id("custom-worker-1")
    ///     .build();
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(backend: B, registry: TaskRegistry) -> PooledWorkerBuilder<B> {
        PooledWorkerBuilder {
            worker_id: None,
            backend,
            registry,
            claim_ttl: Some(Duration::from_secs(5 * 60)),
            batch_size: NonZeroUsize::MIN,
        }
    }

    /// Walk the continuation tree and resolve any Loop nodes whose body has
    /// completed. When the terminal body task's output decodes to
    /// `LoopDecision::Done`, the loop node is marked as completed in the
    /// snapshot. For `Again`, body task results are cleared and the iteration
    /// counter is advanced so the body becomes available for re-execution.
    async fn resolve_loop_completions(
        continuation: &WorkflowContinuation,
        snapshot: &mut WorkflowSnapshot,
        backend: &B,
    ) -> Result<(), crate::error::RuntimeError> {
        Self::resolve_loops_recursive(continuation, snapshot, backend).await
    }

    fn resolve_loops_recursive<'a>(
        continuation: &'a WorkflowContinuation,
        snapshot: &'a mut WorkflowSnapshot,
        backend: &'a B,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<(), crate::error::RuntimeError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match continuation {
                WorkflowContinuation::Loop {
                    id,
                    body,
                    max_iterations,
                    on_max,
                    next,
                } => {
                    // Only resolve if the loop isn't already marked complete.
                    if snapshot.get_task_result(id).is_none() {
                        let terminal_id = body.terminal_task_id();
                        if let Some(result) = snapshot.get_task_result(terminal_id) {
                            let output = result.output.clone();
                            match crate::execution::decode_loop_envelope(&output) {
                                Ok((LoopDecision::Done, inner)) => {
                                    snapshot.clear_loop_iteration(id);
                                    snapshot.mark_task_completed(id.clone(), inner);
                                    backend.save_snapshot(snapshot).await?;
                                }
                                Ok((LoopDecision::Again, again_value)) => {
                                    let current_iter = snapshot.loop_iteration(id);
                                    let next_iter = current_iter + 1;
                                    if next_iter >= *max_iterations {
                                        match on_max {
                                            sayiir_core::workflow::MaxIterationsPolicy::Fail => {
                                                return Err(WorkflowError::MaxIterationsExceeded {
                                                    loop_id: id.clone(),
                                                    max_iterations: *max_iterations,
                                                }
                                                .into());
                                            }
                                            sayiir_core::workflow::MaxIterationsPolicy::ExitWithLast => {
                                                snapshot.clear_loop_iteration(id);
                                                snapshot.mark_task_completed(
                                                    id.clone(),
                                                    again_value,
                                                );
                                                backend.save_snapshot(snapshot).await?;
                                            }
                                        }
                                    } else {
                                        // Clear body task results so the body becomes
                                        // available for re-execution on the next poll.
                                        let body_ser = body.to_serializable();
                                        for tid in &body_ser.task_ids() {
                                            snapshot.remove_task_result(tid);
                                        }
                                        snapshot.set_loop_iteration(id, next_iter);
                                        backend.save_snapshot(snapshot).await?;
                                    }
                                }
                                Err(e) => {
                                    return Err(CodecError::DecodeFailed {
                                        task_id: id.clone(),
                                        expected_type: "LoopEnvelope",
                                        source: e,
                                    }
                                    .into());
                                }
                            }
                        }
                    }
                    // Recurse into body and next.
                    Self::resolve_loops_recursive(body, snapshot, backend).await?;
                    if let Some(next) = next {
                        Self::resolve_loops_recursive(next, snapshot, backend).await?;
                    }
                }
                WorkflowContinuation::Task { next, .. }
                | WorkflowContinuation::Delay { next, .. }
                | WorkflowContinuation::AwaitSignal { next, .. }
                | WorkflowContinuation::Branch { next, .. } => {
                    if let Some(next) = next {
                        Self::resolve_loops_recursive(next, snapshot, backend).await?;
                    }
                }
                WorkflowContinuation::Fork { branches, join, .. } => {
                    for branch in branches {
                        Self::resolve_loops_recursive(branch, snapshot, backend).await?;
                    }
                    if let Some(join) = join {
                        Self::resolve_loops_recursive(join, snapshot, backend).await?;
                    }
                }
            }
            Ok(())
        })
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
            WorkflowContinuation::Delay { id, next, .. }
            | WorkflowContinuation::AwaitSignal { id, next, .. } => {
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
            WorkflowContinuation::Branch { id, next, .. } => {
                // Branch is complete when the branch node itself has a cached result
                if snapshot.get_task_result(id).is_none() {
                    return false;
                }
                next.as_ref()
                    .is_none_or(|n| Self::is_workflow_complete(n, snapshot))
            }
            WorkflowContinuation::Loop { id, next, .. } => {
                // Loop is complete when the loop node itself has a cached result
                if snapshot.get_task_result(id).is_none() {
                    return false;
                }
                next.as_ref()
                    .is_none_or(|n| Self::is_workflow_complete(n, snapshot))
            }
        }
    }
}

/// Generate a default worker ID from `{hostname}-{pid}`.
fn default_worker_id() -> String {
    let host = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    format!("{host}-{}", std::process::id())
}

/// Builder for [`PooledWorker`] with sensible defaults.
///
/// By default, derives the worker ID from `{hostname}-{pid}`.
/// Override with [`worker_id`](Self::worker_id).
///
/// Created via [`PooledWorker::builder`].
pub struct PooledWorkerBuilder<B> {
    worker_id: Option<String>,
    backend: B,
    registry: TaskRegistry,
    claim_ttl: Option<Duration>,
    batch_size: NonZeroUsize,
}

impl<B> PooledWorkerBuilder<B>
where
    B: PersistentBackend + TaskClaimStore + 'static,
{
    /// Set an explicit worker ID.
    ///
    /// If not called, the ID is auto-generated from `{hostname}-{pid}`.
    #[must_use]
    pub fn worker_id(mut self, id: impl Into<String>) -> Self {
        self.worker_id = Some(id.into());
        self
    }

    /// Set the TTL for task claims (default: 5 minutes).
    #[must_use]
    pub fn claim_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.claim_ttl = ttl;
        self
    }

    /// Set the number of tasks to fetch per poll (default: 1).
    #[must_use]
    pub fn batch_size(mut self, size: NonZeroUsize) -> Self {
        self.batch_size = size;
        self
    }

    /// Build the [`PooledWorker`].
    ///
    /// If no `worker_id` was set, generates one from `{hostname}-{pid}`.
    #[must_use]
    pub fn build(self) -> PooledWorker<B> {
        let worker_id = self.worker_id.unwrap_or_else(default_worker_id);
        PooledWorker {
            worker_id,
            backend: Arc::new(self.backend),
            registry: Arc::new(self.registry),
            claim_ttl: self.claim_ttl,
            batch_size: self.batch_size,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::serialization::JsonCodec;
    use sayiir_core::registry::TaskRegistry;
    use sayiir_core::snapshot::WorkflowSnapshot;
    use sayiir_persistence::{InMemoryBackend, SignalStore, SnapshotStore};

    type EmptyWorkflows = WorkflowRegistry<JsonCodec, (), ()>;

    fn make_worker() -> PooledWorker<InMemoryBackend> {
        let backend = InMemoryBackend::new();
        let registry = TaskRegistry::new();
        PooledWorker::new("test-worker", backend, registry)
    }

    #[tokio::test]
    async fn test_spawn_and_shutdown() {
        let worker = make_worker();
        let handle = worker.spawn(Duration::from_millis(50), EmptyWorkflows::new());

        handle.shutdown();

        let result = tokio::time::timeout(Duration::from_secs(5), handle.join()).await;
        assert!(result.is_ok(), "Worker should exit cleanly after shutdown");
        assert!(result.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_handle_is_clone_and_send() {
        let worker = make_worker();
        let handle = worker.spawn(Duration::from_millis(50), EmptyWorkflows::new());

        let handle2 = handle.clone();
        let remote = tokio::spawn(async move {
            handle2.shutdown();
        });
        remote.await.ok();

        let result = tokio::time::timeout(Duration::from_secs(5), handle.join()).await;
        assert!(result.is_ok_and(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn test_cancel_via_handle() {
        let backend = InMemoryBackend::new();
        let registry = TaskRegistry::new();

        // Create a workflow snapshot so store_signal can validate it
        let snapshot = WorkflowSnapshot::new("wf-1".to_string(), "hash-1".to_string());
        backend.save_snapshot(&snapshot).await.ok();

        let worker = PooledWorker::new("test-worker", backend, registry);
        let handle = worker.spawn(Duration::from_millis(50), EmptyWorkflows::new());

        handle
            .cancel_workflow(
                "wf-1",
                Some("test reason".to_string()),
                Some("tester".to_string()),
            )
            .await
            .ok();

        // Verify the signal was stored
        let signal = handle
            .backend()
            .get_signal("wf-1", SignalKind::Cancel)
            .await;
        assert!(signal.is_ok_and(|s| s.is_some()));

        handle.shutdown();
        tokio::time::timeout(Duration::from_secs(5), handle.join())
            .await
            .ok();
    }

    #[test]
    fn test_builder_auto_generates_worker_id() {
        let backend = InMemoryBackend::new();
        let registry = TaskRegistry::new();
        let worker = PooledWorker::builder(backend, registry).build();

        // Should contain PID
        let pid = std::process::id().to_string();
        assert!(
            worker.worker_id.contains(&pid),
            "Auto-generated ID '{}' should contain PID '{}'",
            worker.worker_id,
            pid
        );
    }

    #[test]
    fn test_builder_explicit_worker_id() {
        let backend = InMemoryBackend::new();
        let registry = TaskRegistry::new();
        let worker = PooledWorker::builder(backend, registry)
            .worker_id("my-worker")
            .build();

        assert_eq!(worker.worker_id, "my-worker");
    }

    #[test]
    fn test_builder_custom_settings() {
        let backend = InMemoryBackend::new();
        let registry = TaskRegistry::new();
        let worker = PooledWorker::builder(backend, registry)
            .worker_id("w1")
            .claim_ttl(Some(Duration::from_secs(120)))
            .batch_size(NonZeroUsize::new(8).unwrap())
            .build();

        assert_eq!(worker.worker_id, "w1");
        assert_eq!(worker.claim_ttl, Some(Duration::from_secs(120)));
        assert_eq!(worker.batch_size.get(), 8);
    }

    #[tokio::test]
    async fn test_dropped_handle_shuts_down_worker() {
        let worker = make_worker();
        let handle = worker.spawn(Duration::from_millis(50), EmptyWorkflows::new());

        // Extract the join handle before dropping so we can still await completion
        let join_handle = handle.inner.join_handle.lock().await.take().unwrap();
        drop(handle);

        let result = tokio::time::timeout(Duration::from_secs(5), join_handle)
            .await
            .ok()
            .and_then(Result::ok);
        assert!(
            result.is_some(),
            "Worker should exit when all handles are dropped"
        );
        assert!(result.is_some_and(|r| r.is_ok()));
    }
}
