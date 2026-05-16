//! Python-exposed durable workflow engine with checkpointing.
//!
//! Provides `PyDurableEngine` which bridges Python task implementations
//! to the Rust checkpointing runtime. Supports run, resume, and cancel.

use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;

use sayiir_core::context::{TaskExecutionContext, with_thread_local_task_context};
use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_core::workflow::{ConflictPolicy, WorkflowContinuation, WorkflowStatus};
use sayiir_persistence::{SignalStore, SnapshotStore};
use sayiir_runtime::{
    PrepareRunOutcome, ResumeOutcome, check_existing_instance,
    execute_continuation_with_checkpointing, finalize_execution, prepare_resume, prepare_run,
};

use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;

use crate::backend::{BackendKind, PyInMemoryBackend, PyPostgresBackend, with_backend};
use crate::codec::{decode_to_pyobject, encode_pyobject};
use crate::engine::{PyWorkflowStatus, execute_python_task};
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
    conflict_policy: ConflictPolicy,
}

#[pymethods]
impl PyDurableEngine {
    /// Create a new durable engine.
    ///
    /// Args:
    ///     backend: Either `InMemoryBackend()` or `PostgresBackend(url)`
    ///     `conflict_policy`: What to do when an `instance_id` already exists.
    ///         One of `"fail"` (default), `"use_existing"`, or `"terminate_existing"`.
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
            // Create a fresh pool on the engine's own runtime to avoid
            // cross-runtime PgPool affinity issues.
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

    /// Run a workflow to completion with checkpointing.
    fn run(
        &self,
        py: Python<'_>,
        workflow: &PyWorkflow,
        instance_id: String,
        input: &Bound<'_, PyAny>,
        task_registry: Py<PyDict>,
    ) -> PyResult<PyWorkflowStatus> {
        let continuation = Arc::clone(&workflow.continuation);
        let definition_hash = workflow.definition_hash.clone();
        let first_task = continuation.first_task_hint();
        let registry = Arc::new(task_registry);

        tracing::info!(
            workflow_id = %workflow.workflow_id,
            %instance_id,
            "starting durable workflow execution"
        );

        let workflow_id = workflow.workflow_id.clone();
        let workflow_metadata_json: Option<Arc<str>> =
            workflow.metadata_json.as_deref().map(Arc::from);
        let conflict_policy = self.conflict_policy;

        // Phase 1: check for an existing instance before encoding input.
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
            tracing::info!(
                status = %py_status.status,
                "durable workflow execution finished (existing instance)"
            );
            return Ok(py_status);
        }

