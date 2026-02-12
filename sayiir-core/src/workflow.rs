use crate::codec::Codec;
use crate::codec::sealed;
use crate::context::WorkflowContext;
use crate::error::WorkflowError;
use crate::task::{
    BranchOutputs, ErasedBranch, UntypedCoreTask, branch, to_core_task_arc,
    to_heterogeneous_join_task_arc,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;

/// Generate a `find_duplicate_id` method for continuation-like enums
///
macro_rules! impl_find_duplicate_id {
    ($name:ident, task_fields: { $($task_extra:tt)* }, delay_extra: { $($delay_extra:tt)* }, deref_branch: $deref:expr) => {
        impl $name {
            fn find_duplicate_id(&self) -> Option<String> {
                fn collect(cont: &$name, seen: &mut HashSet<String>) -> Option<String> {
                    match cont {
                        $name::Task { id, next, $($task_extra)* } => {
                            if !seen.insert(id.clone()) {
                                return Some(id.clone());
                            }
                            next.as_ref().and_then(|n| collect(n, seen))
                        }
                        $name::Fork { id, branches, join } => {
                            if !seen.insert(id.clone()) {
                                return Some(id.clone());
                            }
                            let deref_fn: fn(&_) -> &$name = $deref;
                            branches
                                .iter()
                                .find_map(|b| collect(deref_fn(b), seen))
                                .or_else(|| join.as_ref().and_then(|j| collect(j, seen)))
                        }
                        $name::Delay { id, next, $($delay_extra)* } => {
                            if !seen.insert(id.clone()) {
                                return Some(id.clone());
                            }
                            next.as_ref().and_then(|n| collect(n, seen))
                        }
                    }
                }
                collect(self, &mut HashSet::new())
            }
        }
    };
}

/// A workflow structure representing the tasks to execute.
pub enum WorkflowContinuation {
    Task {
        id: String,
        /// Task implementation. `None` for registry-based execution
        /// where tasks are looked up by `id` at runtime.
        func: Option<UntypedCoreTask>,
        next: Option<Box<WorkflowContinuation>>,
    },
    Fork {
        id: String,
        branches: Box<[Arc<WorkflowContinuation>]>,
        join: Option<Box<WorkflowContinuation>>,
    },
    /// A durable delay node. Input passes through unchanged.
    Delay {
        id: String,
        duration: std::time::Duration,
        next: Option<Box<WorkflowContinuation>>,
    },
}

impl_find_duplicate_id!(
    WorkflowContinuation,
    task_fields: { .. },
    delay_extra: { .. },
    deref_branch: |b: &Arc<WorkflowContinuation>| -> &WorkflowContinuation { b }
);

impl WorkflowContinuation {
    /// Derive a fork ID from a list of branch IDs.
    ///
    /// The fork ID is a concatenation of branch IDs separated by `||`.
    #[must_use]
    pub fn derive_fork_id(branch_ids: &[&str]) -> String {
        branch_ids.join("||")
    }

    /// Get the ID of this continuation node.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            WorkflowContinuation::Task { id, .. }
            | WorkflowContinuation::Fork { id, .. }
            | WorkflowContinuation::Delay { id, .. } => id,
        }
    }

    /// Get the first task ID from this continuation.
    ///
    /// For a `Task`, returns its ID. For a `Fork`, returns the first task ID
    /// from the first branch.
    #[must_use]
    pub fn first_task_id(&self) -> &str {
        match self {
            WorkflowContinuation::Task { id, .. } | WorkflowContinuation::Delay { id, .. } => id,
            WorkflowContinuation::Fork { branches, .. } => {
                if let Some(first_branch) = branches.first() {
                    first_branch.first_task_id()
                } else {
                    "unknown"
                }
            }
        }
    }

    /// Convert to a serializable representation (strips out task implementations).
    #[must_use]
    pub fn to_serializable(&self) -> SerializableContinuation {
        match self {
            WorkflowContinuation::Task { id, next, .. } => SerializableContinuation::Task {
                id: id.clone(),
                next: next.as_ref().map(|n| Box::new(n.to_serializable())),
            },
            WorkflowContinuation::Fork { id, branches, join } => SerializableContinuation::Fork {
                id: id.clone(),
                branches: branches.iter().map(|b| b.to_serializable()).collect(),
                join: join.as_ref().map(|j| Box::new(j.to_serializable())),
            },
            #[allow(clippy::cast_possible_truncation)] // Durations > u64::MAX ms are not realistic
            WorkflowContinuation::Delay { id, duration, next } => SerializableContinuation::Delay {
                id: id.clone(),
                duration_ms: duration.as_millis() as u64,
                next: next.as_ref().map(|n| Box::new(n.to_serializable())),
            },
        }
    }
}

/// A serializable workflow continuation (stores only IDs and structure).
///
/// This type can be serialized/deserialized and later converted back into a runnable
/// `WorkflowContinuation` using a `TaskRegistry`.
///
/// # Serialization
///
/// ```rust,ignore
/// // Serialize a workflow
/// let serializable = workflow.continuation().to_serializable();
/// let json = serde_json::to_string(&serializable)?;
///
/// // Deserialize and convert to runnable
/// let serializable: SerializableContinuation = serde_json::from_str(&json)?;
/// let continuation = serializable.to_runnable(&registry)?;
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SerializableContinuation {
    Task {
        id: String,
        next: Option<Box<SerializableContinuation>>,
    },
    Fork {
        id: String,
        branches: Vec<SerializableContinuation>,
        join: Option<Box<SerializableContinuation>>,
    },
    Delay {
        id: String,
        duration_ms: u64,
        next: Option<Box<SerializableContinuation>>,
    },
}

impl_find_duplicate_id!(
    SerializableContinuation,
    task_fields: {},
    delay_extra: { .. },
    deref_branch: |b: &SerializableContinuation| -> &SerializableContinuation { b }
);

impl SerializableContinuation {
    /// Convert this serializable continuation into a runnable `WorkflowContinuation`.
    ///
    /// Looks up each task ID in the registry to get the actual implementation.
    ///
    /// # Errors
    ///
    /// Returns `WorkflowError::TaskNotFound` if any task ID is not in the registry.
    pub fn to_runnable(
        &self,
        registry: &crate::registry::TaskRegistry,
    ) -> Result<WorkflowContinuation, WorkflowError> {
        if let Some(dup) = self.find_duplicate_id() {
            return Err(WorkflowError::DuplicateTaskId(dup));
        }

        self.to_runnable_unchecked(registry)
    }

