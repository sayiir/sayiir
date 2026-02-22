//! Node.js-exposed workflow engine.
//!
//! Two execution modes:
//!   1. **Sync** — `NapiWorkflowEngine.run()` executes all tasks synchronously
//!      in Rust. Fast path for workflows with no async tasks.
//!   2. **Stepper** — `NapiContinuationStepper` yields `(taskId, input)` pairs
//!      to JavaScript one at a time, receives outputs, and advances the
//!      continuation. The TS layer drives the loop with `await`, enabling
//!      true async task support (fetch, timers, file I/O).
//!
//! The stepper exists because Node.js only drains V8 microtasks at the
//! *outermost* napi callback scope boundary. Inside `engine.run()`, we're
//! nested, so no inner call can trigger microtask draining. The stepper
//! returns control to JS after each task, allowing microtasks to drain
//! naturally between steps.

use bytes::Bytes;
use napi::Env;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use sayiir_core::codec::LoopDecision;
use sayiir_core::context::{TaskExecutionContext, with_thread_local_task_context};
use sayiir_core::workflow::{MaxIterationsPolicy, WorkflowContinuation};
use sayiir_runtime::execute_continuation_sync;

use crate::codec::{decode_to_js_value, encode_js_value};
use crate::flow::NapiWorkflow;

// ── Sync engine (fast path for sync-only tasks) ──────────────────────

/// Workflow engine for simple (non-durable) execution.
#[napi]
pub struct NapiWorkflowEngine;

#[napi]
impl NapiWorkflowEngine {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self
    }

    /// Run a workflow to completion synchronously.
    ///
    /// All tasks must return plain values (not Promises). For async tasks,
    /// use the stepper-based `runWorkflow()` from TypeScript instead.
    #[napi]
    pub fn run<'env>(
        &self,
        env: &'env Env,
        workflow: &NapiWorkflow,
        input: Unknown,
        task_registry: Object,
    ) -> Result<Unknown<'env>> {
        let input_bytes = encode_js_value(env, input)?;
        let continuation = Arc::clone(&workflow.continuation);

        tracing::info!(workflow_id = %workflow.workflow_id, "starting workflow execution");

        let workflow_id: Arc<str> = Arc::from(workflow.workflow_id.as_str());
        let workflow_metadata_json: Option<Arc<str>> =
            workflow.metadata_json.as_deref().map(Arc::from);
        let result = execute_continuation_sync(
            &continuation,
            input_bytes,
            &|task_id, input| {
                let task_ctx = TaskExecutionContext {
                    workflow_id: Arc::clone(&workflow_id),
                    instance_id: Arc::clone(&workflow_id), // sync path: no instance_id
                    task_id: Arc::from(task_id),
                    metadata: continuation.build_task_metadata(task_id),
                    workflow_metadata_json: workflow_metadata_json.clone(),
                };
                with_thread_local_task_context(task_ctx, || {
                    execute_js_task(env, task_id, &input, &task_registry).map_err(|e| {
                        let msg: sayiir_core::error::BoxError = e.to_string().into();
                        msg
                    })
                })
            },
            &sayiir_runtime::serialization::JsonCodec,
        )
        .map_err(crate::exceptions::runtime_err_to_napi)?;

        tracing::info!(workflow_id = %workflow.workflow_id, "workflow execution completed");

        decode_to_js_value(env, &result)
    }
}

/// Execute a JavaScript task by calling it from the registry.
///
/// If the task returns a Promise, this function returns an error directing
/// the user to use the async executor.
pub(crate) fn execute_js_task(
    env: &Env,
    task_id: &str,
    input: &Bytes,
    registry: &Object,
) -> Result<Bytes> {
    let callable: Function<Unknown, Unknown> =
        registry.get_named_property(task_id).map_err(|_| {
            Error::new(
                Status::GenericFailure,
                format!("Task '{task_id}' not found in registry"),
            )
        })?;

    tracing::debug!(task_id, input_bytes = input.len(), "executing js task");

    let input_obj = decode_to_js_value(env, input)?;
    let result: Unknown = callable.call(input_obj)?;

    if is_promise(env, &result)? {
        return Err(Error::new(
            Status::GenericFailure,
            format!(
                "Task '{task_id}' returned a Promise. Async tasks are not supported with \
                 the sync engine. Use `await runWorkflow(...)` instead, which supports \
                 both sync and async tasks."
            ),
        ));
    }

    tracing::debug!(task_id, "js task completed");
    encode_js_value(env, result)
}

