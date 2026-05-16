//! Centralised workflow lifecycle client.
//!
//! [`WorkflowClient`] provides a single entry-point for submitting workflows
//! with idempotency (via [`ConflictPolicy`]) and for lifecycle operations
//! (cancel, pause, unpause, send event, status) without requiring a runner
//! or worker.
//!
//! This is the recommended API for the distributed model where a
//! [`PooledWorker`](crate::worker::PooledWorker) executes tasks but a
//! separate process or service needs to submit workflows and control them.

use std::sync::Arc;

use bytes::Bytes;
use sayiir_core::codec::sealed;
use sayiir_core::codec::{Codec, EnvelopeCodec};
use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_core::task::TaskIdentifier;
use sayiir_core::workflow::{ConflictPolicy, Workflow, WorkflowStatus};
use sayiir_persistence::{SignalStore, SnapshotStore, TaskResultStore};

use crate::error::RuntimeError;
use crate::{PrepareRunOutcome, check_existing_instance, prepare_run};

/// A client for submitting and controlling workflow instances.
///
/// Unlike [`CheckpointingRunner`](crate::CheckpointingRunner), the client does
/// **not** execute tasks — it only creates initial snapshots and stores
/// lifecycle signals. A [`PooledWorker`](crate::worker::PooledWorker) (or
/// `CheckpointingRunner::resume`) picks up and executes the work.
///
/// # Example
///
/// ```rust,no_run
/// use sayiir_runtime::WorkflowClient;
/// use sayiir_runtime::persistence::InMemoryBackend;
/// use sayiir_core::workflow::ConflictPolicy;
///
/// let backend = InMemoryBackend::new();
/// let client = WorkflowClient::new(backend)
///     .with_conflict_policy(ConflictPolicy::UseExisting);
/// ```
pub struct WorkflowClient<B> {
    backend: Arc<B>,
    conflict_policy: ConflictPolicy,
}

impl<B> WorkflowClient<B> {
    /// Create a new client wrapping the given backend.
    ///
    /// The default conflict policy is [`ConflictPolicy::Fail`].
    pub fn new(backend: B) -> Self {
        Self {
            backend: Arc::new(backend),
            conflict_policy: ConflictPolicy::default(),
        }
    }

    /// Create a client from a shared backend reference.
    ///
    /// Useful when the same backend is shared with a runner or worker.
    pub fn from_shared(backend: Arc<B>) -> Self {
        Self {
            backend,
            conflict_policy: ConflictPolicy::default(),
        }
    }

    /// Set the conflict policy for duplicate instance IDs.
    #[must_use]
    pub fn with_conflict_policy(mut self, policy: ConflictPolicy) -> Self {
        self.conflict_policy = policy;
        self
    }

    /// Get a reference to the backend.
    #[must_use]
    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }
}

impl<B> WorkflowClient<B>
where
    B: SnapshotStore + SignalStore,
{
    /// Submit a workflow for execution.
    ///
    /// Creates an initial snapshot in the backend so that a
    /// [`PooledWorker`](crate::worker::PooledWorker) can pick it up.
    /// Does **not** execute any tasks.
    ///
    /// Returns `(WorkflowStatus, Option<Bytes>)`:
    /// - `(InProgress, None)` when a fresh snapshot was created.
    /// - `(status, output)` when the conflict policy returns an existing instance.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InstanceAlreadyExists`] when the policy is `Fail`
    /// and the instance already exists, or propagates backend I/O errors.
    pub async fn submit<C, Input, M>(
        &self,
        workflow: &Workflow<C, Input, M>,
        instance_id: impl Into<String>,
        input: Input,
    ) -> Result<(WorkflowStatus, Option<Bytes>), RuntimeError>
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + EnvelopeCodec + sealed::EncodeValue<Input> + 'static,
    {
        let instance_id = instance_id.into();
        let definition_hash = workflow.definition_hash().to_string();
        let conflict_policy = self.conflict_policy;

        // Phase 1: check for existing instance before encoding input.
        if let Some(early) = check_existing_instance(
            &instance_id,
            &definition_hash,
            self.backend.as_ref(),
            conflict_policy,
        )
        .await?
        {
            return Ok(early);
        }

        // Phase 2: encode input and create snapshot.
        let input_bytes = workflow.context().codec.encode(&input)?;
        let first_task = workflow.continuation().first_task_hint();

        match prepare_run(
            instance_id,
            definition_hash,
            input_bytes,
            first_task,
            self.backend.as_ref(),
            conflict_policy,
            true, // prechecked — check_existing_instance already ran
        )
        .await?
        {
            PrepareRunOutcome::Fresh(_) => Ok((WorkflowStatus::InProgress, None)),
            PrepareRunOutcome::ExistingStatus(status, output) => Ok((status, output)),
        }
    }

    /// Request cancellation of a workflow instance.
    ///
    /// Stores a cancel signal in the backend. The worker picks it up
    /// at the next task boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be stored.
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

    /// Request pausing of a workflow instance.
    ///
    /// Stores a pause signal in the backend. The worker picks it up
    /// at the next task boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be stored.
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

    /// Unpause a paused workflow instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the workflow is not found or not paused.
    pub async fn unpause(&self, instance_id: &str) -> Result<(), RuntimeError> {
        self.backend.unpause(instance_id).await?;
        Ok(())
    }

    /// Send an external event (signal) to a workflow instance.
    ///
    /// The payload is buffered in FIFO order per (`instance_id`, `signal_name`).
    ///
    /// # Errors
    ///
    /// Returns an error if the event cannot be stored.
    pub async fn send_event(
        &self,
        instance_id: &str,
        signal_name: &str,
        payload: Bytes,
    ) -> Result<(), RuntimeError> {
        self.backend
            .send_event(instance_id, signal_name, payload)
            .await?;
        Ok(())
    }

    /// Get the current status of a workflow instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot be loaded.
    pub async fn status(&self, instance_id: &str) -> Result<WorkflowStatus, RuntimeError> {
        let snapshot = self.backend.load_snapshot(instance_id).await?;
        Ok(snapshot.state.as_status())
    }
}

impl<B> WorkflowClient<B>
where
    B: SnapshotStore + SignalStore + TaskResultStore,
{
    /// Get a single task result from a workflow instance.
    ///
    /// Returns `Ok(Some(bytes))` if the task has completed, `Ok(None)` if the
    /// task was never executed. For completed/failed workflows, the result is
    /// recovered from the backend's history or cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot be loaded.
    pub async fn get_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
    ) -> Result<Option<Bytes>, RuntimeError> {
        Ok(self.backend.load_task_result(instance_id, task_id).await?)
    }

    /// Type-safe variant of [`get_task_result`](Self::get_task_result) that
    /// derives the `task_id` from a [`TaskIdentifier`] implementor (e.g. a
    /// `#[task]`-generated struct).
    ///
    /// ```rust,ignore
    /// let result = client.get_task_result_of::<ValidateOrderTask>("order-42").await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot be loaded.
    pub async fn get_task_result_of<T: TaskIdentifier>(
        &self,
        instance_id: &str,
    ) -> Result<Option<Bytes>, RuntimeError> {
        self.get_task_result(instance_id, T::task_id()).await
    }
}
