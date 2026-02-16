//! Node.js-exposed distributed worker.
//!
//! Bridges the Rust `PooledWorker` to Node.js using `ThreadsafeFunction`
//! for cross-thread JS task execution. The worker runs on a dedicated
//! background thread with its own tokio runtime.

use bytes::Bytes;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction};
use napi_derive::napi;
use std::sync::Arc;
use std::time::Duration;

use sayiir_core::registry::TaskRegistry;
use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_persistence::SignalStore;
use sayiir_runtime::{
    ExternalTaskExecutor, ExternalWorkflow, PooledWorker, WorkerHandle, WorkflowIndex,
};

use crate::backend::{BackendKind, NapiInMemoryBackend, NapiPostgresBackend};
use crate::exceptions;
use crate::flow::NapiWorkflow;

/// Distributed workflow worker.
#[napi]
pub struct NapiWorker {
    worker_id: String,
    backend_kind: BackendKind,
    poll_interval: Duration,
    claim_ttl: Duration,
}

#[napi]
impl NapiWorker {
    /// Create a worker with an in-memory backend.
    #[napi(factory)]
    pub fn with_in_memory(
        worker_id: String,
        backend: &NapiInMemoryBackend,
        poll_interval_ms: Option<f64>,
        claim_ttl_ms: Option<f64>,
    ) -> Self {
        Self {
            worker_id,
            backend_kind: BackendKind::InMemory(Arc::clone(&backend.inner)),
            poll_interval: Duration::from_millis(
                #[allow(clippy::cast_sign_loss)]
                {
                    poll_interval_ms.unwrap_or(5000.0) as u64
                },
            ),
            claim_ttl: Duration::from_millis(
                #[allow(clippy::cast_sign_loss)]
                {
                    claim_ttl_ms.unwrap_or(300_000.0) as u64
                },
            ),
        }
    }

    /// Create a worker with a Postgres backend.
    #[napi(factory)]
    pub fn with_postgres(
        worker_id: String,
        backend: &NapiPostgresBackend,
        poll_interval_ms: Option<f64>,
        claim_ttl_ms: Option<f64>,
    ) -> Self {
        Self {
            worker_id,
            backend_kind: BackendKind::Postgres(Arc::clone(&backend.inner)),
            poll_interval: Duration::from_millis(
                #[allow(clippy::cast_sign_loss)]
                {
                    poll_interval_ms.unwrap_or(5000.0) as u64
                },
            ),
            claim_ttl: Duration::from_millis(
                #[allow(clippy::cast_sign_loss)]
                {
                    claim_ttl_ms.unwrap_or(300_000.0) as u64
                },
            ),
        }
    }

