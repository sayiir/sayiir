#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]
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

pub mod error;
pub mod execution;
pub mod prelude;
mod runner;
pub mod serialization;
pub mod worker;

// Re-exports
pub use error::RuntimeError;
pub use execution::{
    ResumeOutcome, execute_continuation_async, execute_continuation_sync,
    execute_continuation_with_checkpointing, finalize_execution, prepare_resume, prepare_run,
    serialize_branch_results,
};
pub use runner::WorkflowRunner;
pub use runner::distributed::CheckpointingRunner;
pub use runner::in_process::InProcessRunner;
pub use worker::{PooledWorker, WorkerHandle, WorkflowRegistry};

pub use sayiir_core::sayiir_ctx;
pub use sayiir_persistence as persistence;