/// Check if a JS value is a Promise (has a `.then` property that is a function).
fn is_promise(_env: &Env, value: &Unknown) -> Result<bool> {
    let value_type = value.get_type()?;
    if value_type != ValueType::Object {
        return Ok(false);
    }
    // SAFETY: We just checked the value type is Object.
    let obj: Object = unsafe { value.cast() }?;
    match obj.get::<Unknown>("then") {
        Ok(Some(then_val)) => Ok(then_val.get_type()? == ValueType::Function),
        _ => Ok(false),
    }
}

// ── Continuation Stepper (async-capable, JS-driven loop) ─────────────

/// Result of a single stepper step, returned to JavaScript.
#[napi(object)]
pub struct NapiStepResult {
    /// `"task"` — execute a task, then call `submitResult()`
    /// `"done"` — workflow complete, `output_json` has the result
    pub kind: String,
    /// Task ID to execute (when kind == "task")
    pub task_id: Option<String>,
    /// JSON-encoded input for the task (when kind == "task")
    pub input_json: Option<String>,
    /// JSON-encoded final output (when kind == "done")
    pub output_json: Option<String>,
}

impl NapiStepResult {
    fn task(task_id: &str, input: &Bytes) -> Self {
        Self {
            kind: "task".to_string(),
            task_id: Some(task_id.to_string()),
            input_json: std::str::from_utf8(input).ok().map(ToString::to_string),
            output_json: None,
        }
    }

    fn done(output: &Bytes) -> Self {
        Self {
            kind: "done".to_string(),
            task_id: None,
            input_json: None,
            output_json: std::str::from_utf8(output).ok().map(ToString::to_string),
        }
    }
}

/// Pre-computed execution plan: a flat list of steps derived from the
/// continuation tree. Forks are linearized (branches execute sequentially).
///
/// This avoids needing to clone or hold raw pointers into the continuation
/// tree — we extract everything we need at construction time.
///
/// Branch and Loop nodes produce dynamic control-flow steps that splice
/// new steps into the plan at runtime (since the path depends on task output).
#[derive(Clone)]
enum PlannedStep {
    /// Execute a task: look up `id` in the registry, call with current input.
    Task { id: String },
    /// Save current input as fork input (start of a branch).
    RestoreForkInput,
    /// Save current output as a branch result.
    SaveBranchResult { branch_id: String },
    /// Collect all branch results into a JSON object for the join task.
    CollectBranches,
    /// Save current input before branch key function (restored by `BranchDispatch`).
    SaveBranchInput,
    /// Dispatch to a branch based on the routing key (current input is the key).
    BranchDispatch {
        branch_id: String,
        branches: std::collections::HashMap<String, Vec<PlannedStep>>,
        default: Option<Vec<PlannedStep>>,
    },
    /// Wrap current output in a `{ branch, result }` envelope.
    WrapBranchEnvelope { key: String },
    /// Start a loop: splice body steps + `LoopCheck` into the plan.
    LoopStart {
        loop_id: String,
        body_steps: Vec<PlannedStep>,
        max_iterations: u32,
        on_max: MaxIterationsPolicy,
    },
    /// After a loop body completes, inspect the `LoopResult` and decide
    /// whether to iterate again or exit.
    LoopCheck {
        loop_id: String,
        body_steps: Vec<PlannedStep>,
        max_iterations: u32,
        on_max: MaxIterationsPolicy,
        iteration: u32,
    },
}

/// Drives a workflow step-by-step, yielding `(taskId, input)` pairs to JS.
///
/// This enables async task support by returning control to JavaScript between
/// each task execution, allowing microtasks to drain.
#[napi]
pub struct NapiContinuationStepper {
    /// Flat execution plan.
    steps: Vec<PlannedStep>,
    /// Current position in the plan.
    pos: usize,
    /// Current input/output flowing through the pipeline.
    current_input: Bytes,
    /// Saved fork input (restored at the start of each branch).
    fork_input_stack: Vec<Bytes>,
    /// Accumulated branch results for the current fork.
    branch_results_stack: Vec<Vec<(String, Bytes)>>,
    /// Saved input before branch key function (for routing dispatch).
    branch_input_stack: Vec<Bytes>,
}

