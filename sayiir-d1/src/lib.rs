//! `SQLite` / Cloudflare D1 persistence backend for the Sayiir workflow engine.
//!
//! This crate provides [`SQLiteBackend`], a generic implementation of
//! [`SnapshotStore`](sayiir_persistence::SnapshotStore) and
//! [`SignalStore`](sayiir_persistence::SignalStore) built on top of `sqlx`.
//!
//! The same backend powers two targets:
//!
//! - **`sqlite` feature** — native `sqlx::SqlitePool` (tokio runtime)
//! - **`d1` feature** — Cloudflare D1 via `sqlx-d1::D1Connection` (WASM)
//!
//! Because both share the `sqlx::Executor` abstraction, the full `sqlx`
//! query API is usable directly from downstream crates against either
//! connection — see the re-exports below.
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
//! use sayiir_d1::SQLiteBackend;
//!
//! // sqlite feature:
//! let backend = SQLiteBackend::connect("sqlite://sayiir.db?mode=rwc").await?;
//!
//! // d1 feature (Cloudflare Workers):
//! // let backend = SQLiteBackend::connect(d1_database).await?;
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

/// Re-export of `sqlx` so downstream crates can build typed queries against
/// either the sqlite or D1 connection without depending on `sqlx` directly.
pub use sqlx;

#[cfg(feature = "d1")]
pub use {sqlx_d1, worker::D1Database};

/// D1 specialization of [`SQLiteBackend`].
#[cfg(feature = "d1")]
pub type D1Backend = SQLiteBackend<sqlx_d1::D1Connection>;

/// Native `sqlx` specialization of [`SQLiteBackend`].
#[cfg(feature = "sqlite")]
pub type SqliteBackend = SQLiteBackend<sqlx::SqlitePool>;
