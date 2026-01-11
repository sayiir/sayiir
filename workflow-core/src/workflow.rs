use crate::codec::Codec;
use crate::codec::sealed;
use crate::context::WorkflowContext;
use crate::task::{
    BranchOutputs, ErasedBranch, UntypedCoreTask, branch, to_core_task_arc,
    to_heterogeneous_join_task_arc,
};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;

/// Error returned when building a workflow fails.
#[derive(Debug)]
pub enum WorkflowBuildError {
    /// A duplicate task ID was found.
    DuplicateId(String),
}

impl std::fmt::Display for WorkflowBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowBuildError::DuplicateId(id) => write!(f, "Duplicate task id: '{}'", id),
        }
    }
}

impl std::error::Error for WorkflowBuildError {}

/// A workflow structure representing the tasks to execute.
pub enum WorkflowContinuation {
    Task {
        id: String,
        func: UntypedCoreTask,
        next: Option<Box<WorkflowContinuation>>,
    },
    Fork {
        branches: Box<[Arc<WorkflowContinuation>]>,
        join: Option<Box<WorkflowContinuation>>,
    },
}

impl WorkflowContinuation {
    /// Find the first duplicate ID in this continuation tree, if any.
    fn find_duplicate_id(&self) -> Option<String> {
        fn collect(cont: &WorkflowContinuation, seen: &mut HashSet<String>) -> Option<String> {
            match cont {
                WorkflowContinuation::Task { id, next, .. } => {
                    if !seen.insert(id.clone()) {
                        return Some(id.clone());
                    }
                    next.as_ref().and_then(|n| collect(n, seen))
                }
                WorkflowContinuation::Fork { branches, join } => branches
                    .iter()
                    .find_map(|b| collect(b, seen))
                    .or_else(|| join.as_ref().and_then(|j| collect(j, seen))),
            }
        }
        collect(self, &mut HashSet::new())
    }

    /// Convert to a serializable representation (strips out task implementations).
    pub fn to_serializable(&self) -> SerializableContinuation {
        match self {
            WorkflowContinuation::Task { id, next, .. } => SerializableContinuation::Task {
                id: id.clone(),
                next: next.as_ref().map(|n| Box::new(n.to_serializable())),
            },
            WorkflowContinuation::Fork { branches, join } => SerializableContinuation::Fork {
                branches: branches.iter().map(|b| b.to_serializable()).collect(),
                join: join.as_ref().map(|j| Box::new(j.to_serializable())),
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
        branches: Vec<SerializableContinuation>,
        join: Option<Box<SerializableContinuation>>,
    },
}

/// Error when hydrating a serializable continuation.
#[derive(Debug)]
pub enum ToRunnableError {
    /// A task ID was not found in the registry.
    TaskNotFound(String),
}

impl std::fmt::Display for ToRunnableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToRunnableError::TaskNotFound(id) => write!(f, "Task '{}' not found in registry", id),
        }
    }
}

impl std::error::Error for ToRunnableError {}

