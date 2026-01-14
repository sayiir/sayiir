//! Persistence layer for distributed workflow execution.
//!
//! This module provides traits and implementations for persisting workflow
//! execution state, enabling distributed execution with checkpoint/restore
//! capabilities.
//!
//! # Architecture
//!
//! The persistence layer is built around three core concepts:
//!
//! - **WorkflowSnapshot**: Captures the complete execution state including
//!   which tasks have completed and their outputs.
//! - **PersistentBackend**: A trait that abstracts the storage mechanism
//!   for workflow snapshots.
//! - **DistributedRunner**: A workflow runner that saves checkpoints after
//!   each task completion and can restore from previous snapshots.
//!
//! # Example
//!
//! ```rust,ignore
//! use workflow_runtime::persistence::{
//!     InMemoryBackend, DistributedRunner, PersistentBackend,
//! };
//!
//! // Create a backend (could be Redis, PostgreSQL, etc.)
//! let backend = InMemoryBackend::new();
//!
//! // Create a distributed runner
//! let runner = DistributedRunner::new(backend);
//!
//! // Run a workflow - progress is automatically checkpointed
//! let status = runner.run(&workflow, input).await?;
//!
//! // If the workflow failed midway, it can be resumed
//! let status = runner.resume(&workflow, "workflow-instance-123").await?;
//! ```

mod backend;
mod distributed;
mod in_memory;
mod snapshot;
mod worker;

pub use backend::{BackendError, PersistentBackend};
pub use distributed::DistributedRunner;
pub use in_memory::InMemoryBackend;
pub use snapshot::{ExecutionPosition, TaskResult, WorkflowSnapshot, WorkflowSnapshotState};
pub use worker::WorkerNode;
