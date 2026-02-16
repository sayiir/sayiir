//! Node.js-exposed persistence backends.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use sayiir_persistence::InMemoryBackend;
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;
use std::sync::Arc;

/// In-memory persistence backend.
///
/// Stores workflow snapshots in memory. Suitable for testing and development.
#[napi]
#[derive(Clone)]
pub struct NapiInMemoryBackend {
    pub(crate) inner: Arc<InMemoryBackend>,
}

#[napi]
impl NapiInMemoryBackend {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryBackend::new()),
        }
    }
}

/// `PostgreSQL` persistence backend.
///
/// Connects to a `PostgreSQL` database and stores workflow snapshots durably.
#[napi]
#[derive(Clone)]
pub struct NapiPostgresBackend {
    pub(crate) inner: Arc<PostgresBackend<JsonCodec>>,
    #[allow(dead_code)]
    pub(crate) runtime: Arc<tokio::runtime::Runtime>,
}

#[napi]
impl NapiPostgresBackend {
    /// Connect to a `PostgreSQL` database.
    #[napi(factory)]
    pub fn connect(url: String) -> Result<Self> {
        tracing::info!("connecting to PostgreSQL backend");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

        let backend = runtime
            .block_on(PostgresBackend::<JsonCodec>::connect(&url))
            .map_err(|e| {
                tracing::error!(error = %e, "failed to connect to PostgreSQL");
                Error::new(Status::GenericFailure, e.to_string())
            })?;

        tracing::info!("PostgreSQL backend connected");

        Ok(Self {
            inner: Arc::new(backend),
            runtime: Arc::new(runtime),
        })
    }
}

/// Internal enum dispatching to either backend kind.
pub(crate) enum BackendKind {
    InMemory(Arc<InMemoryBackend>),
    Postgres(Arc<PostgresBackend<JsonCodec>>),
}

/// Dispatch a call to the backend regardless of variant.
macro_rules! with_backend {
    ($self:expr, |$backend:ident| $body:expr) => {
        match &$self.backend {
            BackendKind::InMemory($backend) => $body,
            BackendKind::Postgres($backend) => $body,
        }
    };
}
pub(crate) use with_backend;

// ---------------------------------------------------------------------------
// Trait implementations for BackendKind — needed for PooledWorker
// ---------------------------------------------------------------------------

// Trait implementations use `std::result::Result` explicitly because napi's
// prelude shadows `Result` with `napi::Result`.
use sayiir_core::snapshot::{SignalKind, SignalRequest, WorkflowSnapshot};
use sayiir_core::task_claim::{AvailableTask, TaskClaim};
use sayiir_persistence::{BackendError, SignalStore, SnapshotStore, TaskClaimStore};

macro_rules! dispatch {
    ($self:expr, |$inner:ident| $body:expr) => {
        match $self {
            BackendKind::InMemory($inner) => $body,
            BackendKind::Postgres($inner) => $body,
        }
    };
}

type BResult<T> = std::result::Result<T, BackendError>;

impl SnapshotStore for BackendKind {
    async fn save_snapshot(&self, snapshot: &WorkflowSnapshot) -> BResult<()> {
        dispatch!(self, |b| b.save_snapshot(snapshot).await)
    }

    async fn save_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
        output: bytes::Bytes,
    ) -> BResult<()> {
        dispatch!(self, |b| b
            .save_task_result(instance_id, task_id, output)
            .await)
    }

    async fn load_snapshot(&self, instance_id: &str) -> BResult<WorkflowSnapshot> {
        dispatch!(self, |b| b.load_snapshot(instance_id).await)
    }

    async fn delete_snapshot(&self, instance_id: &str) -> BResult<()> {
        dispatch!(self, |b| b.delete_snapshot(instance_id).await)
    }

    async fn list_snapshots(&self) -> BResult<Vec<String>> {
        dispatch!(self, |b| b.list_snapshots().await)
    }
}

impl SignalStore for BackendKind {
    async fn store_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
        request: SignalRequest,
    ) -> BResult<()> {
        dispatch!(self, |b| b.store_signal(instance_id, kind, request).await)
    }

    async fn get_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> BResult<Option<SignalRequest>> {
        dispatch!(self, |b| b.get_signal(instance_id, kind).await)
    }

    async fn clear_signal(&self, instance_id: &str, kind: SignalKind) -> BResult<()> {
        dispatch!(self, |b| b.clear_signal(instance_id, kind).await)
    }

    async fn send_event(
        &self,
        instance_id: &str,
        signal_name: &str,
        payload: bytes::Bytes,
    ) -> BResult<()> {
        dispatch!(self, |b| b
            .send_event(instance_id, signal_name, payload)
            .await)
    }

    async fn consume_event(
        &self,
        instance_id: &str,
        signal_name: &str,
    ) -> BResult<Option<bytes::Bytes>> {
        dispatch!(self, |b| b.consume_event(instance_id, signal_name).await)
    }
}

impl TaskClaimStore for BackendKind {
    async fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        ttl: Option<chrono::Duration>,
    ) -> BResult<Option<TaskClaim>> {
        dispatch!(self, |b| b
            .claim_task(instance_id, task_id, worker_id, ttl)
            .await)
    }

    async fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> BResult<()> {
        dispatch!(self, |b| b
            .release_task_claim(instance_id, task_id, worker_id)
            .await)
    }

    async fn extend_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        additional_duration: chrono::Duration,
    ) -> BResult<()> {
        dispatch!(self, |b| b
            .extend_task_claim(instance_id, task_id, worker_id, additional_duration)
            .await)
    }

    async fn find_available_tasks(
        &self,
        worker_id: &str,
        limit: usize,
    ) -> BResult<Vec<AvailableTask>> {
        dispatch!(self, |b| b.find_available_tasks(worker_id, limit).await)
    }
}