    /// Convert without duplicate check (called after validation).
    fn to_runnable_unchecked(
        &self,
        registry: &crate::registry::TaskRegistry,
    ) -> Result<WorkflowContinuation, WorkflowError> {
        match self {
            SerializableContinuation::Task { id, next } => {
                let func = registry
                    .get(id)
                    .ok_or_else(|| WorkflowError::TaskNotFound(id.clone()))?;
                let next = next
                    .as_ref()
                    .map(|n| n.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Task {
                    id: id.clone(),
                    func: Some(func),
                    next,
                })
            }
            SerializableContinuation::Fork { id, branches, join } => {
                let branches: Result<Vec<_>, _> = branches
                    .iter()
                    .map(|b| b.to_runnable_unchecked(registry).map(Arc::new))
                    .collect();
                let join = join
                    .as_ref()
                    .map(|j| j.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Fork {
                    id: id.clone(),
                    branches: branches?.into_boxed_slice(),
                    join,
                })
            }
            SerializableContinuation::Delay {
                id,
                duration_ms,
                next,
            } => {
                let next = next
                    .as_ref()
                    .map(|n| n.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Delay {
                    id: id.clone(),
                    duration: std::time::Duration::from_millis(*duration_ms),
                    next,
                })
            }
        }
    }

    /// Get all task IDs referenced in this continuation.
    #[must_use]
    pub fn task_ids(&self) -> Vec<&str> {
        fn collect<'a>(cont: &'a SerializableContinuation, ids: &mut Vec<&'a str>) {
            match cont {
                SerializableContinuation::Task { id, next }
                | SerializableContinuation::Delay { id, next, .. } => {
                    ids.push(id.as_str());
                    if let Some(n) = next {
                        collect(n, ids);
                    }
                }
                SerializableContinuation::Fork { id, branches, join } => {
                    ids.push(id.as_str());
                    for b in branches {
                        collect(b, ids);
                    }
                    if let Some(j) = join {
                        collect(j, ids);
                    }
                }
            }
        }
        let mut ids = Vec::new();
        collect(self, &mut ids);
        ids
    }

    /// Compute a SHA256 hash of this continuation's structure.
    ///
    /// This hash serves as a "version" identifier for the workflow definition.
    /// It can be used to detect when a serialized workflow state was created
    /// with a different workflow definition than the current one.
    ///
    /// The hash is computed from the canonical structure of task IDs and their
    /// arrangement.
    #[must_use]
    pub fn compute_definition_hash(&self) -> String {
        fn hash_continuation(cont: &SerializableContinuation, hasher: &mut Sha256) {
            match cont {
                SerializableContinuation::Task { id, next } => {
                    hasher.update(b"T:"); // Tag for Task
                    hasher.update(id.as_bytes());
                    hasher.update(b";");
                    if let Some(n) = next {
                        hash_continuation(n, hasher);
                    }
                }
                SerializableContinuation::Fork { id, branches, join } => {
                    hasher.update(b"F:");
                    hasher.update(id.as_bytes());
                    hasher.update(b"[");
                    for branch in branches {
                        hash_continuation(branch, hasher);
                        hasher.update(b",");
                    }
                    hasher.update(b"]");
                    if let Some(j) = join {
                        hasher.update(b"J:");
                        hash_continuation(j, hasher);
                    }
                }
                SerializableContinuation::Delay {
                    id,
                    duration_ms,
                    next,
                } => {
                    hasher.update(b"D:");
                    hasher.update(id.as_bytes());
                    hasher.update(b":");
                    hasher.update(duration_ms.to_string().as_bytes());
                    hasher.update(b";");
                    if let Some(n) = next {
                        hash_continuation(n, hasher);
                    }
                }
            }
        }

        let mut hasher = Sha256::new();
        hash_continuation(self, &mut hasher);
        let result = hasher.finalize();
        format!("{result:x}")
    }
}

/// A complete serializable workflow state including version information.
///
/// This type wraps `SerializableContinuation` with workflow identification and
/// a definition hash that serves as a version check. When deserializing, the
/// hash is verified to ensure the serialized state matches the current workflow
/// definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SerializedWorkflowState {
    /// The workflow identifier.
    pub workflow_id: String,
    /// SHA256 hash of the workflow definition structure.
    /// Used to detect version mismatches during deserialization.
    pub definition_hash: String,
    /// The serializable continuation structure.
    pub continuation: SerializableContinuation,
}

/// The status of a workflow execution.
#[derive(Debug)]
pub enum WorkflowStatus {
    /// The workflow is still in progress (task completed, workflow continues).
    InProgress,
    /// The workflow completed successfully.
    Completed,
    /// The workflow failed with an error.
    Failed(String),
    /// The workflow was cancelled.
    Cancelled {
        /// Optional reason for the cancellation.
        reason: Option<String>,
        /// Optional identifier of who cancelled the workflow.
        cancelled_by: Option<String>,
    },
    /// The workflow is waiting for a delay to expire.
    Waiting {
        /// When the delay expires.
        wake_at: chrono::DateTime<chrono::Utc>,
        /// The delay node ID.
        delay_id: String,
    },
}

/// Marker type for empty continuation (no tasks yet).
pub struct NoContinuation;

/// Marker type for no registry (non-serializable workflow).
pub struct NoRegistry;

/// Trait for continuation state - allows unified handling of empty vs existing continuation.
pub trait ContinuationState {
    /// Append a new task to this continuation state, returning a `WorkflowContinuation`.
    fn append(self, new_task: WorkflowContinuation) -> WorkflowContinuation;
}

impl ContinuationState for NoContinuation {
    fn append(self, new_task: WorkflowContinuation) -> WorkflowContinuation {
        new_task
    }
}

impl ContinuationState for WorkflowContinuation {
    fn append(mut self, new_task: WorkflowContinuation) -> WorkflowContinuation {
        append_to_chain(&mut self, new_task);
        self
    }
}

use crate::registry::TaskRegistry;

/// Trait for registry behavior - allows unified implementation of builder methods.
pub trait RegistryBehavior {
    /// Register a task (no-op for `NoRegistry`, actual registration for `TaskRegistry`).
    fn maybe_register<I, O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<O, crate::error::BoxError>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static;

    /// Register a join task (no-op for `NoRegistry`, actual registration for `TaskRegistry`).
    fn maybe_register_join<O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<O, crate::error::BoxError>> + Send + 'static,
        C: Codec + sealed::EncodeValue<O> + Send + Sync + 'static;
}

impl RegistryBehavior for NoRegistry {
    fn maybe_register<I, O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<O, crate::error::BoxError>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        // No-op for non-serializable workflows
    }

    fn maybe_register_join<O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<O, crate::error::BoxError>> + Send + 'static,
        C: Codec + sealed::EncodeValue<O> + Send + Sync + 'static,
    {
        // No-op for non-serializable workflows
    }
}

