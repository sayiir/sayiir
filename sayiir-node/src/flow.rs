//! Node.js-exposed flow builder API.
//!
//! Provides `NapiFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! JavaScript tasks directly. Task nodes have `func: None` since execution is
//! handled by looking up JS callables by task ID in a registry.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use sayiir_core::continuation_builder::FlowBuilder;
use sayiir_core::workflow::WorkflowContinuation;

use crate::task::NapiTaskMetadata;

/// A compiled workflow definition.
#[napi]
pub struct NapiWorkflow {
    pub(crate) workflow_id: String,
    pub(crate) definition_hash: String,
    pub(crate) continuation: Arc<WorkflowContinuation>,
    pub(crate) metadata_json: Option<String>,
}

#[napi]
impl NapiWorkflow {
    #[napi(getter)]
    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    #[napi(getter)]
    pub fn definition_hash(&self) -> &str {
        &self.definition_hash
    }

    #[napi(getter)]
    pub fn metadata_json(&self) -> Option<&str> {
        self.metadata_json.as_deref()
    }
}

/// Workflow builder for constructing task pipelines.
#[napi]
pub struct NapiFlowBuilder {
    workflow_id: String,
    inner: FlowBuilder,
    metadata_json: Option<String>,
}

#[napi]
impl NapiFlowBuilder {
    #[napi(constructor)]
    pub fn new(name: String) -> Self {
        Self {
            workflow_id: name,
            inner: FlowBuilder::new(),
            metadata_json: None,
        }
    }

    /// Generate the next lambda ID (`lambda_0`, `lambda_1`, …).
    #[napi]
    pub fn next_lambda_id(&mut self) -> String {
        self.inner.next_lambda_id()
    }

    /// Set workflow-level metadata as a JSON string.
    #[napi]
    pub fn set_metadata_json(&mut self, json: String) {
        self.metadata_json = Some(json);
    }

    /// Add a sequential task.
    #[napi]
    pub fn then(&mut self, task_id: String, metadata: Option<NapiTaskMetadata>) {
        tracing::trace!(workflow_id = %self.workflow_id, %task_id, "adding sequential task");
        self.inner
            .add_sequential(task_id, metadata.map(Into::into).unwrap_or_default());
    }

    /// Add a fork with branches and a join task.
    #[napi]
    pub fn add_fork(
        &mut self,
        branches: Vec<Vec<NapiBranchTask>>,
        join_id: String,
        join_metadata: Option<NapiTaskMetadata>,
    ) -> Result<()> {
        tracing::trace!(
            workflow_id = %self.workflow_id,
            branch_count = branches.len(),
            %join_id,
            "adding fork"
        );
        let branches = branches
            .into_iter()
            .map(|chain| {
                chain
                    .into_iter()
                    .map(|t| (t.task_id, t.metadata.map(Into::into).unwrap_or_default()))
                    .collect()
            })
            .collect();
        self.inner
            .add_fork(
                branches,
                join_id,
                join_metadata.map(Into::into).unwrap_or_default(),
            )
            .map_err(|e| Error::new(Status::InvalidArg, e))
    }

    /// Wait for an external signal before continuing.
    #[napi]
    pub fn wait_for_signal(
        &mut self,
        signal_id: String,
        signal_name: String,
        timeout_secs: Option<f64>,
    ) -> Result<()> {
        self.inner
            .add_signal(signal_id, signal_name, timeout_secs)
            .map_err(|e| Error::new(Status::InvalidArg, e))
    }

    /// Add a durable delay.
    #[napi]
    pub fn delay(&mut self, delay_id: String, seconds: f64) -> Result<()> {
        self.inner
            .add_delay(delay_id, seconds)
            .map_err(|e| Error::new(Status::InvalidArg, e))
    }

    /// Add a conditional branch node.
    ///
    /// The branch ID is auto-generated internally. The key function must be
    /// registered in the JS task registry as `"{branch_id}::key_fn"`.
    /// `branches` is `[{ key, tasks: [{ taskId, metadata }] }]`.
    ///
    /// Returns the generated branch ID.
    #[napi]
    pub fn add_branch(
        &mut self,
        branches: Vec<NapiBranchEntry>,
        default_branch: Option<Vec<NapiBranchTask>>,
    ) -> Result<String> {
        let branches = branches
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
        let default = default_branch.map(|chain| {
            chain
                .into_iter()
                .map(|t| (t.task_id, t.metadata.map(Into::into).unwrap_or_default()))
                .collect()
        });
        self.inner
            .add_branch(branches, default)
            .map_err(|e| Error::new(Status::InvalidArg, e))
    }

    /// Add a loop node.
    ///
    /// The loop ID is auto-generated internally.
    /// `bodyTaskId` is the task that runs each iteration.
    /// `maxIterations` caps the iteration count.
    /// `onMax` is `"fail"` (default) or `"exit_with_last"`.
    ///
    /// Returns the generated loop ID.
    #[napi]
    pub fn add_loop(
        &mut self,
        body_task_id: String,
        body_metadata: Option<NapiTaskMetadata>,
        max_iterations: u32,
        on_max: Option<String>,
    ) -> Result<String> {
        let on_max: sayiir_core::workflow::MaxIterationsPolicy =
            on_max.as_deref().unwrap_or("fail").parse().map_err(|_| {
                Error::new(
                    Status::InvalidArg,
                    format!(
                        "Invalid onMax policy: '{}'. Use 'fail' or 'exit_with_last'.",
                        on_max.as_deref().unwrap_or("fail")
                    ),
                )
            })?;
        self.inner
            .add_loop(
                body_task_id,
                body_metadata.map(Into::into).unwrap_or_default(),
                max_iterations,
                on_max,
            )
            .map_err(|e| Error::new(Status::InvalidArg, e))
    }

    /// Add a child workflow (inline composition).
    #[napi]
    pub fn add_child_workflow(&mut self, child_id: String, child_builder: &NapiFlowBuilder) {
        self.inner
            .add_child_workflow(child_id, child_builder.inner.tasks().to_vec());
    }

    /// Build the workflow.
    #[napi]
    pub fn build(&self) -> Result<NapiWorkflow> {
        tracing::debug!(workflow_id = %self.workflow_id, "building workflow");
        let continuation = self
            .inner
            .build()
            .map_err(|e| Error::new(Status::InvalidArg, e))?;
        let serializable = continuation.to_serializable();
        let definition_hash = serializable.compute_definition_hash();

        tracing::info!(
            workflow_id = %self.workflow_id,
            %definition_hash,
            "workflow built"
        );

        Ok(NapiWorkflow {
            workflow_id: self.workflow_id.clone(),
            definition_hash,
            continuation: Arc::new(continuation),
            metadata_json: self.metadata_json.clone(),
        })
    }
}

/// A task within a fork branch (used for the `add_fork` API).
#[napi(object)]
pub struct NapiBranchTask {
    pub task_id: String,
    pub metadata: Option<NapiTaskMetadata>,
}

/// A named branch entry for the `add_branch` API.
#[napi(object)]
pub struct NapiBranchEntry {
    pub key: String,
    pub tasks: Vec<NapiBranchTask>,
}
