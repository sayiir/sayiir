//! Python bindings for the Sayiir workflow library.
//!
//! This crate provides PyO3 bindings that enable Python programs to use the
//! Rust workflow orchestration engine with a Pythonic API.

use pyo3::prelude::*;

mod backend;
mod channel;
mod codec;
mod engine;
mod flow;
mod task;

/// The sayiir workflow library for Python.
///
/// This module provides:
/// - TaskChannel: Communication channel between Rust orchestrator and Python executor
/// - PyCodec: JSON-based serialization for Python objects
/// - PyFlowBuilder: Builder for constructing workflows
/// - PyWorkflowEngine: Engine for running workflows with persistence
/// - PyInMemoryBackend: In-memory storage backend for development/testing
#[pymodule]
fn _sayiir(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Channel types
    m.add_class::<channel::TaskChannel>()?;
    m.add_class::<channel::TaskRequest>()?;

    // Flow builder
    m.add_class::<flow::PyFlowBuilder>()?;
    m.add_class::<flow::PyForkBuilder>()?;
    m.add_class::<flow::PyTaskMetadata>()?;
    m.add_class::<flow::PyWorkflow>()?;

    // Engine
    m.add_class::<engine::PyWorkflowEngine>()?;

    // Backend
    m.add_class::<backend::PyInMemoryBackend>()?;

    Ok(())
}
