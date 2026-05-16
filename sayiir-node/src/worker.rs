//! Node.js-exposed distributed worker.
//!
//! Bridges the Rust `PooledWorker` to Node.js using `ThreadsafeFunction`
//! for cross-thread JS task execution. The worker runs on a dedicated
//! background thread with its own tokio runtime.

use bytes::Bytes;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use std::sync::Arc;
use std::time::Duration;

use sayiir_core::registry::TaskRegistry;
use sayiir_runtime::{
    ExternalTaskExecutor, ExternalWorkflow, PooledWorker, WorkerHandle, WorkflowIndex,
};

use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;

use crate::backend::{BackendKind, NapiInMemoryBackend, NapiPostgresBackend};
use crate::flow::NapiWorkflow;

/// Distributed workflow worker.
#[napi]
pub struct NapiWorker {
    worker_id: String,
    backend_kind: BackendKind,
    postgres_url: Option<String>,
    poll_interval: Duration,
    claim_ttl: Duration,
    tags: Vec<String>,
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
        tags: Option<Vec<String>>,
    ) -> Self {
        Self {
            worker_id,
            backend_kind: BackendKind::InMemory(Arc::clone(&backend.inner)),
            postgres_url: None,
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
            tags: tags.unwrap_or_default(),
        }
    }

    /// Create a worker with a Postgres backend.
    #[napi(factory)]
    pub fn with_postgres(
        worker_id: String,
        backend: &NapiPostgresBackend,
        poll_interval_ms: Option<f64>,
        claim_ttl_ms: Option<f64>,
        tags: Option<Vec<String>>,
    ) -> Self {
        Self {
            worker_id,
            backend_kind: BackendKind::Postgres(Arc::clone(&backend.inner)),
            postgres_url: Some(backend.url.clone()),
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
            tags: tags.unwrap_or_default(),
        }
    }

    /// Start the worker with the given workflows and task executor.
    ///
    /// The `task_executor` JS function receives a single JSON string
    /// `{ taskId: string, input: unknown }` and must return a `Promise<string>`
    /// with the JSON-serialized output.
    #[napi]
    #[allow(clippy::too_many_lines)]
    pub fn start(
        &self,
        workflows: Vec<&NapiWorkflow>,
        #[napi(ts_arg_type = "(payload: string) => Promise<string>")] task_executor: Function<
            String,
            Promise<String>,
        >,
    ) -> Result<NapiWorkerHandle> {
        let external_workflows: WorkflowIndex = workflows
            .iter()
            .map(|wf| {
                (
                    wf.definition_hash.clone(),
                    ExternalWorkflow {
                        continuation: Arc::clone(&wf.continuation),
                        workflow_id: Arc::from(wf.workflow_id.as_str()),
                        metadata_json: wf.metadata_json.as_deref().map(Arc::from),
                    },
                )
            })
            .collect();

        // Build a ThreadsafeFunction from the JS executor.
        let tsfn: ThreadsafeFunction<String, Promise<String>, _, _, true> = task_executor
            .build_threadsafe_function()
            .callee_handled::<true>()
            .build()?;

        let tsfn = Arc::new(tsfn);

        let executor: ExternalTaskExecutor = Arc::new(move |task_id: &str, input: Bytes| {
            let tsfn: Arc<ThreadsafeFunction<String, Promise<String>, _, _, true>> =
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
                let output_json: String = tsfn
                    .call_async(Ok(payload))
                    .await
                    .map_err(|e| -> sayiir_core::error::BoxError { e.to_string().into() })?
                    .await
                    .map_err(|e| -> sayiir_core::error::BoxError { e.to_string().into() })?;
                Ok(Bytes::from(output_json.into_bytes()))
            })
        });

        // Spawn worker on a dedicated background thread.
        let postgres_url = self.postgres_url.clone();
        let in_memory_backend = match &self.backend_kind {
            BackendKind::InMemory(b) => Some(Arc::clone(b)),
            BackendKind::Postgres(_) => None,
        };
        let worker_id = self.worker_id.clone();
        let claim_ttl = self.claim_ttl;
        let poll_interval = self.poll_interval;
        let tags = self.tags.clone();

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

                // Create a fresh backend on the worker's own runtime to avoid
                // cross-runtime PgPool affinity issues.
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
                    None => {
                        // InMemory backend — no pool affinity issue, reuse the shared Arc.
                        BackendKind::InMemory(
                            in_memory_backend.expect("InMemory backend must be set"),
                        )
                    }
                };

                let worker = PooledWorker::new(&worker_id, backend_kind, TaskRegistry::default())
                    .with_claim_ttl(Some(claim_ttl))
                    .with_tags(tags);

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
}
