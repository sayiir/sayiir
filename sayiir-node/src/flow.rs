//! Node.js-exposed flow builder API.
//!
//! Provides `NapiFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! JavaScript tasks directly. Task nodes have `func: None` since execution is
//! handled by looking up JS callables by task ID in a registry.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use sayiir_core::task::TaskMetadata;
use sayiir_core::workflow::WorkflowContinuation;

use crate::task::NapiTaskMetadata;

/// A compiled workflow definition.
#[napi]
pub struct NapiWorkflow {
    pub(crate) workflow_id: String,
    pub(crate) definition_hash: String,
    pub(crate) continuation: Arc<WorkflowContinuation>,
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
}

enum BuilderTask {
    Sequential {
        task_id: String,
        metadata: TaskMetadata,
    },
    Fork {
        branches: Vec<Vec<(String, TaskMetadata)>>,
        join_id: String,
        join_metadata: TaskMetadata,
    },
    Delay {
        delay_id: String,
        duration_secs: f64,
    },
    AwaitSignal {
        signal_id: String,
        signal_name: String,
        timeout_secs: Option<f64>,
    },
}

/// Workflow builder for constructing task pipelines.
#[napi]
pub struct NapiFlowBuilder {
    workflow_id: String,
    tasks: Vec<BuilderTask>,
}

#[napi]
impl NapiFlowBuilder {
    #[napi(constructor)]
    pub fn new(name: String) -> Self {
        Self {
            workflow_id: name,
            tasks: Vec::new(),
        }
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
        })
    }
}

/// A task within a fork branch (used for the `add_fork` API).
#[napi(object)]
pub struct NapiBranchTask {
    pub task_id: String,
    pub metadata: Option<NapiTaskMetadata>,
}

impl NapiFlowBuilder {
    fn build_continuation(&self) -> Result<WorkflowContinuation> {
        if self.tasks.is_empty() {
            return Err(Error::new(
                Status::InvalidArg,
                "Workflow must have at least one task",
            ));
        }

        let iter = self.tasks.iter().rev();
        let mut current: Option<WorkflowContinuation> = None;

        for task in iter {
            current = Some(match task {
                BuilderTask::Sequential { task_id, metadata } => WorkflowContinuation::Task {
                    id: task_id.clone(),
                    func: None,
                    timeout: metadata.timeout,
                    retry_policy: metadata.retries.clone(),
                    next: current.map(Box::new),
                },
                BuilderTask::Delay {
                    delay_id,
                    duration_secs,
                } => WorkflowContinuation::Delay {
                    id: delay_id.clone(),
                    duration: std::time::Duration::from_secs_f64(*duration_secs),
                    next: current.map(Box::new),
                },
                BuilderTask::AwaitSignal {
                    signal_id,
                    signal_name,
                    timeout_secs,
                } => WorkflowContinuation::AwaitSignal {
                    id: signal_id.clone(),
                    signal_name: signal_name.clone(),
                    timeout: timeout_secs.map(std::time::Duration::from_secs_f64),
                    next: current.map(Box::new),
                },
                BuilderTask::Fork {
                    branches,
                    join_id,
                    join_metadata,
                } => {
                    let branch_ids: Vec<&str> = branches
                        .iter()
                        .filter_map(|chain| chain.first().map(|(id, _)| id.as_str()))
                        .collect();
                    let fork_id = WorkflowContinuation::derive_fork_id(&branch_ids);

                    let branch_conts: Vec<Arc<WorkflowContinuation>> = branches
                        .iter()
                        .map(|chain| -> Result<Arc<WorkflowContinuation>> {
                            let mut branch_current: Option<WorkflowContinuation> = None;
                            for (id, metadata) in chain.iter().rev() {
                                branch_current = Some(WorkflowContinuation::Task {
                                    id: id.clone(),
                                    func: None,
                                    timeout: metadata.timeout,
                                    retry_policy: metadata.retries.clone(),
                                    next: branch_current.map(Box::new),
                                });
                            }
                            Ok(Arc::new(branch_current.ok_or_else(|| {
                                Error::new(
                                    Status::InvalidArg,
                                    "Each branch must have at least one task",
                                )
                            })?))
                        })
                        .collect::<Result<Vec<_>>>()?;

                    let join_cont = WorkflowContinuation::Task {
                        id: join_id.clone(),
                        func: None,
                        timeout: join_metadata.timeout,
                        retry_policy: join_metadata.retries.clone(),
                        next: current.map(Box::new),
                    };

                    WorkflowContinuation::Fork {
                        id: fork_id,
                        branches: branch_conts.into_boxed_slice(),
                        join: Some(Box::new(join_cont)),
                    }
                }
            });
        }

        current.ok_or_else(|| Error::new(Status::InvalidArg, "Failed to build workflow"))
    }
}
