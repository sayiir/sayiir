//! Task metadata types exposed to Python.

use pyo3::prelude::*;
use std::time::Duration;

use sayiir_core::context::TaskExecutionContext;
use sayiir_core::task::{RetryPolicy, TaskMetadata};

/// Python-exposed retry policy.
#[pyclass(from_py_object)]
#[derive(Clone, Default)]
pub struct PyRetryPolicy {
    #[pyo3(get, set)]
    pub max_retries: u32,
    #[pyo3(get, set)]
    pub initial_delay_secs: f64,
    #[pyo3(get, set)]
    pub backoff_multiplier: f64,
}

#[pymethods]
impl PyRetryPolicy {
    #[new]
    #[pyo3(signature = (max_retries=2, initial_delay_secs=1.0, backoff_multiplier=2.0))]
    fn new(max_retries: u32, initial_delay_secs: f64, backoff_multiplier: f64) -> Self {
        Self {
            max_retries,
            initial_delay_secs,
            backoff_multiplier,
        }
    }
}

impl From<PyRetryPolicy> for RetryPolicy {
    #[allow(clippy::cast_possible_truncation)]
    fn from(py: PyRetryPolicy) -> Self {
        RetryPolicy {
            max_retries: py.max_retries,
            initial_delay: Duration::from_secs_f64(py.initial_delay_secs),
            backoff_multiplier: py.backoff_multiplier as f32,
            max_delay: None,
        }
    }
}

/// Python-exposed task metadata.
#[pyclass(from_py_object)]
#[derive(Clone, Default)]
pub struct PyTaskMetadata {
    #[pyo3(get, set)]
    pub display_name: Option<String>,
    #[pyo3(get, set)]
    pub description: Option<String>,
    #[pyo3(get, set)]
    pub timeout_secs: Option<f64>,
    #[pyo3(get, set)]
    pub retries: Option<PyRetryPolicy>,
    #[pyo3(get, set)]
    pub tags: Option<Vec<String>>,
    #[pyo3(get, set)]
    pub version: Option<String>,
}

#[pymethods]
impl PyTaskMetadata {
    #[new]
    #[pyo3(signature = (display_name=None, description=None, timeout_secs=None, retries=None, tags=None, version=None))]
    fn new(
        display_name: Option<String>,
        description: Option<String>,
        timeout_secs: Option<f64>,
        retries: Option<PyRetryPolicy>,
        tags: Option<Vec<String>>,
        version: Option<String>,
    ) -> Self {
        Self {
            display_name,
            description,
            timeout_secs,
            retries,
            tags,
            version,
        }
    }
}

impl From<PyTaskMetadata> for TaskMetadata {
    fn from(py: PyTaskMetadata) -> Self {
        TaskMetadata {
            display_name: py.display_name,
            description: py.description,
            timeout: py.timeout_secs.map(Duration::from_secs_f64),
            retries: py.retries.map(Into::into),
            tags: py.tags.unwrap_or_default(),
            version: py.version,
        }
    }
}

/// Task execution context available from within a running task.
///
/// Provides read-only access to workflow and task metadata.
/// Retrieve via `get_task_context()`.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub struct PyTaskExecutionContext {
    #[pyo3(get)]
    pub workflow_id: String,
    #[pyo3(get)]
    pub instance_id: String,
    #[pyo3(get)]
    pub task_id: String,
    #[pyo3(get)]
    pub metadata: PyTaskMetadata,
    /// Raw JSON string for workflow metadata (deserialized lazily in the getter).
    workflow_metadata_json: Option<String>,
}

#[pymethods]
impl PyTaskExecutionContext {
    #[getter]
    fn workflow_metadata(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.workflow_metadata_json {
            None => Ok(None),
            Some(json) => {
                let json_mod = py.import("json")?;
                let val = json_mod.call_method1("loads", (json.as_str(),))?;
                Ok(Some(val.unbind()))
            }
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "TaskExecutionContext(workflow_id='{}', instance_id='{}', task_id='{}')",
            self.workflow_id, self.instance_id, self.task_id
        )
    }
}

impl From<TaskExecutionContext> for PyTaskExecutionContext {
    fn from(ctx: TaskExecutionContext) -> Self {
        Self {
            workflow_id: ctx.workflow_id.to_string(),
            instance_id: ctx.instance_id.to_string(),
            task_id: ctx.task_id.to_string(),
            metadata: PyTaskMetadata {
                display_name: ctx.metadata.display_name,
                description: ctx.metadata.description,
                timeout_secs: ctx.metadata.timeout.map(|d| d.as_secs_f64()),
                retries: ctx.metadata.retries.map(|r| PyRetryPolicy {
                    max_retries: r.max_retries,
                    initial_delay_secs: r.initial_delay.as_secs_f64(),
                    backoff_multiplier: f64::from(r.backoff_multiplier),
                }),
                tags: Some(ctx.metadata.tags),
                version: ctx.metadata.version,
            },
            workflow_metadata_json: ctx.workflow_metadata_json.map(|s| s.to_string()),
        }
    }
}

/// Get the current task execution context.
///
/// Returns `None` if called outside of a task execution.
///
/// ```python
/// ctx = get_task_context()
/// if ctx is not None:
///     print(f"Running task {ctx.task_id} in workflow {ctx.workflow_id}")
/// ```
#[pyfunction]
pub fn get_task_context() -> Option<PyTaskExecutionContext> {
    sayiir_core::context::get_task_context().map(Into::into)
}
