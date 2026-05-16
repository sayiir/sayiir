//! Fluent workflow builder API.
//!
//! [`WorkflowBuilder`] is the primary entry point for constructing workflows
//! programmatically. It supports sequential tasks (`.then`), parallel
//! fork/join (`.branches` + `.join`), durable delays, signal waits, and
//! conditional routing (`.route`).
//!
//! For the proc-macro DSL alternative, see the `workflow!` macro in `sayiir-macros`.

use crate::branch_key::BranchKey;
use crate::codec::Codec;
use crate::codec::sealed;
use crate::context::WorkflowContext;
use crate::error::{BuildError, BuildErrors};
use crate::loop_result::LoopResult;
use crate::priority::Priority;
use crate::registry::TaskRegistry;
use crate::task::{
    BranchEnvelope, BranchOutputs, ErasedBranch, RegisterableTask, branch, to_core_loop_task_arc,
    to_core_task_arc, to_heterogeneous_join_task_arc, wrap_core_loop_task, wrap_core_task,
};
use crate::workflow::{MaxIterationsPolicy, SerializableWorkflow, Workflow, WorkflowContinuation};
use std::collections::HashMap;
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
        self.append_to_chain(new_task);
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

    /// Register a `CoreTask` struct (no-op for `NoRegistry`, actual registration for `TaskRegistry`).
    fn maybe_register_core_task<T, C>(
        &mut self,
        _id: &str,
        _codec: Arc<C>,
        _task: Arc<T>,
        _metadata: crate::task::TaskMetadata,
    ) where
        T: crate::task::CoreTask + 'static,
        T::Input: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<T::Output> + 'static;

    /// Register a `CoreTask` struct whose output is `LoopResult<O>` (no-op for `NoRegistry`).
    fn maybe_register_core_loop_task<T, O, C>(
        &mut self,
        _id: &str,
        _codec: Arc<C>,
        _task: Arc<T>,
        _metadata: crate::task::TaskMetadata,
    ) where
        T: crate::task::CoreTask<Output = LoopResult<O>> + 'static,
        T::Input: Send + 'static,
        O: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<O> + 'static;

    /// Register a loop body task with two-step encoding (no-op for `NoRegistry`).
    fn maybe_register_loop<I, O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<LoopResult<O>, crate::error::BoxError>>
            + Send
            + 'static,
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

    fn maybe_register_core_task<T, C>(
        &mut self,
        _id: &str,
        _codec: Arc<C>,
        _task: Arc<T>,
        _metadata: crate::task::TaskMetadata,
    ) where
        T: crate::task::CoreTask + 'static,
        T::Input: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<T::Output> + 'static,
    {
        // No-op for non-serializable workflows
    }

    fn maybe_register_core_loop_task<T, O, C>(
        &mut self,
        _id: &str,
        _codec: Arc<C>,
        _task: Arc<T>,
        _metadata: crate::task::TaskMetadata,
    ) where
        T: crate::task::CoreTask<Output = LoopResult<O>> + 'static,
        T::Input: Send + 'static,
        O: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<O> + 'static,
    {
        // No-op for non-serializable workflows
    }

    fn maybe_register_loop<I, O, F, Fut, C>(&mut self, _id: &str, _codec: Arc<C>, _func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<LoopResult<O>, crate::error::BoxError>>
            + Send
            + 'static,
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

    fn maybe_register_core_task<T, C>(
        &mut self,
        id: &str,
        codec: Arc<C>,
        task: Arc<T>,
        metadata: crate::task::TaskMetadata,
    ) where
        T: crate::task::CoreTask + 'static,
        T::Input: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<T::Output> + 'static,
    {
        self.register_task_arc(id, codec, task, metadata);
    }

    fn maybe_register_core_loop_task<T, O, C>(
        &mut self,
        id: &str,
        codec: Arc<C>,
        task: Arc<T>,
        metadata: crate::task::TaskMetadata,
    ) where
        T: crate::task::CoreTask<Output = LoopResult<O>> + 'static,
        T::Input: Send + 'static,
        O: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<O> + 'static,
    {
        self.register_loop_task_arc(id, codec, task, metadata);
    }

    fn maybe_register_loop<I, O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: &Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: std::future::Future<Output = Result<LoopResult<O>, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        use crate::task::TaskMetadata;
        self.register_loop_fn_arc(id, codec, Arc::clone(func), TaskMetadata::default());
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

/// Fluent builder for assembling workflow pipelines.
///
/// Start with [`WorkflowBuilder::new`], chain steps with `.then()`,
/// `.delay()`, `.wait_for_signal()`, `.branches()`, `.fork()`, or
/// `.route()`, and finish with `.build()`.
///
/// Call `.with_registry()` before adding steps to get a
/// [`SerializableWorkflow`] (required for distributed execution).
pub struct WorkflowBuilder<C, Input, Output, M = (), Cont = NoContinuation, R = NoRegistry> {
    context: WorkflowContext<C, M>,
    continuation: Cont,
    registry: R,
    last_task_id: Option<String>,
    branch_counter: usize,
    loop_counter: usize,
    child_counter: usize,
    errors: BuildErrors,
    _phantom: PhantomData<(Input, Output)>,
}

#[allow(clippy::mismatching_type_param_order)] // Input used for both Input and Output initially
impl<C, Input, M> WorkflowBuilder<C, Input, Input, M, NoContinuation, NoRegistry> {
    /// Create a new workflow builder with a context object.
    ///
    /// The context contains the workflow ID, codec and metadata that will be available
    /// at any execution point via the workflow context.
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
            branch_counter: 0,
            loop_counter: 0,
            child_counter: 0,
            errors: BuildErrors::new(),
            _phantom: PhantomData,
        }
    }

    /// Enable registry tracking for serializable workflows with a new empty registry.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use sayiir_core::prelude::*;
    /// # use sayiir_core::codec::{Encoder, Decoder, sealed};
    /// # use bytes::Bytes;
    /// # use std::sync::Arc;
    /// # struct MyCodec;
    /// # impl Encoder for MyCodec {}
    /// # impl Decoder for MyCodec {}
    /// # impl<T> sealed::EncodeValue<T> for MyCodec {
    /// #     fn encode_value(&self, _: &T) -> Result<Bytes, BoxError> { Ok(Bytes::new()) }
    /// # }
    /// # impl<T> sealed::DecodeValue<T> for MyCodec {
    /// #     fn decode_value(&self, _: Bytes) -> Result<T, BoxError> { Err("dummy".into()) }
    /// # }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let codec = Arc::new(MyCodec);
    /// # let metadata = Arc::new(());
    /// use sayiir_core::prelude::*;
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
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_registry(
        self,
    ) -> WorkflowBuilder<C, Input, Input, M, NoContinuation, TaskRegistry> {
        self.with_existing_registry(TaskRegistry::new())
    }

    /// Enable registry tracking with an existing registry.
    ///
    /// Use this to reference pre-registered tasks via `then_registered` or to
    /// compose workflows from task libraries.
    ///
    /// **Note**: Takes ownership of the registry. For deserialization/hydration,
    /// rebuild the same registry from code on the deserializing side.
    /// See [`TaskRegistry`] docs for the pattern.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use sayiir_core::prelude::*;
    /// # use sayiir_core::codec::{Encoder, Decoder, sealed};
    /// # use sayiir_core::workflow::SerializableContinuation;
    /// # use bytes::Bytes;
    /// # use std::sync::Arc;
    /// # struct MyCodec;
    /// # impl Encoder for MyCodec {}
    /// # impl Decoder for MyCodec {}
    /// # impl<T> sealed::EncodeValue<T> for MyCodec {
    /// #     fn encode_value(&self, _: &T) -> Result<Bytes, BoxError> { Ok(Bytes::new()) }
    /// # }
    /// # impl<T> sealed::DecodeValue<T> for MyCodec {
    /// #     fn decode_value(&self, _: Bytes) -> Result<T, BoxError> { Err("dummy".into()) }
    /// # }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let codec = Arc::new(MyCodec);
    /// # let metadata = Arc::new(());
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
    /// let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
    ///     .with_existing_registry(registry)
    ///     .then_registered::<u32>("step1")
    ///     .build()?;
    ///
    /// // Deserialize (on another side): rebuild registry, then convert to runnable
    /// let registry = build_registry(codec.clone());
    /// let serializable = workflow.continuation().to_serializable();
    /// let runnable = serializable.to_runnable(&registry)?;
    /// # Ok(())
    /// # }
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
            branch_counter: 0,
            loop_counter: 0,
            child_counter: 0,
            errors: BuildErrors::new(),
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

        let task = to_core_task_arc(id, func, codec);

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(task),
            timeout: None,
            retry_policy: None,
            version: None,
            priority: None,
            tags: Vec::new(),
            next: None,
        };

        let continuation = self.continuation.append(new_task);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

