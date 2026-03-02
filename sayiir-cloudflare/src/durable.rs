//! WASM-exposed durable workflow engine with checkpoint-and-exit execution.
//!
//! Provides `WasmDurableEngine` which bridges JS task implementations to the
//! Sayiir checkpointing engine via D1 persistence. Supports run, resume,
//! cancel, pause, and signals.
//!
//! # Execution model
//!
//! Workers are ephemeral — workflows can't park in memory. The execution model:
//!
//! 1. Receive trigger (HTTP request, cron, queue message)
//! 2. Load snapshot from D1
//! 3. Execute tasks until hitting a park point (delay, wait-for-event)
//! 4. Save snapshot to D1
//! 5. Return response with park metadata (wake time, signal name)
//! 6. JS caller schedules wake-up via Queues or external trigger

use std::ops::ControlFlow;
use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use sayiir_core::error::WorkflowError;
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot};
use sayiir_core::workflow::WorkflowContinuation;
use sayiir_d1::D1Backend;
use sayiir_persistence::{SignalStore, SnapshotStore};

use crate::codec::{
    decode_json_string, decode_loop_result, decode_to_js_value, encode_branch_envelope,
    encode_js_value, encode_named_results,
};
use crate::error::{backend_err, to_js_error};
use crate::flow::WasmWorkflow;
use crate::lifecycle::{self, ResumeOutcome};
use crate::status::WasmWorkflowStatus;

/// Durable workflow engine with checkpoint-and-exit execution over D1.
#[wasm_bindgen]
pub struct WasmDurableEngine {
    backend: Arc<D1Backend>,
}

#[wasm_bindgen]
impl WasmDurableEngine {
    /// Create a durable engine with a D1 backend.
    ///
    /// `db` must be a Cloudflare D1 database binding from the Worker env.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the D1 backend initialisation fails.
    pub async fn create(db: sayiir_d1::D1Database) -> Result<WasmDurableEngine, JsValue> {
        let backend = D1Backend::new(db).await.map_err(to_js_error)?;
        Ok(Self {
            backend: Arc::new(backend),
        })
    }

    /// Run a workflow to completion (or until it parks) with checkpointing.
    ///
    /// `task_registry` is a JS object mapping task IDs to functions:
    /// `Record<string, (input: any) => any | Promise<any>>`
    ///
    /// # Errors
    ///
    /// Returns a JS error if snapshot persistence, task execution, or
    /// finalization fails.
    pub async fn run(
        &self,
        workflow: &WasmWorkflow,
        instance_id: String,
        input: JsValue,
        task_registry: js_sys::Object,
    ) -> Result<WasmWorkflowStatus, JsValue> {
        let input_bytes = encode_js_value(&input)?;
        let continuation = Arc::clone(&workflow.continuation);
        let first_task_id = continuation.first_task_id().to_string();

        let mut snapshot = lifecycle::prepare_run(
            instance_id,
            workflow.definition_hash.clone(),
            input_bytes.clone(),
            first_task_id,
            self.backend.as_ref(),
        )
        .await
        .map_err(backend_err)?;

        let result = execute_with_checkpointing(
            &continuation,
            input_bytes,
            &mut snapshot,
            self.backend.as_ref(),
            &task_registry,
        )
        .await;

        let (status, output) =
            lifecycle::finalize_execution(result, &mut snapshot, self.backend.as_ref()).await?;

        Ok(WasmWorkflowStatus::from_core(status, output))
    }

    /// Resume a workflow from a saved checkpoint.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the snapshot cannot be loaded, the definition
    /// hash mismatches, or execution/finalization fails.
    pub async fn resume(
        &self,
        workflow: &WasmWorkflow,
        instance_id: String,
        task_registry: js_sys::Object,
    ) -> Result<WasmWorkflowStatus, JsValue> {
        let continuation = Arc::clone(&workflow.continuation);

        match lifecycle::prepare_resume(
            &instance_id,
            &workflow.definition_hash,
            self.backend.as_ref(),
        )
        .await?
        {
            ResumeOutcome::AlreadyTerminal(status) => {
                let output = if matches!(status, sayiir_core::workflow::WorkflowStatus::Completed) {
                    self.backend
                        .load_snapshot(&instance_id)
                        .await
                        .ok()
                        .and_then(|s| s.state.completed_output().cloned())
                } else {
                    None
                };
                Ok(WasmWorkflowStatus::from_core(status, output))
            }
            ResumeOutcome::Paused(status) | ResumeOutcome::NotReady(status) => {
                Ok(WasmWorkflowStatus::from_core(status, None))
            }
            ResumeOutcome::Ready {
                mut snapshot,
                input_bytes,
            } => {
                let result = execute_with_checkpointing(
                    &continuation,
                    input_bytes,
                    &mut snapshot,
                    self.backend.as_ref(),
                    &task_registry,
                )
                .await;

                let (status, output) =
                    lifecycle::finalize_execution(result, &mut snapshot, self.backend.as_ref())
                        .await?;

                Ok(WasmWorkflowStatus::from_core(status, output))
            }
        }
    }

