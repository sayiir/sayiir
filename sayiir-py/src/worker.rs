//! Python-exposed distributed worker.
//!
//! Bridges the Rust `PooledWorker` to Python by wrapping task execution
//! in a GIL-acquiring closure. The worker spawns on a multi-threaded tokio
//! runtime so the polling/heartbeat loop runs independently of the GIL.

use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use std::time::Duration;

use sayiir_core::registry::TaskRegistry;
use sayiir_runtime::{
    ExternalTaskExecutor, ExternalWorkflow, PooledWorker, WorkerHandle, WorkflowIndex,
};

use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;

use crate::backend::{BackendKind, PyInMemoryBackend, PyPostgresBackend};
use crate::engine::execute_python_task;
use crate::flow::PyWorkflow;

/// Distributed workflow worker.
///
/// Polls a backend for available tasks, claims them, and executes them
/// using registered Python task functions.
///
/// Args:
///     `worker_id`: Unique identifier for this worker node
///     backend: Either `InMemoryBackend()` or `PostgresBackend(url)`
///     `poll_interval_secs`: Seconds between polls (default: 5.0)
///     `claim_ttl_secs`: Task claim TTL in seconds (default: 300.0)
#[pyclass]
pub struct PyWorker {
    worker_id: String,
    backend_kind: BackendKind,
    postgres_url: Option<String>,
    poll_interval: Duration,
    claim_ttl: Duration,
    tags: Vec<String>,
}

#[pymethods]
impl PyWorker {
    #[new]
    #[pyo3(signature = (worker_id, backend, poll_interval_secs=5.0, claim_ttl_secs=300.0, tags=None))]
    fn new(
        worker_id: String,
        backend: &Bound<'_, PyAny>,
        poll_interval_secs: f64,
        claim_ttl_secs: f64,
        tags: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let (backend_kind, postgres_url) = extract_backend(backend)?;
        Ok(Self {
            worker_id,
            backend_kind,
            postgres_url,
            poll_interval: Duration::from_secs_f64(poll_interval_secs),
            claim_ttl: Duration::from_secs_f64(claim_ttl_secs),
            tags: tags.unwrap_or_default(),
        })
    }

    /// Start the worker. Returns a handle for lifecycle control.
    ///
    /// Args:
    ///     workflows: List of `(Workflow, task_registry_dict)` tuples
    fn start(
        &self,
        py: Python<'_>,
        workflows: Vec<(PyRef<'_, PyWorkflow>, Py<PyDict>)>,
    ) -> PyResult<PyWorkerHandle> {
        let mut external_workflows: WorkflowIndex = WorkflowIndex::with_capacity(workflows.len());
        let mut registries: Vec<(String, Arc<Py<PyDict>>)> = Vec::with_capacity(workflows.len());

        for (wf, reg) in &workflows {
            external_workflows.insert(
                wf.definition_hash.clone(),
                ExternalWorkflow {
                    continuation: Arc::clone(&wf.continuation),
                    workflow_id: Arc::from(wf.workflow_id.as_str()),
                    metadata_json: wf.metadata_json.as_deref().map(Arc::from),
                },
            );
            registries.push((wf.definition_hash.clone(), Arc::new(reg.clone_ref(py))));
        }

        let registries = Arc::new(registries);
        let executor: ExternalTaskExecutor = Arc::new(move |task_id: &str, input: Bytes| {
            let reg = Arc::clone(&registries);
            let task_id = task_id.to_string();
            Box::pin(async move {
                Python::try_attach(|py| {
                    // Find the right registry for this task — iterate all registries
                    // and try to find the task in each one.
                    for (_, registry) in reg.iter() {
                        let dict = registry.bind(py);
                        if dict.contains(&task_id).unwrap_or(false) {
                            return execute_python_task(py, &task_id, &input, dict).map_err(
                                |e| -> sayiir_core::error::BoxError { e.to_string().into() },
                            );
                        }
                    }
                    Err(format!("Task '{task_id}' not found in any workflow registry").into())
                })
                .unwrap_or_else(|| Err("Failed to acquire Python GIL".into()))
            })
        });

        // Spawn a dedicated thread with a current-thread tokio runtime that
        // drives the actor loop (polling + heartbeats) without holding the GIL.
        let postgres_url = self.postgres_url.clone();
        let in_memory_backend = match &self.backend_kind {
            BackendKind::InMemory(b) => Some(Arc::clone(b)),
            BackendKind::Postgres(_) => None,
        };
        let worker_id = self.worker_id.clone();
        let claim_ttl = self.claim_ttl;
        let poll_interval = self.poll_interval;
        let tags = self.tags.clone();

        let (handle_tx, handle_rx) =
            std::sync::mpsc::sync_channel::<Result<WorkerHandle<BackendKind>, String>>(1);

        let bg_thread = std::thread::Builder::new()
            .name(format!("sayiir-worker-{}", self.worker_id))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = handle_tx.send(Err(e.to_string()));
                        return;
                    }
                };