impl SerializableContinuation {
    /// Convert this serializable continuation into a runnable WorkflowContinuation.
    ///
    /// Looks up each task ID in the registry to get the actual implementation.
    ///
    /// # Errors
    ///
    /// Returns `ToRunnableError::TaskNotFound` if any task ID is not in the registry.
    pub fn to_runnable(
        &self,
        registry: &crate::registry::TaskRegistry,
    ) -> Result<WorkflowContinuation, ToRunnableError> {
        match self {
            SerializableContinuation::Task { id, next } => {
                let func = registry
                    .get(id)
                    .ok_or_else(|| ToRunnableError::TaskNotFound(id.clone()))?;
                let next = next
                    .as_ref()
                    .map(|n| n.to_runnable(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Task {
                    id: id.clone(),
                    func,
                    next,
                })
            }
            SerializableContinuation::Fork { branches, join } => {
                let branches: Result<Vec<_>, _> = branches
                    .iter()
                    .map(|b| b.to_runnable(registry).map(Arc::new))
                    .collect();
                let join = join
                    .as_ref()
                    .map(|j| j.to_runnable(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Fork {
                    branches: branches?.into_boxed_slice(),
                    join,
                })
            }
        }
    }

    /// Get all task IDs referenced in this continuation.
    pub fn task_ids(&self) -> Vec<&str> {
        fn collect<'a>(cont: &'a SerializableContinuation, ids: &mut Vec<&'a str>) {
            match cont {
                SerializableContinuation::Task { id, next } => {
                    ids.push(id.as_str());
                    if let Some(n) = next {
                        collect(n, ids);
                    }
                }
                SerializableContinuation::Fork { branches, join } => {
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
}

/// The status of a workflow execution.
#[derive(Debug)]
pub enum WorkflowStatus {
    /// The workflow completed successfully.
    Completed,
    /// The workflow failed with an error.
    Failed(anyhow::Error),
}

/// Marker type for empty continuation (no tasks yet).
pub struct NoContinuation;

/// Marker type for no registry (non-serializable workflow).
pub struct NoRegistry;

/// Trait for continuation state - allows unified handling of empty vs existing continuation.
pub trait ContinuationState {
    /// Append a new task to this continuation state, returning a WorkflowContinuation.
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
    /// Register a task (no-op for NoRegistry, actual registration for TaskRegistry).
    fn maybe_register<I, O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<O>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static;

    /// Register a join task (no-op for NoRegistry, actual registration for TaskRegistry).
    fn maybe_register_join<O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<O>> + Send + 'static,
        C: Codec + sealed::EncodeValue<O> + Send + Sync + 'static;
}

impl RegistryBehavior for NoRegistry {
    fn maybe_register<I, O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<O>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        // No-op for non-serializable workflows
    }

    fn maybe_register_join<O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<O>> + Send + 'static,
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
        Fut: std::future::Future<Output = anyhow::Result<O>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        self.register_fn_arc(id, codec, Arc::clone(func));
    }

    fn maybe_register_join<O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: &Arc<F>)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<O>> + Send + 'static,
        C: Codec + sealed::EncodeValue<O> + Send + Sync + 'static,
    {
        self.register_arc_join(id, codec, Arc::clone(func));
    }
}

pub struct WorkflowBuilder<C, Input, Output, M = (), Cont = NoContinuation, R = NoRegistry> {
    context: WorkflowContext<C, M>,
    continuation: Cont,
    registry: R,
    _phantom: PhantomData<(Input, Output)>,
}

/// A built workflow that can be executed.
pub struct Workflow<C, Input, M = ()> {
    context: WorkflowContext<C, M>,
    continuation: WorkflowContinuation,
    _phantom: PhantomData<Input>,
}

impl<C, Input, M> WorkflowBuilder<C, Input, Input, M, NoContinuation, NoRegistry> {
    /// Create a new workflow builder with a context object.
    ///
    /// The context contains both the codec and metadata that will be available
    /// at any execution point via the `sayiir_ctx!` macro.
    pub fn new(ctx: WorkflowContext<C, M>) -> Self
    where
        C: Codec,
        M: Send + Sync + 'static,
    {
        Self {
            context: ctx,
            continuation: NoContinuation,
            registry: NoRegistry,
            _phantom: PhantomData,
        }
    }

    /// Enable registry tracking for serializable workflows with a new empty registry.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .with_registry()  // Enable serialization
    ///     .then("step1", |i: u32| async move { Ok(i + 1) })
    ///     .build()?;  // Returns SerializableWorkflow
    /// ```
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
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .with_existing_registry(registry)
    ///     .then_registered::<u32>("step1")
    ///     .build()?;
    ///
    /// // Deserialize (on another side): rebuild registry, then convert to runnable
    /// let registry = build_registry(codec.clone());
    /// let runnable = serialized_continuation.to_runnable(&registry)?;
    /// ```
    pub fn with_existing_registry(
        self,
        registry: TaskRegistry,
    ) -> WorkflowBuilder<C, Input, Input, M, NoContinuation, TaskRegistry> {
        WorkflowBuilder {
            context: self.context,
            continuation: NoContinuation,
            registry,
            _phantom: PhantomData,
        }
    }
}

/// Methods for adding tasks - unified implementation using RegistryBehavior and ContinuationState.
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
        Fut: std::future::Future<Output = anyhow::Result<NewOutput>> + Send + 'static,
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
            func: task,
            next: None,
        };

