//! `D1Backend` struct, inline JSON codec, and constructors.

use bytes::Bytes;
use sayiir_core::codec::{self, Decoder, Encoder};
use sayiir_core::snapshot::WorkflowSnapshot;
use sayiir_persistence::BackendError;
use sqlx::{Database, Executor, IntoArguments};

use crate::schema::MIGRATION_SQL;

// ---------------------------------------------------------------------------
// Inline JsonCodec (avoids depending on sayiir-runtime which pulls in tokio)
// ---------------------------------------------------------------------------

/// Minimal JSON codec for snapshot serialization.
#[derive(Debug, Clone, Default)]
pub struct JsonCodec;

impl Encoder for JsonCodec {}
impl Decoder for JsonCodec {}

impl codec::sealed::EncodeValue<WorkflowSnapshot> for JsonCodec {
    fn encode_value(
        &self,
        value: &WorkflowSnapshot,
    ) -> Result<Bytes, Box<dyn std::error::Error + Send + Sync>> {
        serde_json::to_vec(value)
            .map(Bytes::from)
            .map_err(Into::into)
    }
}

impl codec::sealed::DecodeValue<WorkflowSnapshot> for JsonCodec {
    fn decode_value(
        &self,
        bytes: Bytes,
    ) -> Result<WorkflowSnapshot, Box<dyn std::error::Error + Send + Sync>> {
        serde_json::from_slice(&bytes).map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// SQLiteBackend
// ---------------------------------------------------------------------------

#[cfg(all(feature = "sqlite", not(feature = "d1")))]
pub type BackendDB = sqlx::Sqlite;
#[cfg(feature = "d1")]
pub type BackendDB = sqlx_d1::D1;

/// Persistence backend for Sayiir workflows using `sqlx-sqlite` or `sqlx-d1`.
#[derive(Clone)]
pub struct SQLiteBackend<T> {
    pub(crate) connection: T,
}

impl<T, DB: Database> SQLiteBackend<T>
where
    for<'c> &'c T: Executor<'c, Database = DB>,
    for<'a> DB::Arguments<'a>: IntoArguments<'a, DB>,
    T: Clone,
{
    /// Create a new `SQLiteBackend` and run schema migrations.
    ///
    /// # Errors
    ///
    /// Returns a `BackendError` if the migration fails.
    pub async fn new(connection: T) -> Result<Self, BackendError> {
        let backend = Self { connection };
        backend.run_migrations().await?;
        Ok(backend)
    }

    /// Run the schema migrations on the database.
    ///
    /// # Errors
    ///
    /// Returns a `BackendError` if the migration fails.
    pub async fn run_migrations(&self) -> Result<(), BackendError> {
        let conn = self.exec();
        sqlx::query(MIGRATION_SQL)
            .execute(&conn)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;
        Ok(())
    }
}

impl<T> SQLiteBackend<T>
where
    T: Clone,
{
    pub(crate) fn exec(&self) -> T {
        self.connection.clone()
    }
}

impl<T> SQLiteBackend<T> {
    /// Encode a snapshot to JSON bytes.
    #[allow(clippy::unused_self)]
    pub(crate) fn encode(&self, snapshot: &WorkflowSnapshot) -> Result<Vec<u8>, BackendError> {
        let codec = JsonCodec;
        codec
            .encode(snapshot)
            .map(|b| b.to_vec())
            .map_err(|e| BackendError::Serialization(e.to_string()))
    }

    /// Decode a snapshot from JSON bytes.
    #[allow(clippy::unused_self)]
    pub(crate) fn decode(&self, data: &[u8]) -> Result<WorkflowSnapshot, BackendError> {
        let codec = JsonCodec;
        codec
            .decode(Bytes::copy_from_slice(data))
            .map_err(|e| BackendError::Serialization(e.to_string()))
    }
}

#[cfg(feature = "sqlite")]
impl SQLiteBackend<sqlx::SqlitePool> {
    /// Create a new `SQLiteBackend` from a database URL.
    ///
    /// # Errors
    ///
    /// Returns a `BackendError` if the connection or migration fails.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        let pool = sqlx::SqlitePool::connect(url)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;
        Self::new(pool).await
    }
}

#[cfg(feature = "d1")]
impl SQLiteBackend<sqlx_d1::D1Connection> {
    /// Create a new `SQLiteBackend` from a `worker::D1Database`.
    ///
    /// # Errors
    ///
    /// Returns a `BackendError` if the migration fails.
    pub async fn connect(d1: worker::D1Database) -> Result<Self, BackendError> {
        let connection = sqlx_d1::D1Connection::new(d1);
        Self::new(connection).await
    }
}
