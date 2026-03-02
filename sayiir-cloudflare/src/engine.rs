//! WASM-exposed continuation stepper.
//!
//! Drives a workflow step-by-step, yielding `(taskId, input)` pairs to
//! JavaScript one at a time. The JS layer calls tasks (including async/Promise),
//! submits results, and the stepper advances to the next step.
//!
//! This is the WASM equivalent of `NapiContinuationStepper` from `sayiir-node`.
//! The execution plan is flattened at construction time into a linear list of
//! `PlannedStep`s — forks are linearized (branches execute sequentially) and
//! branch/loop nodes produce dynamic steps that splice into the plan at runtime.

use bytes::Bytes;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

use sayiir_core::codec::LoopDecision;
use sayiir_core::workflow::{MaxIterationsPolicy, WorkflowContinuation};

use crate::codec::{
    decode_json_string, decode_loop_result, encode_branch_envelope, encode_js_value,
    encode_named_results,
};
use crate::error::to_js_error;
use crate::flow::WasmWorkflow;

/// Result of a single stepper step, returned to JavaScript.
#[wasm_bindgen]
pub struct WasmStepResult {
    kind: String,
    task_id: Option<String>,
    input_json: Option<String>,
    output_json: Option<String>,
}

#[wasm_bindgen]
#[allow(clippy::must_use_candidate)]
impl WasmStepResult {
    /// `"task"` — execute a task, then call `submitResult()`.
    /// `"done"` — workflow complete, `outputJson` has the result.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    /// Task ID to execute (when kind == "task").
    #[wasm_bindgen(getter, js_name = "taskId")]
    pub fn task_id(&self) -> Option<String> {
        self.task_id.clone()
    }

    /// JSON-encoded input for the task (when kind == "task").
    #[wasm_bindgen(getter, js_name = "inputJson")]
    pub fn input_json(&self) -> Option<String> {
        self.input_json.clone()
    }

    /// JSON-encoded final output (when kind == "done").
    #[wasm_bindgen(getter, js_name = "outputJson")]
    pub fn output_json(&self) -> Option<String> {
        self.output_json.clone()
    }
}

