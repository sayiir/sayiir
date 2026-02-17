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
//! This crate provides the execution engine that drives [`sayiir_core`] workflows
//! to completion. It re-exports the core crate, persistence layer, and proc-macros
//! so most users only need a single dependency:
//!
//! ```toml
//! [dependencies]
//! sayiir-runtime = "0.1"
//! ```
//!
//! # Execution Strategies
//!
//! | Scenario | Use |
//! |----------|-----|
//! | Single server, crash recovery needed | [`CheckpointingRunner`] |
//! | Multiple workers, horizontal scaling | [`PooledWorker`] |
//! | Simple in-memory execution | [`InProcessRunner`] |
//!
//! # Quick Example
//!
//! ```rust,no_run
//! use sayiir_runtime::{CheckpointingRunner, WorkflowRunner, workflow, task};
//! use sayiir_runtime::persistence::InMemoryBackend;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let backend = InMemoryBackend::new();
//! let runner = CheckpointingRunner::new(backend);
//!
//! // Run a workflow with automatic checkpointing
//! // let status = runner.run(&workflow, "instance-123", input).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Re-exports
//!
//! This crate re-exports key items for convenience:
//!
//! - [`sayiir_core`] — via `sayiir_runtime::persistence` (through `sayiir_persistence`)
//! - [`sayiir_persistence`] — as [`persistence`]
//! - [`task`] and [`workflow!`] — from `sayiir-macros`
//! - [`sayiir_ctx!`] — context macro from `sayiir-core`
//!
//! For the full README with architecture diagrams and detailed configuration,
//! see the [crate README](https://crates.io/crates/sayiir-runtime).

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
pub use worker::{
    ExternalTaskExecutor, ExternalWorkflow, PooledWorker, WorkerHandle, WorkflowIndex,
    WorkflowRegistry,
};

pub use sayiir_core::sayiir_ctx;
#[cfg(feature = "macros")]
pub use sayiir_macros::{task, workflow};
pub use sayiir_persistence as persistence;