impl RegistryBehavior for TaskRegistry {
    fn maybe_register<I, O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<O, crate::error::BoxError>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        use crate::task::TaskMetadata;
        self.register_fn_arc(id, codec, Arc::clone(func), TaskMetadata::default());
    }

    fn maybe_register_join<O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: &Arc<F>)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<O, crate::error::BoxError>> + Send + 'static,
        C: Codec + sealed::EncodeValue<O> + Send + Sync + 'static,
    {
        use crate::task::TaskMetadata;
        self.register_arc_join(id, codec, Arc::clone(func), TaskMetadata::default());
    }
}

pub struct WorkflowBuilder<C, Input, Output, M = (), Cont = NoContinuation, R = NoRegistry> {
    context: WorkflowContext<C, M>,
    continuation: Cont,
    registry: R,
    last_task_id: Option<String>,
    _phantom: PhantomData<(Input, Output)>,
}

/// A built workflow that can be executed.
pub struct Workflow<C, Input, M = ()> {
    definition_hash: String,
    context: WorkflowContext<C, M>,
    continuation: WorkflowContinuation,
    _phantom: PhantomData<Input>,
}

#[allow(clippy::mismatching_type_param_order)] // Input used for both Input and Output initially
impl<C, Input, M> WorkflowBuilder<C, Input, Input, M, NoContinuation, NoRegistry> {
    /// Create a new workflow builder with a context object.
    ///
    /// The context contains the workflow ID, codec and metadata that will be available
    /// at any execution point via the `sayiir_ctx!` macro.
    #[must_use]
    pub fn new(ctx: WorkflowContext<C, M>) -> Self
    where
        C: Codec,
        M: Send + Sync + 'static,
    {
        Self {
            context: ctx,
            continuation: NoContinuation,
            registry: NoRegistry,
            last_task_id: None,
            _phantom: PhantomData,
        }
    }

    /// Enable registry tracking for serializable workflows with a new empty registry.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sayiir_core::task::TaskMetadata;
    /// use std::time::Duration;
    ///
    /// let ctx = WorkflowContext::new("my-workflow", codec, metadata);
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .with_registry()  // Enable serialization
    ///     .then("step1", |i: u32| async move { Ok(i + 1) })
    ///     .with_metadata(TaskMetadata {
    ///         display_name: Some("Increment".into()),
    ///         timeout: Some(Duration::from_secs(30)),
    ///         ..Default::default()
    ///     })
    ///     .build()?;  // Returns SerializableWorkflow
    /// ```
    #[must_use]
    pub fn with_registry(
        self,
    ) -> WorkflowBuilder<C, Input, Input, M, NoContinuation, TaskRegistry> {
        self.with_existing_registry(TaskRegistry::new())
    }

    /// Enable registry tracking with an existing registry.
    ///
    /// Use this to reference pre-registered tasks via [`then_registered`] or to
    /// compose workflows from task libraries.
    ///
    /// **Note**: Takes ownership of the registry. For deserialization/hydration,
    /// rebuild the same registry from code on the deserializing side.
    /// See [`TaskRegistry`](crate::registry::TaskRegistry) docs for the pattern.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Shared function for building registry (called on both sides)
    /// fn build_registry(codec: Arc<MyCodec>) -> TaskRegistry {
    ///     let mut registry = TaskRegistry::new();
    ///     registry.register_fn("step1", codec.clone(), |i: u32| async move { Ok(i + 1) });
    ///     registry
    /// }
    ///
    /// // Build workflow
    /// let registry = build_registry(codec.clone());
    /// let ctx = WorkflowContext::new("my-workflow", codec.clone(), metadata);
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .with_existing_registry(registry)
    ///     .then_registered::<u32>("step1")
    ///     .build()?;
    ///
    /// // Deserialize (on another side): rebuild registry, then convert to runnable
    /// let registry = build_registry(codec.clone());
    /// let runnable = serialized_continuation.to_runnable(&registry)?;
    /// ```
    #[must_use]
    pub fn with_existing_registry(
        self,
        registry: TaskRegistry,
    ) -> WorkflowBuilder<C, Input, Input, M, NoContinuation, TaskRegistry> {
        WorkflowBuilder {
            context: self.context,
            continuation: NoContinuation,
            registry,
            last_task_id: None,
            _phantom: PhantomData,
        }
    }
}

/// Methods for adding tasks - unified implementation using `RegistryBehavior` and `ContinuationState`.
impl<C, Input, Output, M, Cont, R> WorkflowBuilder<C, Input, Output, M, Cont, R>
where
    R: RegistryBehavior,
    Cont: ContinuationState,
{
    /// Add a sequential task to the workflow.
    pub fn then<F, Fut, NewOutput>(
        mut self,
        id: &str,
        func: F,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, R>
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        Fut: std::future::Future<Output = Result<NewOutput, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput> + 'static,
    {
        let codec = Arc::clone(&self.context.codec);
        let func = Arc::new(func);

        // Register if registry is enabled (no-op for NoRegistry)
        self.registry
            .maybe_register::<Output, NewOutput, _, _, _>(id, codec.clone(), &func);

        let task = to_core_task_arc(func, codec);

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(task),
            next: None,
        };

        let continuation = self.continuation.append(new_task);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            _phantom: PhantomData,
        }
    }
}

/// Delay method — available for all registry/continuation combinations.
impl<C, Input, Output, M, Cont, R> WorkflowBuilder<C, Input, Output, M, Cont, R>
where
    Cont: ContinuationState,
{
    /// Add a durable delay to the workflow.
    ///
    /// The delay is transparent to data flow — the input passes through unchanged.
    /// In non-durable runners the delay is a simple sleep. In durable runners
    /// the workflow parks at the delay, persists `wake_at`, and returns
    /// `WorkflowStatus::Waiting`. A later `resume()` call advances past the
    /// delay once the wall clock reaches `wake_at`.
    #[must_use]
    pub fn delay(
        self,
        id: &str,
        duration: std::time::Duration,
    ) -> WorkflowBuilder<C, Input, Output, M, WorkflowContinuation, R> {
        let new_node = WorkflowContinuation::Delay {
            id: id.to_string(),
            duration,
            next: None,
        };
        let continuation = self.continuation.append(new_node);
        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            _phantom: PhantomData,
        }
    }
}

