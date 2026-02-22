//! Python bindings for the Sayiir workflow library.
//!
//! All orchestration logic runs in Rust. Python provides task implementations.

#![deny(clippy::pedantic)]
#![allow(
    // pyo3 macros generate code that triggers these
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
)]

use pyo3::prelude::*;

mod backend;
mod codec;
mod durable_engine;
mod engine;
pub mod exceptions;
mod flow;
mod task;
mod worker;

/// Python module for Sayiir workflow library.
#[pymodule]
fn _sayiir(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<task::PyRetryPolicy>()?;
    m.add_class::<task::PyTaskMetadata>()?;
    m.add_class::<task::PyTaskExecutionContext>()?;
    m.add_function(wrap_pyfunction!(task::get_task_context, m)?)?;
    m.add_class::<flow::PyFlowBuilder>()?;
    m.add_class::<flow::PyWorkflow>()?;
    m.add_class::<engine::PyWorkflowEngine>()?;
    m.add_class::<engine::PyWorkflowStatus>()?;
    m.add_class::<backend::PyInMemoryBackend>()?;
    m.add_class::<backend::PyPostgresBackend>()?;
    m.add_class::<durable_engine::PyDurableEngine>()?;
    m.add_class::<worker::PyWorker>()?;
    m.add_class::<worker::PyWorkerHandle>()?;

    // Register exception types
    m.add(
        "WorkflowError",
        m.py().get_type::<exceptions::WorkflowError>(),
    )?;
    m.add("TaskError", m.py().get_type::<exceptions::TaskError>())?;
    m.add(
        "BackendError",
        m.py().get_type::<exceptions::BackendError>(),
    )?;
    m.add(
        "DeserializationError",
        m.py().get_type::<exceptions::DeserializationError>(),
    )?;
    Ok(())
}
