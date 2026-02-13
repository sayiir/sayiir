//! PostgreSQL persistence backend for the Sayiir workflow engine.
//!
//! This crate provides [`PostgresBackend`], a production-grade implementation of
//! [`SnapshotStore`](sayiir_persistence::SnapshotStore),
//! [`SignalStore`](sayiir_persistence::SignalStore), and
//! [`TaskClaimStore`](sayiir_persistence::TaskClaimStore) backed by PostgreSQL via
//! [`sqlx`].
//!
//! # Features
//!
//! - **Codec-generic**: Serialise snapshots with any codec (JSON for debuggability,
//!   rkyv/bincode for speed). The data column is always `BYTEA`.
//! - **ACID transactions**: Composite signal operations (`check_and_cancel`,
//!   `check_and_pause`, `unpause`) use single Postgres transactions with
//!   `SELECT … FOR UPDATE` for true atomicity.
//! - **Snapshot history**: Every checkpoint is appended to an immutable history
//!   table for debugging, auditing, and future replay.
//! - **Observability-ready**: Indexed metadata columns (`status`, `current_task_id`,
//!   `completed_task_count`, `error`, timestamps) plus a denormalised
//!   `sayiir_workflow_tasks` table enable monitoring without deserialising blobs.
//! - **Distributed task claiming**: `TaskClaimStore` with TTL-based claims,
//!   expired-claim replacement, and soft worker bias.
//!
//! # PostgreSQL version support
//!
//! Minimum supported version: **PostgreSQL 13**. The schema uses
//! `INSERT … ON CONFLICT DO UPDATE` (9.5+) and `ALTER TABLE … ADD COLUMN IF NOT
//! EXISTS` (9.6+); 13 is the floor because it is the oldest major release still
//! receiving security patches. Integration tests run against both 13 and 17.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use sayiir_postgres::PostgresBackend;
//! use sayiir_runtime::serialization::JsonCodec;
//! use sayiir_persistence::SnapshotStore;
//! use sayiir_core::snapshot::WorkflowSnapshot;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let backend = PostgresBackend::<JsonCodec>::connect("postgresql://localhost/sayiir").await?;
//!
//! let snapshot = WorkflowSnapshot::new("order-123".to_string(), "hash-abc".to_string());
//! backend.save_snapshot(&snapshot).await?;
//!
//! let loaded = backend.load_snapshot("order-123").await?;
//! # Ok(())
//! # }
//! ```

mod backend;
mod error;
mod helpers;
mod signal_store;
mod snapshot_store;
mod task_claim_store;

pub use backend::PostgresBackend;
