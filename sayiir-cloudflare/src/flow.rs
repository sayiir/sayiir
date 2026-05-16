//! WASM-exposed flow builder API.
//!
//! Provides `WasmFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! JavaScript tasks directly. Task nodes have `func: None` since execution is
//! handled by looking up JS callables by task ID in a registry.

use std::sync::Arc;
use wasm_bindgen::prelude::*;

use sayiir_core::continuation_builder::FlowBuilder;
use sayiir_core::workflow::WorkflowContinuation;

use crate::error::to_js_error;

/// A compiled workflow definition.
#[wasm_bindgen]
pub struct WasmWorkflow {
    pub(crate) workflow_id: String,
    pub(crate) definition_hash: String,
    pub(crate) continuation: Arc<WorkflowContinuation>,
    pub(crate) metadata_json: Option<String>,
}

#[wasm_bindgen]
#[allow(clippy::must_use_candidate)]
impl WasmWorkflow {
    #[wasm_bindgen(getter, js_name = "workflowId")]
    pub fn workflow_id(&self) -> String {
        self.workflow_id.clone()
    }

    #[wasm_bindgen(getter, js_name = "definitionHash")]
    pub fn definition_hash(&self) -> String {
        self.definition_hash.clone()
    }

    #[wasm_bindgen(getter, js_name = "metadataJson")]
    pub fn metadata_json(&self) -> Option<String> {
        self.metadata_json.clone()
    }
}

/// Workflow builder for constructing task pipelines.
///
/// Implements the `FlowBuilderBackend` interface from `sayiir-flow-js`.
/// Each method mirrors the TypeScript interface, accepting complex parameters
/// as JSON strings to avoid wasm-bindgen limitations with nested types.
#[wasm_bindgen]
pub struct WasmFlowBuilder {
    workflow_id: String,
    inner: FlowBuilder,
    metadata_json: Option<String>,
}

/// Task metadata passed from JS as a plain object.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TaskMetadataJs {
    display_name: Option<String>,
    description: Option<String>,
    timeout_secs: Option<f64>,
    retries: Option<RetryPolicyJs>,
    tags: Option<Vec<String>>,
    version: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryPolicyJs {
    max_retries: u32,
    initial_delay_secs: f64,
    backoff_multiplier: f64,
    max_delay_secs: Option<f64>,
}

impl From<TaskMetadataJs> for sayiir_core::task::TaskMetadata {
    #[allow(clippy::cast_possible_truncation)]
    fn from(m: TaskMetadataJs) -> Self {
        Self {
            display_name: m.display_name,
            description: m.description,
            timeout: m.timeout_secs.map(std::time::Duration::from_secs_f64),
            retries: m.retries.map(|r| sayiir_core::task::RetryPolicy {
                max_retries: r.max_retries,
                initial_delay: std::time::Duration::from_secs_f64(r.initial_delay_secs),
                backoff_multiplier: r.backoff_multiplier as f32,
                max_delay: r.max_delay_secs.map(std::time::Duration::from_secs_f64),
            }),
            tags: m.tags.unwrap_or_default(),
            version: m.version,
            priority: None,
        }
    }
}

/// A task within a fork branch.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchTaskJs {
    task_id: String,
    metadata: Option<TaskMetadataJs>,
}

/// A named branch entry for conditional routing.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchEntryJs {
    key: String,
    tasks: Vec<BranchTaskJs>,
}

/// Parse an optional `JsValue` into a `TaskMetadataJs`, falling back to default.
fn parse_metadata(val: &JsValue) -> TaskMetadataJs {
    if val.is_undefined() || val.is_null() {
        return TaskMetadataJs::default();
    }
    serde_wasm_bindgen::from_value(val.clone()).unwrap_or_default()
}

/// Parse a `JsValue` as a `serde-wasm-bindgen`–deserializable type.
fn parse_js_value<T: serde::de::DeserializeOwned>(val: &JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(val.clone()).map_err(to_js_error)
}

#[wasm_bindgen]
#[allow(clippy::needless_pass_by_value, clippy::must_use_candidate)]
impl WasmFlowBuilder {
    #[wasm_bindgen(constructor)]
    pub fn new(name: String) -> Self {
        Self {
            workflow_id: name,
            inner: FlowBuilder::new(),
            metadata_json: None,
        }
    }

    /// Generate the next lambda ID (`lambda_0`, `lambda_1`, ...).
    #[wasm_bindgen(js_name = "nextLambdaId")]
    pub fn next_lambda_id(&mut self) -> String {
        self.inner.next_lambda_id()
    }

    /// Set workflow-level metadata as a JSON string.
    #[wasm_bindgen(js_name = "setMetadataJson")]
    pub fn set_metadata_json(&mut self, json: String) {
        self.metadata_json = Some(json);
    }

    /// Add a sequential task.
    #[wasm_bindgen(js_name = "then")]
    pub fn then(&mut self, task_id: String, metadata: JsValue) {
        let meta: sayiir_core::task::TaskMetadata = parse_metadata(&metadata).into();
        self.inner.add_sequential(task_id, meta);
    }

