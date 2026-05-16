//! Python-exposed workflow client for submitting and controlling workflows.
//!
//! Provides `PyWorkflowClient` which wraps the Rust `WorkflowClient` for
//! submit, cancel, pause, unpause, signal, and status operations.

use pyo3::prelude::*;
use std::sync::Arc;

use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_core::workflow::ConflictPolicy;
use sayiir_persistence::{SignalStore, SnapshotStore, TaskResultStore};
use sayiir_runtime::{PrepareRunOutcome, check_existing_instance, prepare_run};

use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;

use crate::backend::{BackendKind, PyInMemoryBackend, PyPostgresBackend, with_backend};
use crate::codec::{decode_to_pyobject, encode_pyobject};
use crate::engine::PyWorkflowStatus;
use crate::exceptions;
use crate::flow::PyWorkflow;

/// Client for submitting and controlling workflow instances.
///
/// Unlike `DurableEngine`, the client does **not** execute tasks — it only
/// creates initial snapshots and stores lifecycle signals. A `Worker`
/// picks up and executes the work.
///
/// Args:
///     backend: Either `InMemoryBackend()` or `PostgresBackend(url)`
///     `conflict_policy`: What to do when an `instance_id` already exists.
///         One of `"fail"` (default), `"use_existing"`, or `"terminate_existing"`.
#[pyclass]
pub struct PyWorkflowClient {
    backend: BackendKind,
    runtime: tokio::runtime::Runtime,
    conflict_policy: ConflictPolicy,
}

#[pymethods]
impl PyWorkflowClient {
    /// Create a new workflow client.
    #[new]
    #[pyo3(signature = (backend, conflict_policy=None))]
    fn new(backend: &Bound<'_, PyAny>, conflict_policy: Option<&str>) -> PyResult<Self> {
        let policy = parse_conflict_policy(conflict_policy)?;

        if let Ok(mem) = backend.extract::<PyInMemoryBackend>() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            Ok(Self {
                backend: BackendKind::InMemory(Arc::clone(&mem.inner)),
                runtime,
                conflict_policy: policy,
            })
        } else if let Ok(pg) = backend.extract::<PyPostgresBackend>() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            let fresh_backend = runtime
                .block_on(PostgresBackend::<JsonCodec>::connect(&pg.url))
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            Ok(Self {
                backend: BackendKind::Postgres(Arc::new(fresh_backend)),
                runtime,
                conflict_policy: policy,
            })
        } else {
            Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                "backend must be InMemoryBackend or PostgresBackend",
            ))
        }
    }

    /// Submit a workflow for execution (does not run tasks).
    ///
    /// Creates an initial snapshot so a Worker can pick it up.
    ///
    /// Returns a `WorkflowStatus` indicating the outcome.
    fn submit(
        &self,
        py: Python<'_>,
        workflow: &PyWorkflow,
        instance_id: String,
        input: &Bound<'_, PyAny>,
    ) -> PyResult<PyWorkflowStatus> {
        let definition_hash = workflow.definition_hash.clone();
        let first_task = workflow.continuation.first_task_hint();
        let conflict_policy = self.conflict_policy;

        // Phase 1: check for existing instance before encoding input.
        let early_return = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            py.detach(|| {
                self.runtime.block_on(check_existing_instance(
                    &instance_id,
                    &definition_hash,
                    backend.as_ref(),
                    conflict_policy,
                ))
            })
            .map_err(runtime_err_to_py)?
        });
        if let Some((status, output_bytes)) = early_return {
            let mut py_status: PyWorkflowStatus = status.into();
            if let Some(bytes) = output_bytes {
                py_status.output = Some(decode_to_pyobject(py, &bytes)?);
            }
            return Ok(py_status);
        }

        // Phase 2: encode input and prepare snapshot (no execution).
        let input_bytes = encode_pyobject(py, input)?;
        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            py.detach(|| {
                self.runtime.block_on(async {
                    match prepare_run(
                        instance_id,
                        definition_hash,
                        input_bytes,
                        first_task,
                        backend.as_ref(),
                        conflict_policy,
                        true, // prechecked — check_existing_instance already ran
                    )
                    .await?
                    {
                        PrepareRunOutcome::Fresh(_) => {
                            Ok((sayiir_core::workflow::WorkflowStatus::InProgress, None))
                        }
                        PrepareRunOutcome::ExistingStatus(status, output) => Ok((status, output)),
                    }
                })
            })
            .map_err(runtime_err_to_py)?
        });

        let mut py_status: PyWorkflowStatus = status.into();
        if let Some(bytes) = output_bytes {
            py_status.output = Some(decode_to_pyobject(py, &bytes)?);
        }
        Ok(py_status)
    }

    /// Request cancellation of a workflow instance.
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

    /// Request pausing of a workflow instance.
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

    /// Unpause a paused workflow instance.
    fn unpause(&self, instance_id: String) -> PyResult<()> {
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.unpause(&instance_id))
                .map(|_| ())
                .map_err(backend_err_to_py)
        })
    }

    /// Send an external signal (event) to a workflow instance.
    fn send_signal(
        &self,
        py: Python<'_>,
        instance_id: String,
        signal_name: String,
        payload: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let payload_bytes = encode_pyobject(py, payload)?;
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.send_event(&instance_id, &signal_name, payload_bytes))
                .map_err(backend_err_to_py)
        })
    }

    /// Get the current status of a workflow instance.
    fn status(&self, py: Python<'_>, instance_id: String) -> PyResult<PyWorkflowStatus> {
        let (status, output_bytes) = with_backend!(self, |backend| {
            self.runtime
                .block_on(async {
                    let snapshot = backend.load_snapshot(&instance_id).await?;
                    let output = snapshot.state.completed_output().cloned();
                    let status = snapshot.state.as_status();
                    Ok::<_, sayiir_persistence::BackendError>((status, output))
                })
                .map_err(backend_err_to_py)?
        });

        let mut py_status: PyWorkflowStatus = status.into();
        if let Some(bytes) = output_bytes {
            py_status.output = Some(decode_to_pyobject(py, &bytes)?);
        }
        Ok(py_status)
    }

    /// Get a single task result from a workflow instance.
    ///
    /// Returns the deserialized task output, or `None` if the task was never
    /// executed. For completed/failed workflows, the result is recovered from
    /// the backend's history or cache.
    fn get_task_result(
        &self,
        py: Python<'_>,
        instance_id: String,
        task_id: String,
    ) -> PyResult<Option<Py<PyAny>>> {
        let bytes = with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.load_task_result(&instance_id, &task_id))
                .map_err(backend_err_to_py)?
        });
        match bytes {
            Some(b) => Ok(Some(decode_to_pyobject(py, &b)?)),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        "WorkflowClient(...)".to_string()
    }
}

