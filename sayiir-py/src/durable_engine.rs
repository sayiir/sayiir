//! Python-exposed durable workflow engine with checkpointing.
//!
//! Provides `PyDurableEngine` which bridges Python task implementations
//! to the Rust checkpointing runtime. Supports run, resume, and cancel.

use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;

use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_core::workflow::WorkflowStatus;
use sayiir_persistence::{SignalStore, SnapshotStore};
use sayiir_runtime::{
    execute_continuation_with_checkpointing, finalize_execution, prepare_resume, prepare_run,
    ResumeOutcome,
};

use crate::backend::{with_backend, BackendKind, PyInMemoryBackend, PyPostgresBackend};
use crate::codec::{decode_to_pyobject, encode_pyobject};
use crate::engine::{execute_python_task, PyWorkflowStatus};
use crate::exceptions;
use crate::flow::PyWorkflow;

/// Durable workflow engine with checkpointing, cancellation, and resume.
///
/// Uses Rust's checkpointing runtime to persist workflow state after each task.
/// Python provides task implementations via a callback dictionary.
///
/// Accepts either `InMemoryBackend` or `PostgresBackend`.
#[pyclass]
pub struct PyDurableEngine {
    backend: BackendKind,
    runtime: tokio::runtime::Runtime,
}

#[pymethods]
impl PyDurableEngine {
    /// Create a new durable engine.
    ///
    /// Args:
    ///     backend: Either `InMemoryBackend()` or `PostgresBackend(url)`
    #[new]
    fn new(backend: &Bound<'_, PyAny>) -> PyResult<Self> {
        if let Ok(mem) = backend.extract::<PyInMemoryBackend>() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            Ok(Self {
                backend: BackendKind::InMemory(Arc::clone(&mem.inner)),
                runtime,
            })
        } else if let Ok(pg) = backend.extract::<PyPostgresBackend>() {
            // Reuse the Postgres backend's runtime for connection keepalive.
            // Build a new current-thread runtime for engine operations.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            Ok(Self {
                backend: BackendKind::Postgres(Arc::clone(&pg.inner)),
                runtime,
            })
        } else {
            Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                "backend must be InMemoryBackend or PostgresBackend",
            ))
        }
    }

    /// Run a workflow to completion with checkpointing.
    fn run(
        &self,
        py: Python<'_>,
        workflow: &PyWorkflow,
        instance_id: String,
        input: &Bound<'_, PyAny>,
        task_registry: Py<PyDict>,
    ) -> PyResult<PyWorkflowStatus> {
        let input_bytes = encode_pyobject(py, input)?;
        let continuation = Arc::clone(&workflow.continuation);
        let definition_hash = workflow.definition_hash.clone();
        let first_task_id = continuation.first_task_id().to_string();
        let registry = Arc::new(task_registry);

        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            py.detach(|| {
                self.runtime.block_on(async {
                    let mut snapshot = prepare_run(
                        instance_id,
                        definition_hash,
                        input_bytes.clone(),
                        first_task_id,
                        backend.as_ref(),
                    )
                    .await?;

                    let executor = make_task_executor(&registry);
                    let result = execute_continuation_with_checkpointing(
                        &continuation,
                        input_bytes,
                        &mut snapshot,
                        backend.as_ref(),
                        &executor,
                    )
                    .await;

                    finalize_execution(result, &mut snapshot, backend.as_ref()).await
                })
            })
            .map_err(|e: sayiir_runtime::RuntimeError| {
                PyErr::new::<exceptions::WorkflowError, _>(e.to_string())
            })?
        });

        let mut py_status: PyWorkflowStatus = status.into();
        if let Some(bytes) = output_bytes {
            py_status.output = Some(decode_to_pyobject(py, &bytes)?);
        }
        Ok(py_status)
    }

    /// Resume a workflow from a saved checkpoint.
    fn resume(
        &self,
        py: Python<'_>,
        workflow: &PyWorkflow,
        instance_id: String,
        task_registry: Py<PyDict>,
    ) -> PyResult<PyWorkflowStatus> {
        let continuation = Arc::clone(&workflow.continuation);
        let definition_hash = workflow.definition_hash.clone();
        let registry = Arc::new(task_registry);

        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            py.detach(|| {
                self.runtime.block_on(async {
                    match prepare_resume(&instance_id, &definition_hash, backend.as_ref()).await? {
                        ResumeOutcome::AlreadyTerminal(status) => {
                            let output = if matches!(status, WorkflowStatus::Completed) {
                                let snapshot = backend.load_snapshot(&instance_id).await.ok();
                                snapshot.and_then(|s| s.state.completed_output().cloned())
                            } else {
                                None
                            };
                            Ok((status, output))
                        }
                        ResumeOutcome::Paused(status) => Ok((status, None)),
                        ResumeOutcome::Ready {
                            mut snapshot,
                            input_bytes,
                        } => {
                            let executor = make_task_executor(&registry);
                            let result = execute_continuation_with_checkpointing(
                                &continuation,
                                input_bytes,
                                &mut snapshot,
                                backend.as_ref(),
                                &executor,
                            )
                            .await;

                            finalize_execution(result, &mut snapshot, backend.as_ref()).await
                        }
                    }
                })
            })
            .map_err(|e: sayiir_runtime::RuntimeError| {
                PyErr::new::<exceptions::WorkflowError, _>(e.to_string())
            })?
        });

        let mut py_status: PyWorkflowStatus = status.into();
        if let Some(bytes) = output_bytes {
            py_status.output = Some(decode_to_pyobject(py, &bytes)?);
        }
        Ok(py_status)
    }

    /// Request cancellation of a running workflow.
    #[pyo3(signature = (instance_id, reason=None, cancelled_by=None))]
    fn cancel(
        &self,
        instance_id: String,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> PyResult<()> {
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.store_signal(
                    &instance_id,
                    SignalKind::Cancel,
                    SignalRequest::new(reason, cancelled_by),
                ))
                .map_err(backend_err_to_py)
        })
    }

    /// Request pausing of a running workflow.
    #[pyo3(signature = (instance_id, reason=None, paused_by=None))]
    fn pause(
        &self,
        instance_id: String,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> PyResult<()> {
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.store_signal(
                    &instance_id,
                    SignalKind::Pause,
                    SignalRequest::new(reason, paused_by),
                ))
                .map_err(backend_err_to_py)
        })
    }

    /// Unpause a paused workflow so it can be resumed.
    fn unpause(&self, instance_id: String) -> PyResult<()> {
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.unpause(&instance_id))
                .map(|_| ())
                .map_err(backend_err_to_py)
        })
    }

    fn __repr__(&self) -> String {
        "DurableEngine(...)".to_string()
    }
}

/// Convert a `BackendError` to a Python exception.
fn backend_err_to_py(e: sayiir_persistence::BackendError) -> PyErr {
    PyErr::new::<exceptions::BackendError, _>(e.to_string())
}

/// Build the task executor callback for `execute_continuation_with_checkpointing`.
///
/// Returns a closure that acquires the GIL and delegates to `execute_python_task`.
#[allow(clippy::type_complexity)]
fn make_task_executor(
    registry: &Arc<Py<PyDict>>,
) -> impl Fn(
    &str,
    Bytes,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Bytes, sayiir_core::error::BoxError>> + Send>,
> + Send
       + Sync
       + '_ {
    move |task_id: &str, task_input: Bytes| {
        let reg = Arc::clone(registry);
        let task_id = task_id.to_string();
        Box::pin(async move {
            Python::try_attach(|py| {
                execute_python_task(py, &task_id, &task_input, reg.bind(py))
                    .map_err(|e| -> sayiir_core::error::BoxError { e.to_string().into() })
            })
            .unwrap_or_else(|| Err("Failed to acquire Python GIL".into()))
        })
    }
}
