//! `PostgresBackend` struct and constructors.

use sayiir_core::codec::{self, Decoder, Encoder};
use sayiir_core::snapshot::WorkflowSnapshot;
use sayiir_persistence::BackendError;
use sqlx::PgPool;

use crate::error::PgError;

/// PostgreSQL persistence backend for Sayiir workflows.
///
/// Generic over a [`Codec`](sayiir_core::codec::Codec) that determines how
/// snapshots are serialized into the `BYTEA` column. Use `JsonCodec` for
/// human-readable storage with Postgres-side queryability, or a binary codec
/// for faster (de)serialization.
///
/// # Example (with `sayiir-runtime` JSON codec)
///
/// ```rust,no_run
/// use sayiir_postgres::PostgresBackend;
/// use sayiir_runtime::serialization::JsonCodec;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let backend = PostgresBackend::<JsonCodec>::connect("postgresql://localhost/sayiir").await?;
/// # Ok(())
/// # }
/// ```
pub struct PostgresBackend<C> {
    pub(crate) pool: PgPool,
    pub(crate) codec: C,
}

impl<C> PostgresBackend<C>
where
    C: Default,
{
    /// Connect to Postgres and run migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or migration fails.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        let pool = PgPool::connect(url).await.map_err(PgError)?;
        Self::init(pool).await
    }

    /// Use an existing connection pool and run migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the migration fails.
    pub async fn connect_with(pool: PgPool) -> Result<Self, BackendError> {
        Self::init(pool).await
    }

    async fn init(pool: PgPool) -> Result<Self, BackendError> {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| BackendError::Backend(format!("migration failed: {e}")))?;
        Ok(Self {
            pool,
            codec: C::default(),
        })
    }
}

impl<C> PostgresBackend<C>
where
    C: Encoder + codec::sealed::EncodeValue<WorkflowSnapshot>,
{
    /// Encode a snapshot using the configured codec.
    pub(crate) fn encode(&self, snapshot: &WorkflowSnapshot) -> Result<Vec<u8>, BackendError> {
        self.codec
            .encode(snapshot)
            .map(|b| b.to_vec())
            .map_err(|e| BackendError::Serialization(e.to_string()))
    }
}

impl<C> PostgresBackend<C>
where
    C: Decoder + codec::sealed::DecodeValue<WorkflowSnapshot>,
{
    /// Decode a snapshot from raw bytes using the configured codec.
    pub(crate) fn decode(&self, data: &[u8]) -> Result<WorkflowSnapshot, BackendError> {
        self.codec
            .decode(bytes::Bytes::copy_from_slice(data))
            .map_err(|e| BackendError::Serialization(e.to_string()))
    }
}