    /// Request cancellation of a running workflow.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the cancellation signal cannot be stored.
    pub async fn cancel(
        &self,
        instance_id: String,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> Result<(), JsValue> {
        self.backend
            .store_signal(
                &instance_id,
                sayiir_core::snapshot::SignalKind::Cancel,
                sayiir_core::snapshot::SignalRequest::new(reason, cancelled_by),
            )
            .await
            .map_err(backend_err)
    }

    /// Request pausing of a running workflow.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the pause signal cannot be stored.
    pub async fn pause(
        &self,
        instance_id: String,
        reason: Option<String>,
        paused_by: Option<String>,
    ) -> Result<(), JsValue> {
        self.backend
            .store_signal(
                &instance_id,
                sayiir_core::snapshot::SignalKind::Pause,
                sayiir_core::snapshot::SignalRequest::new(reason, paused_by),
            )
            .await
            .map_err(backend_err)
    }

    /// Send an external signal (event) to a workflow instance.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the payload cannot be encoded or the signal
    /// cannot be delivered.
    #[wasm_bindgen(js_name = "sendSignal")]
    pub async fn send_signal(
        &self,
        instance_id: String,
        signal_name: String,
        payload: JsValue,
    ) -> Result<(), JsValue> {
        let payload_bytes = encode_js_value(&payload)?;
        self.backend
            .send_event(&instance_id, &signal_name, payload_bytes)
            .await
            .map_err(backend_err)
    }

    /// Unpause a paused workflow so it can be resumed.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the unpause operation fails.
    pub async fn unpause(&self, instance_id: String) -> Result<(), JsValue> {
        self.backend
            .unpause(&instance_id)
            .await
            .map(|_| ())
            .map_err(backend_err)
    }
}

// ── Executor ────────────────────────────────────────────────────────────

/// `Break` = completed or parked; `Continue` = advance to next node.
type StepResult<'a> = ControlFlow<Result<Bytes, WorkflowError>, (&'a WorkflowContinuation, Bytes)>;

struct Executor<'a> {
    snapshot: &'a mut WorkflowSnapshot,
    backend: &'a D1Backend,
    task_registry: &'a js_sys::Object,
}

fn advance_or_break(next: Option<&WorkflowContinuation>, output: Bytes) -> StepResult<'_> {
    match next {
        Some(n) => ControlFlow::Continue((n, output)),
        None => ControlFlow::Break(Ok(output)),
    }
}

impl<'a> Executor<'a> {
    async fn run(
        &mut self,
        cont: &'a WorkflowContinuation,
        input: Bytes,
    ) -> Result<Bytes, WorkflowError> {
        let mut current = cont;
        let mut current_input = input;
        loop {
            match self.step(current, current_input).await {
                ControlFlow::Continue((next, next_input)) => {
                    current = next;
                    current_input = next_input;
                }
                ControlFlow::Break(result) => return result,
            }
        }
    }

    async fn step(&mut self, cont: &'a WorkflowContinuation, input: Bytes) -> StepResult<'a> {
        let result = match cont {
            WorkflowContinuation::Task { .. } => self.handle_task(cont, input).await,
            WorkflowContinuation::Delay { .. } => self.handle_delay(cont, input).await,
            WorkflowContinuation::AwaitSignal { .. } => self.handle_await_signal(cont, input).await,
            WorkflowContinuation::Fork { .. } => self.handle_fork(cont, input).await,
            WorkflowContinuation::Branch { .. } => self.handle_branch(cont, input).await,
            WorkflowContinuation::Loop { .. } => self.handle_loop(cont, input).await,
            WorkflowContinuation::ChildWorkflow { .. } => {
                self.handle_child_workflow(cont, input).await
            }
        };
        match result {
            Ok(cf) => cf,
            Err(e) => ControlFlow::Break(Err(e)),
        }
    }

    async fn checkpoint(&mut self) -> Result<(), WorkflowError> {
        self.backend
            .save_snapshot(self.snapshot)
            .await
            .map_err(|e| WorkflowError::ResumeError(e.to_string()))
    }

    async fn handle_task(
        &mut self,
        cont: &'a WorkflowContinuation,
        input: Bytes,
    ) -> Result<StepResult<'a>, WorkflowError> {
        let WorkflowContinuation::Task { id, next, .. } = cont else {
            unreachable!()
        };

        check_guards(self.backend, &self.snapshot.instance_id, Some(id)).await?;

        let output =
            if let Some(cached) = self.snapshot.get_task_result(id).map(|r| r.output.clone()) {
                cached
            } else {
                let result = call_js_task(self.task_registry, id, &input).await?;
                self.snapshot
                    .mark_task_completed(id.clone(), result.clone());

                if let Some(next_cont) = next.as_deref() {
                    self.snapshot.update_position(ExecutionPosition::AtTask {
                        task_id: next_cont.first_task_id().to_string(),
                    });
                }
                self.checkpoint().await?;

                check_guards(self.backend, &self.snapshot.instance_id, None).await?;

                result
            };

        Ok(advance_or_break(next.as_deref(), output))
    }

