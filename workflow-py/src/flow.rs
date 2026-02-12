//! Python-exposed flow builder API.
//!
//! Provides `PyFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! Python tasks directly.

use bytes::Bytes;
use pyo3::prelude::*;
use std::sync::Arc;

use workflow_core::task::{BytesFuture, CoreTask, TaskMetadata};
use workflow_core::workflow::WorkflowContinuation;

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

    /// Start a fork builder.
    fn fork(&mut self) -> PyForkBuilder {
        PyForkBuilder {
            branches: Vec::new(),
        }
    }

    /// Add a fork with branches (each branch is a chain of tasks) and a join.
    #[pyo3(signature = (branches, join_id, join_metadata=None))]
    fn add_fork(
        &mut self,
        branches: Vec<Vec<(String, Option<PyTaskMetadata>)>>,
        join_id: String,
        join_metadata: Option<PyTaskMetadata>,
    ) {
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
                BuilderTask::Sequential { task_id, .. } => {
                    let wrapper = PlaceholderTask(task_id.clone());
                    WorkflowContinuation::Task {
                        id: task_id.clone(),
                        func: Box::new(wrapper),
                        next: current.map(Box::new),
                    }
                }
                BuilderTask::Fork {
                    branches, join_id, ..
                } => {
                    let branch_conts: Vec<Arc<WorkflowContinuation>> = branches
                        .iter()
                        .map(|chain| {
                            // Build the chain in reverse to link tasks together
                            let mut branch_current: Option<WorkflowContinuation> = None;
                            for (id, _) in chain.iter().rev() {
                                let wrapper = PlaceholderTask(id.clone());
                                branch_current = Some(WorkflowContinuation::Task {
                                    id: id.clone(),
                                    func: Box::new(wrapper),
                                    next: branch_current.map(Box::new),
                                });
                            }
                            Arc::new(branch_current.expect("branch must have at least one task"))
                        })
                        .collect();

                    let join_wrapper = PlaceholderTask(join_id.clone());
                    let join_cont = WorkflowContinuation::Task {
                        id: join_id.clone(),
                        func: Box::new(join_wrapper),
                        next: current.map(Box::new),
                    };

                    WorkflowContinuation::Fork {
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

/// Placeholder task - not actually executed.
/// The engine calls Python directly using the task ID from the continuation.
#[allow(dead_code)]
struct PlaceholderTask(String);

impl CoreTask for PlaceholderTask {
    type Input = Bytes;
    type Output = Bytes;
    type Future = BytesFuture;

    fn run(&self, _input: Bytes) -> Self::Future {
        // This is never called - engine uses direct Python calls
        BytesFuture::new(async move {
            Err(anyhow::anyhow!(
                "PlaceholderTask should not be executed directly"
            ))
        })
    }
}

/// Python-exposed fork builder.
#[pyclass]
pub struct PyForkBuilder {
    branches: Vec<(String, TaskMetadata)>,
}

#[pymethods]
impl PyForkBuilder {
    #[pyo3(signature = (task_id, metadata=None))]
    fn branch(&mut self, task_id: String, metadata: Option<PyTaskMetadata>) {
        self.branches
            .push((task_id, metadata.map(Into::into).unwrap_or_default()));
    }
}
