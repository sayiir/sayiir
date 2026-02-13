use crate::codec::Codec;
use crate::codec::sealed;
use crate::context::WorkflowContext;
use crate::error::WorkflowError;
use crate::registry::TaskRegistry;
use crate::task::{
    BranchOutputs, ErasedBranch, branch, to_core_task_arc, to_heterogeneous_join_task_arc,
};
use crate::workflow::{SerializableWorkflow, Workflow, WorkflowContinuation};
use std::marker::PhantomData;
use std::sync::Arc;

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
        C: Codec
            + sealed::EncodeValue<O>
            + sealed::DecodeValue<crate::branch_results::NamedBranchResults>
            + Send
            + Sync
            + 'static;
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
        C: Codec
            + sealed::EncodeValue<O>
            + sealed::DecodeValue<crate::branch_results::NamedBranchResults>
            + Send
            + Sync
            + 'static,
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
        C: Codec
            + sealed::EncodeValue<O>
            + sealed::DecodeValue<crate::branch_results::NamedBranchResults>
            + Send
            + Sync
            + 'static,
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
            timeout: None,
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
        let timeout = self.registry.get_metadata(id).and_then(|m| m.timeout);

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(func),
            timeout,
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
}

/// Metadata attachment — only available after a task has been added.
impl<C, Input, Output, M> WorkflowBuilder<C, Input, Output, M, WorkflowContinuation, TaskRegistry> {
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
            let timeout = metadata.timeout;
            self.registry.set_metadata(id, metadata);
            // Also update the timeout on the continuation node so it's available
            // for direct execution (not just the serializable roundtrip path).
            self.continuation.set_task_timeout(id, timeout);
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
        C: Codec
            + sealed::EncodeValue<JoinOutput>
            + sealed::DecodeValue<crate::branch_results::NamedBranchResults>
            + Send
            + Sync
            + 'static,
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
                    timeout: None,
                    next: None,
                })
            })
            .collect();

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(join_task_fn),
            timeout: None,
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
        let join_timeout = self.registry.get_metadata(id).and_then(|m| m.timeout);

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
                let timeout = self.registry.get_metadata(&b.id).and_then(|m| m.timeout);
                Arc::new(WorkflowContinuation::Task {
                    id: b.id,
                    func: Some(b.task),
                    timeout,
                    next: None,
                })
            })
            .collect();

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(join_task_fn),
            timeout: join_timeout,
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
