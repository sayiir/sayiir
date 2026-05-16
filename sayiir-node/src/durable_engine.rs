//! Node.js-exposed durable workflow engine with checkpointing.
//!
//! Provides `NapiDurableEngine` which bridges JS task implementations
//! to the Rust checkpointing runtime. Supports run, resume, cancel, pause, and signals.

use bytes::Bytes;
use napi::Env;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use sayiir_core::context::{TaskExecutionContext, with_thread_local_task_context};
use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_core::workflow::{
    ConflictPolicy, FlatWorkflowStatus, WorkflowContinuation, WorkflowStatus,
};
use sayiir_persistence::{SignalStore, SnapshotStore};
use sayiir_runtime::{
    PrepareRunOutcome, ResumeOutcome, check_existing_instance,
    execute_continuation_with_checkpointing, finalize_execution, prepare_resume, prepare_run,
};

use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;

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
    pub(crate) fn from_core(status: WorkflowStatus, output: Option<Bytes>) -> Self {
        let output_json = output.and_then(|bytes| {
            std::str::from_utf8(&bytes)
                .ok()
                .map(std::string::ToString::to_string)
        });
        let flat = FlatWorkflowStatus::from(status);
        Self {
            status: flat.status,
            error: flat.error,
            reason: flat.reason,
            cancelled_by: flat.cancelled_by,
            paused_by: flat.paused_by,
            output_json,
            wake_at: flat.wake_at,
            delay_id: flat.delay_id,
            signal_id: flat.signal_id,
            signal_name: flat.signal_name,
        }
    }
}

/// Durable workflow engine with checkpointing, cancellation, and resume.
#[napi]
pub struct NapiDurableEngine {
    backend: BackendKind,
    runtime: tokio::runtime::Runtime,
    conflict_policy: ConflictPolicy,
}

#[napi]
impl NapiDurableEngine {
    /// Create a new durable engine with an in-memory backend.
    ///
    /// `conflict_policy`: `"fail"` (default), `"use_existing"`, or `"terminate_existing"`.
    #[napi(factory)]
    pub fn with_in_memory(
        backend: &NapiInMemoryBackend,
        conflict_policy: Option<String>,
    ) -> Result<Self> {
        let policy = parse_conflict_policy(conflict_policy.as_deref())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

        Ok(Self {
            backend: BackendKind::InMemory(Arc::clone(&backend.inner)),
            runtime,
            conflict_policy: policy,
        })
    }

    /// Create a new durable engine with a Postgres backend.
    ///
    /// `conflict_policy`: `"fail"` (default), `"use_existing"`, or `"terminate_existing"`.
    #[napi(factory)]
    pub fn with_postgres(
        backend: &NapiPostgresBackend,
        conflict_policy: Option<String>,
    ) -> Result<Self> {
        let policy = parse_conflict_policy(conflict_policy.as_deref())?;
        // Create a fresh pool on the engine's own runtime to avoid
        // cross-runtime PgPool affinity issues.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

        let fresh_backend = runtime
            .block_on(PostgresBackend::<JsonCodec>::connect(&backend.url))
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

        Ok(Self {
            backend: BackendKind::Postgres(Arc::new(fresh_backend)),
            runtime,
            conflict_policy: policy,
        })
    }

