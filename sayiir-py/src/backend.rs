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