    async fn handle_delay(
        &mut self,
        cont: &'a WorkflowContinuation,
        input: Bytes,
    ) -> Result<StepResult<'a>, WorkflowError> {
        let WorkflowContinuation::Delay { id, duration, next } = cont else {
            unreachable!()
        };

        check_guards(self.backend, &self.snapshot.instance_id, Some(id)).await?;

        if self.snapshot.get_task_result(id).is_some() {
            return Ok(advance_or_break(next.as_deref(), input));
        }

        // Park at delay — save checkpoint and return
        let wake_at =
            Utc::now() + chrono::Duration::from_std(*duration).unwrap_or(chrono::Duration::zero());
        let next_task_id = next.as_deref().map(|n| n.first_task_id().to_string());

        self.snapshot.update_position(ExecutionPosition::AtDelay {
            delay_id: id.clone(),
            entered_at: Utc::now(),
            wake_at,
            next_task_id,
        });
        self.snapshot.mark_task_completed(id.clone(), input);
        self.checkpoint().await?;

        Err(WorkflowError::Waiting { wake_at })
    }

    async fn handle_await_signal(
        &mut self,
        cont: &'a WorkflowContinuation,
        _input: Bytes,
    ) -> Result<StepResult<'a>, WorkflowError> {
        let WorkflowContinuation::AwaitSignal {
            id,
            signal_name,
            timeout,
            next,
        } = cont
        else {
            unreachable!()
        };

        check_guards(self.backend, &self.snapshot.instance_id, Some(id)).await?;

        if let Some(result) = self.snapshot.get_task_result(id) {
            let payload = result.output.clone();
            return Ok(advance_or_break(next.as_deref(), payload));
        }

        // Try to consume a buffered signal
        match self
            .backend
            .consume_event(&self.snapshot.instance_id, signal_name)
            .await
        {
            Ok(Some(payload)) => {
                self.snapshot
                    .mark_task_completed(id.clone(), payload.clone());
                if let Some(next_cont) = next.as_deref() {
                    self.snapshot.update_position(ExecutionPosition::AtTask {
                        task_id: next_cont.first_task_id().to_string(),
                    });
                }
                self.checkpoint().await?;
                Ok(advance_or_break(next.as_deref(), payload))
            }
            Ok(None) => {
                // Park at signal
                let wake_at = timeout.map(|d| {
                    Utc::now() + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::zero())
                });
                let next_task_id = next.as_deref().map(|n| n.first_task_id().to_string());

                self.snapshot.update_position(ExecutionPosition::AtSignal {
                    signal_id: id.clone(),
                    signal_name: signal_name.clone(),
                    wake_at,
                    next_task_id,
                });
                self.checkpoint().await?;

                Err(WorkflowError::AwaitingSignal {
                    signal_id: id.clone(),
                    signal_name: signal_name.clone(),
                    wake_at,
                })
            }
            Err(e) => Err(WorkflowError::ResumeError(e.to_string())),
        }
    }

    async fn handle_fork(
        &mut self,
        cont: &'a WorkflowContinuation,
        input: Bytes,
    ) -> Result<StepResult<'a>, WorkflowError> {
        let WorkflowContinuation::Fork { branches, join, .. } = cont else {
            unreachable!()
        };

        check_guards(self.backend, &self.snapshot.instance_id, None).await?;

        // Execute branches sequentially (Workers are single-threaded)
        let mut branch_results = Vec::with_capacity(branches.len());
        for branch in branches {
            let branch_id = branch.id().to_string();

            if let Some(cached) = self.snapshot.get_task_result(&branch_id) {
                branch_results.push((branch_id, cached.output.clone()));
                continue;
            }

            let output = Box::pin(self.run(branch, input.clone())).await?;
            branch_results.push((branch_id, output));
        }

        let encoded = encode_named_results(&branch_results)
            .map_err(|e| WorkflowError::ResumeError(format!("{e:?}")))?;

        let output = if let Some(join_cont) = join.as_deref() {
            Box::pin(self.run(join_cont, encoded)).await?
        } else {
            encoded
        };

        Ok(advance_or_break(cont.get_next(), output))
    }

    async fn handle_branch(
        &mut self,
        cont: &'a WorkflowContinuation,
        input: Bytes,
    ) -> Result<StepResult<'a>, WorkflowError> {
        let WorkflowContinuation::Branch {
            id,
            branches,
            default,
            next,
            ..
        } = cont
        else {
            unreachable!()
        };

        check_guards(self.backend, &self.snapshot.instance_id, Some(id)).await?;

        if let Some(result) = self.snapshot.get_task_result(id) {
            return Ok(advance_or_break(next.as_deref(), result.output.clone()));
        }

        let key_fn_id = sayiir_core::workflow::key_fn_id(id);
        let key_bytes = call_js_task(self.task_registry, &key_fn_id, &input).await?;
        let key = decode_json_string(&key_bytes).map_err(|e| {
            WorkflowError::ResumeError(format!("Failed to decode branch key: {e:?}"))
        })?;

        let chosen = branches
            .get(&key)
            .map(AsRef::as_ref)
            .or(default.as_deref())
            .ok_or_else(|| WorkflowError::BranchKeyNotFound {
                branch_id: id.clone(),
                key: key.clone(),
            })?;

        let branch_output = Box::pin(self.run(chosen, input)).await?;

        let envelope_bytes = encode_branch_envelope(&key, &branch_output)
            .map_err(|e| WorkflowError::ResumeError(format!("{e:?}")))?;

        self.snapshot
            .mark_task_completed(id.clone(), envelope_bytes.clone());
        self.checkpoint().await?;

        Ok(advance_or_break(next.as_deref(), envelope_bytes))
    }

    async fn handle_loop(
        &mut self,
        cont: &'a WorkflowContinuation,
        input: Bytes,
    ) -> Result<StepResult<'a>, WorkflowError> {
        let WorkflowContinuation::Loop {
            id,
            body,
            max_iterations,
            on_max,
            next,
        } = cont
        else {
            unreachable!()
        };

        check_guards(self.backend, &self.snapshot.instance_id, Some(id)).await?;

        if let Some(result) = self.snapshot.get_task_result(id) {
            return Ok(advance_or_break(next.as_deref(), result.output.clone()));
        }

        let start = self.snapshot.loop_iteration(id);
        let mut loop_input = input;

        for iteration in start..*max_iterations {
            let output = Box::pin(self.run(body, loop_input.clone())).await?;

            // Clear body task results for next iteration
            let body_ser = body.to_serializable();
            for tid in &body_ser.task_ids() {
                self.snapshot.remove_task_result(tid);
            }

            let (tag, inner_bytes) = decode_loop_result(&output)
                .map_err(|e| WorkflowError::ResumeError(format!("{e:?}")))?;

            if tag == sayiir_core::codec::LoopDecision::Done.as_ref() {
                self.snapshot.clear_loop_iteration(id);
                self.snapshot
                    .mark_task_completed(id.clone(), inner_bytes.clone());
                self.checkpoint().await?;
                return Ok(advance_or_break(next.as_deref(), inner_bytes));
            } else if tag == sayiir_core::codec::LoopDecision::Again.as_ref() {
                self.snapshot.set_loop_iteration(id, iteration + 1);
                self.snapshot.update_position(ExecutionPosition::InLoop {
                    loop_id: id.clone(),
                    iteration: iteration + 1,
                    next_task_id: Some(body.first_task_id().to_string()),
                });
                self.checkpoint().await?;
                loop_input = inner_bytes;
            } else {
                return Err(WorkflowError::ResumeError(format!(
                    "Unknown LoopResult tag: '{tag}'"
                )));
            }
        }

        // Max iterations exceeded
        match on_max {
            sayiir_core::workflow::MaxIterationsPolicy::Fail => {
                Err(WorkflowError::MaxIterationsExceeded {
                    loop_id: id.clone(),
                    max_iterations: *max_iterations,
                })
            }
            sayiir_core::workflow::MaxIterationsPolicy::ExitWithLast => {
                self.snapshot.clear_loop_iteration(id);
                self.snapshot
                    .mark_task_completed(id.clone(), loop_input.clone());
                self.checkpoint().await?;
                Ok(advance_or_break(next.as_deref(), loop_input))
            }
        }
    }

    async fn handle_child_workflow(
        &mut self,
        cont: &'a WorkflowContinuation,
        input: Bytes,
    ) -> Result<StepResult<'a>, WorkflowError> {
        let WorkflowContinuation::ChildWorkflow {
            id, child, next, ..
        } = cont
        else {
            unreachable!()
        };

        check_guards(self.backend, &self.snapshot.instance_id, Some(id)).await?;

        if let Some(result) = self.snapshot.get_task_result(id) {
            return Ok(advance_or_break(next.as_deref(), result.output.clone()));
        }

        let output = Box::pin(self.run(child, input)).await?;

        self.snapshot
            .mark_task_completed(id.clone(), output.clone());
        self.checkpoint().await?;

        Ok(advance_or_break(next.as_deref(), output))
    }
}