#[napi]
impl NapiContinuationStepper {
    /// Create a new stepper from a workflow and input.
    #[napi(constructor)]
    pub fn new(env: Env, workflow: &NapiWorkflow, input: Unknown) -> Result<Self> {
        let input_bytes = encode_js_value(&env, input)?;

        let mut steps = Vec::new();
        Self::flatten(&workflow.continuation, &mut steps)?;

        let mut stepper = Self {
            steps,
            pos: 0,
            current_input: input_bytes,
            fork_input_stack: Vec::new(),
            branch_results_stack: Vec::new(),
            branch_input_stack: Vec::new(),
        };

        // Skip past any non-task steps at the start.
        stepper.skip_non_tasks()?;

        Ok(stepper)
    }

    /// Get the current step (what JS should do next).
    #[napi]
    pub fn current(&self) -> NapiStepResult {
        if self.pos >= self.steps.len() {
            return NapiStepResult::done(&self.current_input);
        }

        match &self.steps[self.pos] {
            PlannedStep::Task { id } => NapiStepResult::task(id, &self.current_input),
            _ => NapiStepResult::done(&self.current_input),
        }
    }

    /// Submit a task result and advance to the next step.
    #[napi]
    pub fn submit_result(&mut self, env: Env, output: Unknown) -> Result<NapiStepResult> {
        let output_bytes = encode_js_value(&env, output)?;
        self.current_input = output_bytes;

        // Move past the just-completed task.
        self.pos += 1;

        // Process any non-task steps (fork management) and find the next task.
        self.skip_non_tasks()?;

        Ok(self.current())
    }
}

impl NapiContinuationStepper {
    /// Flatten a continuation tree into a linear execution plan.
    fn flatten(cont: &WorkflowContinuation, steps: &mut Vec<PlannedStep>) -> Result<()> {
        match cont {
            WorkflowContinuation::Task { id, next, .. } => {
                steps.push(PlannedStep::Task { id: id.clone() });
                if let Some(next) = next {
                    Self::flatten(next, steps)?;
                }
            }
            WorkflowContinuation::Fork { branches, join, .. } => {
                for branch in branches {
                    let branch_id = branch.first_task_id().to_string();
                    // Mark: restore fork input for this branch
                    steps.push(PlannedStep::RestoreForkInput);
                    // Flatten branch tasks
                    Self::flatten(branch, steps)?;
                    // Mark: save branch result
                    steps.push(PlannedStep::SaveBranchResult { branch_id });
                }

                // Collect results and pass to join
                steps.push(PlannedStep::CollectBranches);

                if let Some(join) = join {
                    Self::flatten(join, steps)?;
                }
            }
            WorkflowContinuation::Delay { next, .. } => {
                // In the stepper (simple executor), skip delays
                if let Some(next) = next {
                    Self::flatten(next, steps)?;
                }
            }
            WorkflowContinuation::AwaitSignal { id, .. } => {
                return Err(Error::new(
                    Status::GenericFailure,
                    format!(
                        "AwaitSignal '{id}' not supported in the simple executor. \
                         Use the durable engine instead."
                    ),
                ));
            }
            WorkflowContinuation::Branch {
                id,
                branches,
                default,
                next,
                ..
            } => {
                // Save current input before the key function overwrites it
                steps.push(PlannedStep::SaveBranchInput);
                // The key function is registered as "{id}::key_fn"
                let key_fn_id = sayiir_core::workflow::key_fn_id(id);
                steps.push(PlannedStep::Task { id: key_fn_id });

                // Pre-flatten each branch for runtime dispatch
                let mut branch_map = std::collections::HashMap::new();
                for (key, cont) in branches {
                    let mut branch_steps = Vec::new();
                    Self::flatten(cont, &mut branch_steps)?;
                    branch_map.insert(key.clone(), branch_steps);
                }
                let default_steps = default
                    .as_ref()
                    .map(|d| {
                        let mut ds = Vec::new();
                        Self::flatten(d, &mut ds)?;
                        Ok::<_, Error>(ds)
                    })
                    .transpose()?;

                steps.push(PlannedStep::BranchDispatch {
                    branch_id: id.clone(),
                    branches: branch_map,
                    default: default_steps,
                });

                if let Some(next) = next {
                    Self::flatten(next, steps)?;
                }
            }
            WorkflowContinuation::Loop {
                id,
                body,
                max_iterations,
                on_max,
                next,
            } => {
                let mut body_steps = Vec::new();
                Self::flatten(body, &mut body_steps)?;

                steps.push(PlannedStep::LoopStart {
                    loop_id: id.clone(),
                    body_steps,
                    max_iterations: *max_iterations,
                    on_max: *on_max,
                });

                if let Some(next) = next {
                    Self::flatten(next, steps)?;
                }
            }
            WorkflowContinuation::ChildWorkflow { child, next, .. } => {
                // Flatten child's continuation inline — it's just more steps.
                Self::flatten(child, steps)?;
                if let Some(next) = next {
                    Self::flatten(next, steps)?;
                }
            }
        }
        Ok(())
    }