/// Methods for referencing pre-registered tasks (only available with `TaskRegistry`).
impl<C, Input, Output, M, Cont> WorkflowBuilder<C, Input, Output, M, Cont, TaskRegistry>
where
    Cont: ContinuationState,
{
    /// Reference a pre-registered task by ID.
    ///
    /// The task must have been registered in the registry before calling this method.
    /// Type safety is maintained through the `NewOutput` type parameter - ensure it
    /// matches the registered task's output type.
    ///
    /// # Errors
    ///
    /// Returns `WorkflowError::TaskNotFound` if the task ID is not in the registry.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sayiir_core::task::TaskMetadata;
    ///
    /// let mut registry = TaskRegistry::new();
    /// registry.register_fn("double", codec.clone(), |i: u32| async move { Ok(i * 2) });
    ///
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .with_existing_registry(registry)
    ///     .then_registered::<u32>("double")?
    ///     .with_metadata(TaskMetadata {
    ///         display_name: Some("Double".into()),
    ///         ..Default::default()
    ///     })
    ///     .build()?;
    /// ```
    pub fn then_registered<NewOutput>(
        self,
        id: &str,
    ) -> Result<
        WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, TaskRegistry>,
        WorkflowError,
    >
    where
        Output: Send + 'static,
        NewOutput: Send + 'static,
    {
        let func = self
            .registry
            .get(id)
            .ok_or_else(|| WorkflowError::TaskNotFound(id.to_string()))?;

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(func),
            next: None,
        };

        let continuation = self.continuation.append(new_task);

        Ok(WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            _phantom: PhantomData,
        })
    }

    /// Attach metadata to the most recently added task.
    ///
    /// This method allows chaining metadata after `then()`, `then_registered()`,
    /// or `join()` calls.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sayiir_core::task::{TaskMetadata, RetryPolicy};
    /// use std::time::Duration;
    ///
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .with_registry()
    ///     .then("double", |i: u32| async move { Ok(i * 2) })
    ///     .with_metadata(TaskMetadata {
    ///         display_name: Some("Double".into()),
    ///         timeout: Some(Duration::from_secs(30)),
    ///         ..Default::default()
    ///     })
    ///     .then("add_ten", |i: u32| async move { Ok(i + 10) })
    ///     .build()?;
    /// ```
    #[must_use]
    pub fn with_metadata(mut self, metadata: crate::task::TaskMetadata) -> Self {
        if let Some(ref id) = self.last_task_id {
            self.registry.set_metadata(id, metadata);
        }
        self
    }
}

/// Fork methods - unified implementation.
impl<C, Input, Output, M, Cont, R> WorkflowBuilder<C, Input, Output, M, Cont, R> {
    /// Fork the workflow into multiple parallel branches with heterogeneous outputs.
    ///
    /// Each branch receives the same input (the current workflow's output) and executes in parallel.
    /// Branches can return different types. After all branches complete, use `join()` to combine
    /// the results using `BranchOutputs` for type-safe named access.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sayiir_core::task::TaskMetadata;
    ///
    /// workflow
    ///     .then("prepare", |input| async { Ok(input) })
    ///     .with_metadata(TaskMetadata {
    ///         display_name: Some("Prepare Input".into()),
    ///         ..Default::default()
    ///     })
    ///     .branches(|b| {
    ///         b.add("count", |i: u32| async move { Ok(i * 2) });
    ///         b.add("name", |i: u32| async move { Ok(format!("item_{}", i)) });
    ///     })
    ///     .join("combine", |outputs: BranchOutputs<_>| async move {
    ///         let count: u32 = outputs.get("count")?;
    ///         let name: String = outputs.get("name")?;
    ///         Ok(format!("{}: {}", name, count))
    ///     })
    ///     .with_metadata(TaskMetadata {
    ///         display_name: Some("Combine Results".into()),
    ///         ..Default::default()
    ///     })
    /// ```
    pub fn branches<F>(self, f: F) -> ForkBuilder<C, Input, Output, M, Cont, R>
    where
        F: FnOnce(&mut BranchCollector<C, Output>),
        C: Codec,
    {
        let codec = Arc::clone(&self.context.codec);
        let mut collector = BranchCollector {
            codec,
            branches: Vec::new(),
            _phantom: PhantomData,
        };
        f(&mut collector);

        ForkBuilder {
            context: self.context,
            continuation: self.continuation,
            branches: collector.branches,
            registry: self.registry,
            _phantom: PhantomData,
        }
    }

    /// Fork the workflow into multiple parallel branches (low-level API).
    pub fn fork(self) -> ForkBuilder<C, Input, Output, M, Cont, R> {
        ForkBuilder {
            context: self.context,
            continuation: self.continuation,
            branches: Vec::new(),
            registry: self.registry,
            _phantom: PhantomData,
        }
    }
}

/// Build method for `WorkflowBuilder` without registry - returns Workflow.
impl<C, Input, Output, M> WorkflowBuilder<C, Input, Output, M, WorkflowContinuation, NoRegistry> {
    /// Build the workflow into an executable workflow.
    ///
    /// # Errors
    ///
    /// Returns an error if duplicate task IDs are found.
    pub fn build(self) -> Result<Workflow<C, Input, M>, WorkflowError>
    where
        Input: Send + 'static,
        Output: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec
            + sealed::DecodeValue<Input>
            + sealed::DecodeValue<Output>
            + sealed::EncodeValue<Input>
            + sealed::EncodeValue<Output>,
    {
        if let Some(dup) = self.continuation.find_duplicate_id() {
            return Err(WorkflowError::DuplicateTaskId(dup));
        }

        let definition_hash = self
            .continuation
            .to_serializable()
            .compute_definition_hash();

        Ok(Workflow {
            definition_hash,
            continuation: self.continuation,
            context: self.context,
            _phantom: PhantomData,
        })
    }
}

/// Build method for `WorkflowBuilder` with registry - returns `SerializableWorkflow`.
impl<C, Input, Output, M> WorkflowBuilder<C, Input, Output, M, WorkflowContinuation, TaskRegistry> {
    /// Build the workflow into a serializable workflow.
    ///
    /// # Errors
    ///
    /// Returns an error if duplicate task IDs are found.
    pub fn build(self) -> Result<SerializableWorkflow<C, Input, M>, WorkflowError>
    where
        Input: Send + 'static,
        Output: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec
            + sealed::DecodeValue<Input>
            + sealed::DecodeValue<Output>
            + sealed::EncodeValue<Input>
            + sealed::EncodeValue<Output>,
    {
        if let Some(dup) = self.continuation.find_duplicate_id() {
            return Err(WorkflowError::DuplicateTaskId(dup));
        }

        let definition_hash = self
            .continuation
            .to_serializable()
            .compute_definition_hash();

        let inner = Workflow {
            definition_hash,
            continuation: self.continuation,
            context: self.context,
            _phantom: PhantomData,
        };

        Ok(SerializableWorkflow {
            inner,
            registry: self.registry,
        })
    }
}

/// Helper function to append a task to the continuation chain.
fn append_to_chain(continuation: &mut WorkflowContinuation, new_task: WorkflowContinuation) {
    match continuation {
        WorkflowContinuation::Task { next, .. } | WorkflowContinuation::Delay { next, .. } => {
            match next {
                Some(next_box) => append_to_chain(next_box, new_task),
                None => *next = Some(Box::new(new_task)),
            }
        }
        WorkflowContinuation::Fork { join, .. } => match join {
            Some(join_box) => append_to_chain(join_box, new_task),
            None => *join = Some(Box::new(new_task)),
        },
    }
}

