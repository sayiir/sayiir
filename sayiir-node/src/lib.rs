//! Node.js bindings for the Sayiir workflow library.
//!
//! All orchestration logic runs in Rust. JavaScript provides task implementations.

#![deny(clippy::pedantic)]
#![allow(
    // napi macros generate code that triggers these
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    // napi-derive generates these
    clippy::module_name_repetitions,
    clippy::cast_possible_truncation,
    // napi-rs generated code may trigger this
    clippy::trivially_copy_pass_by_ref,
)]

mod backend;
mod codec;
mod durable_engine;
mod engine;
mod exceptions;
mod flow;
mod task;
mod telemetry;
mod worker;
mod workflow_client;
