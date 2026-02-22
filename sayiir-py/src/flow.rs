//! Python-exposed flow builder API.
//!
//! Provides `PyFlowBuilder` for constructing workflows. The builder creates
//! `WorkflowContinuation` structures that the engine executes by calling
//! Python tasks directly. Task nodes have `func: None` since execution is
//! handled by looking up Python callables by task ID in a registry.

use pyo3::prelude::*;
use std::sync::Arc;

use sayiir_core::continuation_builder::FlowBuilder;
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
    inner: FlowBuilder,
    metadata_json: Option<String>,
}

#[pymethods]
impl PyFlowBuilder {
    #[new]
    fn new(name: String) -> Self {
        Self {
            workflow_id: name,
            inner: FlowBuilder::new(),
            metadata_json: None,
        }
    }

    /// Generate the next lambda ID (`lambda_0`, `lambda_1`, …).
    fn next_lambda_id(&mut self) -> String {
        self.inner.next_lambda_id()
    }

    /// Set workflow-level metadata as a JSON string.
    fn set_metadata_json(&mut self, json: String) {
        self.metadata_json = Some(json);
    }

    /// Add a sequential task.
    #[pyo3(signature = (task_id, metadata=None))]
    fn then(&mut self, task_id: String, metadata: Option<PyTaskMetadata>) {
        tracing::trace!(workflow_id = %self.workflow_id, %task_id, "adding sequential task");
        self.inner
            .add_sequential(task_id, metadata.map(Into::into).unwrap_or_default());
    }

    /// Wait for an external signal before continuing.
    #[pyo3(signature = (signal_id, signal_name, timeout_secs=None))]
    fn wait_for_signal(
        &mut self,
        signal_id: String,
        signal_name: String,
        timeout_secs: Option<f64>,
    ) -> PyResult<()> {
        self.inner
            .add_signal(signal_id, signal_name, timeout_secs)
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
    }

    /// Add a durable delay.
    fn delay(&mut self, delay_id: String, seconds: f64) -> PyResult<()> {
        self.inner
            .add_delay(delay_id, seconds)
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
    }

    /// Add a fork with branches (each branch is a chain of tasks) and a join.
    #[pyo3(signature = (branches, join_id, join_metadata=None))]
    fn add_fork(
        &mut self,
        branches: Vec<Vec<(String, Option<PyTaskMetadata>)>>,
        join_id: String,
        join_metadata: Option<PyTaskMetadata>,
    ) -> PyResult<()> {
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
                    .map(|(id, meta)| (id, meta.map(Into::into).unwrap_or_default()))
                    .collect()
            })
            .collect();
        self.inner
            .add_fork(
                branches,
                join_id,
                join_metadata.map(Into::into).unwrap_or_default(),
            )
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
    }

    /// Add a conditional branch node.
    ///
    /// The branch ID is auto-generated internally. The key function is
    /// registered in the Python registry as `"{branch_id}::key_fn"`.
    /// `branches` is a list of `(key, [(task_id, metadata), ...])` pairs.
    /// `default` is an optional chain for unmatched keys.
    ///
    /// Returns the generated branch ID.
    #[pyo3(signature = (branches, default=None))]
    #[allow(clippy::type_complexity)]
    fn add_branch(
        &mut self,
        branches: Vec<(String, Vec<(String, Option<PyTaskMetadata>)>)>,
        default: Option<Vec<(String, Option<PyTaskMetadata>)>>,
    ) -> PyResult<String> {
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
        self.inner
            .add_branch(branches, default)
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
    }

    /// Add a loop node.
    ///
    /// The loop ID is auto-generated internally.
    /// `body_task_id` is the task that runs each iteration.
    /// `max_iterations` caps the iteration count.
    /// `on_max` is `"fail"` (default) or `"exit_with_last"`.
    ///
    /// Returns the generated loop ID.
    #[pyo3(signature = (body_task_id, body_metadata=None, max_iterations=10, on_max="fail"))]
    fn add_loop(
        &mut self,
        body_task_id: String,
        body_metadata: Option<PyTaskMetadata>,
        max_iterations: u32,
        on_max: &str,
    ) -> PyResult<String> {
        let on_max: sayiir_core::workflow::MaxIterationsPolicy = on_max.parse().map_err(|_| {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "Invalid on_max policy: '{on_max}'. Use 'fail' or 'exit_with_last'."
            ))
        })?;
        self.inner
            .add_loop(
                body_task_id,
                body_metadata.map(Into::into).unwrap_or_default(),
                max_iterations,
                on_max,
            )
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
    }

    /// Add a child workflow (inline composition).
    fn add_child_workflow(&mut self, child_id: String, child_builder: &PyFlowBuilder) {
        self.inner
            .add_child_workflow(child_id, child_builder.inner.tasks().to_vec());
    }

    /// Build the workflow.
    fn build(&self) -> PyResult<PyWorkflow> {
        tracing::debug!(workflow_id = %self.workflow_id, "building workflow");
        let continuation = self
            .inner
            .build()
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)?;
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