        let continuation = self.continuation.append(new_task);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            _phantom: PhantomData,
        }
    }
}

/// Methods for referencing pre-registered tasks (only available with TaskRegistry).
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
    /// # Panics
    ///
    /// Panics if the task ID is not found in the registry.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut registry = TaskRegistry::new();
    /// registry.register_fn("double", codec.clone(), |i: u32| async move { Ok(i * 2) });
    ///
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .with_existing_registry(registry)
    ///     .then_registered::<u32>("double")
    ///     .build()?;
    /// ```
    pub fn then_registered<NewOutput>(
        self,
        id: &str,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, TaskRegistry>
    where
        Output: Send + 'static,
        NewOutput: Send + 'static,
    {
        let func = self
            .registry
            .get(id)
            .unwrap_or_else(|| panic!("Task '{}' not found in registry", id));

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func,
            next: None,
        };

        let continuation = self.continuation.append(new_task);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            _phantom: PhantomData,
        }
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
    /// workflow
    ///     .then("prepare", |input| async { Ok(input) })
    ///     .branches(|b| {
    ///         b.add("count", |i: u32| async move { Ok(i * 2) });
    ///         b.add("name", |i: u32| async move { Ok(format!("item_{}", i)) });
    ///     })
    ///     .join("combine", |outputs: BranchOutputs<_>| async move {
    ///         let count: u32 = outputs.get("count")?;
    ///         let name: String = outputs.get("name")?;
    ///         Ok(format!("{}: {}", name, count))
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

/// Build method for WorkflowBuilder without registry - returns Workflow.
impl<C, Input, Output, M> WorkflowBuilder<C, Input, Output, M, WorkflowContinuation, NoRegistry> {
    /// Build the workflow into an executable workflow.
    ///
    /// # Errors
    ///
    /// Returns an error if duplicate task IDs are found.
    pub fn build(self) -> Result<Workflow<C, Input, M>, WorkflowBuildError>
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
            return Err(WorkflowBuildError::DuplicateId(dup));
        }

        Ok(Workflow {
            continuation: self.continuation,
            context: self.context,
            _phantom: PhantomData,
        })
    }
}

/// Build method for WorkflowBuilder with registry - returns SerializableWorkflow.
impl<C, Input, Output, M> WorkflowBuilder<C, Input, Output, M, WorkflowContinuation, TaskRegistry> {
    /// Build the workflow into a serializable workflow.
    ///
    /// # Errors
    ///
    /// Returns an error if duplicate task IDs are found.
    pub fn build(self) -> Result<SerializableWorkflow<C, Input, M>, WorkflowBuildError>
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
            return Err(WorkflowBuildError::DuplicateId(dup));
        }

        let inner = Workflow {
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
        WorkflowContinuation::Task { next, .. } => match next {
            Some(next_box) => append_to_chain(next_box, new_task),
            None => *next = Some(Box::new(new_task)),
        },
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
        Fut: std::future::Future<Output = anyhow::Result<BranchOutput>> + Send + 'static,
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

/// ForkBuilder methods - unified implementation using RegistryBehavior and ContinuationState.
impl<C, Input, Output, M, Cont, R> ForkBuilder<C, Input, Output, M, Cont, R>
where
    R: RegistryBehavior,
    Cont: ContinuationState,
{
    /// Add a branch to the fork.
    pub fn branch<F, Fut, BranchOutput>(mut self, id: &str, func: F) -> Self
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        BranchOutput: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<BranchOutput>> + Send + 'static,
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
        Fut: std::future::Future<Output = anyhow::Result<JoinOutput>> + Send + 'static,
        C: Codec + sealed::EncodeValue<JoinOutput> + Send + Sync + 'static,
    {
        let codec = Arc::clone(&self.context.codec);
        let func = Arc::new(func);

        // Register if registry is enabled (no-op for NoRegistry)
        self.registry
            .maybe_register_join::<JoinOutput, _, _, _>(id, codec.clone(), &func);

        let join_task_fn = to_heterogeneous_join_task_arc(func, codec);

        let branch_continuations: Vec<Arc<WorkflowContinuation>> = self
            .branches
            .into_iter()
            .map(|b| {
                Arc::new(WorkflowContinuation::Task {
                    id: b.id,
                    func: b.task,
                    next: None,
                })
            })
            .collect();

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: join_task_fn,
            next: None,
        };

        let fork_continuation = WorkflowContinuation::Fork {
            branches: branch_continuations.into_boxed_slice(),
            join: Some(Box::new(join_task)),
        };

        let continuation = self.continuation.append(fork_continuation);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            _phantom: PhantomData,
        }
    }
}

