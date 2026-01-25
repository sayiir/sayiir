//! Flow builder for constructing workflows from Python.
//!
//! This module provides Python-accessible classes for building workflows
//! using the fluent builder pattern.

use pyo3::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use workflow_core::registry::TaskRegistry;
use workflow_core::task::{RetryPolicy, TaskMetadata};

use crate::channel::TaskChannel;
use crate::codec::{PyCodec, SerializerKind};
use crate::task::ChannelTaskWrapper;

/// Task metadata for configuration (retries, timeout, tags).
#[pyclass]
#[derive(Clone, Default)]
pub struct PyTaskMetadata {
    /// Number of retry attempts on failure
    #[pyo3(get, set)]
    pub retries: u32,
    /// Timeout in seconds (None = no timeout)
    #[pyo3(get, set)]
    pub timeout: Option<f64>,
    /// Tags for categorization
    #[pyo3(get, set)]
    pub tags: Vec<String>,
}

#[pymethods]
impl PyTaskMetadata {
    #[new]
    #[pyo3(signature = (retries=0, timeout=None, tags=None))]
    fn new(retries: u32, timeout: Option<f64>, tags: Option<Vec<String>>) -> Self {
        Self {
            retries,
            timeout,
            tags: tags.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "TaskMetadata(retries={}, timeout={:?}, tags={:?})",
            self.retries, self.timeout, self.tags
        )
    }
}

impl PyTaskMetadata {
    /// Convert to the core TaskMetadata type.
    pub fn to_core_metadata(&self) -> TaskMetadata {
        TaskMetadata {
            display_name: None,
            description: None,
            timeout: self.timeout.map(Duration::from_secs_f64),
            retries: if self.retries > 0 {
                Some(RetryPolicy {
                    max_attempts: self.retries,
                    initial_delay: Duration::from_millis(100),
                    backoff_multiplier: 2.0,
                })
            } else {
                None
            },
            tags: self.tags.clone(),
        }
    }
}

/// Internal state for building a workflow.
struct FlowBuilderState {
    /// Name of the workflow
    name: String,
    /// Task registry for serializable workflows
    registry: TaskRegistry,
    /// Channel for task execution
    channel: Arc<TaskChannel>,
    /// Codec for serialization
    codec: Arc<PyCodec>,
    /// Ordered list of task IDs
    tasks: Vec<String>,
    /// Fork/join structure (simplified for now)
    forks: Vec<ForkDef>,
}

/// Definition of a fork with branches
#[allow(dead_code)]
struct ForkDef {
    branches: Vec<BranchDef>,
    join_task_id: Option<String>,
}

/// Definition of a single branch in a fork
#[allow(dead_code)]
struct BranchDef {
    name: String,
    task_id: String,
}

/// Builder for constructing workflows.
///
/// Provides a fluent API for defining sequential tasks and fork/join patterns.
#[pyclass]
pub struct PyFlowBuilder {
    state: Mutex<FlowBuilderState>,
}

impl PyFlowBuilder {
    /// Create a new flow builder with a shared channel.
    pub fn new_with_channel(name: String, channel: Arc<TaskChannel>) -> Self {
        Self {
            state: Mutex::new(FlowBuilderState {
                name,
                registry: TaskRegistry::new(),
                channel,
                codec: Arc::new(PyCodec::new()),
                tasks: Vec::new(),
                forks: Vec::new(),
            }),
        }
    }

    /// Get a reference to the internal registry.
    pub fn registry(&self) -> TaskRegistry {
        TaskRegistry::new()
    }

    /// Get the channel.
    pub fn channel(&self) -> Arc<TaskChannel> {
        self.state.lock().unwrap().channel.clone()
    }
}

#[pymethods]
impl PyFlowBuilder {
    /// Create a new flow builder with the given name.
    ///
    /// Args:
    ///     name: The workflow name
    ///     serializer: Serialization format - "json" (default) or "pickle"
    #[new]
    #[pyo3(signature = (name, serializer=None))]
    fn py_new(name: String, serializer: Option<&str>) -> PyResult<Self> {
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

        let channel_arc = Arc::new(TaskChannel::with_serializer(serializer_kind));
        let codec = Arc::new(PyCodec::with_kind(serializer_kind));

        Ok(Self {
            state: Mutex::new(FlowBuilderState {
                name,
                registry: TaskRegistry::new(),
                channel: channel_arc,
                codec,
                tasks: Vec::new(),
                forks: Vec::new(),
            }),
        })
    }

