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

mod runner;
pub mod serialization;
pub mod worker;

// Re-exports
pub use runner::WorkflowRunner;
pub use runner::distributed::CheckpointingRunner;
pub use runner::in_process::InProcessRunner;
pub use worker::PooledWorker;

pub use workflow_core::sayiir_ctx;
pub use workflow_persistence as persistence;
