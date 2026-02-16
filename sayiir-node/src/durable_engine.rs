//! Node.js-exposed durable workflow engine with checkpointing.
//!
//! Provides `NapiDurableEngine` which bridges JS task implementations
//! to the Rust checkpointing runtime. Supports run, resume, cancel, pause, and signals.

use bytes::Bytes;
use napi::bindgen_prelude::*;
use napi::{Env, JsObject, JsUnknown};
use napi_derive::napi;
use std::sync::Arc;

use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_core::workflow::WorkflowStatus;
use sayiir_persistence::{SignalStore, SnapshotStore};
use sayiir_runtime::{
    ResumeOutcome, execute_continuation_with_checkpointing, finalize_execution, prepare_resume,
    prepare_run,
};

use crate::backend::{BackendKind, NapiInMemoryBackend, NapiPostgresBackend, with_backend};
use crate::codec::encode_js_value;
use crate::exceptions;
use crate::flow::NapiWorkflow;

/// Workflow status result from durable execution.
#[napi(object)]
pub struct NapiWorkflowStatus {
    pub status: String,
    pub error: Option<String>,
    pub reason: Option<String>,
    pub cancelled_by: Option<String>,
    pub paused_by: Option<String>,
    /// JSON-serialized output (decoded in TypeScript layer).
    pub output_json: Option<String>,
    /// ISO-8601 wake-up timestamp for `waiting` and `awaiting_signal` statuses.
    pub wake_at: Option<String>,
    /// Delay step identifier (present when status is `waiting`).
    pub delay_id: Option<String>,
    /// Signal step identifier (present when status is `awaiting_signal`).
    pub signal_id: Option<String>,
    /// Signal name (present when status is `awaiting_signal`).
    pub signal_name: Option<String>,
}

impl NapiWorkflowStatus {
    fn from_core(status: WorkflowStatus, output: Option<Bytes>) -> Self {
        let output_json = output.and_then(|bytes| {
            std::str::from_utf8(&bytes)
                .ok()
                .map(std::string::ToString::to_string)
        });

        let mut result = Self {
            status: String::new(),
            error: None,
            reason: None,
            cancelled_by: None,
            paused_by: None,
            output_json,
            wake_at: None,
            delay_id: None,
            signal_id: None,
            signal_name: None,
        };

        match status {
            WorkflowStatus::Completed => {
                result.status = "completed".to_string();
            }
            WorkflowStatus::InProgress => {
                result.status = "in_progress".to_string();
            }
            WorkflowStatus::Failed(e) => {
                result.status = "failed".to_string();
                result.error = Some(e);
            }
            WorkflowStatus::Cancelled {
                reason,
                cancelled_by,
            } => {
                result.status = "cancelled".to_string();
                result.reason = reason;
                result.cancelled_by = cancelled_by;
            }
            WorkflowStatus::Paused { reason, paused_by } => {
                result.status = "paused".to_string();
                result.reason = reason;
                result.paused_by = paused_by;
            }
            WorkflowStatus::Waiting { wake_at, delay_id } => {
                result.status = "waiting".to_string();
                result.wake_at = Some(wake_at.to_rfc3339());
                result.delay_id = Some(delay_id);
            }
            WorkflowStatus::AwaitingSignal {
                signal_id,
                signal_name,
                wake_at,
            } => {
                result.status = "awaiting_signal".to_string();
                result.signal_id = Some(signal_id);
                result.signal_name = Some(signal_name);
                result.wake_at = wake_at.map(|t| t.to_rfc3339());
            }
        }

        result
    }
}

/// Durable workflow engine with checkpointing, cancellation, and resume.
#[napi]
pub struct NapiDurableEngine {
    backend: BackendKind,
    runtime: tokio::runtime::Runtime,
}

#[napi]
impl NapiDurableEngine {
    /// Create a new durable engine with an in-memory backend.
    #[napi(factory)]
    pub fn with_in_memory(backend: &NapiInMemoryBackend) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

