//! Cloudflare D1 (`SQLite`) persistence backend for the Sayiir workflow engine.
//!
//! This crate provides [`D1Backend`], an implementation of
//! [`SnapshotStore`](sayiir_persistence::SnapshotStore) and
//! [`SignalStore`](sayiir_persistence::SignalStore) backed by Cloudflare D1
//! via `wasm-bindgen` FFI bindings.
//!
//! # Features
//!
//! - **WASM-native**: Targets `wasm32-unknown-unknown` with zero tokio dependency.
//! - **JSON codec**: Snapshots are serialized as JSON into D1 `BLOB` columns.
//! - **Snapshot history**: Every checkpoint is appended to an immutable history
//!   table for debugging and auditing.
//! - **Signal support**: Cancel, pause, and external event buffering.
//!
//! # D1 / `SQLite` adaptations
//!
//! - `BLOB` instead of `BYTEA`
//! - `TEXT` with ISO 8601 timestamps instead of `TIMESTAMPTZ`
//! - `INTEGER PRIMARY KEY AUTOINCREMENT` instead of `BIGSERIAL`
//! - No `sayiir_workflow_tasks` or `sayiir_task_claims` tables (single-Worker,
//!   no distributed task claiming)
//!
//! # Example
//!
//! ```rust,ignore
//! use sayiir_d1::D1Backend;
//! use sayiir_persistence::SnapshotStore;
//! use sayiir_core::snapshot::WorkflowSnapshot;
//!
//! // `db` is a D1Database binding from the Worker env
//! let backend = D1Backend::new(db).await?;
//!
//! let snapshot = WorkflowSnapshot::new("order-123".into(), "hash-abc".into());
//! backend.save_snapshot(&snapshot).await?;
//!
//! let loaded = backend.load_snapshot("order-123").await?;
//! ```

#![deny(clippy::pedantic)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]

mod backend;
mod helpers;
mod schema;
mod signal_store;
mod snapshot_store;

pub use backend::SQLiteBackend;