/// ForkBuilder methods for referencing pre-registered tasks (only available with TaskRegistry).
impl<C, Input, Output, M, Cont> ForkBuilder<C, Input, Output, M, Cont, TaskRegistry>
where
    Cont: ContinuationState,
{
    /// Add a pre-registered branch task by ID.
    ///
    /// # Panics
    ///
    /// Panics if the task ID is not found in the registry.
    pub fn branch_registered(mut self, id: &str) -> Self
    where
        Output: Send + 'static,
    {
        let task = self
            .registry
            .get(id)
            .unwrap_or_else(|| panic!("Task '{}' not found in registry", id));

        self.branches.push(ErasedBranch {
            id: id.to_string(),
            task,
        });
        self
    }

    /// Join using a pre-registered join task by ID.
    ///
    /// # Panics
    ///
    /// Panics if the task ID is not found in the registry.
    pub fn join_registered<JoinOutput>(
        self,
        id: &str,
    ) -> WorkflowBuilder<C, Input, JoinOutput, M, WorkflowContinuation, TaskRegistry>
    where
        Output: Send + 'static,
        JoinOutput: Send + 'static,
    {
        let join_task_fn = self
            .registry
            .get(id)
            .unwrap_or_else(|| panic!("Task '{}' not found in registry", id));

        let branch_continuations: Vec<Arc<WorkflowContinuation>> = self
            .branches
            .into_iter()
            .map(|b| {
                Arc::new(WorkflowContinuation::Task {
                    id: b.id,
                    func: b.task,
                    next: None,
                })
            })
            .collect();

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: join_task_fn,
            next: None,
        };

        let fork_continuation = WorkflowContinuation::Fork {
            branches: branch_continuations.into_boxed_slice(),
            join: Some(Box::new(join_task)),
        };

        let continuation = self.continuation.append(fork_continuation);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            _phantom: PhantomData,
        }
    }
}

impl<C, Input, M> Workflow<C, Input, M> {
    /// Get a reference to the context of this workflow.
    pub fn context(&self) -> &WorkflowContext<C, M> {
        &self.context
    }

    /// Get a reference to the codec used by this workflow.
    pub fn codec(&self) -> &Arc<C> {
        &self.context.codec
    }

    /// Get a reference to the continuation of this workflow.
    pub fn continuation(&self) -> &WorkflowContinuation {
        &self.continuation
    }

    /// Get a reference to the metadata attached to this workflow.
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
///     .build()?;
///
/// // Serialize
/// let serialized = workflow.to_serializable();
/// let json = serde_json::to_string(&serialized)?;
///
/// // Deserialize (uses internal registry)
/// let deserialized: SerializableContinuation = serde_json::from_str(&json)?;
/// let restored = workflow.to_runnable(&deserialized)?;
/// ```
pub struct SerializableWorkflow<C, Input, M = ()> {
    inner: Workflow<C, Input, M>,
    registry: TaskRegistry,
}

impl<C, Input, M> SerializableWorkflow<C, Input, M> {
    /// Get a reference to the inner workflow.
    pub fn workflow(&self) -> &Workflow<C, Input, M> {
        &self.inner
    }

    /// Get a reference to the context.
    pub fn context(&self) -> &WorkflowContext<C, M> {
        self.inner.context()
    }

    /// Get a reference to the codec.
    pub fn codec(&self) -> &Arc<C> {
        self.inner.codec()
    }

    /// Get a reference to the continuation.
    pub fn continuation(&self) -> &WorkflowContinuation {
        self.inner.continuation()
    }

