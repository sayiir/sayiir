//! Node.js-exposed workflow client for submitting and controlling workflows.
//!
//! Provides `NapiWorkflowClient` which wraps the Rust lifecycle functions for
//! submit, cancel, pause, unpause, signal, and status operations.

use bytes::Bytes;
use napi::Env;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_core::workflow::ConflictPolicy;
use sayiir_persistence::{SignalStore, SnapshotStore, TaskResultStore};
use sayiir_runtime::{PrepareRunOutcome, check_existing_instance, prepare_run};

use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;

use crate::backend::{BackendKind, NapiInMemoryBackend, NapiPostgresBackend, with_backend};
use crate::durable_engine::NapiWorkflowStatus;
use crate::exceptions;
use crate::flow::NapiWorkflow;

/// Client for submitting and controlling workflow instances.
///
/// Unlike `DurableEngine`, the client does **not** execute tasks — it only
/// creates initial snapshots and stores lifecycle signals.
#[napi]
pub struct NapiWorkflowClient {
    backend: BackendKind,
    runtime: tokio::runtime::Runtime,
    conflict_policy: ConflictPolicy,
}

#[napi]
impl NapiWorkflowClient {
    /// Create a workflow client with an in-memory backend.
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

    /// Create a workflow client with a Postgres backend.
    #[napi(factory)]
    pub fn with_postgres(
        backend: &NapiPostgresBackend,
        conflict_policy: Option<String>,
    ) -> Result<Self> {
        let policy = parse_conflict_policy(conflict_policy.as_deref())?;
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

    /// Submit a workflow for execution (does not run tasks).
    #[napi]
    pub fn submit(
        &self,
        env: Env,
        workflow: &NapiWorkflow,
        instance_id: String,
        input: Unknown,
    ) -> Result<NapiWorkflowStatus> {
        let definition_hash = workflow.definition_hash.clone();
        let first_task = workflow.continuation.first_task_hint();
        let conflict_policy = self.conflict_policy;

        // Phase 1: check for existing instance before encoding input.
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
            return Ok(NapiWorkflowStatus::from_core(status, output_bytes));
        }

        // Phase 2: encode input and prepare snapshot (no execution).
        let input_bytes = crate::codec::encode_js_value(&env, input)?;
        let (status, output_bytes) = with_backend!(self, |backend| {
            let backend = Arc::clone(backend);
            self.runtime
                .block_on(async {
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
                .map_err(exceptions::runtime_err_to_napi)?
        });

        Ok(NapiWorkflowStatus::from_core(status, output_bytes))
    }

    /// Request cancellation of a workflow instance.
    #[napi]
    pub fn cancel(
        &self,
        instance_id: String,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> Result<()> {
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

    /// Request pausing of a workflow instance.
    #[napi]
    pub fn pause(
        &self,
        instance_id: String,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> Result<()> {
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

    /// Unpause a paused workflow instance.
    #[napi]
    pub fn unpause(&self, instance_id: String) -> Result<()> {
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.unpause(&instance_id))
                .map(|_| ())
                .map_err(exceptions::backend_err_to_napi)
        })
    }

    /// Send an external signal (event) to a workflow instance.
    #[napi]
    pub fn send_signal(
        &self,
        instance_id: String,
        signal_name: String,
        payload_json: String,
    ) -> Result<()> {
        let payload_bytes = Bytes::from(payload_json.into_bytes());
        with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.send_event(&instance_id, &signal_name, payload_bytes))
                .map_err(exceptions::backend_err_to_napi)
        })
    }

    /// Get a single task result from a workflow instance.
    ///
    /// Returns the JSON-encoded task output, or `null` if the task was never
    /// executed. For completed/failed workflows, the result is recovered from
    /// the backend's history or cache.
    #[napi(js_name = "getTaskResult")]
    pub fn get_task_result(&self, instance_id: String, task_id: String) -> Result<Option<String>> {
        let bytes = with_backend!(self, |backend| {
            self.runtime
                .block_on(backend.load_task_result(&instance_id, &task_id))
                .map_err(exceptions::backend_err_to_napi)?
        });
        Ok(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// Get the current status of a workflow instance.
    #[napi]
    pub fn status(&self, instance_id: String) -> Result<NapiWorkflowStatus> {
        let (status, output_bytes) = with_backend!(self, |backend| {
            self.runtime
                .block_on(async {
                    let snapshot = backend.load_snapshot(&instance_id).await?;
                    let output = snapshot.state.completed_output().cloned();
                    let status = snapshot.state.as_status();
                    Ok::<_, sayiir_persistence::BackendError>((status, output))
                })
                .map_err(exceptions::backend_err_to_napi)?
        });

        Ok(NapiWorkflowStatus::from_core(status, output_bytes))
    }
}

/// Parse an optional conflict policy string.
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
