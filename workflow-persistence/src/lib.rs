//! Persistence layer for workflow execution state.
//!
//! This crate provides traits and implementations for persisting workflow
//! execution state, enabling distributed execution with checkpoint/restore
//! capabilities.
//!
//! # Architecture
//!
//! The persistence layer is built around two core concepts:
//!
//! - **PersistentBackend**: A trait that abstracts the storage mechanism
//!   for workflow snapshots.
//! - **InMemoryBackend**: A reference implementation using an in-memory HashMap.
//!
//! # Example
//!
//! ```rust,ignore
//! use workflow_persistence::{InMemoryBackend, PersistentBackend};
//! use workflow_core::snapshot::WorkflowSnapshot;
//!
//! // Create a backend (could be Redis, PostgreSQL, etc.)
//! let backend = InMemoryBackend::new();
//!
//! // Save a snapshot
//! let snapshot = WorkflowSnapshot::new("instance-123".to_string(), "hash-abc".to_string());
//! backend.save_snapshot(snapshot).await?;
//!
//! // Load it back
//! let loaded = backend.load_snapshot("instance-123").await?;
//! ```
//!
//! # Implementing Custom Backends
//!
//! To implement a custom persistence backend (e.g., Redis, PostgreSQL):
//!
//! 1. Add `workflow-persistence` as a dependency
//! 2. Implement the `PersistentBackend` trait
//! 3. Handle snapshot serialization/deserialization
//! 4. Implement atomic task claiming for distributed execution
//!
//! ```rust,ignore
//! use workflow_persistence::{PersistentBackend, BackendError};
//! use workflow_core::snapshot::WorkflowSnapshot;
//! use async_trait::async_trait;
//!
//! pub struct RedisBackend {
//!     // your Redis client
//! }
//!
//! #[async_trait]
//! impl PersistentBackend for RedisBackend {
//!     async fn save_snapshot(&self, snapshot: WorkflowSnapshot) -> Result<(), BackendError> {
//!         // serialize and save to Redis
//!     }
//!     // ... implement other methods
//! }
//! ```

mod backend;
mod in_memory;

pub use backend::{BackendError, PersistentBackend};
pub use in_memory::InMemoryBackend;