/// Parse an optional conflict policy string.
fn parse_conflict_policy(s: Option<&str>) -> PyResult<ConflictPolicy> {
    match s {
        None => Ok(ConflictPolicy::default()),
        Some(val) => val.parse::<ConflictPolicy>().map_err(|_| {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "invalid conflict_policy: {val:?} (expected \"fail\", \"use_existing\", or \"terminate_existing\")"
            ))
        }),
    }
}

/// Convert a `RuntimeError` to a Python exception.
fn runtime_err_to_py(e: sayiir_runtime::RuntimeError) -> PyErr {
    match &e {
        sayiir_runtime::RuntimeError::Codec(_) => {
            PyErr::new::<exceptions::DeserializationError, _>(e.to_string())
        }
        sayiir_runtime::RuntimeError::Backend(_) => {
            PyErr::new::<exceptions::BackendError, _>(e.to_string())
        }
        sayiir_runtime::RuntimeError::Task(_) => {
            PyErr::new::<exceptions::TaskError, _>(e.to_string())
        }
        sayiir_runtime::RuntimeError::InstanceAlreadyExists(_) => {
            PyErr::new::<exceptions::InstanceAlreadyExistsError, _>(e.to_string())
        }
        _ => PyErr::new::<exceptions::WorkflowError, _>(e.to_string()),
    }
}

/// Convert a `BackendError` to a Python exception.
fn backend_err_to_py(e: sayiir_persistence::BackendError) -> PyErr {
    PyErr::new::<exceptions::BackendError, _>(e.to_string())
}
