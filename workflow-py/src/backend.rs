//! Python wrappers for persistence backends.
//!
//! This module provides Python-accessible wrappers for the workflow
//! persistence backends, enabling workflow state checkpointing.

use pyo3::prelude::*;
use std::sync::Arc;

use workflow_persistence::InMemoryBackend;

/// In-memory persistence backend for development and testing.
///
/// This backend stores workflow state in memory. It's useful for:
/// - Development and testing
/// - Single-process workflows that don't need persistence
/// - Quick prototyping
///
/// For production use with crash recovery, use a persistent backend
/// like Redis or PostgreSQL.
#[pyclass]
pub struct PyInMemoryBackend {
    inner: Arc<InMemoryBackend>,
}

impl PyInMemoryBackend {
    /// Get the inner backend Arc.
    pub fn inner(&self) -> Arc<InMemoryBackend> {
        self.inner.clone()
    }
}

#[pymethods]
impl PyInMemoryBackend {
    /// Create a new in-memory backend.
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryBackend::new()),
        }
    }

    /// List all workflow instance IDs stored in this backend.
    ///
    /// Returns:
    ///     A list of instance IDs
    fn list_instances<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let _inner = self.inner.clone();

        // For now, return an empty list - full implementation would be async
        let result = pyo3::types::PyList::empty(py);
        Ok(result.into_any())
    }

    fn __repr__(&self) -> String {
        "InMemoryBackend()".to_string()
    }
}
