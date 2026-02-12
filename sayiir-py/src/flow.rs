//! Python-exposed flow builder API.
//!
//! Provides `PyFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! Python tasks directly. Task nodes have `func: None` since execution is
//! handled by looking up Python callables by task ID in a registry.

use pyo3::prelude::*;
use std::sync::Arc;

use sayiir_core::task::TaskMetadata;
use sayiir_core::workflow::WorkflowContinuation;

use crate::task::PyTaskMetadata;

/// A Python-exposed workflow.
#[pyclass]
pub struct PyWorkflow {
    pub(crate) workflow_id: String,
    pub(crate) definition_hash: String,
    pub(crate) continuation: Arc<WorkflowContinuation>,
}

#[pymethods]
impl PyWorkflow {
    #[getter]
    fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    #[getter]
    fn definition_hash(&self) -> &str {
        &self.definition_hash
    }
}

/// Python-exposed workflow builder.
#[pyclass]
pub struct PyFlowBuilder {
    workflow_id: String,
    tasks: Vec<BuilderTask>,
}

enum BuilderTask {
    Sequential {
        task_id: String,
        #[allow(dead_code)]
        metadata: TaskMetadata,
    },
    Fork {
        /// Each branch is a chain of (`task_id`, metadata) pairs.
        branches: Vec<Vec<(String, TaskMetadata)>>,
        join_id: String,
        #[allow(dead_code)]
        join_metadata: TaskMetadata,
    },
    Delay {
        delay_id: String,
        duration_secs: f64,
    },
}

#[pymethods]
impl PyFlowBuilder {
    #[new]
    fn new(name: String) -> Self {
        Self {
            workflow_id: name,
            tasks: Vec::new(),
        }
    }

    /// Add a sequential task.
    #[pyo3(signature = (task_id, metadata=None))]
    fn then(&mut self, task_id: String, metadata: Option<PyTaskMetadata>) {
        self.tasks.push(BuilderTask::Sequential {
            task_id,
            metadata: metadata.map(Into::into).unwrap_or_default(),
        });
    }

    /// Add a durable delay.
    fn delay(&mut self, delay_id: String, seconds: f64) -> PyResult<()> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "delay duration must be a finite non-negative number",
            ));
        }
        self.tasks.push(BuilderTask::Delay {
            delay_id,
            duration_secs: seconds,
        });
        Ok(())
    }

    /// Add a fork with branches (each branch is a chain of tasks) and a join.
    #[pyo3(signature = (branches, join_id, join_metadata=None))]
    fn add_fork(
        &mut self,
        branches: Vec<Vec<(String, Option<PyTaskMetadata>)>>,
        join_id: String,
        join_metadata: Option<PyTaskMetadata>,
    ) -> PyResult<()> {
        if branches.is_empty() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Fork must have at least one branch",
            ));
        }
        for (i, branch) in branches.iter().enumerate() {
            if branch.is_empty() {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "Branch {i} must have at least one task"
                )));
            }
        }
        let branches = branches
            .into_iter()
            .map(|chain| {
                chain
                    .into_iter()
                    .map(|(id, meta)| (id, meta.map(Into::into).unwrap_or_default()))
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

    /// Build the workflow.
    fn build(&self) -> PyResult<PyWorkflow> {
        let continuation = self.build_continuation()?;
        let serializable = continuation.to_serializable();
        let definition_hash = serializable.compute_definition_hash();

        Ok(PyWorkflow {
            workflow_id: self.workflow_id.clone(),
            definition_hash,
            continuation: Arc::new(continuation),
        })
    }
}

impl PyFlowBuilder {
    fn build_continuation(&self) -> PyResult<WorkflowContinuation> {
        if self.tasks.is_empty() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Workflow must have at least one task",
            ));
        }

        let iter = self.tasks.iter().rev();
        let mut current: Option<WorkflowContinuation> = None;

        for task in iter {
            current = Some(match task {
                BuilderTask::Sequential { task_id, .. } => WorkflowContinuation::Task {
                    id: task_id.clone(),
                    func: None,
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
                BuilderTask::Fork {
                    branches, join_id, ..
                } => {
                    let branch_ids: Vec<&str> = branches
                        .iter()
                        .filter_map(|chain| chain.first().map(|(id, _)| id.as_str()))
                        .collect();
                    let fork_id = WorkflowContinuation::derive_fork_id(&branch_ids);

                    let branch_conts: Vec<Arc<WorkflowContinuation>> = branches
                        .iter()
                        .map(|chain| -> PyResult<Arc<WorkflowContinuation>> {
                            // Build the chain in reverse to link tasks together
                            let mut branch_current: Option<WorkflowContinuation> = None;
                            for (id, _) in chain.iter().rev() {
                                branch_current = Some(WorkflowContinuation::Task {
                                    id: id.clone(),
                                    func: None,
                                    next: branch_current.map(Box::new),
                                });
                            }
                            Ok(Arc::new(branch_current.ok_or_else(|| {
                                PyErr::new::<pyo3::exceptions::PyValueError, _>(
                                    "Each branch must have at least one task",
                                )
                            })?))
                        })
                        .collect::<PyResult<Vec<_>>>()?;

                    let join_cont = WorkflowContinuation::Task {
                        id: join_id.clone(),
                        func: None,
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

        current.ok_or_else(|| {
            PyErr::new::<pyo3::exceptions::PyValueError, _>("Failed to build workflow")
        })
    }
}
