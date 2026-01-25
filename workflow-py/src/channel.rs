//! Task execution channel for Python-Rust communication.
//!
//! This module provides the communication channel between the Rust workflow
//! orchestrator and Python task executor. When a task needs to execute,
//! Rust sends a TaskRequest through the channel, Python executes the task,
//! and sends back a TaskResponse.

use bytes::Bytes;
use crossbeam_channel::{self, Receiver, Sender};
use dashmap::DashMap;
use pyo3::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::Waker;

use crate::codec::{PyCodec, SerializerKind};

/// Request for Python to execute a task.
#[pyclass]
#[derive(Clone)]
pub struct TaskRequest {
    /// Unique ID for correlating request/response
    #[pyo3(get)]
    pub request_id: u64,
    /// The task identifier
    #[pyo3(get)]
    pub task_id: String,
    /// Serialized input data
    input: Bytes,
    /// Serialization format used
    serializer_kind: SerializerKind,
}

#[pymethods]
impl TaskRequest {
    /// Get the input data as a Python object (deserializes using configured format).
    fn get_input(&self, py: Python<'_>) -> PyResult<PyObject> {
        PyCodec::decode_to_pyobject(py, &self.input, self.serializer_kind)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
    }

    fn __repr__(&self) -> String {
        format!(
            "TaskRequest(request_id={}, task_id='{}')",
            self.request_id, self.task_id
        )
    }
}

/// Response from Python after task execution.
pub struct TaskResponse {
    /// Correlates with TaskRequest.request_id
    pub request_id: u64,
    /// Result: Ok(output_bytes) or Err(error_message)
    pub result: Result<Bytes, String>,
}

/// Communication channel between Rust orchestrator and Python executor.
///
/// The Rust side sends TaskRequests and receives TaskResponses.
/// The Python side polls for requests, executes tasks, and submits results.
#[pyclass]
pub struct TaskChannel {
    /// Sender for task requests (Rust -> Python)
    request_tx: Sender<TaskRequest>,
    /// Receiver for task requests (Python polls this) - crossbeam receivers are Clone + Send
    request_rx: Receiver<TaskRequest>,
    /// Map of pending responses keyed by request_id (concurrent hash map)
    pending_responses: Arc<DashMap<u64, TaskResponse>>,
    /// Wakers waiting for responses, keyed by request_id
    wakers: Arc<DashMap<u64, Waker>>,
    /// Counter for generating unique request IDs
    next_request_id: AtomicU64,
    /// Serialization format
    serializer_kind: SerializerKind,
}

impl TaskChannel {
    /// Create a new TaskChannel with JSON serialization (default).
    pub fn new() -> Self {
        Self::with_serializer(SerializerKind::Json)
    }

    /// Create a new TaskChannel with the specified serialization format.
    pub fn with_serializer(serializer_kind: SerializerKind) -> Self {
        let (request_tx, request_rx) = crossbeam_channel::unbounded();
        Self {
            request_tx,
            request_rx,
            pending_responses: Arc::new(DashMap::new()),
            wakers: Arc::new(DashMap::new()),
            next_request_id: AtomicU64::new(1),
            serializer_kind,
        }
    }

    /// Get the serialization format.
    pub fn serializer_kind(&self) -> SerializerKind {
        self.serializer_kind
    }

    /// Generate the next unique request ID.
    pub fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a task request to Python (called from Rust).
    pub fn send_request(&self, request: TaskRequest) -> anyhow::Result<()> {
        self.request_tx
            .send(request)
            .map_err(|e| anyhow::anyhow!("Failed to send task request: {}", e))
    }

    /// Try to get a response for the given request_id (non-blocking).
    /// Returns Some(response) if available, None if still pending.
    pub fn try_recv_response(&self, request_id: u64) -> Option<TaskResponse> {
        self.pending_responses.remove(&request_id).map(|(_, v)| v)
    }

    /// Register a waker to be notified when response for request_id arrives.
    pub fn register_waker(&self, request_id: u64, waker: Waker) {
        self.wakers.insert(request_id, waker);
    }

    /// Store a response and wake the waiting future.
    fn store_response(&self, response: TaskResponse) {
        let request_id = response.request_id;
        self.pending_responses.insert(request_id, response);
        // Wake the future waiting for this response
        if let Some((_, waker)) = self.wakers.remove(&request_id) {
            waker.wake();
        }
    }
}

impl Default for TaskChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[pymethods]
impl TaskChannel {
    /// Create a new TaskChannel.
    ///
    /// Args:
    ///     serializer: Serialization format - "json" (default) or "pickle"
    #[new]
    #[pyo3(signature = (serializer=None))]
    fn py_new(serializer: Option<&str>) -> PyResult<Self> {
        let kind = match serializer {
            None | Some("json") => SerializerKind::Json,
            Some("pickle") => SerializerKind::Pickle,
            Some(other) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Unknown serializer '{}'. Use 'json' or 'pickle'.",
                    other
                )));
            }
        };
        Ok(Self::with_serializer(kind))
    }

    /// Get the serialization format as a string.
    #[getter]
    fn serializer(&self) -> &'static str {
        match self.serializer_kind {
            SerializerKind::Json => "json",
            SerializerKind::Pickle => "pickle",
        }
    }

    /// Poll for the next task request (non-blocking, returns None if empty).
    ///
    /// Returns a TaskRequest if one is available, or None if the queue is empty.
    fn poll_task(&self) -> Option<TaskRequest> {
        self.request_rx.try_recv().ok()
    }

    /// Submit a successful task result.
    ///
    /// Args:
    ///     request_id: The request ID from the TaskRequest
    ///     result: The result object to serialize and send back
    fn submit_result(
        &self,
        py: Python<'_>,
        request_id: u64,
        result: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let output = PyCodec::encode_pyobject(py, result, self.serializer_kind)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

        self.store_response(TaskResponse {
            request_id,
            result: Ok(output),
        });
        Ok(())
    }

    /// Submit a task error.
    ///
    /// Args:
    ///     request_id: The request ID from the TaskRequest
    ///     error: The error message
    fn submit_error(&self, request_id: u64, error: String) {
        self.store_response(TaskResponse {
            request_id,
            result: Err(error),
        });
    }

    fn __repr__(&self) -> String {
        format!("TaskChannel(serializer='{}')", self.serializer())
    }
}

/// Create a TaskRequest (used internally by ChannelTaskWrapper).
pub fn create_request(
    request_id: u64,
    task_id: String,
    input: Bytes,
    serializer_kind: SerializerKind,
) -> TaskRequest {
    TaskRequest {
        request_id,
        task_id,
        input,
        serializer_kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_creation() {
        let channel = TaskChannel::new();
        assert_eq!(channel.next_request_id(), 1);
        assert_eq!(channel.next_request_id(), 2);
    }

    #[test]
    fn test_channel_with_pickle() {
        let channel = TaskChannel::with_serializer(SerializerKind::Pickle);
        assert_eq!(channel.serializer_kind(), SerializerKind::Pickle);
    }

    #[test]
    fn test_poll_empty() {
        let channel = TaskChannel::new();
        assert!(channel.poll_task().is_none());
    }

    #[test]
    fn test_send_and_poll() {
        let channel = TaskChannel::new();
        let request = create_request(
            1,
            "test_task".to_string(),
            Bytes::from("{}"),
            SerializerKind::Json,
        );
        channel.send_request(request).unwrap();

        let polled = channel.poll_task();
        assert!(polled.is_some());
        let polled = polled.unwrap();
        assert_eq!(polled.request_id, 1);
        assert_eq!(polled.task_id, "test_task");
    }
}