impl WasmStepResult {
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
/// continuation tree.
#[derive(Clone)]
pub(crate) enum PlannedStep {
    /// Execute a task: look up `id` in the registry, call with current input.
    Task { id: String },
    /// Save current input as fork input (start of a branch).
    RestoreForkInput,
    /// Save current output as a branch result.
    SaveBranchResult { branch_id: String },
    /// Collect all branch results into a JSON object for the join task.
    CollectBranches,
    /// Save current input before branch key function.
    SaveBranchInput,
    /// Dispatch to a branch based on the routing key.
    BranchDispatch {
        branch_id: String,
        branches: HashMap<String, Vec<PlannedStep>>,
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
#[wasm_bindgen]
pub struct WasmContinuationStepper {
    steps: Vec<PlannedStep>,
    pos: usize,
    current_input: Bytes,
    fork_input_stack: Vec<Bytes>,
    branch_results_stack: Vec<Vec<(String, Bytes)>>,
    branch_input_stack: Vec<Bytes>,
}

#[wasm_bindgen]
#[allow(clippy::must_use_candidate, clippy::needless_pass_by_value)]
impl WasmContinuationStepper {
    /// Create a new stepper from a workflow and input.
    ///
    /// # Errors
    ///
    /// Returns a JS error if input encoding fails or the workflow contains
    /// unsupported nodes (e.g. `AwaitSignal`).
    #[wasm_bindgen(constructor)]
    pub fn new(
        workflow: &WasmWorkflow,
        input: JsValue,
    ) -> Result<WasmContinuationStepper, JsValue> {
        let input_bytes = encode_js_value(&input)?;

        let mut steps = Vec::new();
        flatten(&workflow.continuation, &mut steps)?;

        let mut stepper = Self {
            steps,
            pos: 0,
            current_input: input_bytes,
            fork_input_stack: Vec::new(),
            branch_results_stack: Vec::new(),
            branch_input_stack: Vec::new(),
        };

        stepper.skip_non_tasks()?;

        Ok(stepper)
    }

    /// Get the current step (what JS should do next).
    pub fn current(&self) -> WasmStepResult {
        if self.pos >= self.steps.len() {
            return WasmStepResult::done(&self.current_input);
        }

        #[allow(clippy::indexing_slicing)] // bounds checked above
        match &self.steps[self.pos] {
            PlannedStep::Task { id } => WasmStepResult::task(id, &self.current_input),
            _ => WasmStepResult::done(&self.current_input),
        }
    }

    /// Submit a task result and advance to the next step.
    ///
    /// # Errors
    ///
    /// Returns a JS error if output encoding fails or a branch/loop step
    /// encounters invalid data.
    #[wasm_bindgen(js_name = "submitResult")]
    pub fn submit_result(&mut self, output: JsValue) -> Result<WasmStepResult, JsValue> {
        let output_bytes = encode_js_value(&output)?;
        self.current_input = output_bytes;

        self.pos += 1;
        self.skip_non_tasks()?;

        Ok(self.current())
    }
}

impl WasmContinuationStepper {
    /// Process non-task steps until we reach the next task or end.
    #[allow(clippy::too_many_lines)]
    fn skip_non_tasks(&mut self) -> Result<(), JsValue> {
        while self.pos < self.steps.len() {
            #[allow(clippy::indexing_slicing)] // bounds checked by while condition
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
                    self.current_input = encode_named_results(&results)?;
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

    /// Handle `BranchDispatch` and `WrapBranchEnvelope` steps.
    #[allow(clippy::indexing_slicing)] // pos is always within bounds when called from skip_non_tasks
    fn handle_branch_step(&mut self) -> Result<(), JsValue> {
        match &self.steps[self.pos] {
            PlannedStep::BranchDispatch {
                branch_id,
                branches,
                default,
            } => {
                let key = decode_json_string(&self.current_input)?;
                let chosen = branches
                    .get(&key)
                    .or(default.as_ref())
                    .ok_or_else(|| {
                        to_js_error(format!(
                            "Branch node '{branch_id}': no branch matches key '{key}'"
                        ))
                    })?
                    .clone();

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
                self.current_input = encode_branch_envelope(key, &self.current_input)?;
                self.pos += 1;
                Ok(())
            }
            _ => Ok(()), // unreachable in practice
        }
    }

    /// Handle `LoopStart` and `LoopCheck` steps.
    #[allow(clippy::indexing_slicing)] // pos is always within bounds when called from skip_non_tasks
    fn handle_loop_step(&mut self) -> Result<(), JsValue> {
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
                let (tag, inner_bytes) = decode_loop_result(&self.current_input)?;
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
                                MaxIterationsPolicy::Fail => Err(to_js_error(format!(
                                    "Loop '{loop_id}' exceeded max iterations ({max_iterations})"
                                ))),
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
                    other => Err(to_js_error(format!("Unknown LoopResult tag: '{other}'"))),
                }
            }
            _ => Ok(()), // unreachable in practice
        }
    }
}

/// Flatten a continuation tree into a linear execution plan.
pub(crate) fn flatten(
    cont: &WorkflowContinuation,
    steps: &mut Vec<PlannedStep>,
) -> Result<(), JsValue> {
    match cont {
        WorkflowContinuation::Task { id, next, .. } => {
            steps.push(PlannedStep::Task { id: id.clone() });
            if let Some(next) = next {
                flatten(next, steps)?;
            }
        }
        WorkflowContinuation::Fork { branches, join, .. } => {
            for branch in branches {
                let branch_id = branch.first_task_id().to_string();
                steps.push(PlannedStep::RestoreForkInput);
                flatten(branch, steps)?;
                steps.push(PlannedStep::SaveBranchResult { branch_id });
            }
            steps.push(PlannedStep::CollectBranches);
            if let Some(join) = join {
                flatten(join, steps)?;
            }
        }
        WorkflowContinuation::Delay { next, .. } => {
            // In the simple stepper, skip delays
            if let Some(next) = next {
                flatten(next, steps)?;
            }
        }
        WorkflowContinuation::AwaitSignal { id, .. } => {
            return Err(to_js_error(format!(
                "AwaitSignal '{id}' not supported in the simple stepper. \
                 Use the durable engine instead."
            )));
        }
        WorkflowContinuation::Branch {
            id,
            branches,
            default,
            next,
            ..
        } => {
            steps.push(PlannedStep::SaveBranchInput);
            let key_fn_id = sayiir_core::workflow::key_fn_id(id);
            steps.push(PlannedStep::Task { id: key_fn_id });

            let mut branch_map = HashMap::new();
            for (key, cont) in branches {
                let mut branch_steps = Vec::new();
                flatten(cont, &mut branch_steps)?;
                branch_map.insert(key.clone(), branch_steps);
            }
            let default_steps = default
                .as_ref()
                .map(|d| {
                    let mut ds = Vec::new();
                    flatten(d, &mut ds)?;
                    Ok::<_, JsValue>(ds)
                })
                .transpose()?;

            steps.push(PlannedStep::BranchDispatch {
                branch_id: id.clone(),
                branches: branch_map,
                default: default_steps,
            });

            if let Some(next) = next {
                flatten(next, steps)?;
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
            flatten(body, &mut body_steps)?;

            steps.push(PlannedStep::LoopStart {
                loop_id: id.clone(),
                body_steps,
                max_iterations: *max_iterations,
                on_max: *on_max,
            });

            if let Some(next) = next {
                flatten(next, steps)?;
            }
        }
        WorkflowContinuation::ChildWorkflow { child, next, .. } => {
            flatten(child, steps)?;
            if let Some(next) = next {
                flatten(next, steps)?;
            }
        }
    }
    Ok(())
}
