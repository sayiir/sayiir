//! Core types and traits for the Sayiir durable workflow engine.
//!
//! This crate defines the foundational abstractions that every other `sayiir-*`
//! crate builds on. It is intentionally **runtime-agnostic** — no persistence,
//! no execution strategy, just pure workflow modelling.
//!
//! # Key Abstractions
//!
//! | Type / Trait | Purpose |
//! |---|---|
//! | [`Workflow`] / [`SerializableWorkflow`] | The continuation tree that describes *what* to execute |
//! | [`WorkflowBuilder`] | Fluent builder for assembling sequential, forked, and joined pipelines |
//! | [`CoreTask`] | Trait implemented by every task (input → output, with metadata) |
//! | [`Codec`] | Trait for pluggable serialization (JSON, rkyv, …) |
//! | [`WorkflowContext`] | Per-execution context carrying the codec, workflow ID, and user metadata |
//! | [`WorkflowSnapshot`](snapshot::WorkflowSnapshot) | Checkpoint of in-flight execution state (completed tasks + outputs) |
//! | [`TaskRegistry`] | Name → factory map used for deserializing workflows across process boundaries |
//!
//! # Architecture
//!
//! ```text
//! sayiir-core          (this crate — types & traits only)
//!   ↑
//! sayiir-persistence   (SnapshotStore, SignalStore, TaskClaimStore)
//!   ↑
//! sayiir-runtime       (CheckpointingRunner, PooledWorker, execution loop)
//!   ↑
//! sayiir-macros        (#[task], workflow! — optional convenience)
//! ```
//!
//! # Quick Example
//!
//! ```rust,ignore
//! use sayiir_core::prelude::*;
//! use std::sync::Arc;
//!
//! // Create a context (codec + workflow ID)
//! // (Codec is generic — bring your own or use sayiir-runtime's JsonCodec / RkyvCodec)
//! let ctx = WorkflowContext::new("order-pipeline", Arc::new(my_codec), Arc::new(()));
//!
//! // Build a workflow: validate → charge
//! let wf = WorkflowBuilder::new(ctx)
//!     .then("validate", |order: String| async move { Ok(order) })
//!     .then("charge", |order: String| async move { Ok(42u64) })
//!     .build()?;
//! ```
//!
//! For the proc-macro experience (`#[task]` + `workflow!`), see
//! [`sayiir-macros`](https://docs.rs/sayiir-macros).
//!
//! [`Workflow`]: workflow::Workflow
//! [`SerializableWorkflow`]: workflow::SerializableWorkflow
//! [`WorkflowBuilder`]: builder::WorkflowBuilder
//! [`CoreTask`]: task::CoreTask
//! [`Codec`]: codec::Codec
//! [`WorkflowContext`]: context::WorkflowContext
//! [`TaskRegistry`]: registry::TaskRegistry

#![deny(missing_docs)]
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

pub mod branch_key;
pub mod branch_results;
pub mod builder;
pub mod codec;
pub mod context;
pub mod continuation_builder;
pub mod error;
pub mod prelude;
pub mod registry;
pub mod snapshot;
pub mod task;
pub mod task_claim;
pub mod workflow;