                // Create a fresh backend on worker's runtime (PgPool affinity).
                let backend_kind = match postgres_url {
                    Some(url) => {
                        match runtime.block_on(PostgresBackend::<JsonCodec>::connect(&url)) {
                            Ok(b) => BackendKind::Postgres(Arc::new(b)),
                            Err(e) => {
                                let _ = handle_tx.send(Err(e.to_string()));
                                return;
                            }
                        }
                    }
                    None => BackendKind::InMemory(
                        in_memory_backend.expect("InMemory backend must be set"),
                    ),
                };

                let worker = PooledWorker::new(&worker_id, backend_kind, TaskRegistry::default())
                    .with_claim_ttl(Some(claim_ttl))
                    .with_tags(tags);

                // We need to enter the runtime context before spawning.
                let _guard = runtime.enter();
                let handle =
                    worker.spawn_with_executor(poll_interval, external_workflows, executor);
                let _ = handle_tx.send(Ok(handle.clone()));

                // Drive the runtime until the worker shuts down.
                runtime.block_on(async {
                    let _ = handle.join().await;
                });
            })
            .map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                    "Failed to spawn worker thread: {e}"
                ))
            })?;

        let handle = handle_rx
            .recv()
            .map_err(|_| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                    "Worker thread exited before sending handle",
                )
            })?
            .map_err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>)?;

        Ok(PyWorkerHandle {
            handle,
            bg_thread: Some(std::sync::Mutex::new(Some(bg_thread))),
        })
    }

    fn __repr__(&self) -> String {
        format!("Worker(id='{}')", self.worker_id)
    }
}

/// Handle for controlling a running worker.
#[pyclass]
pub struct PyWorkerHandle {
    #[allow(dead_code)]
    handle: WorkerHandle<BackendKind>,
    /// Background thread driving the tokio runtime. Joined on drop/join.
    bg_thread: Option<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>>,
}

#[pymethods]
impl PyWorkerHandle {
    /// Request a graceful shutdown.
    fn shutdown(&self) {
        self.handle.shutdown();
    }

    /// Wait for the worker to finish. Releases the GIL while waiting.
    ///
    /// This only waits — call [`shutdown`] first to request a graceful stop.
    fn join(&self, py: Python<'_>) -> PyResult<()> {
        if let Some(mutex) = &self.bg_thread {
            let thread = mutex
                .lock()
                .map_err(|_| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Mutex poisoned"))?
                .take();
            if let Some(jh) = thread {
                py.detach(move || {
                    jh.join().map_err(|_| {
                        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Worker thread panicked")
                    })
                })
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    fn __repr__(&self) -> String {
        "WorkerHandle(...)".to_string()
    }
}

fn extract_backend(backend: &Bound<'_, PyAny>) -> PyResult<(BackendKind, Option<String>)> {
    if let Ok(mem) = backend.extract::<PyInMemoryBackend>() {
        Ok((BackendKind::InMemory(Arc::clone(&mem.inner)), None))
    } else if let Ok(pg) = backend.extract::<PyPostgresBackend>() {
        Ok((
            BackendKind::Postgres(Arc::clone(&pg.inner)),
            Some(pg.url.clone()),
        ))
    } else {
        Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
            "backend must be InMemoryBackend or PostgresBackend",
        ))
    }
}
