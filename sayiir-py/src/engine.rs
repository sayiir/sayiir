//! Python-exposed workflow engine.
//!
//! Rust drives all execution logic. Python only provides task implementations
//! via a callback dictionary passed to `run()`.

use bytes::Bytes;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;

use sayiir_core::workflow::{FlatWorkflowStatus, WorkflowStatus, WorkflowStatusKind};
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
        self.status == WorkflowStatusKind::Completed.as_ref()
    }

    fn is_failed(&self) -> bool {
        self.status == WorkflowStatusKind::Failed.as_ref()
    }

    fn is_cancelled(&self) -> bool {
        self.status == WorkflowStatusKind::Cancelled.as_ref()
    }

    fn is_in_progress(&self) -> bool {
        self.status == WorkflowStatusKind::InProgress.as_ref()
    }

    fn is_paused(&self) -> bool {
        self.status == WorkflowStatusKind::Paused.as_ref()
    }

    fn is_waiting(&self) -> bool {
        self.status == WorkflowStatusKind::Waiting.as_ref()
    }

    fn is_awaiting_signal(&self) -> bool {
        self.status == WorkflowStatusKind::AwaitingSignal.as_ref()
    }

    fn __repr__(&self) -> String {
        match self.status.as_str() {
            s if s == WorkflowStatusKind::Completed.as_ref() => {
                "WorkflowStatus::Completed".to_string()
            }
            s if s == WorkflowStatusKind::Failed.as_ref() => format!(
                "WorkflowStatus::Failed({})",
                self.error.as_deref().unwrap_or("unknown")
            ),
            s if s == WorkflowStatusKind::Cancelled.as_ref() => format!(
                "WorkflowStatus::Cancelled(reason={:?}, by={:?})",
                self.reason, self.cancelled_by
            ),
            s if s == WorkflowStatusKind::Paused.as_ref() => format!(
                "WorkflowStatus::Paused(reason={:?}, by={:?})",
                self.reason, self.paused_by
            ),
            s if s == WorkflowStatusKind::Waiting.as_ref() => format!(
                "WorkflowStatus::Waiting(delay_id={:?}, wake_at={:?})",
                self.delay_id, self.wake_at
            ),
            s if s == WorkflowStatusKind::AwaitingSignal.as_ref() => format!(
                "WorkflowStatus::AwaitingSignal(signal_name={:?}, signal_id={:?}, wake_at={:?})",
                self.signal_name, self.signal_id, self.wake_at
            ),
            _ => format!("WorkflowStatus::{}", self.status),
        }
    }
}

impl From<WorkflowStatus> for PyWorkflowStatus {
    fn from(status: WorkflowStatus) -> Self {
        let flat = FlatWorkflowStatus::from(status);
        Self {
            status: flat.status,
            error: flat.error,
            reason: flat.reason,
            cancelled_by: flat.cancelled_by,
            paused_by: flat.paused_by,
            output: None,
            wake_at: flat.wake_at,
            delay_id: flat.delay_id,
            signal_id: flat.signal_id,
            signal_name: flat.signal_name,
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

        let result = execute_continuation_sync(
            &continuation,
            input_bytes,
            &|task_id, input| {
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
            },
            &sayiir_runtime::serialization::JsonCodec,
        )
        .map_err(|e| match &e {
            sayiir_runtime::RuntimeError::Codec(_) => {
                PyErr::new::<crate::exceptions::DeserializationError, _>(e.to_string())
            }
            _ => PyErr::new::<crate::exceptions::TaskError, _>(e.to_string()),
        })?;

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
