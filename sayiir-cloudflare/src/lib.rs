//! Cloudflare Workers WASM bindings for the Sayiir workflow engine.
//!
//! Compiles to `wasm32-unknown-unknown` and exposes the workflow builder,
//! stepper, and durable engine via `wasm-bindgen` for use inside Cloudflare
//! Workers.
//!
//! # Exports
//!
//! - [`WasmFlowBuilder`] — builds `WorkflowContinuation` (mirrors `NapiFlowBuilder`)
//! - [`WasmContinuationStepper`] — yields tasks one-by-one to JS via `current()` / `submitResult()`
//! - [`WasmDurableEngine`] — checkpoint-and-exit orchestration with D1 persistence
//!

#![cfg(target_arch = "wasm32")]
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

mod codec;
mod durable;
mod engine;
mod error;
mod flow;
mod lifecycle;
mod status;

pub use durable::WasmDurableEngine;
pub use engine::WasmContinuationStepper;
pub use flow::{WasmFlowBuilder, WasmWorkflow};
pub use status::WasmWorkflowStatus;