/// Loop method — available for all registry/continuation combinations.
impl<C, Input, Output, M, Cont, R> WorkflowBuilder<C, Input, Output, M, Cont, R>
where
    R: RegistryBehavior,
    Cont: ContinuationState,
{
    /// Add a loop task to the workflow.
    ///
    /// The task function receives the current value and must return
    /// `LoopResult<NewOutput>`:
    /// - `LoopResult::Again(val)` feeds `val` back as the next iteration's input.
    /// - `LoopResult::Done(val)` exits the loop; `val` becomes the loop's output.
    ///
    /// The `max_iterations` parameter sets a safety limit. When reached, the
    /// default `MaxIterationsPolicy::Fail` causes the workflow to fail.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sayiir_core::LoopResult;
    ///
    /// workflow
    ///     .loop_task("refine", |draft: String| async move {
    ///         if is_good_enough(&draft) {
    ///             Ok(LoopResult::Done(draft))
    ///         } else {
    ///             Ok(LoopResult::Again(improve(draft)))
    ///         }
    ///     }, 10)
    /// ```
    ///
    /// If `max_iterations` is 0, an error is recorded and reported at `build()` time.
    pub fn loop_task<F, Fut, NewOutput>(
        self,
        id: &str,
        func: F,
        max_iterations: u32,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, R>
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        Fut: std::future::Future<Output = Result<LoopResult<NewOutput>, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput> + 'static,
    {
        self.loop_task_with_policy(id, func, max_iterations, MaxIterationsPolicy::Fail)
    }

    /// Add a loop task with a custom `MaxIterationsPolicy`.
    ///
    /// Same as [`loop_task`](Self::loop_task) but allows specifying what happens
    /// when `max_iterations` is reached.
    ///
    pub fn loop_task_with_policy<F, Fut, NewOutput>(
        mut self,
        id: &str,
        func: F,
        max_iterations: u32,
        on_max: MaxIterationsPolicy,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, R>
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        Fut: std::future::Future<Output = Result<LoopResult<NewOutput>, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput> + 'static,
    {
        if max_iterations == 0 {
            self.errors
                .push(BuildError::InvalidMaxIterations(id.to_string()));
        }
        let codec = Arc::clone(&self.context.codec);
        let func = Arc::new(func);

        self.registry
            .maybe_register_loop::<Output, NewOutput, _, _, _>(id, codec.clone(), &func);

        let task = to_core_loop_task_arc(id, func, codec);
        let loop_id = crate::workflow::loop_node_id(self.loop_counter);
        self.loop_counter += 1;

        let body = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(task),
            timeout: None,
            retry_policy: None,
            version: None,
            priority: None,
            tags: Vec::new(),
            next: None,
        };

        let loop_node = WorkflowContinuation::Loop {
            id: loop_id.clone(),
            body: Box::new(body),
            max_iterations,
            on_max,
            next: None,
        };

        let continuation = self.continuation.append(loop_node);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(loop_id),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

/// Type-safe builder methods for `RegisterableTask` structs (generated by `#[task]`).
///
/// These methods extract the task ID, output type, and metadata directly from
/// the type, eliminating stringly-typed wiring.
impl<C, Input, Output, M, Cont, R> WorkflowBuilder<C, Input, Output, M, Cont, R>
where
    R: RegistryBehavior,
    Cont: ContinuationState,
{
    /// Add a `#[task]` struct to the workflow (no injected deps — requires `Default`).
    ///
    /// The task ID, output type, timeout, and retry policy are all derived from
    /// `T`'s [`RegisterableTask`] implementation.
    ///
    /// For tasks that carry injected dependencies, use
    /// [`then_task_with`](Self::then_task_with) instead.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Given:  #[task] async fn add_ten(input: u32) -> Result<u32, BoxError> { … }
    ///
    /// WorkflowBuilder::new(ctx)
    ///     .with_registry()
    ///     .then_task::<AddTenTask>()
    ///     .then_task::<DoubleTask>()
    ///     .build()?;
    /// ```
    pub fn then_task<T>(self) -> WorkflowBuilder<C, Input, T::Output, M, WorkflowContinuation, R>
    where
        T: RegisterableTask<Input = Output> + Default,
        Output: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<T::Output> + 'static,
    {
        self.then_task_with(T::default())
    }

    /// Add a `#[task]` struct instance to the workflow (for tasks with injected deps).
    ///
    /// The task ID, output type, timeout, and retry policy are derived from
    /// `T`'s [`RegisterableTask`] implementation. The caller provides the
    /// constructed instance.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Given:  #[task] async fn fetch(url: String, #[inject] client: HttpClient) -> …
    ///
    /// WorkflowBuilder::new(ctx)
    ///     .with_registry()
    ///     .then_task_with(FetchTask::new(client.clone()))
    ///     .build()?;
    /// ```
    pub fn then_task_with<T>(
        mut self,
        task: T,
    ) -> WorkflowBuilder<C, Input, T::Output, M, WorkflowContinuation, R>
    where
        T: RegisterableTask<Input = Output>,
        Output: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<T::Output> + 'static,
    {
        let id = T::task_id();
        let metadata = T::metadata();
        let codec = Arc::clone(&self.context.codec);
        let task = Arc::new(task);

        self.registry.maybe_register_core_task::<T, C>(
            id,
            codec.clone(),
            Arc::clone(&task),
            metadata.clone(),
        );

        let untyped = wrap_core_task(id, task, codec);
        let timeout = metadata.timeout;
        let retry_policy = metadata.retries;
        let tags = metadata.tags;
        let version = metadata.version;
        let priority = metadata.priority.map(Priority::as_u8);

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(untyped),
            timeout,
            retry_policy,
            version,
            priority,
            tags,
            next: None,
        };

        let continuation = self.continuation.append(new_task);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }

    /// Add a `#[task]` loop struct (no injected deps — requires `Default`).
    ///
    /// The task's output must be `LoopResult<NewOutput>`. The loop node ID
    /// is auto-derived. See [`loop_task`](Self::loop_task) for semantics.
    pub fn loop_task_struct<T, NewOutput>(
        self,
        max_iterations: u32,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, R>
    where
        T: RegisterableTask<Input = Output, Output = LoopResult<NewOutput>> + Default,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput> + 'static,
    {
        self.loop_task_struct_with_policy(T::default(), max_iterations, MaxIterationsPolicy::Fail)
    }

    /// Add a `#[task]` loop struct instance (for tasks with injected deps).
    ///
    /// Uses the default `MaxIterationsPolicy::Fail`. For a custom policy,
    /// use [`loop_task_struct_with_policy`](Self::loop_task_struct_with_policy).
    pub fn loop_task_struct_with<T, NewOutput>(
        self,
        task: T,
        max_iterations: u32,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, R>
    where
        T: RegisterableTask<Input = Output, Output = LoopResult<NewOutput>>,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput> + 'static,
    {
        self.loop_task_struct_with_policy(task, max_iterations, MaxIterationsPolicy::Fail)
    }

    /// Add a `#[task]` loop struct instance with a custom `MaxIterationsPolicy`.
    ///
    /// The task's output must be `LoopResult<NewOutput>`. The loop node ID
    /// is auto-derived. See [`loop_task`](Self::loop_task) for semantics.
    pub fn loop_task_struct_with_policy<T, NewOutput>(
        mut self,
        task: T,
        max_iterations: u32,
        on_max: MaxIterationsPolicy,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, R>
    where
        T: RegisterableTask<Input = Output, Output = LoopResult<NewOutput>>,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput> + 'static,
    {
        let id = T::task_id();
        if max_iterations == 0 {
            self.errors
                .push(BuildError::InvalidMaxIterations(id.to_string()));
        }

        let metadata = T::metadata();
        let codec = Arc::clone(&self.context.codec);
        let task = Arc::new(task);

        self.registry
            .maybe_register_core_loop_task::<T, NewOutput, C>(
                id,
                codec.clone(),
                Arc::clone(&task),
                metadata.clone(),
            );

        let untyped = wrap_core_loop_task(id, task, codec);
        let timeout = metadata.timeout;
        let retry_policy = metadata.retries;
        let tags = metadata.tags;
        let version = metadata.version;
        let priority = metadata.priority.map(Priority::as_u8);

        let loop_id = crate::workflow::loop_node_id(self.loop_counter);
        self.loop_counter += 1;

        let body = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(untyped),
            timeout,
            retry_policy,
            version,
            priority,
            tags,
            next: None,
        };

        let loop_node = WorkflowContinuation::Loop {
            id: loop_id.clone(),
            body: Box::new(body),
            max_iterations,
            on_max,
            next: None,
        };

        let continuation = self.continuation.append(loop_node);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(loop_id),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
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
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }

    /// Wait for a named external signal before continuing.
    ///
    /// The signal payload (if any) becomes the input to the next step.
    /// If a timeout is specified and expires before a signal arrives,
    /// `None` is passed as the payload.
    #[must_use]
    pub fn wait_for_signal(
        self,
        id: &str,
        signal_name: &str,
        timeout: Option<std::time::Duration>,
    ) -> WorkflowBuilder<C, Input, Output, M, WorkflowContinuation, R> {
        let new_node = WorkflowContinuation::AwaitSignal {
            id: id.to_string(),
            signal_name: signal_name.to_string(),
            timeout,
            next: None,
        };
        let continuation = self.continuation.append(new_node);
        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }

    /// Inline a child workflow into the pipeline.
    ///
    /// The child's entire continuation tree is embedded as a
    /// `WorkflowContinuation::ChildWorkflow` node. The child's output becomes
    /// the input to the next step.
    ///
    /// For serializable workflows (built with `.with_registry()`), use
    /// [`then_serializable_flow`](WorkflowBuilder::then_serializable_flow)
    /// instead so that the child's task registry is merged into the parent's.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let child = WorkflowBuilder::new(child_ctx)
    ///     .then("double", |i: u32| async move { Ok(i * 2) })
    ///     .build()?;
    ///
    /// let parent = WorkflowBuilder::new(parent_ctx)
    ///     .then("add_one", |i: u32| async move { Ok(i + 1) })
    ///     .then_flow(child)
    ///     .build()?;
    /// ```
    #[must_use]
    pub fn then_flow<ChildOutput>(
        mut self,
        child: Workflow<C, ChildOutput, M>,
    ) -> WorkflowBuilder<C, Input, ChildOutput, M, WorkflowContinuation, R> {
        let child_id = format!("child_{}", self.child_counter);
        self.child_counter += 1;

        let child_cont = child.into_continuation();
        let new_node = WorkflowContinuation::ChildWorkflow {
            id: child_id.clone(),
            child: Arc::new(child_cont),
            next: None,
        };

        let continuation = self.continuation.append(new_node);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(child_id),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

/// Child workflow composition with registry merging (only available with `TaskRegistry`).
impl<C, Input, Output, M, Cont> WorkflowBuilder<C, Input, Output, M, Cont, TaskRegistry>
where
    Cont: ContinuationState,
{
    /// Inline a serializable child workflow, merging its task registry into the parent's.
    ///
    /// This is the serializable counterpart of [`then_flow`](WorkflowBuilder::then_flow).
    /// The child's task registry entries are merged into the parent's registry so
    /// that distributed execution can look up all task implementations.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let child = WorkflowBuilder::new(child_ctx)
    ///     .with_registry()
    ///     .then("double", |i: u32| async move { Ok(i * 2) })
    ///     .build()?;
    ///
    /// let parent = WorkflowBuilder::new(parent_ctx)
    ///     .with_registry()
    ///     .then("add_one", |i: u32| async move { Ok(i + 1) })
    ///     .then_serializable_flow(child)
    ///     .build()?;
    /// ```
    #[must_use]
    pub fn then_serializable_flow<ChildOutput>(
        mut self,
        child: SerializableWorkflow<C, ChildOutput, M>,
    ) -> WorkflowBuilder<C, Input, ChildOutput, M, WorkflowContinuation, TaskRegistry> {
        let child_id = format!("child_{}", self.child_counter);
        self.child_counter += 1;

        let (child_cont, child_registry) = child.into_parts();
        self.registry.merge(child_registry);

        let new_node = WorkflowContinuation::ChildWorkflow {
            id: child_id.clone(),
            child: Arc::new(child_cont),
            next: None,
        };

        let continuation = self.continuation.append(new_node);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(child_id),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
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
    /// If the task ID is not found in the registry, the error is recorded and
    /// reported at `build()` time.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use sayiir_core::prelude::*;
    /// # use sayiir_core::codec::{Encoder, Decoder, sealed};
    /// # use bytes::Bytes;
    /// # use std::sync::Arc;
    /// # struct MyCodec;
    /// # impl Encoder for MyCodec {}
    /// # impl Decoder for MyCodec {}
    /// # impl<T> sealed::EncodeValue<T> for MyCodec {
    /// #     fn encode_value(&self, _: &T) -> Result<Bytes, BoxError> { Ok(Bytes::new()) }
    /// # }
    /// # impl<T> sealed::DecodeValue<T> for MyCodec {
    /// #     fn decode_value(&self, _: Bytes) -> Result<T, BoxError> { Err("dummy".into()) }
    /// # }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let codec = Arc::new(MyCodec);
    /// # let ctx = WorkflowContext::new("my-workflow", codec.clone(), Arc::new(()));
    /// use sayiir_core::prelude::*;
    ///
    /// let mut registry = TaskRegistry::new();
    /// registry.register_fn("double", codec.clone(), |i: u32| async move { Ok(i * 2) });
    ///
    /// let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
    ///     .with_existing_registry(registry)
    ///     .then_registered::<u32>("double")
    ///     .with_metadata(TaskMetadata {
    ///         display_name: Some("Double".into()),
    ///         ..Default::default()
    ///     })
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn then_registered<NewOutput>(
        mut self,
        id: &str,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, TaskRegistry>
    where
        Output: Send + 'static,
        NewOutput: Send + 'static,
    {
        let func = self.registry.get(id);
        if func.is_none() {
            self.errors.push(BuildError::TaskNotFound(id.to_string()));
        }
        let meta = self.registry.get_metadata(id);
        let timeout = meta.and_then(|m| m.timeout);
        let retry_policy = self
            .registry
            .get_metadata(id)
            .and_then(|m| m.retries.clone());
        let version = self
            .registry
            .get_metadata(id)
            .and_then(|m| m.version.clone());
        let priority = self
            .registry
            .get_metadata(id)
            .and_then(|m| m.priority.map(Priority::as_u8));
        let tags = self
            .registry
            .get_metadata(id)
            .map(|m| m.tags.clone())
            .unwrap_or_default();

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func,
            timeout,
            retry_policy,
            version,
            priority,
            tags,
            next: None,
        };

        let continuation = self.continuation.append(new_task);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(id.to_string()),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }

    /// Reference a pre-registered loop body task by ID.
    ///
    /// The body task must already be registered in the registry under
    /// `body_task_id`. The loop node ID is derived as `"loop_{body_task_id}"`.
    ///
    /// If the body task is not found, or `max_iterations` is 0, the error is
    /// recorded and reported at `build()` time.
    pub fn loop_task_registered<NewOutput>(
        mut self,
        body_task_id: &str,
        max_iterations: u32,
        on_max: MaxIterationsPolicy,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, WorkflowContinuation, TaskRegistry>
    where
        Output: Send + 'static,
        NewOutput: Send + 'static,
    {
        if max_iterations == 0 {
            self.errors
                .push(BuildError::InvalidMaxIterations(body_task_id.to_string()));
        }
        let func = self.registry.get(body_task_id);
        if func.is_none() {
            self.errors
                .push(BuildError::TaskNotFound(body_task_id.to_string()));
        }
        let meta = self.registry.get_metadata(body_task_id);
        let timeout = meta.and_then(|m| m.timeout);
        let retry_policy = self
            .registry
            .get_metadata(body_task_id)
            .and_then(|m| m.retries.clone());
        let version = self
            .registry
            .get_metadata(body_task_id)
            .and_then(|m| m.version.clone());
        let priority = self
            .registry
            .get_metadata(body_task_id)
            .and_then(|m| m.priority.map(Priority::as_u8));
        let tags = self
            .registry
            .get_metadata(body_task_id)
            .map(|m| m.tags.clone())
            .unwrap_or_default();

        let body = WorkflowContinuation::Task {
            id: body_task_id.to_string(),
            func,
            timeout,
            retry_policy,
            version,
            priority,
            tags,
            next: None,
        };

        let loop_id = crate::workflow::loop_node_id(self.loop_counter);
        self.loop_counter += 1;
        let loop_node = WorkflowContinuation::Loop {
            id: loop_id.clone(),
            body: Box::new(body),
            max_iterations,
            on_max,
            next: None,
        };

        let continuation = self.continuation.append(loop_node);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(loop_id),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
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
    /// ```rust
    /// # use sayiir_core::prelude::*;
    /// # use sayiir_core::codec::{Encoder, Decoder, sealed};
    /// # use bytes::Bytes;
    /// # use std::sync::Arc;
    /// # struct MyCodec;
    /// # impl Encoder for MyCodec {}
    /// # impl Decoder for MyCodec {}
    /// # impl<T> sealed::EncodeValue<T> for MyCodec {
    /// #     fn encode_value(&self, _: &T) -> Result<Bytes, BoxError> { Ok(Bytes::new()) }
    /// # }
    /// # impl<T> sealed::DecodeValue<T> for MyCodec {
    /// #     fn decode_value(&self, _: Bytes) -> Result<T, BoxError> { Err("dummy".into()) }
    /// # }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let codec = Arc::new(MyCodec);
    /// # let ctx = WorkflowContext::new("my-workflow", codec, Arc::new(()));
    /// use sayiir_core::prelude::*;
    /// use sayiir_core::task::RetryPolicy;
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
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_metadata(mut self, metadata: crate::task::TaskMetadata) -> Self {
        if let Some(ref id) = self.last_task_id {
            let timeout = metadata.timeout;
            let retry_policy = metadata.retries.clone();
            let version = metadata.version.clone();
            self.registry.set_metadata(id, metadata);
            // Also update the timeout, retry policy, and version on the continuation node
            // so they're available for direct execution (not just the serializable roundtrip path).
            self.continuation.set_task_timeout(id, timeout);
            self.continuation.set_task_retry_policy(id, retry_policy);
            self.continuation.set_task_version(id, version);
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
    ///         let count: u32 = outputs.get_by_id("count")?;
    ///         let name: String = outputs.get_by_id("name")?;
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
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
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
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
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
    /// Returns accumulated build errors or duplicate task IDs.
    pub fn build(mut self) -> Result<Workflow<C, Input, M>, BuildErrors>
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
            self.errors.push(BuildError::DuplicateTaskId(dup));
        }
        if !self.errors.is_empty() {
            return Err(self.errors);
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
    /// Returns accumulated build errors or duplicate task IDs.
    pub fn build(mut self) -> Result<SerializableWorkflow<C, Input, M>, BuildErrors>
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
            self.errors.push(BuildError::DuplicateTaskId(dup));
        }
        if !self.errors.is_empty() {
            return Err(self.errors);
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

    /// Add a `#[task]` struct as a branch (no injected deps — requires `Default`).
    pub fn add_task<T>(&mut self)
    where
        T: RegisterableTask<Input = Input> + Default,
        Input: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<T::Output> + 'static,
    {
        self.add_task_with(T::default());
    }

    /// Add a `#[task]` struct instance as a branch (for tasks with injected deps).
    pub fn add_task_with<T>(&mut self, task: T)
    where
        T: RegisterableTask<Input = Input>,
        Input: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<T::Output> + 'static,
    {
        let id = T::task_id();
        let codec = Arc::clone(&self.codec);
        let task = Arc::new(task);
        let untyped = wrap_core_task(id, task, codec);
        self.branches.push(ErasedBranch {
            id: id.to_string(),
            task: untyped,
        });
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
    branch_counter: usize,
    loop_counter: usize,
    child_counter: usize,
    errors: BuildErrors,
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

    /// Add a `#[task]` struct as a fork branch (no injected deps — requires `Default`).
    #[must_use]
    pub fn branch_task<T>(self) -> Self
    where
        T: RegisterableTask<Input = Output> + Default,
        Output: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<T::Output> + 'static,
    {
        self.branch_task_with(T::default())
    }

    /// Add a `#[task]` struct instance as a fork branch (for tasks with injected deps).
    #[must_use]
    pub fn branch_task_with<T>(mut self, task: T) -> Self
    where
        T: RegisterableTask<Input = Output>,
        Output: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<T::Output> + 'static,
    {
        let id = T::task_id();
        let metadata = T::metadata();
        let codec = Arc::clone(&self.context.codec);
        let task = Arc::new(task);

        self.registry.maybe_register_core_task::<T, C>(
            id,
            codec.clone(),
            Arc::clone(&task),
            metadata,
        );

        let untyped = wrap_core_task(id, task, codec);
        self.branches.push(ErasedBranch {
            id: id.to_string(),
            task: untyped,
        });
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

        let join_task_fn = to_heterogeneous_join_task_arc(id, func, codec);

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
                    retry_policy: None,
                    version: None,
                    priority: None,
                    tags: Vec::new(),
                    next: None,
                })
            })
            .collect();

        //

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(join_task_fn),
            timeout: None,
            retry_policy: None,
            version: None,
            priority: None,
            tags: Vec::new(),
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
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
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
    /// If the task ID is not found, the error is recorded and reported at
    /// `build()` time.
    #[must_use]
    pub fn branch_registered(mut self, id: &str) -> Self
    where
        Output: Send + 'static,
    {
        match self.registry.get(id) {
            Some(task) => {
                self.branches.push(ErasedBranch {
                    id: id.to_string(),
                    task,
                });
            }
            None => {
                self.errors.push(BuildError::TaskNotFound(id.to_string()));
            }
        }
        self
    }

    /// Join using a pre-registered join task by ID.
    ///
    /// If the task ID is not found, the error is recorded and reported at
    /// `build()` time.
    pub fn join_registered<JoinOutput>(
        mut self,
        id: &str,
    ) -> WorkflowBuilder<C, Input, JoinOutput, M, WorkflowContinuation, TaskRegistry>
    where
        Output: Send + 'static,
        JoinOutput: Send + 'static,
    {
        let join_task_fn = self.registry.get(id);
        if join_task_fn.is_none() {
            self.errors.push(BuildError::TaskNotFound(id.to_string()));
        }
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
                let meta = self.registry.get_metadata(&b.id);
                let timeout = meta.and_then(|m| m.timeout);
                let retry_policy = self
                    .registry
                    .get_metadata(&b.id)
                    .and_then(|m| m.retries.clone());
                let version = self
                    .registry
                    .get_metadata(&b.id)
                    .and_then(|m| m.version.clone());
                let priority = self
                    .registry
                    .get_metadata(&b.id)
                    .and_then(|m| m.priority.map(Priority::as_u8));
                let tags = self
                    .registry
                    .get_metadata(&b.id)
                    .map(|m| m.tags.clone())
                    .unwrap_or_default();
                Arc::new(WorkflowContinuation::Task {
                    id: b.id,
                    func: Some(b.task),
                    timeout,
                    retry_policy,
                    version,
                    priority,
                    tags,
                    next: None,
                })
            })
            .collect();

        let join_retry_policy = self
            .registry
            .get_metadata(id)
            .and_then(|m| m.retries.clone());
        let join_version = self
            .registry
            .get_metadata(id)
            .and_then(|m| m.version.clone());
        let join_priority = self
            .registry
            .get_metadata(id)
            .and_then(|m| m.priority.map(Priority::as_u8));
        let join_tags = self
            .registry
            .get_metadata(id)
            .map(|m| m.tags.clone())
            .unwrap_or_default();
        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: join_task_fn,
            timeout: join_timeout,
            retry_policy: join_retry_policy,
            version: join_version,
            priority: join_priority,
            tags: join_tags,
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
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

// ============================================================================
// Conditional Branching (route)
// ============================================================================

/// A lightweight sub-builder for constructing branch continuations.
///
/// This is used inside `RouteBuilder::branch()` closures to build
/// the continuation chain for each named branch.
pub struct SubBuilder<C, Input, Output, R = NoRegistry> {
    codec: Arc<C>,
    continuation: Option<WorkflowContinuation>,
    registry: R,
    errors: BuildErrors,
    _phantom: PhantomData<(Input, Output)>,
}

impl<C, Input, Output, R> SubBuilder<C, Input, Output, R>
where
    R: RegistryBehavior,
{
    /// Add a sequential task to this sub-flow.
    pub fn then<F, Fut, NewOutput>(
        mut self,
        id: &str,
        func: F,
    ) -> SubBuilder<C, Input, NewOutput, R>
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        Fut: std::future::Future<Output = Result<NewOutput, crate::error::BoxError>>
            + Send
            + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput> + 'static,
    {
        let codec = Arc::clone(&self.codec);
        let func = Arc::new(func);

        self.registry
            .maybe_register::<Output, NewOutput, _, _, _>(id, codec.clone(), &func);

        let task = to_core_task_arc(id, func, codec);

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: Some(task),
            timeout: None,
            retry_policy: None,
            version: None,
            priority: None,
            tags: Vec::new(),
            next: None,
        };

        let continuation = match self.continuation {
            None => Some(new_task),
            Some(mut cont) => {
                cont.append_to_chain(new_task);
                Some(cont)
            }
        };

        SubBuilder {
            codec: self.codec,
            continuation,
            registry: self.registry,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

/// Delay and signal methods for `SubBuilder`.
impl<C, Input, Output, R> SubBuilder<C, Input, Output, R> {
    /// Add a durable delay to this sub-flow.
    #[must_use]
    pub fn delay(self, id: &str, duration: std::time::Duration) -> Self {
        let new_node = WorkflowContinuation::Delay {
            id: id.to_string(),
            duration,
            next: None,
        };
        let continuation = match self.continuation {
            None => Some(new_node),
            Some(mut cont) => {
                cont.append_to_chain(new_node);
                Some(cont)
            }
        };
        SubBuilder {
            codec: self.codec,
            continuation,
            registry: self.registry,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }

    /// Wait for a named external signal in this sub-flow.
    #[must_use]
    pub fn wait_for_signal(
        self,
        id: &str,
        signal_name: &str,
        timeout: Option<std::time::Duration>,
    ) -> Self {
        let new_node = WorkflowContinuation::AwaitSignal {
            id: id.to_string(),
            signal_name: signal_name.to_string(),
            timeout,
            next: None,
        };
        let continuation = match self.continuation {
            None => Some(new_node),
            Some(mut cont) => {
                cont.append_to_chain(new_node);
                Some(cont)
            }
        };
        SubBuilder {
            codec: self.codec,
            continuation,
            registry: self.registry,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }

    /// Consume the sub-builder and return the built continuation, registry, and any errors.
    fn build(mut self) -> (Option<WorkflowContinuation>, BuildErrors, R) {
        if self.continuation.is_none() {
            self.errors.push(BuildError::EmptyBranch);
        }
        (self.continuation, self.errors, self.registry)
    }
}

/// Builder for constructing conditional branches fluently.
///
/// Created by calling `.route()` on a `WorkflowBuilder`. Add branches with
/// `.branch()`, optionally set a default with `.default_branch()`, then close
/// with `.done()` to return to the parent builder.
///
/// The `K` type parameter constrains the routing keys to a `BranchKey` enum,
/// providing compile-time safety: `.branch()` takes a `K` variant and `.done()`
/// checks exhaustiveness.
pub struct RouteBuilder<C, Input, Output, BranchOut, K, M, Cont, R = NoRegistry> {
    context: WorkflowContext<C, M>,
    parent_continuation: Cont,
    registry: R,
    branch_id: String,
    key_fn: crate::task::UntypedCoreTask,
    named_branches: HashMap<String, Box<WorkflowContinuation>>,
    default_branch: Option<Box<WorkflowContinuation>>,
    branch_counter: usize,
    loop_counter: usize,
    child_counter: usize,
    errors: BuildErrors,
    _phantom: PhantomData<(Input, Output, BranchOut, K)>,
}

/// `route` method — available for all registry/continuation combinations.
impl<C, Input, Output, M, Cont, R> WorkflowBuilder<C, Input, Output, M, Cont, R>
where
    R: RegistryBehavior,
    Cont: ContinuationState,
{
    /// Add a conditional branching node to the workflow.
    ///
    /// The `key_fn` extracts a typed routing key from the previous step's output.
    /// The key type `K` must implement [`BranchKey`], constraining the set of
    /// valid branch names at compile time. The builder's `.done()` method
    /// verifies exhaustiveness: every variant in `K` must have a corresponding
    /// `.branch()` call (or a `.default_branch()` must be provided).
    ///
    /// # Type Parameters
    ///
    /// - `BranchOut`: The common return type of all branches.
    /// - `K`: A [`BranchKey`] enum whose variants map to branch names.
    pub fn route<BranchOut, K, KeyFn, Fut>(
        mut self,
        key_fn: KeyFn,
    ) -> RouteBuilder<C, Input, Output, BranchOut, K, M, Cont, R>
    where
        K: BranchKey,
        KeyFn: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        BranchOut: Send + 'static,
        Fut: std::future::Future<Output = Result<K, crate::error::BoxError>> + Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<String> + 'static,
    {
        let codec = Arc::clone(&self.context.codec);
        self.branch_counter += 1;
        let branch_id = format!("branch_{}", self.branch_counter);
        let key_fn_id = crate::workflow::key_fn_id(&branch_id);

        // Wrap the typed key_fn to produce String for the core layer
        let key_fn_arc = Arc::new(key_fn);
        let key_fn_string = {
            let key_fn_arc = Arc::clone(&key_fn_arc);
            Arc::new(move |input: Output| {
                let key_fn = Arc::clone(&key_fn_arc);
                async move {
                    let k = key_fn(input).await?;
                    Ok(k.as_key().to_string())
                }
            })
        };

        // Register the string-producing wrapper in the registry (if enabled)
        self.registry.maybe_register::<Output, String, _, _, _>(
            &key_fn_id,
            codec.clone(),
            &key_fn_string,
        );

        let key_fn_task = to_core_task_arc(&key_fn_id, key_fn_string, codec);

        RouteBuilder {
            context: self.context,
            parent_continuation: self.continuation,
            registry: self.registry,
            branch_id,
            key_fn: key_fn_task,
            named_branches: HashMap::new(),
            default_branch: None,
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

/// Builder for constructing conditional branches using pre-registered tasks.
///
/// Created by calling `.route_registered()` on a `WorkflowBuilder`.
/// Add branches with `.branch_registered()`, optionally set a default with
/// `.default_registered()`, then close with `.done()`.
pub struct RouteRegisteredBuilder<C, Input, Output, BranchOut, M, Cont> {
    context: WorkflowContext<C, M>,
    parent_continuation: Cont,
    registry: TaskRegistry,
    branch_id: String,
    key_fn: Option<crate::task::UntypedCoreTask>,
    named_branches: HashMap<String, Box<WorkflowContinuation>>,
    default_branch: Option<Box<WorkflowContinuation>>,
    branch_counter: usize,
    loop_counter: usize,
    child_counter: usize,
    errors: BuildErrors,
    _phantom: PhantomData<(Input, Output, BranchOut)>,
}

/// `route_registered` — only available with `TaskRegistry`.
impl<C, Input, Output, M, Cont> WorkflowBuilder<C, Input, Output, M, Cont, TaskRegistry>
where
    Cont: ContinuationState,
{
    /// Add a conditional branching node using pre-registered tasks.
    ///
    /// The key function must already be registered in the registry under
    /// [`key_fn_id(id)`](crate::workflow::key_fn_id) (the convention used by
    /// both the programmatic `route` API and the `workflow!` macro).
    ///
    /// If the key function is not found, the error is recorded and reported
    /// at `build()` time.
    pub fn route_registered<BranchOut>(
        mut self,
        id: &str,
    ) -> RouteRegisteredBuilder<C, Input, Output, BranchOut, M, Cont> {
        let key_fn_id_str = crate::workflow::key_fn_id(id);
        let key_fn = self.registry.get(&key_fn_id_str);
        if key_fn.is_none() {
            self.errors.push(BuildError::TaskNotFound(key_fn_id_str));
        }

        RouteRegisteredBuilder {
            context: self.context,
            parent_continuation: self.continuation,
            registry: self.registry,
            branch_id: id.to_string(),
            key_fn,
            named_branches: HashMap::new(),
            default_branch: None,
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

impl<C, Input, Output, BranchOut, M, Cont>
    RouteRegisteredBuilder<C, Input, Output, BranchOut, M, Cont>
where
    Cont: ContinuationState,
{
    /// Build a chain of `WorkflowContinuation::Task` nodes from registered task IDs.
    ///
    /// Returns `(Ok(chain), errors)` on success or `(Err(()), errors)` if any task
    /// was not found (errors are pushed to the returned vec).
    fn build_chain_from_registry(
        registry: &TaskRegistry,
        task_ids: &[&str],
        errors: &mut BuildErrors,
    ) -> Option<Box<WorkflowContinuation>> {
        let mut current: Option<WorkflowContinuation> = None;
        let mut ok = true;
        for id in task_ids.iter().rev() {
            let func = registry.get(id);
            if func.is_none() {
                errors.push(BuildError::TaskNotFound((*id).to_string()));
                ok = false;
            }
            let meta = registry.get_metadata(id);
            let timeout = meta.and_then(|m| m.timeout);
            let retry_policy = registry.get_metadata(id).and_then(|m| m.retries.clone());
            let version = registry.get_metadata(id).and_then(|m| m.version.clone());
            let priority = registry
                .get_metadata(id)
                .and_then(|m| m.priority.map(Priority::as_u8));
            let tags = registry
                .get_metadata(id)
                .map(|m| m.tags.clone())
                .unwrap_or_default();
            current = Some(WorkflowContinuation::Task {
                id: (*id).to_string(),
                func,
                timeout,
                retry_policy,
                version,
                priority,
                tags,
                next: current.map(Box::new),
            });
        }
        if !ok || current.is_none() {
            if current.is_none() {
                errors.push(BuildError::EmptyFork);
            }
            return None;
        }
        current.map(Box::new)
    }

    /// Add a named branch containing a chain of pre-registered tasks.
    ///
    /// If any task ID is not found, the error is recorded and reported at
    /// `build()` time.
    #[must_use]
    pub fn branch_registered(mut self, key: &str, task_ids: &[&str]) -> Self {
        if let Some(chain) =
            Self::build_chain_from_registry(&self.registry, task_ids, &mut self.errors)
        {
            self.named_branches.insert(key.to_string(), chain);
        }
        self
    }

    /// Set the default branch (used when no key matches) from pre-registered tasks.
    ///
    /// If any task ID is not found, the error is recorded and reported at
    /// `build()` time.
    #[must_use]
    pub fn default_registered(mut self, task_ids: &[&str]) -> Self {
        if let Some(chain) =
            Self::build_chain_from_registry(&self.registry, task_ids, &mut self.errors)
        {
            self.default_branch = Some(chain);
        }
        self
    }

    /// Close the branching node and return to the parent builder.
    pub fn done(
        self,
    ) -> WorkflowBuilder<C, Input, BranchEnvelope<BranchOut>, M, WorkflowContinuation, TaskRegistry>
    {
        let branch_node = WorkflowContinuation::Branch {
            id: self.branch_id.clone(),
            key_fn: self.key_fn,
            branches: self.named_branches,
            default: self.default_branch,
            next: None,
        };

        let continuation = self.parent_continuation.append(branch_node);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(self.branch_id),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}

impl<C, Input, Output, BranchOut, K, M, Cont, R>
    RouteBuilder<C, Input, Output, BranchOut, K, M, Cont, R>
where
    K: BranchKey,
    R: RegistryBehavior,
    Cont: ContinuationState,
{
    /// Add a named branch to the conditional branching node.
    ///
    /// The `key` is a variant of the `BranchKey` enum, ensuring only declared
    /// keys can be used. The closure receives a [`SubBuilder`] and must chain
    /// at least one step on it. The final output type must match `BranchOut`.
    ///
    /// If the closure doesn't add any steps, the error is recorded and
    /// reported at `build()` time.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn branch<F>(mut self, key: K, build_fn: F) -> Self
    where
        F: FnOnce(SubBuilder<C, Output, Output, R>) -> SubBuilder<C, Output, BranchOut, R>,
    {
        let sub = SubBuilder {
            codec: Arc::clone(&self.context.codec),
            continuation: None,
            registry: self.registry,
            errors: BuildErrors::new(),
            _phantom: PhantomData,
        };
        let built = build_fn(sub);
        let (continuation, errors, registry) = built.build();
        self.registry = registry;
        self.errors.extend(errors);
        if let Some(cont) = continuation {
            self.named_branches
                .insert(key.as_key().to_string(), Box::new(cont));
        }
        self
    }

    /// Set the default branch (used when no key matches).
    ///
    /// The closure receives a [`SubBuilder`] and must chain at least one step on it.
    ///
    /// If the closure doesn't add any steps, the error is recorded and
    /// reported at `build()` time.
    #[must_use]
    pub fn default_branch<F>(mut self, build_fn: F) -> Self
    where
        F: FnOnce(SubBuilder<C, Output, Output, R>) -> SubBuilder<C, Output, BranchOut, R>,
    {
        let sub = SubBuilder {
            codec: Arc::clone(&self.context.codec),
            continuation: None,
            registry: self.registry,
            errors: BuildErrors::new(),
            _phantom: PhantomData,
        };
        let built = build_fn(sub);
        let (continuation, errors, registry) = built.build();
        self.registry = registry;
        self.errors.extend(errors);
        if let Some(cont) = continuation {
            self.default_branch = Some(Box::new(cont));
        }
        self
    }

    /// Close the branching node and return to the parent builder.
    ///
    /// Performs exhaustiveness checks against `K::all_keys()`:
    /// - **Missing branches**: keys in the enum with no `.branch()` call and no
    ///   `.default_branch()` → error recorded.
    /// - **Orphan branches**: keys passed to `.branch()` that are not in the enum
    ///   → error recorded.
    ///
    /// All errors are reported at `build()` time.
    #[allow(clippy::type_complexity)]
    pub fn done(
        mut self,
    ) -> WorkflowBuilder<C, Input, BranchEnvelope<BranchOut>, M, WorkflowContinuation, R> {
        let all_keys: std::collections::HashSet<&str> = K::all_keys().iter().copied().collect();
        let declared_keys: std::collections::HashSet<&str> =
            self.named_branches.keys().map(String::as_str).collect();

        // Check for orphan branches (keys not in the enum)
        let orphans: Vec<String> = declared_keys
            .difference(&all_keys)
            .map(|k| (*k).to_string())
            .collect();
        if !orphans.is_empty() {
            self.errors.push(BuildError::OrphanBranches {
                branch_id: self.branch_id.clone(),
                orphan_keys: orphans,
            });
        }

        // Check for missing branches (keys in the enum with no branch and no default)
        if self.default_branch.is_none() {
            let missing: Vec<String> = all_keys
                .difference(&declared_keys)
                .map(|k| (*k).to_string())
                .collect();
            if !missing.is_empty() {
                self.errors.push(BuildError::MissingBranches {
                    branch_id: self.branch_id.clone(),
                    missing_keys: missing,
                });
            }
        }

        let branch_node = WorkflowContinuation::Branch {
            id: self.branch_id.clone(),
            key_fn: Some(self.key_fn),
            branches: self.named_branches,
            default: self.default_branch,
            next: None,
        };

        let continuation = self.parent_continuation.append(branch_node);

        WorkflowBuilder {
            continuation,
            context: self.context,
            registry: self.registry,
            last_task_id: Some(self.branch_id),
            branch_counter: self.branch_counter,
            loop_counter: self.loop_counter,
            child_counter: self.child_counter,
            errors: self.errors,
            _phantom: PhantomData,
        }
    }
}
