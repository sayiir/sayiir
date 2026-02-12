//! Task metadata types exposed to Python.

use pyo3::prelude::*;
use std::time::Duration;

use workflow_core::task::{RetryPolicy, TaskMetadata};

/// Python-exposed retry policy.
#[pyclass(from_py_object)]
#[derive(Clone, Default)]
pub struct PyRetryPolicy {
    #[pyo3(get, set)]
    pub max_attempts: u32,
    #[pyo3(get, set)]
    pub initial_delay_secs: f64,
    #[pyo3(get, set)]
    pub backoff_multiplier: f64,
}

#[pymethods]
impl PyRetryPolicy {
    #[new]
    #[pyo3(signature = (max_attempts=3, initial_delay_secs=1.0, backoff_multiplier=2.0))]
    fn new(max_attempts: u32, initial_delay_secs: f64, backoff_multiplier: f64) -> Self {
        Self {
            max_attempts,
            initial_delay_secs,
            backoff_multiplier,
        }
    }
}

impl From<PyRetryPolicy> for RetryPolicy {
    #[allow(clippy::cast_possible_truncation)]
    fn from(py: PyRetryPolicy) -> Self {
        RetryPolicy {
            max_attempts: py.max_attempts,
            initial_delay: Duration::from_secs_f64(py.initial_delay_secs),
            backoff_multiplier: py.backoff_multiplier as f32,
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
}

#[pymethods]
impl PyTaskMetadata {
    #[new]
    #[pyo3(signature = (display_name=None, description=None, timeout_secs=None, retries=None, tags=None))]
    fn new(
        display_name: Option<String>,
        description: Option<String>,
        timeout_secs: Option<f64>,
        retries: Option<PyRetryPolicy>,
        tags: Option<Vec<String>>,
    ) -> Self {
        Self {
            display_name,
            description,
            timeout_secs,
            retries,
            tags,
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
        }
    }
}
