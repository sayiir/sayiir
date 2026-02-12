//! Python-exposed in-memory persistence backend.

use pyo3::prelude::*;
use sayiir_persistence::InMemoryBackend;
use std::sync::Arc;

/// Python-exposed in-memory persistence backend.
///
/// Stores workflow snapshots in memory. Suitable for testing and development.
/// For production, implement a custom backend (Redis, `PostgreSQL`, etc.).
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct PyInMemoryBackend {
    pub(crate) inner: Arc<InMemoryBackend>,
}

#[pymethods]
impl PyInMemoryBackend {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryBackend::new()),
        }
    }

    fn __repr__(&self) -> String {
        "InMemoryBackend()".to_string()
    }
}
