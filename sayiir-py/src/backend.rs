//! Python-exposed persistence backends.

use pyo3::prelude::*;
use sayiir_persistence::InMemoryBackend;
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;
use std::sync::Arc;

/// Python-exposed in-memory persistence backend.
///
/// Stores workflow snapshots in memory. Suitable for testing and development.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct PyInMemoryBackend {
    pub(crate) inner: Arc<InMemoryBackend>,
}

#[pymethods]
impl PyInMemoryBackend {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryBackend::new()),
        }
    }

    fn __repr__(&self) -> String {
        "InMemoryBackend()".to_string()
    }
}

/// Python-exposed `PostgreSQL` persistence backend.
///
/// Connects to a `PostgreSQL` database and stores workflow snapshots durably.
/// Runs migrations automatically on first connect.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct PyPostgresBackend {
    pub(crate) inner: Arc<PostgresBackend<JsonCodec>>,
    /// Kept alive for connection pool background tasks.
    #[allow(dead_code)]
    pub(crate) runtime: Arc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyPostgresBackend {
    /// Create a new `PostgreSQL` backend.
    ///
    /// Args:
    ///     url: Connection URL (e.g. `postgresql://localhost/sayiir`)
    #[new]
    fn new(url: &str) -> PyResult<Self> {
        tracing::info!("connecting to PostgreSQL backend");
        tracing::debug!(url, "PostgreSQL connection URL");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

        let backend = runtime
            .block_on(PostgresBackend::<JsonCodec>::connect(url))
            .map_err(|e: sayiir_persistence::BackendError| {
                tracing::error!(error = %e, "failed to connect to PostgreSQL");
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string())
            })?;

        tracing::info!("PostgreSQL backend connected");

        Ok(Self {
            inner: Arc::new(backend),
            runtime: Arc::new(runtime),
        })
    }

    fn __repr__(&self) -> String {
        "PostgresBackend(...)".to_string()
    }
}

/// Internal enum dispatching to either backend kind.
///
/// Since `PersistentBackend` uses RPITIT (not object-safe), we use enum
/// dispatch instead of `dyn`.
pub(crate) enum BackendKind {
    InMemory(Arc<InMemoryBackend>),
    Postgres(Arc<PostgresBackend<JsonCodec>>),
}

/// Dispatch a call to the backend regardless of variant.
///
/// The `$body` expression uses `$backend` which is monomorphised for each
/// concrete type — required because the runtime functions take
/// `&impl PersistentBackend`.
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

use sayiir_core::snapshot::{SignalKind, SignalRequest, WorkflowSnapshot};
use sayiir_core::task_claim::{AvailableTask, TaskClaim};
use sayiir_persistence::{BackendError, SignalStore, SnapshotStore, TaskClaimStore};

/// Dispatch macro for trait methods on `BackendKind`.
macro_rules! dispatch {
    ($self:expr, |$inner:ident| $body:expr) => {
        match $self {
            BackendKind::InMemory($inner) => $body,
            BackendKind::Postgres($inner) => $body,
        }
    };
}

impl SnapshotStore for BackendKind {
    async fn save_snapshot(&self, snapshot: &WorkflowSnapshot) -> Result<(), BackendError> {
        dispatch!(self, |b| b.save_snapshot(snapshot).await)
    }

    async fn save_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
        output: bytes::Bytes,
    ) -> Result<(), BackendError> {
        dispatch!(self, |b| b
            .save_task_result(instance_id, task_id, output)
            .await)
    }

    async fn load_snapshot(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        dispatch!(self, |b| b.load_snapshot(instance_id).await)
    }

    async fn delete_snapshot(&self, instance_id: &str) -> Result<(), BackendError> {
        dispatch!(self, |b| b.delete_snapshot(instance_id).await)
    }

    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError> {
        dispatch!(self, |b| b.list_snapshots().await)
    }
}

impl SignalStore for BackendKind {
    async fn store_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
        request: SignalRequest,
    ) -> Result<(), BackendError> {
        dispatch!(self, |b| b.store_signal(instance_id, kind, request).await)
    }

    async fn get_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> Result<Option<SignalRequest>, BackendError> {
        dispatch!(self, |b| b.get_signal(instance_id, kind).await)
    }

    async fn clear_signal(&self, instance_id: &str, kind: SignalKind) -> Result<(), BackendError> {
        dispatch!(self, |b| b.clear_signal(instance_id, kind).await)
    }

    async fn send_event(
        &self,
        instance_id: &str,
        signal_name: &str,
        payload: bytes::Bytes,
    ) -> Result<(), BackendError> {
        dispatch!(self, |b| b
            .send_event(instance_id, signal_name, payload)
            .await)
    }

    async fn consume_event(
        &self,
        instance_id: &str,
        signal_name: &str,
    ) -> Result<Option<bytes::Bytes>, BackendError> {
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
    ) -> Result<Option<TaskClaim>, BackendError> {
        dispatch!(self, |b| b
            .claim_task(instance_id, task_id, worker_id, ttl)
            .await)
    }

    async fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> Result<(), BackendError> {
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
    ) -> Result<(), BackendError> {
        dispatch!(self, |b| b
            .extend_task_claim(instance_id, task_id, worker_id, additional_duration)
            .await)
    }

    async fn find_available_tasks(
        &self,
        worker_id: &str,
        limit: usize,
    ) -> Result<Vec<AvailableTask>, BackendError> {
        dispatch!(self, |b| b.find_available_tasks(worker_id, limit).await)
    }
}
