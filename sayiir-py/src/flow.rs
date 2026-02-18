//! Python-exposed flow builder API.
//!
//! Provides `PyFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! Python tasks directly. Task nodes have `func: None` since execution is
//! handled by looking up Python callables by task ID in a registry.

use pyo3::prelude::*;
use std::sync::Arc;

use sayiir_core::continuation_builder::BuilderTask;
use sayiir_core::workflow::WorkflowContinuation;

use crate::task::PyTaskMetadata;

/// A Python-exposed workflow.
#[pyclass]
pub struct PyWorkflow {
    pub(crate) workflow_id: String,
    pub(crate) definition_hash: String,
    pub(crate) continuation: Arc<WorkflowContinuation>,
    pub(crate) metadata_json: Option<String>,
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

    #[getter]
    fn metadata_json(&self) -> Option<&str> {
        self.metadata_json.as_deref()
    }
}

/// Python-exposed workflow builder.
#[pyclass]
pub struct PyFlowBuilder {
    workflow_id: String,
    tasks: Vec<BuilderTask>,
    metadata_json: Option<String>,
}

#[pymethods]
impl PyFlowBuilder {
    #[new]
    fn new(name: String) -> Self {
        Self {
            workflow_id: name,
            tasks: Vec::new(),
            metadata_json: None,
        }
    }

    /// Set workflow-level metadata as a JSON string.
    fn set_metadata_json(&mut self, json: String) {
        self.metadata_json = Some(json);
    }

    /// Add a sequential task.
    #[pyo3(signature = (task_id, metadata=None))]
    fn then(&mut self, task_id: String, metadata: Option<PyTaskMetadata>) {
        tracing::trace!(workflow_id = %self.workflow_id, %task_id, "adding sequential task");
        self.tasks.push(BuilderTask::Sequential {
            task_id,
            metadata: metadata.map(Into::into).unwrap_or_default(),
        });
    }

    /// Wait for an external signal before continuing.
    #[pyo3(signature = (signal_id, signal_name, timeout_secs=None))]
    fn wait_for_signal(
        &mut self,
        signal_id: String,
        signal_name: String,
        timeout_secs: Option<f64>,
    ) -> PyResult<()> {
        if let Some(t) = timeout_secs
            && (!t.is_finite() || t < 0.0)
        {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
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
        tracing::trace!(
            workflow_id = %self.workflow_id,
            branch_count = branches.len(),
            %join_id,
            "adding fork"
        );
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

    /// Add a conditional branch node.
    ///
    /// `branch_id` is the node ID. The key function is registered in the Python
    /// registry as `"{branch_id}::key_fn"`.
    /// `branches` is a list of `(key, [(task_id, metadata), ...])` pairs.
    /// `default` is an optional chain for unmatched keys.
    #[pyo3(signature = (branch_id, branches, default=None))]
    #[allow(clippy::type_complexity)]
    fn add_branch(
        &mut self,
        branch_id: String,
        branches: Vec<(String, Vec<(String, Option<PyTaskMetadata>)>)>,
        default: Option<Vec<(String, Option<PyTaskMetadata>)>>,
    ) -> PyResult<()> {
        if branches.is_empty() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "route must have at least one branch",
            ));
        }
        for (key, chain) in &branches {
            if chain.is_empty() {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "Branch '{key}' must have at least one task"
                )));
            }
        }
        let branches = branches
            .into_iter()
            .map(|(key, chain)| {
                let chain = chain
                    .into_iter()
                    .map(|(id, meta)| (id, meta.map(Into::into).unwrap_or_default()))
                    .collect();
                (key, chain)
            })
            .collect();
        let default = default.map(|chain| {
            chain
                .into_iter()
                .map(|(id, meta)| (id, meta.map(Into::into).unwrap_or_default()))
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
    fn build(&self) -> PyResult<PyWorkflow> {
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

        Ok(PyWorkflow {
            workflow_id: self.workflow_id.clone(),
            definition_hash,
            continuation: Arc::new(continuation),
            metadata_json: self.metadata_json.clone(),
        })
    }
}

impl PyFlowBuilder {
    fn build_continuation(&self) -> PyResult<WorkflowContinuation> {
        sayiir_core::continuation_builder::build_continuation(&self.tasks)
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
    }
}