    /// Start the worker with the given workflows and task executor.
    ///
    /// The `task_executor` JS function receives a single JSON string
    /// `{ taskId: string, input: unknown }` and must return a `Promise<string>`
    /// with the JSON-serialized output.
    #[napi]
    pub fn start(
        &self,
        workflows: Vec<&NapiWorkflow>,
        #[napi(ts_arg_type = "(payload: string) => Promise<string>")] task_executor: JsFunction,
    ) -> Result<NapiWorkerHandle> {
        let external_workflows: WorkflowIndex = workflows
            .iter()
            .map(|wf| {
                (
                    wf.definition_hash.clone(),
                    ExternalWorkflow {
                        continuation: Arc::clone(&wf.continuation),
                    },
                )
            })
            .collect();

        // Create a ThreadsafeFunction from the JS executor.
        // We pass a single JSON string containing both task_id and input.
        // The JS function signature: (payload: string) => Promise<string>
        let tsfn: ThreadsafeFunction<String, ErrorStrategy::CalleeHandled> = task_executor
            .create_threadsafe_function(
                0,
                |ctx: napi::threadsafe_function::ThreadSafeCallContext<String>| {
                    Ok(vec![ctx.env.create_string(&ctx.value)?.into_unknown()])
                },
            )?;

        let tsfn = Arc::new(tsfn);

        let executor: ExternalTaskExecutor = Arc::new(move |task_id: &str, input: Bytes| {
            let tsfn: Arc<ThreadsafeFunction<String, ErrorStrategy::CalleeHandled>> =
                Arc::clone(&tsfn);
            let task_id = task_id.to_string();
            let input_json = String::from_utf8_lossy(&input).into_owned();
            Box::pin(async move {
                // Encode task_id + input as a JSON object for the single-arg tsfn
                let payload = serde_json::json!({
                    "taskId": task_id,
                    "input": serde_json::from_str::<serde_json::Value>(&input_json)
                        .unwrap_or(serde_json::Value::Null),
                })
                .to_string();
                let promise: Promise<String> = tsfn
                    .call_async(Ok(payload))
                    .await
                    .map_err(|e| -> sayiir_core::error::BoxError { e.to_string().into() })?;
                let output_json: String = promise
                    .await
                    .map_err(|e: Error| -> sayiir_core::error::BoxError { e.to_string().into() })?;
                Ok(Bytes::from(output_json.into_bytes()))
            })
        });

        // Spawn worker on a dedicated background thread.
        let backend_kind = clone_backend(&self.backend_kind);
        let worker_id = self.worker_id.clone();
        let claim_ttl = self.claim_ttl;
        let poll_interval = self.poll_interval;

        let (handle_tx, handle_rx) = std::sync::mpsc::sync_channel::<
            std::result::Result<WorkerHandle<BackendKind>, String>,
        >(1);

        std::thread::Builder::new()
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

                let worker = PooledWorker::new(&worker_id, backend_kind, TaskRegistry::default())
                    .with_claim_ttl(Some(claim_ttl));

                let _guard = runtime.enter();
                let handle =
                    worker.spawn_with_executor(poll_interval, external_workflows, executor);
                let join_handle = handle.clone();
                let _ = handle_tx.send(Ok(handle));

                runtime.block_on(async {
                    let _ = join_handle.join().await;
                });
            })
            .map_err(|e| {
                Error::new(
                    Status::GenericFailure,
                    format!("Failed to spawn worker thread: {e}"),
                )
            })?;

        let handle = handle_rx
            .recv()
            .map_err(|_| {
                Error::new(
                    Status::GenericFailure,
                    "Worker thread exited before sending handle",
                )
            })?
            .map_err(|e| Error::new(Status::GenericFailure, e))?;

        Ok(NapiWorkerHandle { handle })
    }
}

/// Handle for controlling a running worker.
#[napi]
pub struct NapiWorkerHandle {
    handle: WorkerHandle<BackendKind>,
}

#[napi]
impl NapiWorkerHandle {
    /// Request a graceful shutdown.
    #[napi]
    pub fn shutdown(&self) {
        self.handle.shutdown();
    }

    /// Request cancellation of a workflow.
    #[napi]
    pub fn cancel_workflow(
        &self,
        instance_id: String,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> Result<()> {
        run_blocking(async {
            self.handle
                .backend()
                .store_signal(
                    &instance_id,
                    SignalKind::Cancel,
                    SignalRequest::new(reason, cancelled_by),
                )
                .await
        })
        .map_err(exceptions::backend_err_to_napi)
    }

    /// Request pausing of a workflow.
    #[napi]
    pub fn pause_workflow(
        &self,
        instance_id: String,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> Result<()> {
        run_blocking(async {
            self.handle
                .backend()
                .store_signal(
                    &instance_id,
                    SignalKind::Pause,
                    SignalRequest::new(reason, paused_by),
                )
                .await
        })
        .map_err(exceptions::backend_err_to_napi)
    }

    /// Unpause a paused workflow.
    #[napi]
    pub fn unpause_workflow(&self, instance_id: String) -> Result<()> {
        run_blocking(async {
            self.handle
                .backend()
                .unpause(&instance_id)
                .await
                .map(|_| ())
        })
        .map_err(exceptions::backend_err_to_napi)
    }

    /// Send an external signal to a workflow.
    #[napi]
    pub fn send_signal(
        &self,
        instance_id: String,
        signal_name: String,
        payload_json: String,
    ) -> Result<()> {
        let payload_bytes = Bytes::from(payload_json.into_bytes());
        run_blocking(async {
            self.handle
                .backend()
                .send_event(&instance_id, &signal_name, payload_bytes)
                .await
        })
        .map_err(exceptions::backend_err_to_napi)
    }
}

fn clone_backend(kind: &BackendKind) -> BackendKind {
    match kind {
        BackendKind::InMemory(b) => BackendKind::InMemory(Arc::clone(b)),
        BackendKind::Postgres(b) => BackendKind::Postgres(Arc::clone(b)),
    }
}

/// Run a future on a throwaway current-thread runtime.
fn run_blocking(
    f: impl std::future::Future<Output = std::result::Result<(), sayiir_persistence::BackendError>>,
) -> std::result::Result<(), sayiir_persistence::BackendError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| sayiir_persistence::BackendError::Backend(e.to_string()))?;
    rt.block_on(f)
}