    /// Add a fork with branches and a join task.
    ///
    /// `branches_js` is `BranchTask[][]` as a JsValue.
    /// `join_metadata` is an optional `TaskMetadata` as a JsValue.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the branch JSON is malformed or the fork is
    /// invalid.
    #[wasm_bindgen(js_name = "addFork")]
    pub fn add_fork(
        &mut self,
        branches_js: JsValue,
        join_id: String,
        join_metadata: JsValue,
    ) -> Result<(), JsValue> {
        let raw: Vec<Vec<BranchTaskJs>> = parse_js_value(&branches_js)?;
        let branches = raw
            .into_iter()
            .map(|chain| {
                chain
                    .into_iter()
                    .map(|t| (t.task_id, t.metadata.map(Into::into).unwrap_or_default()))
                    .collect()
            })
            .collect();
        let join_meta: sayiir_core::task::TaskMetadata = parse_metadata(&join_metadata).into();
        self.inner
            .add_fork(branches, join_id, join_meta)
            .map_err(to_js_error)
    }

    /// Wait for an external signal before continuing.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the signal definition is invalid.
    #[wasm_bindgen(js_name = "waitForSignal")]
    pub fn wait_for_signal(
        &mut self,
        signal_id: String,
        signal_name: String,
        timeout_secs: Option<f64>,
    ) -> Result<(), JsValue> {
        self.inner
            .add_signal(signal_id, signal_name, timeout_secs)
            .map_err(to_js_error)
    }

    /// Add a durable delay.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the delay definition is invalid.
    pub fn delay(&mut self, delay_id: String, seconds: f64) -> Result<(), JsValue> {
        self.inner.add_delay(delay_id, seconds).map_err(to_js_error)
    }

    /// Add a conditional branch node.
    ///
    /// `branches_js` is `BranchEntry[]` as a JsValue.
    /// `default_branch_js` is an optional `BranchTask[]` as a JsValue.
    ///
    /// Returns the generated branch ID.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the branch JSON is malformed or the branch
    /// definition is invalid.
    #[wasm_bindgen(js_name = "addBranch")]
    pub fn add_branch(
        &mut self,
        branches_js: JsValue,
        default_branch_js: JsValue,
    ) -> Result<String, JsValue> {
        let entries: Vec<BranchEntryJs> = parse_js_value(&branches_js)?;
        let branches = entries
            .into_iter()
            .map(|entry| {
                let chain = entry
                    .tasks
                    .into_iter()
                    .map(|t| (t.task_id, t.metadata.map(Into::into).unwrap_or_default()))
                    .collect();
                (entry.key, chain)
            })
            .collect();

        let default = if default_branch_js.is_undefined() || default_branch_js.is_null() {
            None
        } else {
            let raw: Vec<BranchTaskJs> = parse_js_value(&default_branch_js)?;
            Some(
                raw.into_iter()
                    .map(|t| (t.task_id, t.metadata.map(Into::into).unwrap_or_default()))
                    .collect(),
            )
        };

        self.inner
            .add_branch(branches, default)
            .map_err(to_js_error)
    }

    /// Add a loop node.
    ///
    /// Returns the generated loop ID.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the loop definition is invalid.
    #[wasm_bindgen(js_name = "addLoop")]
    pub fn add_loop(
        &mut self,
        body_task_id: String,
        body_metadata: JsValue,
        max_iterations: u32,
        on_max: Option<String>,
    ) -> Result<String, JsValue> {
        let on_max: sayiir_core::workflow::MaxIterationsPolicy =
            on_max.as_deref().unwrap_or("fail").parse().map_err(|_| {
                to_js_error(format!(
                    "Invalid onMax policy: '{}'. Use 'fail' or 'exit_with_last'.",
                    on_max.as_deref().unwrap_or("fail")
                ))
            })?;
        let meta: sayiir_core::task::TaskMetadata = parse_metadata(&body_metadata).into();
        self.inner
            .add_loop(body_task_id, meta, max_iterations, on_max)
            .map_err(to_js_error)
    }

    /// Add a child workflow (inline composition).
    #[wasm_bindgen(js_name = "addChildWorkflow")]
    pub fn add_child_workflow(&mut self, child_id: String, child_builder: &WasmFlowBuilder) {
        self.inner
            .add_child_workflow(child_id, child_builder.inner.tasks().to_vec());
    }

    /// Build the workflow.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the workflow definition is incomplete or
    /// invalid.
    pub fn build(&self) -> Result<WasmWorkflow, JsValue> {
        let continuation = self.inner.build().map_err(to_js_error)?;
        let serializable = continuation.to_serializable();
        let definition_hash = serializable.compute_definition_hash();

        Ok(WasmWorkflow {
            workflow_id: self.workflow_id.clone(),
            definition_hash,
            continuation: Arc::new(continuation),
            metadata_json: self.metadata_json.clone(),
        })
    }
}
