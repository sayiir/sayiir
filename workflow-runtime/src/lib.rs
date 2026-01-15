//! Workflow runtime for executing durable workflows.
//!
//! This crate provides two main execution strategies:
//!
//! - [`CheckpointingRunner`]: Single-process execution with checkpointing for crash recovery
//! - [`PooledWorker`]: Multi-worker distributed execution with task claiming
//!
//! # Choosing an Execution Strategy
//!
//! | Scenario | Use |
//! |----------|-----|
//! | Single server, crash recovery needed | [`CheckpointingRunner`] |
//! | Multiple workers, horizontal scaling | [`PooledWorker`] |
//! | Simple in-memory execution | [`InProcessRunner`] |

mod runner;
pub mod serialization;
pub mod worker;

// Re-exports
pub use runner::WorkflowRunner;
pub use runner::in_process::InProcessRunner;
pub use runner::distributed::CheckpointingRunner;
pub use worker::PooledWorker;

pub use workflow_core::sayiir_ctx;
pub use workflow_persistence as persistence;