/// Collector for adding branches in a closure.
///
/// Used by [`WorkflowBuilder::branches`] to collect multiple branches.
pub struct BranchCollector<C, Input> {
    codec: Arc<C>,
    branches: Vec<ErasedBranch>,
    _phantom: PhantomData<Input>,
}

impl<C, Input> BranchCollector<C, Input> {
    /// Add a branch to the fork.
    ///
    /// Each branch receives the same input and can return a different output type.
    /// Duplicate IDs are checked at `build()` time.
    pub fn add<F, Fut, BranchOutput>(&mut self, id: &str, func: F)
    where
        F: Fn(Input) -> Fut + Send + Sync + 'static,
        Input: Send + 'static,
        BranchOutput: Send + 'static,
        Fut: std::future::Future<Output = Result<BranchOutput, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<BranchOutput>,
    {
        let erased = branch(id, func).erase(Arc::clone(&self.codec));
        self.branches.push(erased);
    }
}

/// Builder for constructing fork branches fluently.
///
/// Created by calling `.fork()` on a `WorkflowBuilder`. Add branches with `.branch()`,
/// then complete with `.join()`.
pub struct ForkBuilder<C, Input, Output, M, Cont = NoContinuation, R = NoRegistry> {
    context: WorkflowContext<C, M>,
    continuation: Cont,
    branches: Vec<ErasedBranch>,
    registry: R,
    _phantom: PhantomData<(Input, Output)>,
}

/// For `ForkBuilder` methods - unified implementation using `RegistryBehavior` and `ContinuationState`.
impl<C, Input, Output, M, Cont, R> ForkBuilder<C, Input, Output, M, Cont, R>
where
    R: RegistryBehavior,
    Cont: ContinuationState,
{
    /// Add a branch to the fork.
    ///
    /// # Returns
    ///
    /// Returns a new `ForkBuilder` with the branch added.
    ///
    #[must_use]
    pub fn branch<F, Fut, BranchOutput>(mut self, id: &str, func: F) -> Self
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        BranchOutput: Send + 'static,
        Fut: std::future::Future<Output = Result<BranchOutput, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<BranchOutput> + 'static,
    {
        let codec = Arc::clone(&self.context.codec);
        let func = Arc::new(func);

        // Register if registry is enabled (no-op for NoRegistry)
        self.registry
            .maybe_register::<Output, BranchOutput, _, _, _>(id, codec.clone(), &func);

        // Create branch using a closure that calls through the Arc
        let func_clone = Arc::clone(&func);
        let erased = branch(id, move |input| func_clone(input)).erase(codec);
        self.branches.push(erased);
        self
    }

    /// Join the results from all branches.
    pub fn join<F, Fut, JoinOutput>(
        mut self,
        id: &str,
        func: F,
    ) -> WorkflowBuilder<C, Input, JoinOutput, M, WorkflowContinuation, R>
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        JoinOutput: Send + 'static,
        Fut: std::future::Future<Output = Result<JoinOutput, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::EncodeValue<JoinOutput> + Send + Sync + 'static,
    {
        let codec = Arc::clone(&self.context.codec);
        let func = Arc::new(func);

        // Register if registry is enabled (no-op for NoRegistry)
        self.registry
            .maybe_register_join::<JoinOutput, _, _, _>(id, codec.clone(), &func);

        let join_task_fn = to_heterogeneous_join_task_arc(func, codec);

        let fork_id = WorkflowContinuation::derive_fork_id(
            &self
                .branches
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>(),
        );

        let branch_continuations: Vec<Arc<WorkflowContinuation>> = self
            .branches
            .into_iter()
            .map(|b| {
                Arc::new(WorkflowContinuation::Task {
                    id: b.id,
                    func: Some(b.task),
                    next: None,
                })
            })
            .collect();

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(join_task_fn),
            next: None,
        };

        let fork_continuation = WorkflowContinuation::Fork {
            id: fork_id,
            branches: branch_continuations.into_boxed_slice(),
            join: Some(Box::new(join_task)),
        };

        let continuation = self.continuation.append(fork_continuation);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            _phantom: PhantomData,
        }
    }
}

/// For `ForkBuilder` methods for referencing pre-registered tasks (only available with `TaskRegistry`).
impl<C, Input, Output, M, Cont> ForkBuilder<C, Input, Output, M, Cont, TaskRegistry>
where
    Cont: ContinuationState,
{
    /// Add a pre-registered branch task by ID.
    ///
    /// # Errors
    ///
    /// Returns `WorkflowError::TaskNotFound` if the task ID is not in the registry.
    pub fn branch_registered(mut self, id: &str) -> Result<Self, WorkflowError>
    where
        Output: Send + 'static,
    {
        let task = self
            .registry
            .get(id)
            .ok_or_else(|| WorkflowError::TaskNotFound(id.to_string()))?;

        self.branches.push(ErasedBranch {
            id: id.to_string(),
            task,
        });
        Ok(self)
    }

    /// Join using a pre-registered join task by ID.
    ///
    /// # Errors
    ///
    /// Returns `WorkflowError::TaskNotFound` if the task ID is not in the registry.
    pub fn join_registered<JoinOutput>(
        self,
        id: &str,
    ) -> Result<
        WorkflowBuilder<C, Input, JoinOutput, M, WorkflowContinuation, TaskRegistry>,
        WorkflowError,
    >
    where
        Output: Send + 'static,
        JoinOutput: Send + 'static,
    {
        let join_task_fn = self
            .registry
            .get(id)
            .ok_or_else(|| WorkflowError::TaskNotFound(id.to_string()))?;

        let fork_id = WorkflowContinuation::derive_fork_id(
            &self
                .branches
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>(),
        );

        let branch_continuations: Vec<Arc<WorkflowContinuation>> = self
            .branches
            .into_iter()
            .map(|b| {
                Arc::new(WorkflowContinuation::Task {
                    id: b.id,
                    func: Some(b.task),
                    next: None,
                })
            })
            .collect();

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(join_task_fn),
            next: None,
        };

        let fork_continuation = WorkflowContinuation::Fork {
            id: fork_id,
            branches: branch_continuations.into_boxed_slice(),
            join: Some(Box::new(join_task)),
        };

        let continuation = self.continuation.append(fork_continuation);

        Ok(WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            _phantom: PhantomData,
        })
    }
}

