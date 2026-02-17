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
    pub paused_by: Option<String>,
    #[pyo3(get)]
    pub output: Option<Py<PyAny>>,
    /// ISO-8601 wake-up timestamp for `waiting` and `awaiting_signal` statuses.
    #[pyo3(get)]
    pub wake_at: Option<String>,
    /// Delay step identifier (present when status is `waiting`).
    #[pyo3(get)]
    pub delay_id: Option<String>,
    /// Signal step identifier (present when status is `awaiting_signal`).
    #[pyo3(get)]
    pub signal_id: Option<String>,
    /// Signal name (present when status is `awaiting_signal`).
    #[pyo3(get)]
    pub signal_name: Option<String>,
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

    fn is_paused(&self) -> bool {
        self.status == "paused"
    }

    fn is_waiting(&self) -> bool {
        self.status == "waiting"
    }

    fn is_awaiting_signal(&self) -> bool {
        self.status == "awaiting_signal"
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
            "paused" => format!(
                "WorkflowStatus::Paused(reason={:?}, by={:?})",
                self.reason, self.paused_by
            ),
            "waiting" => format!(
                "WorkflowStatus::Waiting(delay_id={:?}, wake_at={:?})",
                self.delay_id, self.wake_at
            ),
            "awaiting_signal" => format!(
                "WorkflowStatus::AwaitingSignal(signal_name={:?}, signal_id={:?}, wake_at={:?})",
                self.signal_name, self.signal_id, self.wake_at
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
                paused_by: None,
                output: None,
                wake_at: None,
                delay_id: None,
                signal_id: None,
                signal_name: None,
            },
            WorkflowStatus::InProgress => PyWorkflowStatus {
                status: "in_progress".to_string(),
                error: None,
                reason: None,
                cancelled_by: None,
                paused_by: None,
                output: None,
                wake_at: None,
                delay_id: None,
                signal_id: None,
                signal_name: None,
            },
            WorkflowStatus::Failed(e) => PyWorkflowStatus {
                status: "failed".to_string(),
                error: Some(e.clone()),
                reason: None,
                cancelled_by: None,
                paused_by: None,
                output: None,
                wake_at: None,
                delay_id: None,
                signal_id: None,
                signal_name: None,
            },
            WorkflowStatus::Cancelled {
                reason,
                cancelled_by,
            } => PyWorkflowStatus {
                status: "cancelled".to_string(),
                error: None,
                reason,
                cancelled_by,
                paused_by: None,
                output: None,
                wake_at: None,
                delay_id: None,
                signal_id: None,
                signal_name: None,
            },
            WorkflowStatus::Paused { reason, paused_by } => PyWorkflowStatus {
                status: "paused".to_string(),
                error: None,
                reason,
                cancelled_by: None,
                paused_by,
                output: None,
                wake_at: None,
                delay_id: None,
                signal_id: None,
                signal_name: None,
            },
            WorkflowStatus::Waiting { wake_at, delay_id } => PyWorkflowStatus {
                status: "waiting".to_string(),
                error: None,
                reason: None,
                cancelled_by: None,
                paused_by: None,
                output: None,
                wake_at: Some(wake_at.to_rfc3339()),
                delay_id: Some(delay_id),
                signal_id: None,
                signal_name: None,
            },
            WorkflowStatus::AwaitingSignal {
                signal_id,
                signal_name,
                wake_at,
            } => PyWorkflowStatus {
                status: "awaiting_signal".to_string(),
                error: None,
                reason: None,
                cancelled_by: None,
                paused_by: None,
                output: None,
                wake_at: wake_at.map(|t| t.to_rfc3339()),
                delay_id: None,
                signal_id: Some(signal_id),
                signal_name: Some(signal_name),
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

        tracing::info!(workflow_id = %workflow.workflow_id, "starting workflow execution");

        let result = execute_continuation_sync(&continuation, input_bytes, &|task_id, input| {
            execute_python_task(py, task_id, &input, task_registry).map_err(|e| {
                let traceback = e
                    .traceback(py)
                    .and_then(|tb| tb.format().ok())
                    .unwrap_or_default();

                // Preserve the Python traceback through the Rust execution loop.
                let msg: sayiir_core::error::BoxError = if traceback.is_empty() {
                    e.to_string().into()
                } else {
                    format!("{e}\n\n{traceback}").into()
                };
                msg
            })
        })
        .map_err(|e| PyErr::new::<crate::exceptions::TaskError, _>(e.to_string()))?;

        tracing::info!(workflow_id = %workflow.workflow_id, "workflow execution completed");

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
    let callable = registry.get_item(task_id)?.ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!("Task '{task_id}' not found"))
    })?;

    tracing::debug!(task_id, input_bytes = input.len(), "executing python task");

    let input_obj = decode_to_pyobject(py, input)?;
    let result = callable.call1((input_obj,))?;

    let result = if result.getattr(intern!(py, "__await__")).is_ok() {
        tracing::debug!(task_id, "task returned coroutine, awaiting synchronously");

        let asyncio = py.import(intern!(py, "asyncio"))?;
        let has_running_loop = asyncio
            .call_method0(intern!(py, "get_running_loop"))
            .is_ok();

        if has_running_loop {
            tracing::trace!(
                task_id,
                "event loop already running, using background thread"
            );
            py.run(
                c"
def _run_coro_in_thread(coro):
    import asyncio, threading
    result = [None]
    exc = [None]
    def target():
        loop = asyncio.new_event_loop()
        try:
            result[0] = loop.run_until_complete(coro)
        except BaseException as e:
            exc[0] = e
        finally:
            loop.close()
    t = threading.Thread(target=target)
    t.start()
    t.join()
    if exc[0] is not None:
        raise exc[0]
    return result[0]
",
                None,
                None,
            )?;
            let globals = py
                .import(intern!(py, "__main__"))?
                .getattr(intern!(py, "__dict__"))?;
            let run_fn = globals.get_item("_run_coro_in_thread")?;
            run_fn.call1((result,))?
        } else {
            asyncio.call_method1(intern!(py, "run"), (result,))?
        }
    } else {
        result
    };

    tracing::debug!(task_id, "python task completed");

    encode_pyobject(py, &result)
}