/// Execute a workflow continuation with checkpointing after each task.
///
/// Walks the continuation tree, executing tasks by calling into JS via the
/// task registry. Checkpoints after each task, checks cancel/pause signals
/// at task boundaries, and parks at delays/signals.
///
/// Fork branches are executed sequentially (Workers are single-threaded).
async fn execute_with_checkpointing(
    continuation: &WorkflowContinuation,
    input: Bytes,
    snapshot: &mut WorkflowSnapshot,
    backend: &D1Backend,
    task_registry: &js_sys::Object,
) -> Result<Bytes, WorkflowError> {
    Executor {
        snapshot,
        backend,
        task_registry,
    }
    .run(continuation, input)
    .await
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Call a JavaScript task function from the registry.
///
/// Looks up `task_id` in the registry object, calls it with the decoded input,
/// and encodes the result back to Bytes. Supports both sync and async (Promise)
/// return values.
async fn call_js_task(
    registry: &js_sys::Object,
    task_id: &str,
    input: &Bytes,
) -> Result<Bytes, WorkflowError> {
    let func: js_sys::Function = js_sys::Reflect::get(registry, &JsValue::from_str(task_id))
        .map_err(|_| WorkflowError::TaskNotFound(task_id.to_string()))?
        .dyn_into()
        .map_err(|_| WorkflowError::TaskNotFound(format!("'{task_id}' is not a function")))?;

    let input_js = decode_to_js_value(input)
        .map_err(|e| WorkflowError::ResumeError(format!("Failed to decode task input: {e:?}")))?;

    let result = func.call1(&JsValue::NULL, &input_js).map_err(|e| {
        let msg = e
            .as_string()
            .or_else(|| {
                js_sys::Reflect::get(&e, &JsValue::from_str("message"))
                    .ok()
                    .and_then(|m| m.as_string())
            })
            .unwrap_or_else(|| format!("{e:?}"));
        WorkflowError::TaskPanicked(format!("Task '{task_id}': {msg}"))
    })?;

    // If the result is a Promise (thenable), await it
    let resolved = if is_thenable(&result) {
        let promise = js_sys::Promise::from(result);
        JsFuture::from(promise).await.map_err(|e| {
            let msg = e
                .as_string()
                .or_else(|| {
                    js_sys::Reflect::get(&e, &JsValue::from_str("message"))
                        .ok()
                        .and_then(|m| m.as_string())
                })
                .unwrap_or_else(|| format!("{e:?}"));
            WorkflowError::TaskPanicked(format!("Task '{task_id}': {msg}"))
        })?
    } else {
        result
    };

    encode_js_value(&resolved)
        .map_err(|e| WorkflowError::ResumeError(format!("Failed to encode task output: {e:?}")))
}

/// Check if a JS value is a thenable (has a `.then` method).
fn is_thenable(value: &JsValue) -> bool {
    if !value.is_object() {
        return false;
    }
    js_sys::Reflect::get(value, &JsValue::from_str("then"))
        .ok()
        .is_some_and(|then| then.is_function())
}

/// Check cancel and pause signals, returning an error if the workflow should stop.
async fn check_guards(
    backend: &D1Backend,
    instance_id: &str,
    scope: Option<&str>,
) -> Result<(), WorkflowError> {
    if backend
        .check_and_cancel(instance_id, scope)
        .await
        .map_err(|e| WorkflowError::ResumeError(e.to_string()))?
    {
        return Err(WorkflowError::cancelled());
    }
    if backend
        .check_and_pause(instance_id)
        .await
        .map_err(|e| WorkflowError::ResumeError(e.to_string()))?
    {
        return Err(WorkflowError::paused());
    }
    Ok(())
}