impl<C, Input, M> Workflow<C, Input, M> {
    /// Get the workflow ID.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        &self.context.workflow_id
    }

    /// Get the definition hash.
    ///
    /// This hash is computed from the workflow's continuation structure and serves
    /// as a version identifier. It can be used to detect when a serialized workflow
    /// state was created with a different workflow definition.
    #[must_use]
    pub fn definition_hash(&self) -> &str {
        &self.definition_hash
    }

    /// Get a reference to the context of this workflow.
    #[must_use]
    pub fn context(&self) -> &WorkflowContext<C, M> {
        &self.context
    }

    /// Get a reference to the codec used by this workflow.
    #[must_use]
    pub fn codec(&self) -> &Arc<C> {
        &self.context.codec
    }

    /// Get a reference to the continuation of this workflow.
    #[must_use]
    pub fn continuation(&self) -> &WorkflowContinuation {
        &self.continuation
    }

    /// Get a reference to the metadata attached to this workflow.
    #[must_use]
    pub fn metadata(&self) -> &Arc<M> {
        &self.context.metadata
    }
}

// ============================================================================
// Serializable Workflow
// ============================================================================

/// A workflow that can be serialized and deserialized.
///
/// This is a wrapper around `Workflow` that carries an internal `TaskRegistry`,
/// automatically populated during building. This enables serialization without
/// manually setting up a separate registry.
///
/// # Example
///
/// ```rust,ignore
/// // Build a serializable workflow (closures must be Clone)
/// let workflow = WorkflowBuilder::new(ctx)
///     .with_registry()  // Enable serialization
///     .then("step1", |i: u32| async move { Ok(i + 1) })
///     .then("step2", |i: u32| async move { Ok(i * 2) })
///     .build("my-workflow")?;
///
/// // Serialize
/// let serialized = workflow.to_serializable();
/// let json = serde_json::to_string(&serialized)?;
///
/// // Deserialize (uses internal registry)
/// let deserialized: SerializedWorkflowState = serde_json::from_str(&json)?;
/// let restored = workflow.to_runnable(&deserialized)?;
/// ```
pub struct SerializableWorkflow<C, Input, M = ()> {
    inner: Workflow<C, Input, M>,
    registry: TaskRegistry,
}

impl<C, Input, M> SerializableWorkflow<C, Input, M> {
    /// Get the workflow ID.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        self.inner.workflow_id()
    }

    /// Get the definition hash.
    #[must_use]
    pub fn definition_hash(&self) -> &str {
        self.inner.definition_hash()
    }

    /// Get a reference to the inner workflow.
    #[must_use]
    pub fn workflow(&self) -> &Workflow<C, Input, M> {
        &self.inner
    }

    /// Get a reference to the context.
    #[must_use]
    pub fn context(&self) -> &WorkflowContext<C, M> {
        self.inner.context()
    }

    /// Get a reference to the codec.
    #[must_use]
    pub fn codec(&self) -> &Arc<C> {
        self.inner.codec()
    }

    /// Get a reference to the continuation.
    #[must_use]
    pub fn continuation(&self) -> &WorkflowContinuation {
        self.inner.continuation()
    }

    /// Get a reference to the metadata.
    #[must_use]
    pub fn metadata(&self) -> &Arc<M> {
        self.inner.metadata()
    }

    /// Get a reference to the internal task registry.
    #[must_use]
    pub fn registry(&self) -> &TaskRegistry {
        &self.registry
    }

    /// Convert to a serializable state representation.
    ///
    /// Returns a `SerializedWorkflowState` that includes the workflow ID,
    /// definition hash, and continuation structure. This can be serialized
    /// and later deserialized to resume the workflow.
    #[must_use]
    pub fn to_serializable(&self) -> SerializedWorkflowState {
        SerializedWorkflowState {
            workflow_id: self.inner.workflow_id().to_string(),
            definition_hash: self.inner.definition_hash.clone(),
            continuation: self.inner.continuation().to_serializable(),
        }
    }

    /// Convert a serialized workflow state to runnable using the internal registry.
    ///
    /// # Errors
    ///
    /// Returns `WorkflowError::DefinitionMismatch` if the definition hash doesn't
    /// match this workflow's hash, indicating the serialized state was created with
    /// a different workflow definition.
    ///
    /// Returns `WorkflowError::TaskNotFound` if any task ID is not in the registry.
    pub fn to_runnable(
        &self,
        state: &SerializedWorkflowState,
    ) -> Result<WorkflowContinuation, WorkflowError> {
        if state.definition_hash != self.inner.definition_hash {
            return Err(WorkflowError::DefinitionMismatch {
                expected: self.inner.definition_hash.clone(),
                found: state.definition_hash.clone(),
            });
        }
        state.continuation.to_runnable(&self.registry)
    }
}

#[cfg(test)]
mod tests {
    use crate::codec::{Decoder, Encoder, sealed};
    use crate::error::BoxError;
    use crate::workflow::WorkflowBuilder;
    use bytes::Bytes;

    struct DummyCodec;

    impl Encoder for DummyCodec {}
    impl Decoder for DummyCodec {}

    impl<Input> sealed::EncodeValue<Input> for DummyCodec {
        fn encode_value(&self, _value: &Input) -> Result<Bytes, BoxError> {
            Ok(Bytes::new())
        }
    }
    impl<Output> sealed::DecodeValue<Output> for DummyCodec {
        fn decode_value(&self, _bytes: Bytes) -> Result<Output, BoxError> {
            Err("Not implemented".into())
        }
    }

