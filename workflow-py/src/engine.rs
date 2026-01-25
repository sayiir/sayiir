//! Workflow engine wrapping CheckpointingRunner.
//!
//! This module provides the main execution engine for Python workflows,
//! integrating with the persistence backend for checkpointing and recovery.

use pyo3::prelude::*;
use std::sync::Arc;

use workflow_persistence::InMemoryBackend;
use workflow_runtime::CheckpointingRunner;

use crate::backend::PyInMemoryBackend;
use crate::channel::TaskChannel;
use crate::codec::SerializerKind;
use crate::flow::PyWorkflow;

/// Workflow execution engine with persistence support.
///
/// The engine manages workflow execution, checkpointing, and recovery.
/// It coordinates between the Rust orchestrator and Python task executor.
#[pyclass]
pub struct PyWorkflowEngine {
    /// The channel for communicating with Python task executor
    channel: Arc<TaskChannel>,
    /// The persistence backend
    #[allow(dead_code)]
    backend: Arc<InMemoryBackend>,
    /// The checkpointing runner
    #[allow(dead_code)]
    runner: CheckpointingRunner<InMemoryBackend>,
}

#[pymethods]
impl PyWorkflowEngine {
    /// Create a new workflow engine.
    ///
    /// Args:
    ///     backend: Optional persistence backend (defaults to InMemoryBackend)
    ///     serializer: Serialization format - "json" (default) or "pickle"
    #[new]
    #[pyo3(signature = (backend=None, serializer=None))]
    fn new(backend: Option<&PyInMemoryBackend>, serializer: Option<&str>) -> PyResult<Self> {
        let serializer_kind = match serializer {
            None | Some("json") => SerializerKind::Json,
            Some("pickle") => SerializerKind::Pickle,
            Some(other) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Unknown serializer '{}'. Use 'json' or 'pickle'.",
                    other
                )));
            }
        };

        let backend = backend
            .map(|b| b.inner())
            .unwrap_or_else(|| Arc::new(InMemoryBackend::new()));

        let channel = Arc::new(TaskChannel::with_serializer(serializer_kind));
        let runner = CheckpointingRunner::new((*backend).clone());

        Ok(Self {
            channel,
            backend,
            runner,
        })
    }

    /// Get the serialization format as a string.
    #[getter]
    fn serializer(&self) -> &'static str {
        match self.channel.serializer_kind() {
            SerializerKind::Json => "json",
            SerializerKind::Pickle => "pickle",
        }
    }

    /// Get the task channel for the Python executor.
    ///
    /// The Python TaskExecutor needs this channel to poll for tasks
    /// and submit results.
    fn get_channel(&self) -> TaskChannel {
        // Create a new channel with the same serializer
        TaskChannel::with_serializer(self.channel.serializer_kind())
    }

    /// Run a workflow with the given input.
    ///
    /// Args:
    ///     workflow: The workflow to run
    ///     instance_id: Unique identifier for this workflow run
    ///     input: Input data for the workflow
    ///
    /// Returns:
    ///     The workflow output on success
    ///
    /// This method is async and should be awaited from Python.
    fn run<'py>(
        &self,
        py: Python<'py>,
        _workflow: &PyWorkflow,
        _instance_id: String,
        _input: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // For now, return a placeholder future
        // The actual implementation will need async bridging
        let asyncio = py.import("asyncio")?;
        let future = asyncio
            .call_method1("get_event_loop", ())?
            .call_method1("create_future", ())?;

        // In a full implementation, this would:
        // 1. Start the Rust orchestrator
        // 2. The orchestrator sends TaskRequests through the channel
        // 3. Python executor polls and executes tasks
        // 4. Results flow back through the channel
        // 5. Final result is set on the future

        Ok(future)
    }

    /// Resume a workflow from a checkpoint.
    ///
    /// Args:
    ///     workflow: The workflow to resume
    ///     instance_id: The workflow instance to resume
    ///
    /// Returns:
    ///     The workflow output on success
    fn resume<'py>(
        &self,
        py: Python<'py>,
        _workflow: &PyWorkflow,
        _instance_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let asyncio = py.import("asyncio")?;
        let future = asyncio
            .call_method1("get_event_loop", ())?
            .call_method1("create_future", ())?;
        Ok(future)
    }

    /// Cancel a running workflow.
    ///
    /// Args:
    ///     instance_id: The workflow instance to cancel
    ///     reason: Optional reason for cancellation
    #[pyo3(signature = (instance_id, reason=None))]
    fn cancel(&self, instance_id: String, reason: Option<String>) -> PyResult<()> {
        let _ = (instance_id, reason);
        // In a full implementation, this would call the backend's
        // request_cancellation method
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!("WorkflowEngine(serializer='{}')", self.serializer())
    }
}