        Ok(Self {
            backend: BackendKind::InMemory(Arc::clone(&backend.inner)),
            runtime,
        })
    }

    /// Create a new durable engine with a Postgres backend.
    #[napi(factory)]
    pub fn with_postgres(backend: &NapiPostgresBackend) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

        Ok(Self {
            backend: BackendKind::Postgres(Arc::clone(&backend.inner)),
            runtime,
        })
    }

    /// Run a workflow to completion with checkpointing.
    #[napi]
    pub fn run(
        &self,
        env: Env,
        workflow: &NapiWorkflow,
        instance_id: String,
        input: JsUnknown,
        task_registry: JsObject,
    ) -> Result<NapiWorkflowStatus> {
        let input_bytes = encode_js_value(&env, &input)?;
        let continuation = Arc::clone(&workflow.continuation);
        let definition_hash = workflow.definition_hash.clone();
        let first_task_id = continuation.first_task_id().to_string();

        tracing::info!(
            workflow_id = %workflow.workflow_id,
            %instance_id,
            "starting durable workflow execution"
        );

        let executor = make_task_executor(&env, &task_registry);

        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            self.runtime
                .block_on(async {
                    let mut snapshot = prepare_run(
                        instance_id,
                        definition_hash,
                        input_bytes.clone(),
                        first_task_id,
                        backend.as_ref(),
                    )
                    .await?;

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
                .map_err(|e: sayiir_runtime::RuntimeError| {
                    exceptions::workflow_error(e.to_string())
                })?
        });

        tracing::info!(status = %status_to_str(&status), "durable workflow execution finished");

        Ok(NapiWorkflowStatus::from_core(status, output_bytes))
    }

    /// Resume a workflow from a saved checkpoint.
    #[napi]
    pub fn resume(
        &self,
        env: Env,
        workflow: &NapiWorkflow,
        instance_id: String,
        task_registry: JsObject,
    ) -> Result<NapiWorkflowStatus> {
        let continuation = Arc::clone(&workflow.continuation);
        let definition_hash = workflow.definition_hash.clone();

        tracing::info!(
            workflow_id = %workflow.workflow_id,
            %instance_id,
            "resuming workflow from checkpoint"
        );

        let executor = make_task_executor(&env, &task_registry);

        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
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
            .map_err(|e: sayiir_runtime::RuntimeError| {
                exceptions::workflow_error(e.to_string())
            })?
        });

        Ok(NapiWorkflowStatus::from_core(status, output_bytes))
    }

    /// Request cancellation of a running workflow.
    #[napi]
    pub fn cancel(
        &self,
        instance_id: String,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> Result<()> {
        tracing::info!(%instance_id, ?reason, ?cancelled_by, "requesting workflow cancellation");
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.store_signal(
                    &instance_id,
                    SignalKind::Cancel,
                    SignalRequest::new(reason, cancelled_by),
                ))
                .map_err(exceptions::backend_err_to_napi)
        })
    }

    /// Request pausing of a running workflow.
    #[napi]
    pub fn pause(
        &self,
        instance_id: String,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> Result<()> {
        tracing::info!(%instance_id, ?reason, ?paused_by, "requesting workflow pause");
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.store_signal(
                    &instance_id,
                    SignalKind::Pause,
                    SignalRequest::new(reason, paused_by),
                ))
                .map_err(exceptions::backend_err_to_napi)
        })
    }

    /// Send an external signal (event) to a workflow instance.
    #[napi]
    pub fn send_signal(
        &self,
        env: Env,
        instance_id: String,
        signal_name: String,
        payload: JsUnknown,
    ) -> Result<()> {
        let payload_bytes = encode_js_value(&env, &payload)?;
        tracing::info!(%instance_id, %signal_name, "sending external signal");
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.send_event(&instance_id, &signal_name, payload_bytes))
                .map_err(exceptions::backend_err_to_napi)
        })
    }

    /// Unpause a paused workflow so it can be resumed.
    #[napi]
    pub fn unpause(&self, instance_id: String) -> Result<()> {
        tracing::info!(%instance_id, "unpausing workflow");
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.unpause(&instance_id))
                .map(|_| ())
                .map_err(exceptions::backend_err_to_napi)
        })
    }
}

fn status_to_str(s: &WorkflowStatus) -> &'static str {
    match s {
        WorkflowStatus::Completed => "completed",
        WorkflowStatus::InProgress => "in_progress",
        WorkflowStatus::Failed(_) => "failed",
        WorkflowStatus::Cancelled { .. } => "cancelled",
        WorkflowStatus::Paused { .. } => "paused",
        WorkflowStatus::Waiting { .. } => "waiting",
        WorkflowStatus::AwaitingSignal { .. } => "awaiting_signal",
    }
}

/// Wrapper around a raw `napi_env` pointer that we assert is `Send`+`Sync`.
///
/// SAFETY: This is only safe when we guarantee execution stays on the main
/// JS thread. We achieve this by using `current_thread` tokio runtime with
/// `block_on`, which runs all futures on the calling thread.
struct SendEnv(napi::sys::napi_env);
unsafe impl Send for SendEnv {}
unsafe impl Sync for SendEnv {}

/// Build the task executor callback for `execute_continuation_with_checkpointing`.
///
/// Since we use `current_thread` runtime with `block_on`, the executor never
/// crosses threads. We wrap the raw env pointer in a `SendEnv` to satisfy
/// the Send+Sync bounds.
#[allow(clippy::type_complexity)]
fn make_task_executor(
    env: &Env,
    task_registry: &JsObject,
) -> impl Fn(
    &str,
    Bytes,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = std::result::Result<Bytes, sayiir_core::error::BoxError>>
            + Send,
    >,
> + Send
+ Sync {
    let send_env = Arc::new(SendEnv(env.raw()));
    let registry_ref = env
        .create_reference(task_registry)
        .unwrap_or_else(|_| panic!("Failed to create reference to task registry"));
    let registry_ref = Arc::new(registry_ref);

    move |task_id: &str,
          task_input: Bytes|
          -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = std::result::Result<Bytes, sayiir_core::error::BoxError>,
                > + Send,
        >,
    > {
        let task_id = task_id.to_string();
        let registry_ref = Arc::clone(&registry_ref);
        let send_env = Arc::clone(&send_env);

        Box::pin(async move {
            // SAFETY: We know we're on the main JS thread because we use
            // current_thread runtime with block_on.
            let env = unsafe { Env::from_raw(send_env.0) };
            let registry: JsObject = env.get_reference_value(&registry_ref).map_err(
                |e| -> sayiir_core::error::BoxError {
                    format!("Failed to get registry reference: {e}").into()
                },
            )?;

            crate::engine::execute_js_task(&env, &task_id, &task_input, &registry)
                .map_err(|e| -> sayiir_core::error::BoxError { e.to_string().into() })
        })
    }
}