    /// Run a workflow to completion with checkpointing.
    #[napi]
    pub fn run(
        &self,
        env: Env,
        workflow: &NapiWorkflow,
        instance_id: String,
        input: Unknown,
        task_registry: Object,
    ) -> Result<NapiWorkflowStatus> {
        let continuation = Arc::clone(&workflow.continuation);
        let definition_hash = workflow.definition_hash.clone();
        let first_task = continuation.first_task_hint();

        tracing::info!(
            workflow_id = %workflow.workflow_id,
            %instance_id,
            "starting durable workflow execution"
        );

        let conflict_policy = self.conflict_policy;

        // Phase 1: check for an existing instance before encoding input.
        let early_return = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            self.runtime
                .block_on(check_existing_instance(
                    &instance_id,
                    &definition_hash,
                    backend.as_ref(),
                    conflict_policy,
                ))
                .map_err(exceptions::runtime_err_to_napi)?
        });
        if let Some((status, output_bytes)) = early_return {
            tracing::info!(status = %status.as_ref(), "durable workflow execution finished (existing instance)");
            return Ok(NapiWorkflowStatus::from_core(status, output_bytes));
        }

        // Phase 2: no existing instance (or TerminateExisting) — encode and run.
        let input_bytes = encode_js_value(&env, input)?;
        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            self.runtime
                .block_on(async {
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

                    let wf_metadata_json: Option<Arc<str>> =
                        workflow.metadata_json.as_deref().map(Arc::from);
                    let executor = make_task_executor(
                        &env,
                        &task_registry,
                        &workflow.workflow_id,
                        &snapshot.instance_id,
                        &continuation,
                        wf_metadata_json,
                    )
                    .map_err(|e| {
                        sayiir_runtime::RuntimeError::from(sayiir_core::error::BoxError::from(
                            e.to_string(),
                        ))
                    })?;

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
                .map_err(exceptions::runtime_err_to_napi)?
        });

        tracing::info!(status = %status.as_ref(), "durable workflow execution finished");

        Ok(NapiWorkflowStatus::from_core(status, output_bytes))
    }

    /// Resume a workflow from a saved checkpoint.
    #[napi]
    pub fn resume(
        &self,
        env: Env,
        workflow: &NapiWorkflow,
        instance_id: String,
        task_registry: Object,
    ) -> Result<NapiWorkflowStatus> {
        let continuation = Arc::clone(&workflow.continuation);
        let definition_hash = workflow.definition_hash.clone();

        tracing::info!(
            workflow_id = %workflow.workflow_id,
            %instance_id,
            "resuming workflow from checkpoint"
        );

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
                        let wf_metadata_json: Option<Arc<str>> =
                            workflow.metadata_json.as_deref().map(Arc::from);
                        let executor = make_task_executor(
                            &env,
                            &task_registry,
                            &workflow.workflow_id,
                            &snapshot.instance_id,
                            &continuation,
                            wf_metadata_json,
                        )
                        .map_err(|e| sayiir_runtime::RuntimeError::from(
                            sayiir_core::error::BoxError::from(e.to_string()),
                        ))?;

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
        payload: Unknown,
    ) -> Result<()> {
        let payload_bytes = encode_js_value(&env, payload)?;
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

/// Wrapper around a raw `napi_env` pointer that we assert is `Send`+`Sync`.
///
/// SAFETY: This is only safe when we guarantee execution stays on the main
/// JS thread. We achieve this by using `current_thread` tokio runtime with
/// `block_on`, which runs all futures on the calling thread.
struct SendEnv(napi::sys::napi_env);
unsafe impl Send for SendEnv {}
unsafe impl Sync for SendEnv {}

/// Wrapper around `ObjectRef` that we assert is `Send`+`Sync`.
///
/// SAFETY: Same invariant as `SendEnv` — only safe when execution stays on the
/// main JS thread via `current_thread` tokio runtime with `block_on`.
///
/// `LEAK_CHECK = false` because this ref is intentionally held for the duration
/// of the engine call and cleaned up by the GC when the closure is dropped.
struct SendObjectRef(ObjectRef<false>);
unsafe impl Send for SendObjectRef {}
unsafe impl Sync for SendObjectRef {}

/// Build the task executor callback for `execute_continuation_with_checkpointing`.
///
/// Since we use `current_thread` runtime with `block_on`, the executor never
/// crosses threads. We wrap the raw env pointer in a `SendEnv` to satisfy
/// the Send+Sync bounds.
///
/// We use `ObjectRef` to hold the task registry across async boundaries.
/// Sets `TaskExecutionContext` via thread-local before calling the JS task.
#[allow(clippy::type_complexity)]
fn make_task_executor<'a>(
    env: &Env,
    task_registry: &Object,
    workflow_id: &str,
    instance_id: &str,
    continuation: &'a WorkflowContinuation,
    workflow_metadata_json: Option<Arc<str>>,
) -> Result<
    impl Fn(
        &str,
        Bytes,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = std::result::Result<Bytes, sayiir_core::error::BoxError>,
                > + Send,
        >,
    > + Send
    + Sync
    + 'a,
> {
    let send_env = Arc::new(SendEnv(env.raw()));
    let registry_ref = task_registry.create_ref().map_err(|e| {
        Error::new(
            Status::GenericFailure,
            format!("Failed to create reference to task registry: {e}"),
        )
    })?;
    let registry_ref = Arc::new(SendObjectRef(registry_ref));
    let wf_id: Arc<str> = Arc::from(workflow_id);
    let inst_id: Arc<str> = Arc::from(instance_id);

    Ok(
        move |task_id: &str,
              task_input: Bytes|
              -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = std::result::Result<Bytes, sayiir_core::error::BoxError>,
                    > + Send,
            >,
        > {
            let task_id_owned = task_id.to_string();
            let task_ctx = TaskExecutionContext {
                workflow_id: Arc::clone(&wf_id),
                instance_id: Arc::clone(&inst_id),
                task_id: Arc::from(task_id),
                metadata: continuation.build_task_metadata(task_id),
                workflow_metadata_json: workflow_metadata_json.clone(),
            };
            let registry_ref = Arc::clone(&registry_ref);
            let send_env = Arc::clone(&send_env);

            Box::pin(async move {
                // SAFETY: We know we're on the main JS thread because we use
                // current_thread runtime with block_on.
                let env = Env::from_raw(send_env.0);
                let registry: Object = registry_ref.0.get_value(&env).map_err(
                    |e| -> sayiir_core::error::BoxError {
                        format!("Failed to get registry reference: {e}").into()
                    },
                )?;

                with_thread_local_task_context(task_ctx, || {
                    crate::engine::execute_js_task(&env, &task_id_owned, &task_input, &registry)
                        .map_err(|e| -> sayiir_core::error::BoxError { e.to_string().into() })
                })
            })
        },
    )
}

/// Parse an optional conflict policy string into a `ConflictPolicy`.
fn parse_conflict_policy(s: Option<&str>) -> Result<ConflictPolicy> {
    match s {
        None => Ok(ConflictPolicy::default()),
        Some(val) => val.parse::<ConflictPolicy>().map_err(|_| {
            Error::new(
                Status::InvalidArg,
                format!(
                    "invalid conflictPolicy: {val:?} (expected \"fail\", \"useExisting\", or \"terminateExisting\")"
                ),
            )
        }),
    }
}
