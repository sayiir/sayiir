//! `D1Backend` struct, inline JSON codec, and constructors.

use bytes::Bytes;
use sqlx::Pool;
use sayiir_core::codec::{self, Decoder, Encoder};
use sayiir_core::snapshot::WorkflowSnapshot;
use sayiir_persistence::BackendError;

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

#[cfg(feature = "sqlite")]
pub type BackendDB = sqlx::Sqlite;
#[cfg(feature = "d1")]
pub type BackendDB = sqlx_d1::D1;

/// Persistence backend for Sayiir workflows using `sqlx-sqlite` or `sqlx-d1`.
#[derive(Clone)]
pub struct SQLiteBackend {
    pub(crate) pool: Pool<BackendDB>,
}

impl SQLiteBackend {
    /// Create a new `SQLiteBackend` and run schema migrations.
    ///
    /// # Errors
    ///
    /// Returns a `BackendError` if the migration fails.
    pub async fn new(pool: Pool<BackendDB>) -> Result<Self, BackendError> {
        let backend = Self { pool };
        backend.run_migrations().await?;
        Ok(backend)
    }

    /// Create a new `SQLiteBackend` by connecting to the given database URL
    /// and running schema migrations.
    ///
    /// # Errors
    ///
    /// Returns a `BackendError` if the connection or migration fails.
    #[cfg(feature = "sqlite")]
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        let pool = Pool::<BackendDB>::connect(url)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;
        Self::new(pool).await
    }

    /// Create a new D1 backend and run schema migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the migration DDL fails.
    #[cfg(feature = "d1")]
    pub async fn connect(db: worker::D1Database) -> Result<Self, BackendError> {
        let pool = Pool::<BackendDB>::connect_with(sqlx_d1::D1ConnectOptions::new(db))
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        Self::new(pool).await
    }

    /// Run the schema migrations on the database.
    ///
    /// # Errors
    ///
    /// Returns a `BackendError` if the migration fails.
    pub async fn run_migrations(&self) -> Result<(), BackendError> {
        sqlx::query(MIGRATION_SQL)
            .execute(self.pool())
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn pool(&self) -> &Pool<BackendDB> {
        &self.pool
    }

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