    #[test]
    fn test_workflow_build() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Verify the workflow was built successfully
        // The workflow can be executed using a WorkflowRunner from sayiir-runtime
        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_workflow_with_metadata() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(
            "test-workflow",
            Arc::new(DummyCodec),
            Arc::new("test_metadata"),
        );
        let workflow: Workflow<DummyCodec, u32, &str> = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        assert_eq!(**workflow.metadata(), "test_metadata");
    }

    #[test]
    fn test_task_order() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("first", |i: u32| async move { Ok(i + 1) })
            .then("second", |i: u32| async move { Ok(i + 2) })
            .then("third", |i: u32| async move { Ok(i + 3) })
            .build()
            .unwrap();

        // Verify the continuation chain structure
        // Tasks should be linked in order: first -> second -> third
        let mut current = workflow.continuation();
        let mut task_ids = Vec::new();

        loop {
            match current {
                crate::workflow::WorkflowContinuation::Task { id, next, .. } => {
                    task_ids.push(id.clone());
                    match next {
                        Some(next_box) => current = next_box.as_ref(),
                        None => break,
                    }
                }
                _ => break,
            }
        }

        assert_eq!(
            task_ids,
            vec!["first", "second", "third"],
            "Tasks should execute in the order they were added"
        );
    }

    #[test]
    fn test_heterogeneous_fork_join_compiles() {
        use crate::context::WorkflowContext;
        use crate::task::BranchOutputs;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        // This test verifies that the heterogeneous fork-join API compiles correctly.
        // Each branch can return a different type thanks to type erasure.
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                // Returns u32
                b.add("count", |i: u32| async move { Ok(i * 2) });
                // Returns String - heterogeneous output type!
                b.add("name", |i: u32| async move { Ok(format!("item_{}", i)) });
                // Returns f64 - another different type!
                b.add("ratio", |i: u32| async move { Ok(i as f64 / 100.0) });
            })
            .join("combine", |outputs: BranchOutputs<DummyCodec>| async move {
                // In a real workflow with a proper codec, you would:
                // let count: u32 = outputs.get("count")?;
                // let name: String = outputs.get("name")?;
                // let ratio: f64 = outputs.get("ratio")?;
                // For this test, just verify the API compiles
                let _ = outputs.len();
                Ok(format!("combined {} branches", outputs.len()))
            })
            .then("final", |s: String| async move { Ok(s.len() as u32) })
            .build()
            .unwrap();

        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_duplicate_branch_id_returns_error() {
        use crate::context::WorkflowContext;
        use crate::error::WorkflowError;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let result = WorkflowBuilder::<_, u32, _>::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                b.add("count", |i: u32| async move { Ok(i * 2) });
                b.add("count", |i: u32| async move { Ok(i * 3) }); // Duplicate!
            })
            .join("combine", |_outputs| async move { Ok(0u32) })
            .build();

        assert!(matches!(
            result,
            Err(WorkflowError::DuplicateTaskId(id)) if id == "count"
        ));
    }

    #[test]
    fn test_serializable_continuation() {
        use crate::context::WorkflowContext;
        use crate::error::WorkflowError;
        use crate::registry::TaskRegistry;
        use std::sync::Arc;

        // Build a workflow
        let codec = Arc::new(DummyCodec);
        let ctx = WorkflowContext::new("test-workflow", codec.clone(), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Convert to serializable
        let serializable = workflow.continuation().to_serializable();

        // Check structure
        let task_ids = serializable.task_ids();
        assert_eq!(task_ids, vec!["step1", "step2"]);

        // Hydration fails without registry
        let empty_registry = TaskRegistry::new();
        let result = serializable.to_runnable(&empty_registry);
        assert!(matches!(result, Err(WorkflowError::TaskNotFound(id)) if id == "step1"));

        // Hydration succeeds with proper registry
        let mut registry = TaskRegistry::new();
        registry.register_fn("step1", codec.clone(), |i: u32| async move { Ok(i + 1) });
        registry.register_fn("step2", codec.clone(), |i: u32| async move { Ok(i * 2) });

        let hydrated = serializable.to_runnable(&registry);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_serializable_fork_join() {
        use crate::context::WorkflowContext;
        use crate::task::BranchOutputs;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                b.add("branch_a", |i: u32| async move { Ok(i * 2) });
                b.add("branch_b", |i: u32| async move { Ok(i + 10) });
            })
            .join(
                "merge",
                |_: BranchOutputs<DummyCodec>| async move { Ok(0u32) },
            )
            .build()
            .unwrap();

        let serializable = workflow.continuation().to_serializable();
        let task_ids = serializable.task_ids();

        // Should contain: prepare, fork (branch_a||branch_b), branch_a, branch_b, merge
        assert!(task_ids.contains(&"prepare"));
        assert!(task_ids.contains(&"branch_a||branch_b"));
        assert!(task_ids.contains(&"branch_a"));
        assert!(task_ids.contains(&"branch_b"));
        assert!(task_ids.contains(&"merge"));
        assert_eq!(task_ids.len(), 5);
    }

    #[test]
    fn test_serializable_workflow_builder() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);
        let ctx = WorkflowContext::new("test-workflow", codec, Arc::new(()));

        // Build with with_registry() - registry is auto-populated
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Registry was auto-populated
        assert!(workflow.registry().contains("step1"));
        assert!(workflow.registry().contains("step2"));
        assert_eq!(workflow.registry().len(), 2);

        // Can serialize
        let serializable = workflow.to_serializable();
        assert_eq!(serializable.continuation.task_ids(), vec!["step1", "step2"]);

        // Can hydrate using internal registry
        let hydrated = workflow.to_runnable(&serializable);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_with_existing_registry_and_then_registered() {
        use crate::context::WorkflowContext;
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);

        // Pre-register tasks in a registry
        let mut registry = TaskRegistry::new();
        registry.register_fn("double", codec.clone(), |i: u32| async move { Ok(i * 2) });
        registry.register_fn("add_ten", codec.clone(), |i: u32| async move { Ok(i + 10) });

        // Build workflow using existing registry and referencing pre-registered tasks
        let ctx = WorkflowContext::new("test-workflow", codec.clone(), Arc::new(()));
        let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
            .with_existing_registry(registry)
            .then_registered::<u32>("double")
            .unwrap()
            .then_registered::<u32>("add_ten")
            .unwrap()
            .build()
            .unwrap();

        // Registry should contain the pre-registered tasks
        assert!(workflow.registry().contains("double"));
        assert!(workflow.registry().contains("add_ten"));

        // Workflow structure should reference those tasks
        let serializable = workflow.to_serializable();
        assert_eq!(
            serializable.continuation.task_ids(),
            vec!["double", "add_ten"]
        );

        // Can hydrate using the same registry
        let hydrated = workflow.to_runnable(&serializable);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_mixed_inline_and_registered_tasks() {
        use crate::context::WorkflowContext;
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);

        // Pre-register one task
        let mut registry = TaskRegistry::new();
        registry.register_fn(
            "preregistered",
            codec.clone(),
            |i: u32| async move { Ok(i * 2) },
        );

        // Build workflow mixing pre-registered and inline tasks
        let ctx = WorkflowContext::new("test-workflow", codec.clone(), Arc::new(()));
        let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
            .with_existing_registry(registry)
            .then_registered::<u32>("preregistered") // Use pre-registered
            .unwrap()
            .then("inline", |i: u32| async move { Ok(i + 5) }) // Define inline
            .build()
            .unwrap();

        // Registry should have both tasks
        assert!(workflow.registry().contains("preregistered"));
        assert!(workflow.registry().contains("inline"));
        assert_eq!(workflow.registry().len(), 2);
    }

    #[test]
    fn test_workflow_id_and_definition_hash() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("my-workflow-id", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Check workflow_id is set correctly
        assert_eq!(workflow.workflow_id(), "my-workflow-id");

        // Definition hash should be non-empty
        assert!(!workflow.definition_hash().is_empty());

        // Serializable state should contain the same id and hash
        let state = workflow.to_serializable();
        assert_eq!(state.workflow_id, "my-workflow-id");
        assert_eq!(state.definition_hash, workflow.definition_hash());
    }

    #[test]
    fn test_definition_hash_changes_with_structure() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        // Build two workflows with different structures
        let ctx1 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow1 = WorkflowBuilder::new(ctx1)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        let ctx2 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow2 = WorkflowBuilder::new(ctx2)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        assert_ne!(workflow1.definition_hash(), workflow2.definition_hash());
    }

    #[test]
    fn test_definition_mismatch_error() {
        use crate::context::WorkflowContext;
        use crate::error::WorkflowError;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Create a state with wrong hash
        let mut state = workflow.to_serializable();
        state.definition_hash = "wrong-hash".to_string();

        // to_runnable should fail with DefinitionMismatch
        let result = workflow.to_runnable(&state);
        assert!(matches!(
            result,
            Err(WorkflowError::DefinitionMismatch { .. })
        ));
    }

    #[test]
    fn test_duplicate_id_tampering_detection() {
        use crate::error::WorkflowError;
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableContinuation;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);

        // Create a registry with tasks
        let mut registry = TaskRegistry::new();
        registry.register_fn("step1", codec.clone(), |i: u32| async move { Ok(i + 1) });
        registry.register_fn("step2", codec.clone(), |i: u32| async move { Ok(i * 2) });

        // Manually construct a tampered continuation with duplicate IDs
        let tampered = SerializableContinuation::Task {
            id: "step1".to_string(),
            next: Some(Box::new(SerializableContinuation::Task {
                id: "step1".to_string(), // Duplicate!
                next: None,
            })),
        };

        // to_runnable should detect the tampering
        let result = tampered.to_runnable(&registry);
        assert!(matches!(
            result,
            Err(WorkflowError::DuplicateTaskId(id)) if id == "step1"
        ));
    }

    // ========================================================================
    // Delay tests
    // ========================================================================

    #[test]
    fn test_delay_builder() {
        use crate::context::WorkflowContext;
        use crate::workflow::{Workflow, WorkflowContinuation};
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_1s", Duration::from_secs(1))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Verify the chain structure: Task -> Delay -> Task
        let mut ids = Vec::new();
        let mut current = workflow.continuation();
        loop {
            match current {
                WorkflowContinuation::Task { id, next, .. } => {
                    ids.push(format!("task:{id}"));
                    match next {
                        Some(n) => current = n,
                        None => break,
                    }
                }
                WorkflowContinuation::Delay {
                    id, duration, next, ..
                } => {
                    ids.push(format!("delay:{id}:{}ms", duration.as_millis()));
                    match next {
                        Some(n) => current = n,
                        None => break,
                    }
                }
                _ => break,
            }
        }

        assert_eq!(
            ids,
            vec!["task:step1", "delay:wait_1s:1000ms", "task:step2"]
        );
    }

    #[test]
    fn test_delay_serialization_roundtrip() {
        use crate::context::WorkflowContext;
        use crate::workflow::SerializableContinuation;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_5s", Duration::from_secs(5))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Convert to serializable
        let serializable = workflow.to_serializable();

        // Check structure
        let task_ids = serializable.continuation.task_ids();
        assert_eq!(task_ids, vec!["step1", "wait_5s", "step2"]);

        // Check delay duration is preserved
        match &serializable.continuation {
            SerializableContinuation::Task { next, .. } => {
                let next = next.as_ref().unwrap();
                match next.as_ref() {
                    SerializableContinuation::Delay {
                        id, duration_ms, ..
                    } => {
                        assert_eq!(id, "wait_5s");
                        assert_eq!(*duration_ms, 5000);
                    }
                    other => panic!("Expected Delay, got {other:?}"),
                }
            }
            other => panic!("Expected Task, got {other:?}"),
        }

        // Hydrate back to runnable
        let hydrated = workflow.to_runnable(&serializable);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_delay_first_task_id() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .delay("initial_delay", Duration::from_secs(10))
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        assert_eq!(workflow.continuation().first_task_id(), "initial_delay");
    }

    #[test]
    fn test_delay_duplicate_id_detection() {
        use crate::context::WorkflowContext;
        use crate::error::WorkflowError;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let result = WorkflowBuilder::<_, u32, _>::new(ctx)
            .then("dup", |i: u32| async move { Ok(i + 1) })
            .delay("dup", Duration::from_secs(1))
            .build();

        assert!(matches!(
            result,
            Err(WorkflowError::DuplicateTaskId(id)) if id == "dup"
        ));
    }

    #[test]
    fn test_delay_definition_hash_includes_duration() {
        use crate::context::WorkflowContext;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;
        use std::time::Duration;

        // Workflow with 1-second delay
        let ctx1 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf1: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx1)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait", Duration::from_secs(1))
            .build()
            .unwrap();

        // Workflow with 60-second delay (same ID, different duration)
        let ctx2 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf2: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx2)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait", Duration::from_secs(60))
            .build()
            .unwrap();

        // Hashes should differ because duration differs
        assert_ne!(wf1.definition_hash(), wf2.definition_hash());
    }

    #[test]
    fn test_delay_definition_hash_differs_from_task() {
        use crate::context::WorkflowContext;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;
        use std::time::Duration;

        // Workflow with task
        let ctx1 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf1: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx1)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Workflow with delay instead
        let ctx2 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf2: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx2)
            .with_registry()
            .delay("step1", Duration::from_secs(1))
            .build()
            .unwrap();

        // Hashes should differ (Task vs Delay are tagged differently)
        assert_ne!(wf1.definition_hash(), wf2.definition_hash());
    }

    #[test]
    fn test_delay_task_ids() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("fetch", |i: u32| async move { Ok(i) })
            .delay("wait_24h", Duration::from_secs(86400))
            .then("process", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        let serializable = workflow.continuation().to_serializable();
        let ids = serializable.task_ids();
        assert_eq!(ids, vec!["fetch", "wait_24h", "process"]);
    }

    #[test]
    fn test_delay_only_workflow() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;
        use std::time::Duration;

        use crate::workflow::Workflow;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .delay("just_wait", Duration::from_millis(10))
            .build()
            .unwrap();

        assert_eq!(workflow.continuation().first_task_id(), "just_wait");

        let serializable = workflow.continuation().to_serializable();
        assert_eq!(serializable.task_ids(), vec!["just_wait"]);
    }

    #[test]
    fn test_delay_to_runnable_no_registry_needed() {
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableContinuation;

        // A delay doesn't need a registry entry (it has no func)
        let delay = SerializableContinuation::Delay {
            id: "wait".to_string(),
            duration_ms: 5000,
            next: None,
        };

        let empty_registry = TaskRegistry::new();
        let result = delay.to_runnable(&empty_registry);
        assert!(result.is_ok());

        let runnable = result.unwrap();
        match runnable {
            crate::workflow::WorkflowContinuation::Delay {
                id, duration, next, ..
            } => {
                assert_eq!(id, "wait");
                assert_eq!(duration, std::time::Duration::from_millis(5000));
                assert!(next.is_none());
            }
            _ => panic!("Expected Delay variant"),
        }
    }
}
