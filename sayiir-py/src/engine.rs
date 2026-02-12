//! Python-exposed workflow engine.
//!
//! Rust drives all execution logic. Python only provides task implementations
//! via a callback dictionary passed to `run()`.

use bytes::Bytes;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;

use sayiir_core::workflow::WorkflowStatus;
use sayiir_runtime::execute_continuation_sync;

use crate::codec::{decode_to_pyobject, encode_pyobject};
use crate::flow::PyWorkflow;

/// Python-exposed workflow status.
#[pyclass]
#[derive(Debug)]
pub struct PyWorkflowStatus {
    #[pyo3(get)]
    pub status: String,
    #[pyo3(get)]
    pub error: Option<String>,
    #[pyo3(get)]
    pub reason: Option<String>,
    #[pyo3(get)]
    pub cancelled_by: Option<String>,
    #[pyo3(get)]
    pub output: Option<Py<PyAny>>,
}

#[pymethods]
impl PyWorkflowStatus {
    fn is_completed(&self) -> bool {
        self.status == "completed"
    }

    fn is_failed(&self) -> bool {
        self.status == "failed"
    }

    fn is_cancelled(&self) -> bool {
        self.status == "cancelled"
    }

    fn is_in_progress(&self) -> bool {
        self.status == "in_progress"
    }

    fn __repr__(&self) -> String {
        match self.status.as_str() {
            "completed" => "WorkflowStatus::Completed".to_string(),
            "failed" => format!(
                "WorkflowStatus::Failed({})",
                self.error.as_deref().unwrap_or("unknown")
            ),
            "cancelled" => format!(
                "WorkflowStatus::Cancelled(reason={:?}, by={:?})",
                self.reason, self.cancelled_by
            ),
            _ => format!("WorkflowStatus::{}", self.status),
        }
    }
}

impl From<WorkflowStatus> for PyWorkflowStatus {
    fn from(status: WorkflowStatus) -> Self {
        match status {
            WorkflowStatus::Completed => PyWorkflowStatus {
                status: "completed".to_string(),
                error: None,
                reason: None,
                cancelled_by: None,
                output: None,
            },
            WorkflowStatus::InProgress => PyWorkflowStatus {
                status: "in_progress".to_string(),
                error: None,
                reason: None,
                cancelled_by: None,
                output: None,
            },
            WorkflowStatus::Failed(e) => PyWorkflowStatus {
                status: "failed".to_string(),
                error: Some(e.clone()),
                reason: None,
                cancelled_by: None,
                output: None,
            },
            WorkflowStatus::Cancelled {
                reason,
                cancelled_by,
            } => PyWorkflowStatus {
                status: "cancelled".to_string(),
                error: None,
                reason,
                cancelled_by,
                output: None,
            },
            WorkflowStatus::Waiting { wake_at, delay_id } => PyWorkflowStatus {
                status: "waiting".to_string(),
                error: None,
                reason: Some(format!("Delay '{delay_id}' until {wake_at}")),
                cancelled_by: None,
                output: None,
            },
        }
    }
}

/// Python-exposed workflow engine.
///
/// Rust drives all execution. Python provides task implementations via `run()`.
#[pyclass]
pub struct PyWorkflowEngine;

#[pymethods]
impl PyWorkflowEngine {
    #[new]
    fn new() -> Self {
        Self
    }

    /// Run a workflow to completion.
    ///
    /// Args:
    ///     workflow: The workflow to execute
    ///     input: Input data for the first task
    ///     `task_registry`: dict mapping `task_id` -> callable
    ///
    /// Returns:
    ///     The final workflow output
    ///
    /// All orchestration logic runs in Rust. Python tasks are called directly.
    fn run(
        &self,
        py: Python<'_>,
        workflow: &PyWorkflow,
        input: &Bound<'_, PyAny>,
        task_registry: &Bound<'_, PyDict>,
    ) -> PyResult<Py<PyAny>> {
        let input_bytes = encode_pyobject(py, input)?;
        let continuation = Arc::clone(&workflow.continuation);

        // Use shared execution logic from sayiir-runtime.
        // Preserve the Python traceback through the Rust execution loop.
        let result = execute_continuation_sync(&continuation, input_bytes, &|task_id, input| {
            execute_python_task(py, task_id, &input, task_registry).map_err(|e| {
                let traceback = e
                    .traceback(py)
                    .and_then(|tb| tb.format().ok())
                    .unwrap_or_default();
                let msg: sayiir_core::error::BoxError = if traceback.is_empty() {
                    e.to_string().into()
                } else {
                    format!("{e}\n\n{traceback}").into()
                };
                msg
            })
        })
        .map_err(|e| PyErr::new::<crate::exceptions::TaskError, _>(e.to_string()))?;

        decode_to_pyobject(py, &result)
    }

    fn __repr__(&self) -> String {
        "WorkflowEngine()".to_string()
    }
}

/// Execute a Python task by calling it from the registry.
pub(crate) fn execute_python_task(
    py: Python<'_>,
    task_id: &str,
    input: &Bytes,
    registry: &Bound<'_, PyDict>,
) -> PyResult<Bytes> {
    // Look up task — the registry maps task_id to the callable directly
    let callable = registry.get_item(task_id)?.ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!("Task '{task_id}' not found"))
    })?;

    // Decode input
    let input_obj = decode_to_pyobject(py, input)?;

    // Call function
    let result = callable.call1((input_obj,))?;

    // Handle async (coroutine) — run synchronously via asyncio.run().
    // This creates a new event loop, so it must NOT be called from within
    // an already-running loop (e.g. Jupyter). For that use case, the planned
    // async execution path will be needed.
    let result = if result.getattr(intern!(py, "__await__")).is_ok() {
        let asyncio = py.import(intern!(py, "asyncio"))?;
        asyncio.call_method1(intern!(py, "run"), (result,))?
    } else {
        result
    };

    encode_pyobject(py, &result)
}