    /// Process non-task steps (fork/branch/loop bookkeeping) until we reach
    /// the next task or the end of the plan.
    #[allow(clippy::too_many_lines)]
    fn skip_non_tasks(&mut self) -> Result<()> {
        while self.pos < self.steps.len() {
            match &self.steps[self.pos] {
                PlannedStep::Task { .. } => return Ok(()),
                PlannedStep::RestoreForkInput => {
                    if let Some(fork_input) = self.fork_input_stack.last() {
                        self.current_input = fork_input.clone();
                    } else {
                        self.fork_input_stack.push(self.current_input.clone());
                        self.branch_results_stack.push(Vec::new());
                    }
                    self.pos += 1;
                }
                PlannedStep::SaveBranchResult { branch_id } => {
                    let branch_id = branch_id.clone();
                    if let Some(results) = self.branch_results_stack.last_mut() {
                        results.push((branch_id, self.current_input.clone()));
                    }
                    self.pos += 1;
                }
                PlannedStep::CollectBranches => {
                    self.fork_input_stack.pop();
                    let results = self.branch_results_stack.pop().unwrap_or_default();
                    self.current_input = Self::encode_branch_map(results)?;
                    self.pos += 1;
                }
                PlannedStep::SaveBranchInput => {
                    self.branch_input_stack.push(self.current_input.clone());
                    self.pos += 1;
                }
                PlannedStep::BranchDispatch { .. } | PlannedStep::WrapBranchEnvelope { .. } => {
                    self.handle_branch_step()?;
                }
                PlannedStep::LoopStart { .. } | PlannedStep::LoopCheck { .. } => {
                    self.handle_loop_step()?;
                }
            }
        }
        Ok(())
    }

    /// Encode a list of branch results into a merged JSON object.
    fn encode_branch_map(results: Vec<(String, Bytes)>) -> Result<Bytes> {
        let branch_map: serde_json::Map<String, serde_json::Value> = results
            .into_iter()
            .filter_map(|(id, bytes)| {
                let val: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                Some((id, val))
            })
            .collect();
        let merged = serde_json::Value::Object(branch_map);
        Ok(Bytes::from(serde_json::to_vec(&merged).map_err(|e| {
            Error::new(Status::GenericFailure, e.to_string())
        })?))
    }

