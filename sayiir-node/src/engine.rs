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

use sayiir_core::workflow::WorkflowContinuation;
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

        let result = execute_continuation_sync(
            &continuation,
            input_bytes,
            &|task_id, input| {
                execute_js_task(env, task_id, &input, &task_registry).map_err(|e| {
                    let msg: sayiir_core::error::BoxError = e.to_string().into();
                    msg
                })
            },
            &sayiir_runtime::serialization::JsonCodec,
        )
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

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
enum PlannedStep {
    /// Execute a task: look up `id` in the registry, call with current input.
    Task { id: String },
    /// Save current input as fork input (start of a branch).
    RestoreForkInput,
    /// Save current output as a branch result.
    SaveBranchResult { branch_id: String },
    /// Collect all branch results into a JSON object for the join task.
    CollectBranches,
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
            WorkflowContinuation::Branch { id, .. } => {
                return Err(Error::new(
                    Status::GenericFailure,
                    format!(
                        "Branch '{id}' not supported in the simple executor. \
                         Use the durable engine instead."
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Process non-task steps (fork bookkeeping) until we reach the next task
    /// or the end of the plan.
    fn skip_non_tasks(&mut self) -> Result<()> {
        while self.pos < self.steps.len() {
            match &self.steps[self.pos] {
                PlannedStep::Task { .. } => {
                    // Found a task — stop here so JS can execute it.
                    return Ok(());
                }
                PlannedStep::RestoreForkInput => {
                    // Start of a new branch: save current input as fork input
                    // (first branch) or restore it (subsequent branches).
                    if let Some(fork_input) = self.fork_input_stack.last() {
                        self.current_input = fork_input.clone();
                    } else {
                        // First RestoreForkInput for this fork — save the current input.
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
                    // Pop fork state and merge branch results into a JSON object.
                    self.fork_input_stack.pop();
                    let results = self.branch_results_stack.pop().unwrap_or_default();

                    let branch_map: serde_json::Map<String, serde_json::Value> = results
                        .into_iter()
                        .filter_map(|(id, bytes)| {
                            let val: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                            Some((id, val))
                        })
                        .collect();

                    let merged = serde_json::Value::Object(branch_map);
                    self.current_input = Bytes::from(
                        serde_json::to_vec(&merged)
                            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?,
                    );

                    self.pos += 1;
                }
            }
        }
        Ok(())
    }
}
