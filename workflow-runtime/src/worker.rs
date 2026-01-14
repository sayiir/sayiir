//! Worker node for distributed workflow execution.
//!
//! A worker node polls the backend for available tasks, claims them,
//! executes them, and updates the snapshot. Multiple workers can
//! collaborate to execute the same workflow instance.

use bytes::Bytes;
use chrono::Duration as ChronoDuration;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use workflow_core::codec::Codec;
use workflow_core::codec::sealed;
use workflow_core::context::with_context;
use workflow_core::registry::TaskRegistry;
use workflow_core::task_claim::AvailableTask;
use workflow_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};
use workflow_persistence::backend::PersistentBackend;
use workflow_persistence::snapshot::{ExecutionPosition, WorkflowSnapshot};

/// A worker node that executes tasks from workflows.
///
/// Workers poll the backend for available tasks, claim them, execute them,
/// and update the workflow snapshot. Multiple workers can collaborate
/// to execute the same workflow instance.
///
/// # Example
///
/// ```rust,ignore
/// use workflow_runtime::persistence::{WorkerNode, InMemoryBackend};
/// use workflow_core::registry::TaskRegistry;
///
/// let backend = InMemoryBackend::new();
/// let registry = TaskRegistry::new(); // Must contain all task implementations
/// let worker = WorkerNode::new("worker-1", backend, registry);
///
/// // Start polling for work
/// worker.start_polling(Duration::from_secs(1)).await?;
/// ```
pub struct WorkerNode<B> {
    worker_id: String,
    backend: Arc<B>,
    registry: Arc<TaskRegistry>,
    claim_ttl: Option<ChronoDuration>,
}

impl<B> WorkerNode<B>
where
    B: PersistentBackend,
{
    /// Create a new worker node.
    ///
    /// # Parameters
    ///
    /// - `worker_id`: Unique identifier for this worker node
    /// - `backend`: The persistent backend to use
    /// - `registry`: Task registry containing all task implementations
    /// - `claim_ttl`: Optional TTL for task claims (default: 5 minutes)
    pub fn new(worker_id: impl Into<String>, backend: B, registry: TaskRegistry) -> Self {
        Self {
            worker_id: worker_id.into(),
            backend: Arc::new(backend),
            registry: Arc::new(registry),
            claim_ttl: Some(ChronoDuration::minutes(5)), // Default 5 minutes
        }
    }

    /// Set the TTL for task claims.
    #[must_use]
    pub fn with_claim_ttl(mut self, ttl: Option<ChronoDuration>) -> Self {
        self.claim_ttl = ttl;
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
        // Validate definition hash
        if available_task.workflow_definition_hash != workflow.definition_hash() {
            return Err(anyhow::anyhow!(
                "Workflow definition hash mismatch: expected {}, found {}",
                workflow.definition_hash(),
                available_task.workflow_definition_hash
            ));
        }

        // Claim the task
        let claim = self
            .backend
            .claim_task(
                &available_task.instance_id,
                &available_task.task_id,
                &self.worker_id,
                self.claim_ttl,
            )
            .await?;

        match claim {
            Some(_) => {
                // Claim successful, continue
            }
            None => {
                // Task was already claimed by another worker
                return Ok(WorkflowStatus::InProgress);
            }
        }

        // Load snapshot
        let mut snapshot = self
            .backend
            .load_snapshot(&available_task.instance_id)
            .await?;

        // Check if task is already completed
        if snapshot.get_task_result(&available_task.task_id).is_some() {
            // Task already completed, release claim and continue
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

        // Verify task exists in workflow
        if !Self::find_task_id_in_continuation(workflow.continuation(), &available_task.task_id) {
            // Release claim
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

        // Execute the task
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

        match result {
            Ok(output) => {
                // Mark task as completed
                snapshot.mark_task_completed(available_task.task_id.clone(), output.clone());

                // Update position to next task
                Self::update_position_after_task(
                    workflow.continuation(),
                    &available_task.task_id,
                    &mut snapshot,
                );

                // Save snapshot
                self.backend.save_snapshot(snapshot.clone()).await?;

                // Release claim
                self.backend
                    .release_task_claim(
                        &available_task.instance_id,
                        &available_task.task_id,
                        &self.worker_id,
                    )
                    .await?;

                // Check if workflow is complete
                if Self::is_workflow_complete(workflow.continuation(), &snapshot) {
                    snapshot.mark_completed(output);
                    self.backend.save_snapshot(snapshot).await?;
                    Ok(WorkflowStatus::Completed)
                } else {
                    Ok(WorkflowStatus::InProgress) // Task completed, workflow continues
                }
            }
            Err(e) => {
                // Release claim on error
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
    /// Returns when an error occurs or the future is cancelled.
    ///
    /// # Parameters
    ///
    /// - `poll_interval`: How often to poll for new tasks
    /// - `workflows`: Map of workflow definition hash to workflow (for task execution)
    pub async fn start_polling<C, Input, M>(
        &self,
        poll_interval: Duration,
        workflows: Vec<(String, Arc<Workflow<C, Input, M>>)>,
    ) -> anyhow::Result<()>
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
                .find_available_tasks(&self.worker_id, 10)
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
                    if let WorkflowContinuation::Task { id, .. } = branch.as_ref() {
                        if snapshot.get_task_result(id).is_none() {
                            return false;
                        }
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