        // Phase 2: no existing instance (or TerminateExisting) — encode and run.
        let input_bytes = encode_pyobject(py, input)?;
        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            py.detach(|| {
                self.runtime.block_on(async {
                    let mut snapshot = match prepare_run(
                        instance_id,
                        definition_hash,
                        input_bytes.clone(),
                        first_task,
                        backend.as_ref(),
                        conflict_policy,
                        true, // prechecked — check_existing_instance already ran
                    )
                    .await?
                    {
                        PrepareRunOutcome::Fresh(s) => *s,
                        PrepareRunOutcome::ExistingStatus(status, output) => {
                            return Ok((status, output));
                        }
                    };

                    let snap_instance_id = snapshot.instance_id.clone();
                    let executor = make_task_executor(
                        &registry,
                        &workflow_id,
                        &snap_instance_id,
                        &continuation,
                        workflow_metadata_json.clone(),
                    );
                    let result = execute_continuation_with_checkpointing(
                        &continuation,
                        input_bytes,
                        &mut snapshot,
                        backend.as_ref(),
                        &executor,
                        &JsonCodec,
                    )
                    .await;

                    finalize_execution(result, &mut snapshot, backend.as_ref()).await
                })
            })
            .map_err(runtime_err_to_py)?
        });

        let mut py_status: PyWorkflowStatus = status.into();

        tracing::info!(
            status = %py_status.status,
            "durable workflow execution finished"
        );

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
        let workflow_id = workflow.workflow_id.clone();
        let workflow_metadata_json: Option<Arc<str>> =
            workflow.metadata_json.as_deref().map(Arc::from);
        let registry = Arc::new(task_registry);

        tracing::info!(
            workflow_id = %workflow.workflow_id,
            %instance_id,
            "resuming workflow from checkpoint"
        );

        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            py.detach(|| {
                self.runtime.block_on(async {
                    match prepare_resume(&instance_id, &definition_hash, backend.as_ref()).await? {
                        ResumeOutcome::AlreadyTerminal(status) => {
                            tracing::debug!(%instance_id, status = ?status, "workflow already terminal");
                            let output = if matches!(status, WorkflowStatus::Completed) {
                                let snapshot = backend.load_snapshot(&instance_id).await.ok();
                                snapshot.and_then(|s| s.state.completed_output().cloned())
                            } else {
                                None
                            };
                            Ok((status, output))
                        }
                        ResumeOutcome::Paused(status) => {
                            tracing::debug!(%instance_id, "workflow is paused, cannot resume");
                            Ok((status, None))
                        }
                        ResumeOutcome::NotReady(status) => {
                            tracing::debug!(%instance_id, status = ?status, "workflow not ready to resume");
                            Ok((status, None))
                        }
                        ResumeOutcome::Ready {
                            mut snapshot,
                            input_bytes,
                        } => {
                            let snap_instance_id = snapshot.instance_id.clone();
                            let executor = make_task_executor(
                                &registry,
                                &workflow_id,
                                &snap_instance_id,
                                &continuation,
                                workflow_metadata_json.clone(),
                            );
                            let result = execute_continuation_with_checkpointing(
                                &continuation,
                                input_bytes,
                                &mut snapshot,
                                backend.as_ref(),
                                &executor,
                                &JsonCodec,
                            )
                            .await;

                            finalize_execution(result, &mut snapshot, backend.as_ref()).await
                        }
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

    /// Request cancellation of a running workflow.
    #[pyo3(signature = (instance_id, reason=None, cancelled_by=None))]
    fn cancel(
        &self,
        instance_id: String,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> PyResult<()> {
        tracing::info!(%instance_id, ?reason, ?cancelled_by, "requesting workflow cancellation");
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
        tracing::info!(%instance_id, ?reason, ?paused_by, "requesting workflow pause");
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

    /// Send an external signal (event) to a workflow instance.
    ///
    /// The payload is buffered per (`instance_id`, `signal_name`) in FIFO order.
    /// The next time the workflow resumes and reaches the matching
    /// `wait_for_signal` node, it will consume the oldest buffered event.
    fn send_signal(
        &self,
        py: Python<'_>,
        instance_id: String,
        signal_name: String,
        payload: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let payload_bytes = encode_pyobject(py, payload)?;
        tracing::info!(%instance_id, %signal_name, "sending external signal");
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.send_event(&instance_id, &signal_name, payload_bytes))
                .map_err(backend_err_to_py)
        })
    }

    /// Unpause a paused workflow so it can be resumed.
    fn unpause(&self, instance_id: String) -> PyResult<()> {
        tracing::info!(%instance_id, "unpausing workflow");
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

/// Parse an optional conflict policy string into a `ConflictPolicy`.
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

/// Convert a `RuntimeError` to a Python exception with proper dispatch.
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

/// Build the task executor callback for `execute_continuation_with_checkpointing`.
///
/// Returns a closure that acquires the GIL and delegates to `execute_python_task`.
/// Sets `TaskExecutionContext` via thread-local before calling the Python task.
#[allow(clippy::type_complexity)]
fn make_task_executor<'a>(
    registry: &'a Arc<Py<PyDict>>,
    workflow_id: &'a str,
    instance_id: &'a str,
    continuation: &'a WorkflowContinuation,
    workflow_metadata_json: Option<Arc<str>>,
) -> impl Fn(
    &str,
    Bytes,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Bytes, sayiir_core::error::BoxError>> + Send>,
> + Send
+ Sync
+ 'a {
    move |task_id: &str, task_input: Bytes| {
        let reg = Arc::clone(registry);
        let task_id_owned = task_id.to_string();
        let task_ctx = TaskExecutionContext {
            workflow_id: Arc::from(workflow_id),
            instance_id: Arc::from(instance_id),
            task_id: Arc::from(task_id),
            metadata: continuation.build_task_metadata(task_id),
            workflow_metadata_json: workflow_metadata_json.clone(),
        };
        Box::pin(async move {
            Python::try_attach(|py| {
                with_thread_local_task_context(task_ctx, || {
                    execute_python_task(py, &task_id_owned, &task_input, reg.bind(py))
                        .map_err(|e| -> sayiir_core::error::BoxError { e.to_string().into() })
                })
            })
            .unwrap_or_else(|| Err("Failed to acquire Python GIL".into()))
        })
    }
}
