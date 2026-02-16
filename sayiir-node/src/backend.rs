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