    /// Get a reference to the metadata.
    pub fn metadata(&self) -> &Arc<M> {
        self.inner.metadata()
    }

    /// Get a reference to the internal task registry.
    pub fn registry(&self) -> &TaskRegistry {
        &self.registry
    }

    /// Convert to a serializable representation.
    pub fn to_serializable(&self) -> SerializableContinuation {
        self.inner.continuation().to_serializable()
    }

    /// Convert a serializable continuation to runnable using the internal registry.
    pub fn to_runnable(
        &self,
        serializable: &SerializableContinuation,
    ) -> Result<WorkflowContinuation, ToRunnableError> {
        serializable.to_runnable(&self.registry)
    }
}

#[cfg(test)]
mod tests {
    use crate::codec::{Decoder, Encoder, sealed};
    use crate::workflow::WorkflowBuilder;
    use anyhow::Result;
    use bytes::Bytes;

    struct DummyCodec;

    impl Encoder for DummyCodec {}
    impl Decoder for DummyCodec {}

    impl<Input> sealed::EncodeValue<Input> for DummyCodec {
        fn encode_value(&self, _value: &Input) -> Result<Bytes> {
            Ok(Bytes::new())
        }
    }
    impl<Output> sealed::DecodeValue<Output> for DummyCodec {
        fn decode_value(&self, _bytes: Bytes) -> Result<Output> {
            Err(anyhow::anyhow!("Not implemented"))
        }
    }

    #[test]
    fn test_workflow_build() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Verify the workflow was built successfully
        // The workflow can be executed using a WorkflowRunner from workflow-runtime
        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_workflow_with_metadata() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new("test_metadata"));
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

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
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

        // Tasks should execute in the order they were added
        assert_eq!(task_ids, vec!["first", "second", "third"]);
    }

    #[test]
    fn test_heterogeneous_fork_join_compiles() {
        use crate::context::WorkflowContext;
        use crate::task::BranchOutputs;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
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
        use crate::workflow::WorkflowBuildError;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
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
            Err(WorkflowBuildError::DuplicateId(id)) if id == "count"
        ));
    }

    #[test]
    fn test_serializable_continuation() {
        use crate::context::WorkflowContext;
        use crate::registry::TaskRegistry;
        use crate::workflow::ToRunnableError;
        use std::sync::Arc;

        // Build a workflow
        let codec = Arc::new(DummyCodec);
        let ctx = WorkflowContext::new(codec.clone(), Arc::new(()));
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
        assert!(matches!(result, Err(ToRunnableError::TaskNotFound(id)) if id == "step1"));

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

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
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

        // Should contain: prepare, branch_a, branch_b, merge
        assert!(task_ids.contains(&"prepare"));
        assert!(task_ids.contains(&"branch_a"));
        assert!(task_ids.contains(&"branch_b"));
        assert!(task_ids.contains(&"merge"));
        assert_eq!(task_ids.len(), 4);
    }

    #[test]
    fn test_serializable_workflow_builder() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);
        let ctx = WorkflowContext::new(codec, Arc::new(()));

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
        assert_eq!(serializable.task_ids(), vec!["step1", "step2"]);

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
        let ctx = WorkflowContext::new(codec.clone(), Arc::new(()));
        let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
            .with_existing_registry(registry)
            .then_registered::<u32>("double")
            .then_registered::<u32>("add_ten")
            .build()
            .unwrap();

        // Registry should contain the pre-registered tasks
        assert!(workflow.registry().contains("double"));
        assert!(workflow.registry().contains("add_ten"));

        // Workflow structure should reference those tasks
        let serializable = workflow.to_serializable();
        assert_eq!(serializable.task_ids(), vec!["double", "add_ten"]);

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
        let ctx = WorkflowContext::new(codec.clone(), Arc::new(()));
        let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
            .with_existing_registry(registry)
            .then_registered::<u32>("preregistered") // Use pre-registered
            .then("inline", |i: u32| async move { Ok(i + 5) }) // Define inline
            .build()
            .unwrap();

        // Registry should have both tasks
        assert!(workflow.registry().contains("preregistered"));
        assert!(workflow.registry().contains("inline"));
        assert_eq!(workflow.registry().len(), 2);
    }
}