    /// Handle `BranchDispatch` and `WrapBranchEnvelope` steps.
    fn handle_branch_step(&mut self) -> Result<()> {
        match &self.steps[self.pos] {
            PlannedStep::BranchDispatch {
                branch_id,
                branches,
                default,
            } => {
                // current_input is the key function's output (the routing key)
                let key: String = serde_json::from_slice(&self.current_input)
                    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
                let chosen = branches
                    .get(&key)
                    .or(default.as_ref())
                    .ok_or_else(|| {
                        Error::new(
                            Status::GenericFailure,
                            format!("Branch node '{branch_id}': no branch matches key '{key}'"),
                        )
                    })?
                    .clone();

                // Restore the original input from before the key function ran
                if let Some(saved) = self.branch_input_stack.pop() {
                    self.current_input = saved;
                }

                let wrap = PlannedStep::WrapBranchEnvelope { key };
                let insert_pos = self.pos + 1;
                let mut to_insert = chosen;
                to_insert.push(wrap);
                self.steps.splice(insert_pos..insert_pos, to_insert);
                self.pos += 1;
                Ok(())
            }
            PlannedStep::WrapBranchEnvelope { key } => {
                let result_value: serde_json::Value =
                    serde_json::from_slice(&self.current_input).unwrap_or(serde_json::Value::Null);
                let envelope = serde_json::json!({ "branch": key, "result": result_value });
                self.current_input = Bytes::from(
                    serde_json::to_vec(&envelope)
                        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?,
                );
                self.pos += 1;
                Ok(())
            }
            _ => unreachable!(),
        }
    }

    /// Handle `LoopStart` and `LoopCheck` steps.
    fn handle_loop_step(&mut self) -> Result<()> {
        match &self.steps[self.pos] {
            PlannedStep::LoopStart {
                loop_id,
                body_steps,
                max_iterations,
                on_max,
            } => {
                let check = PlannedStep::LoopCheck {
                    loop_id: loop_id.clone(),
                    body_steps: body_steps.clone(),
                    max_iterations: *max_iterations,
                    on_max: *on_max,
                    iteration: 0,
                };
                let insert_pos = self.pos + 1;
                let mut to_insert = body_steps.clone();
                to_insert.push(check);
                self.steps.splice(insert_pos..insert_pos, to_insert);
                self.pos += 1;
                Ok(())
            }
            PlannedStep::LoopCheck {
                loop_id,
                body_steps,
                max_iterations,
                on_max,
                iteration,
            } => {
                let (tag, inner_bytes) = Self::decode_loop_result(&self.current_input)?;
                match tag.as_str() {
                    s if s == LoopDecision::Done.as_ref() => {
                        self.current_input = inner_bytes;
                        self.pos += 1;
                        Ok(())
                    }
                    s if s == LoopDecision::Again.as_ref() => {
                        let next_iter = iteration + 1;
                        if next_iter >= *max_iterations {
                            match on_max {
                                MaxIterationsPolicy::Fail => Err(Error::new(
                                    Status::GenericFailure,
                                    format!(
                                        "Loop '{loop_id}' exceeded max iterations ({max_iterations})"
                                    ),
                                )),
                                MaxIterationsPolicy::ExitWithLast => {
                                    self.current_input = inner_bytes;
                                    self.pos += 1;
                                    Ok(())
                                }
                            }
                        } else {
                            self.current_input = inner_bytes;
                            let check = PlannedStep::LoopCheck {
                                loop_id: loop_id.clone(),
                                body_steps: body_steps.clone(),
                                max_iterations: *max_iterations,
                                on_max: *on_max,
                                iteration: next_iter,
                            };
                            let insert_pos = self.pos + 1;
                            let mut to_insert = body_steps.clone();
                            to_insert.push(check);
                            self.steps.splice(insert_pos..insert_pos, to_insert);
                            self.pos += 1;
                            Ok(())
                        }
                    }
                    other => Err(Error::new(
                        Status::GenericFailure,
                        format!("Unknown LoopResult tag: '{other}'"),
                    )),
                }
            }
            _ => unreachable!(),
        }
    }

    /// Decode a `LoopResult` JSON value into `(tag, inner_bytes)`.
    fn decode_loop_result(bytes: &[u8]) -> Result<(String, Bytes)> {
        let v: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        let tag = v
            .get("_loop")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::new(
                    Status::GenericFailure,
                    "Missing or invalid '_loop' tag in LoopResult".to_string(),
                )
            })?
            .to_string();
        let inner = v.get("value").ok_or_else(|| {
            Error::new(
                Status::GenericFailure,
                "Missing 'value' field in LoopResult".to_string(),
            )
        })?;
        let inner_bytes = Bytes::from(
            serde_json::to_vec(inner)
                .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?,
        );
        Ok((tag, inner_bytes))
    }
}
