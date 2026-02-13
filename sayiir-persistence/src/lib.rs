//! Persistence layer for workflow execution state.
//!
//! This crate provides traits and implementations for persisting workflow
//! execution state, enabling distributed execution with checkpoint/restore
//! capabilities.
//!
//! # Trait Hierarchy
//!
//! The persistence layer is built around focused sub-traits:
//!
//! - **[`SnapshotStore`]**: Core CRUD for workflow snapshots (5 methods).
//! - **[`SignalStore`]**: Cancel + pause signal primitives with default composite
//!   implementations (3 required + 3 default methods).
//! - **[`TaskClaimStore`]**: Distributed task claiming (4 methods, opt-in).
//! - **[`PersistentBackend`]**: Supertrait = `SnapshotStore + SignalStore`,
//!   blanket-implemented so backends never need to impl it directly.
//!
//! A minimal backend only needs **8 methods** (`SnapshotStore` + 3 `SignalStore`
//! primitives) to satisfy `PersistentBackend`.
//!
//! # Example
//!
//! ```rust,no_run
//! use sayiir_persistence::{InMemoryBackend, SnapshotStore};
//! use sayiir_core::snapshot::WorkflowSnapshot;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a backend (could be Redis, PostgreSQL, etc.)
//! let backend = InMemoryBackend::new();
//!
//! // Save a snapshot
//! let snapshot = WorkflowSnapshot::new("instance-123".to_string(), "hash-abc".to_string());
//! backend.save_snapshot(&snapshot).await?;
//!
//! // Load it back
//! let loaded = backend.load_snapshot("instance-123").await?;
//! # Ok(())
//! # }
//! ```

mod backend;
mod in_memory;

pub use backend::{BackendError, PersistentBackend, SignalStore, SnapshotStore, TaskClaimStore};
pub use in_memory::InMemoryBackend;
