//! Node.js-exposed flow builder API.
//!
//! Provides `NapiFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! JavaScript tasks directly. Task nodes have `func: None` since execution is
//! handled by looking up JS callables by task ID in a registry.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use sayiir_core::continuation_builder::BuilderTask;
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
    tasks: Vec<BuilderTask>,
    metadata_json: Option<String>,
}

#[napi]
impl NapiFlowBuilder {
    #[napi(constructor)]
    pub fn new(name: String) -> Self {
        Self {
            workflow_id: name,
            tasks: Vec::new(),
            metadata_json: None,
        }
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
        self.tasks.push(BuilderTask::Sequential {
            task_id,
            metadata: metadata.map(Into::into).unwrap_or_default(),
        });
    }

    /// Add a fork with branches and a join task.
    #[napi]
    pub fn add_fork(
        &mut self,
        branches: Vec<Vec<NapiBranchTask>>,
        join_id: String,
        join_metadata: Option<NapiTaskMetadata>,
    ) -> Result<()> {
        if branches.is_empty() {
            return Err(Error::new(
                Status::InvalidArg,
                "Fork must have at least one branch",
            ));
        }
        tracing::trace!(
            workflow_id = %self.workflow_id,
            branch_count = branches.len(),
            %join_id,
            "adding fork"
        );
        for (i, branch) in branches.iter().enumerate() {
            if branch.is_empty() {
                return Err(Error::new(
                    Status::InvalidArg,
                    format!("Branch {i} must have at least one task"),
                ));
            }
        }
        let branches = branches
            .into_iter()
            .map(|chain| {
                chain
                    .into_iter()
                    .map(|t| (t.task_id, t.metadata.map(Into::into).unwrap_or_default()))
                    .collect()
            })
            .collect();
        self.tasks.push(BuilderTask::Fork {
            branches,
            join_id,
            join_metadata: join_metadata.map(Into::into).unwrap_or_default(),
        });
        Ok(())
    }

    /// Wait for an external signal before continuing.
    #[napi]
    pub fn wait_for_signal(
        &mut self,
        signal_id: String,
        signal_name: String,
        timeout_secs: Option<f64>,
    ) -> Result<()> {
        if let Some(t) = timeout_secs
            && (!t.is_finite() || t < 0.0)
        {
            return Err(Error::new(
                Status::InvalidArg,
                "timeout must be a finite non-negative number",
            ));
        }
        self.tasks.push(BuilderTask::AwaitSignal {
            signal_id,
            signal_name,
            timeout_secs,
        });
        Ok(())
    }

    /// Add a durable delay.
    #[napi]
    pub fn delay(&mut self, delay_id: String, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(Error::new(
                Status::InvalidArg,
                "delay duration must be a finite non-negative number",
            ));
        }
        self.tasks.push(BuilderTask::Delay {
            delay_id,
            duration_secs: seconds,
        });
        Ok(())
    }

    /// Add a conditional branch node.
    ///
    /// `branch_id` is the node ID. The key function must be registered in the
    /// JS task registry as `"{branch_id}::key_fn"`.
    /// `branches` is `[{ key, tasks: [{ taskId, metadata }] }]`.
    #[napi]
    pub fn add_branch(
        &mut self,
        branch_id: String,
        branches: Vec<NapiBranchEntry>,
        default_branch: Option<Vec<NapiBranchTask>>,
    ) -> Result<()> {
        if branches.is_empty() {
            return Err(Error::new(
                Status::InvalidArg,
                "route must have at least one branch",
            ));
        }
        for entry in &branches {
            if entry.tasks.is_empty() {
                return Err(Error::new(
                    Status::InvalidArg,
                    format!("Branch '{}' must have at least one task", entry.key),
                ));
            }
        }
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
        self.tasks.push(BuilderTask::Branch {
            branch_id,
            branches,
            default,
        });
        Ok(())
    }

    /// Build the workflow.
    #[napi]
    pub fn build(&self) -> Result<NapiWorkflow> {
        tracing::debug!(
            workflow_id = %self.workflow_id,
            task_count = self.tasks.len(),
            "building workflow"
        );
        let continuation = self.build_continuation()?;
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

impl NapiFlowBuilder {
    fn build_continuation(&self) -> Result<WorkflowContinuation> {
        sayiir_core::continuation_builder::build_continuation(&self.tasks)
            .map_err(|e| Error::new(Status::InvalidArg, e))
    }
}