    /// Add a sequential task to the workflow.
    ///
    /// Args:
    ///     task_id: Unique identifier for the task
    ///     metadata: Optional task metadata (retries, timeout, tags)
    ///
    /// Returns:
    ///     Self for method chaining
    #[pyo3(signature = (task_id, metadata=None))]
    fn then(&self, task_id: String, metadata: Option<PyTaskMetadata>) -> PyResult<()> {
        let mut state = self.state.lock().unwrap();

        // Clone what we need before the mutable borrow
        let channel = state.channel.clone();
        let codec = state.codec.clone();

        // Create the channel task wrapper
        let task = ChannelTaskWrapper::new(task_id.clone(), channel);

        // Register the task with metadata
        let task_meta = metadata.map(|m| m.to_core_metadata()).unwrap_or_default();
        state
            .registry
            .register_with_metadata(&task_id, codec, task, task_meta);

        state.tasks.push(task_id);
        Ok(())
    }

    /// Start a fork for parallel execution.
    ///
    /// Returns:
    ///     A ForkBuilder for adding branches
    fn fork(slf: PyRef<'_, Self>) -> PyForkBuilder {
        PyForkBuilder {
            parent: slf.into(),
            branches: Vec::new(),
        }
    }

    /// Build the workflow.
    ///
    /// Returns:
    ///     A PyWorkflow that can be run by the engine
    fn build(&self) -> PyResult<PyWorkflow> {
        let state = self.state.lock().unwrap();
        Ok(PyWorkflow {
            name: state.name.clone(),
            task_ids: state.tasks.clone(),
        })
    }

    fn __repr__(&self) -> String {
        let state = self.state.lock().unwrap();
        format!(
            "FlowBuilder(name='{}', tasks={})",
            state.name,
            state.tasks.len()
        )
    }
}

/// Builder for fork/join parallel execution.
#[pyclass]
pub struct PyForkBuilder {
    parent: Py<PyFlowBuilder>,
    branches: Vec<(String, String, Option<PyTaskMetadata>)>, // (name, task_id, metadata)
}

#[pymethods]
impl PyForkBuilder {
    /// Add a branch to the fork.
    ///
    /// Args:
    ///     name: Name of the branch (used to access output in join)
    ///     task_id: The task to execute in this branch
    ///     metadata: Optional task metadata
    ///
    /// Returns:
    ///     Self for method chaining
    #[pyo3(signature = (name, task_id, metadata=None))]
    fn branch(
        mut slf: PyRefMut<'_, Self>,
        name: String,
        task_id: String,
        metadata: Option<PyTaskMetadata>,
    ) -> PyRefMut<'_, Self> {
        slf.branches.push((name, task_id, metadata));
        slf
    }

    /// Join the fork branches with a combining task.
    ///
    /// Args:
    ///     task_id: The task that receives outputs from all branches
    ///     metadata: Optional task metadata
    ///
    /// Returns:
    ///     The parent FlowBuilder for continued chaining
    #[pyo3(signature = (task_id, metadata=None))]
    fn join(
        slf: PyRef<'_, Self>,
        py: Python<'_>,
        task_id: String,
        metadata: Option<PyTaskMetadata>,
    ) -> PyResult<Py<PyFlowBuilder>> {
        let parent = slf.parent.bind(py);
        let parent_ref = parent.borrow();
        let mut state = parent_ref.state.lock().unwrap();

        // Clone what we need before registering
        let channel = state.channel.clone();
        let codec = state.codec.clone();

        // Register all branch tasks
        for (_branch_name, branch_task_id, branch_meta) in &slf.branches {
            let task = ChannelTaskWrapper::new(branch_task_id.clone(), channel.clone());
            let task_meta = branch_meta
                .clone()
                .map(|m| m.to_core_metadata())
                .unwrap_or_default();
            state
                .registry
                .register_with_metadata(branch_task_id, codec.clone(), task, task_meta);
        }

        // Register join task
        let join_task = ChannelTaskWrapper::new(task_id.clone(), channel);
        let join_meta = metadata.map(|m| m.to_core_metadata()).unwrap_or_default();
        state
            .registry
            .register_with_metadata(&task_id, codec, join_task, join_meta);

        // Record the fork structure
        state.forks.push(ForkDef {
            branches: slf
                .branches
                .iter()
                .map(|(name, task_id, _)| BranchDef {
                    name: name.clone(),
                    task_id: task_id.clone(),
                })
                .collect(),
            join_task_id: Some(task_id),
        });

        drop(state);
        drop(parent_ref);
        Ok(slf.parent.clone_ref(py))
    }

    fn __repr__(&self) -> String {
        format!("ForkBuilder(branches={})", self.branches.len())
    }
}

/// A built workflow ready for execution.
#[pyclass]
#[derive(Clone)]
pub struct PyWorkflow {
    /// Name of the workflow
    #[pyo3(get)]
    pub name: String,
    /// List of task IDs in execution order
    task_ids: Vec<String>,
}

#[pymethods]
impl PyWorkflow {
    /// Get the list of task IDs in this workflow.
    fn get_task_ids(&self) -> Vec<String> {
        self.task_ids.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "Workflow(name='{}', tasks={})",
            self.name,
            self.task_ids.len()
        )
    }
}
