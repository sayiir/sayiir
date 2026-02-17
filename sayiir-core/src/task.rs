use crate::codec::{Codec, sealed};
use crate::error::{BoxError, WorkflowError};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

/// Metadata associated with a task definition.
///
/// This provides optional configuration for task execution behavior,
/// including display information, timeouts, and retry policies.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskMetadata {
    /// Human-readable name for the task (for UI/logging).
    pub display_name: Option<String>,
    /// Description of what the task does.
    pub description: Option<String>,
    /// Maximum time the task is allowed to run.
    pub timeout: Option<Duration>,
    /// Retry policy for failed task executions.
    pub retries: Option<RetryPolicy>,
    /// Tags for categorization and filtering.
    pub tags: Vec<String>,
}

/// Configuration for retrying failed task executions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retries after the initial attempt.
    #[serde(alias = "max_attempts")]
    pub max_retries: u32,
    /// Initial delay before the first retry.
    pub initial_delay: Duration,
    /// Multiplier applied to delay after each retry (for exponential backoff).
    pub backoff_multiplier: f32,
    /// Maximum delay between retries (caps exponential growth).
    #[serde(default)]
    pub max_delay: Option<Duration>,
}

pub use crate::branch_results::NamedBranchResults;

/// A type-safe map of branch outputs for heterogeneous fork-join.
///
/// Each branch can return a different type. Use `get::<T>(name)` to retrieve
/// a branch's output with the correct type.
///
/// # Example
///
/// ```rust,ignore
/// .join("combine", |outputs: BranchOutputs<MyCodec>| async move {
///     let count: u32 = outputs.get("counter")?;
///     let name: String = outputs.get("fetch_name")?;
///     let items: Vec<Item> = outputs.get("load_items")?;
///     Ok(format!("{}: {} items for {}", count, items.len(), name))
/// })
/// ```
pub struct BranchOutputs<C> {
    outputs: HashMap<String, Bytes>,
    codec: Arc<C>,
}

impl<C> BranchOutputs<C> {
    /// Create a new `BranchOutputs` from raw data.
    pub fn new(outputs: HashMap<String, Bytes>, codec: Arc<C>) -> Self {
        Self { outputs, codec }
    }

    /// Get the names of all branches.
    pub fn branch_names(&self) -> impl Iterator<Item = &str> {
        self.outputs.keys().map(std::string::String::as_str)
    }

    /// Check if a branch exists.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.outputs.contains_key(name)
    }

    /// Get the number of branches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    /// Check if there are no branches.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }
}

impl<C: Codec> BranchOutputs<C> {
    /// Get a branch output by name, deserializing to the requested type.
    ///
    /// # Errors
    ///
    /// Returns an error if the branch doesn't exist or deserialization fails.
    pub fn get<T>(&self, name: &str) -> Result<T, BoxError>
    where
        C: sealed::DecodeValue<T>,
    {
        let bytes = self
            .outputs
            .get(name)
            .ok_or_else(|| WorkflowError::BranchNotFound(name.to_string()))?;

        self.codec.decode(bytes.clone())
    }
}

/// A core task is a task that can be run by the workflow runtime.
///
/// Tasks can be defined either as closures (via `WorkflowBuilder::then`) or as
/// structs implementing this trait directly. Struct-based tasks are useful for:
/// - Reusable task logic across workflows
/// - Tasks with configuration/state
/// - Serializable workflows (tasks can be registered by ID)
///
/// # Example
///
/// ```rust
/// use sayiir_core::prelude::*;
/// use std::pin::Pin;
/// use std::future::Future;
///
/// /// A task that doubles its input.
/// struct DoubleTask;
///
/// impl CoreTask for DoubleTask {
///     type Input = u32;
///     type Output = u32;
///     type Future = Pin<Box<dyn Future<Output = Result<u32, BoxError>> + Send>>;
///
///     fn run(&self, input: u32) -> Self::Future {
///         Box::pin(async move { Ok(input * 2) })
///     }
/// }
///
/// /// A configurable task with state.
/// struct MultiplyTask {
///     factor: u32,
/// }
///
/// impl CoreTask for MultiplyTask {
///     type Input = u32;
///     type Output = u32;
///     type Future = Pin<Box<dyn Future<Output = Result<u32, BoxError>> + Send>>;
///
///     fn run(&self, input: u32) -> Self::Future {
///         let factor = self.factor;
///         Box::pin(async move { Ok(input * factor) })
///     }
/// }
/// ```
pub trait CoreTask: Send + Sync {
    type Input;
    type Output;
    type Future: Future<Output = Result<Self::Output, BoxError>> + Send;

    /// Run the task with the given input and return the output.
    fn run(&self, input: Self::Input) -> Self::Future;
}

/// Wrapper that enables closures to implement `CoreTask`.
///
/// Use the [`fn_task`] helper function to create instances with inferred types.
///
/// # Example
///
/// ```rust,ignore
/// use sayiir_core::task::fn_task;
///
/// // Both work with the same `register` method:
/// registry.register("closure", codec.clone(), fn_task(|i: u32| async move { Ok(i * 2) }));
/// registry.register("struct", codec.clone(), MyTask::new());
/// ```
pub struct FnTask<F, I, O, Fut>(F, PhantomData<fn(I) -> (O, Fut)>);

impl<F, I, O, Fut> CoreTask for FnTask<F, I, O, Fut>
where
    F: Fn(I) -> Fut + Send + Sync,
    I: Send,
    O: Send,
    Fut: Future<Output = Result<O, BoxError>> + Send,
{
    type Input = I;
    type Output = O;
    type Future = Fut;

    fn run(&self, input: I) -> Self::Future {
        (self.0)(input)
    }
}

/// Create a `FnTask` from a closure with inferred types.
///
/// This is the preferred way to wrap closures for use with the unified `register` API.
///
/// # Example
///
/// ```rust,ignore
/// use sayiir_core::task::fn_task;
///
/// registry.register("double", codec, fn_task(|i: u32| async move { Ok(i * 2) }));
/// ```
pub fn fn_task<F, I, O, Fut>(f: F) -> FnTask<F, I, O, Fut>
where
    F: Fn(I) -> Fut,
{
    FnTask(f, PhantomData)
}

/// A type-erased future that outputs `Result<Bytes>`.
///
/// This is a newtype around a pinned boxed future, providing a concrete type
/// for the `Future` associated type in `UntypedCoreTask`. While it still uses
/// boxing internally (necessary for type erasure), the named type provides:
/// - Better error messages and stack traces
/// - A concrete type instead of `dyn Future`
/// - Clearer API boundaries
pub struct BytesFuture(Pin<Box<dyn Future<Output = Result<Bytes, BoxError>> + Send>>);

impl Future for BytesFuture {
    type Output = Result<Bytes, BoxError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.as_mut().poll(cx)
    }
}

impl BytesFuture {
    /// Create a new `BytesFuture` from any future that outputs `Result<Bytes>`.
    pub fn new<F>(fut: F) -> Self
    where
        F: Future<Output = Result<Bytes, BoxError>> + Send + 'static,
    {
        BytesFuture(Box::pin(fut))
    }
}

/// A boxed core task that can be used to run a task without knowing the input and output types.
///
/// Uses `BytesFuture` as the concrete future type, which internally boxes the future.
/// This boxing is necessary for type erasure when storing heterogeneous tasks.
pub type UntypedCoreTask =
    Box<dyn CoreTask<Input = Bytes, Output = Bytes, Future = BytesFuture> + Send + Sync>;

/// Implement `CoreTask<Bytes, Bytes>` for a struct that has `func` and `codec` fields.
///
/// The generated `run` method: decodes the input via the codec, calls the function,
/// and encodes the output back to `Bytes`.
macro_rules! impl_codec_task {
    (
        $wrapper:ident < $($gen:ident),+ >
        where $func_type:ty : Fn($input:ty) -> $fut_type:ty,
              $($bound:tt)+
    ) => {
        impl< $($gen),+ > CoreTask for $wrapper < $($gen),+ >
        where
            $func_type : Fn($input) -> $fut_type + Send + Sync + 'static,
            $($bound)+
        {
            type Input = Bytes;
            type Output = Bytes;
            type Future = BytesFuture;

            fn run(&self, input: Bytes) -> Self::Future {
                let func = Arc::clone(&self.func);
                let codec = Arc::clone(&self.codec);
                BytesFuture::new(async move {
                    let decoded_input = codec.decode::<$input>(input)?;
                    let output = func(decoded_input).await?;
                    codec.encode(&output)
                })
            }
        }
    };
}

/// Internal wrapper that implements `CoreTask<Input = Bytes, Output = Bytes>` for async functions.
struct UntypedTaskFnWrapper<F, I, O, Fut, C> {
    func: Arc<F>,
    codec: Arc<C>,
    _phantom: std::marker::PhantomData<fn(I) -> (O, Fut)>,
}

impl_codec_task!(
    UntypedTaskFnWrapper<F, I, O, Fut, C>
    where F: Fn(I) -> Fut,
          I: Send + 'static,
          O: Send + 'static,
          Fut: Future<Output = Result<O, BoxError>> + Send + 'static,
          C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
);

/// Create a new untyped task from any function using a codec.
///
/// The function must be Send + Sync + 'static and return a Future that resolves to a Result.
/// Both input and output types must be Send for type erasure to work.
/// The codec must be able to decode the input type and encode the output type.
pub fn to_core_task<F, I, O, Fut, C>(func: F, codec: Arc<C>) -> UntypedCoreTask
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: Send + 'static,
    O: Send + 'static,
    Fut: Future<Output = Result<O, BoxError>> + Send + 'static,
    C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
{
    to_core_task_arc(Arc::new(func), codec)
}

/// Create a new untyped task from an Arc-wrapped function.
///
/// This variant accepts an already-Arc'd function, avoiding the need
/// for the function to implement Clone.
pub fn to_core_task_arc<F, I, O, Fut, C>(func: Arc<F>, codec: Arc<C>) -> UntypedCoreTask
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: Send + 'static,
    O: Send + 'static,
    Fut: Future<Output = Result<O, BoxError>> + Send + 'static,
    C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
{
    Box::new(UntypedTaskFnWrapper {
        func,
        codec,
        _phantom: std::marker::PhantomData,
    })
}

/// A boxed async function for use in fork branches (internal).
type BoxedBranchFn<I, O> = Box<
    dyn Fn(I) -> std::pin::Pin<Box<dyn Future<Output = Result<O, BoxError>> + Send>> + Send + Sync,
>;

/// A branch for use with `fork()` (internal).
pub(crate) struct Branch<I, O> {
    id: String,
    func: BoxedBranchFn<I, O>,
}

/// Create a branch (internal helper used by `ForkBuilder`).
pub(crate) fn branch<F, Fut, I, O>(id: &str, f: F) -> Branch<I, O>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, BoxError>> + Send + 'static,
    I: 'static,
    O: 'static,
{
    Branch {
        id: id.to_string(),
        func: Box::new(move |i| Box::pin(f(i))),
    }
}

/// A type-erased branch for heterogeneous fork operations (internal).
pub(crate) struct ErasedBranch {
    pub(crate) id: String,
    pub(crate) task: UntypedCoreTask,
}

impl<I, O> Branch<I, O> {
    /// Convert this branch to a type-erased branch.
    ///
    /// This is used internally by `fork()` to allow heterogeneous output types.
    pub fn erase<C>(self, codec: Arc<C>) -> ErasedBranch
    where
        I: Send + 'static,
        O: Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
    {
        ErasedBranch {
            id: self.id.clone(),
            task: branch_to_core_task(self, codec),
        }
    }
}

/// Convert a Branch to an `UntypedCoreTask` (internal).
#[allow(clippy::items_after_statements)]
pub(crate) fn branch_to_core_task<I, O, C>(branch: Branch<I, O>, codec: Arc<C>) -> UntypedCoreTask
where
    I: Send + 'static,
    O: Send + 'static,
    C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
{
    // Wrap the boxed function in Arc so it can be cloned into the future
    let func = Arc::new(branch.func);

    struct ArcBranchWrapper<I, O, C> {
        func: Arc<BoxedBranchFn<I, O>>,
        codec: Arc<C>,
        _phantom: PhantomData<fn(I) -> O>,
    }

    impl_codec_task!(
        ArcBranchWrapper<I, O, C>
        where BoxedBranchFn<I, O>: Fn(I) -> std::pin::Pin<Box<dyn Future<Output = Result<O, BoxError>> + Send>>,
              I: Send + 'static,
              O: Send + 'static,
              C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
    );

    Box::new(ArcBranchWrapper {
        func,
        codec,
        _phantom: PhantomData,
    })
}

/// Join task wrapper for heterogeneous branch outputs.
///
/// This wrapper receives serialized named branch results and passes a
/// `BranchOutputs` map to the user function for type-safe access.
#[allow(clippy::type_complexity)]
struct HeterogeneousJoinTaskWrapper<F, JoinOutput, Fut, C> {
    func: Arc<F>,
    codec: Arc<C>,
    _phantom: PhantomData<fn(BranchOutputs<C>) -> (JoinOutput, Fut)>,
}

impl<F, JoinOutput, Fut, C> CoreTask for HeterogeneousJoinTaskWrapper<F, JoinOutput, Fut, C>
where
    F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
    JoinOutput: Send + 'static,
    Fut: Future<Output = Result<JoinOutput, BoxError>> + Send + 'static,
    C: Codec
        + sealed::EncodeValue<JoinOutput>
        + sealed::DecodeValue<NamedBranchResults>
        + Send
        + Sync
        + 'static,
{
    type Input = Bytes;
    type Output = Bytes;
    type Future = BytesFuture;

    fn run(&self, input: Bytes) -> Self::Future {
        let func = Arc::clone(&self.func);
        let codec = Arc::clone(&self.codec);
        BytesFuture::new(async move {
            let named_results: NamedBranchResults = codec.decode(input)?;
            let branch_outputs = BranchOutputs::new(named_results.into_map(), codec.clone());

            let output = func(branch_outputs).await?;
            codec.encode(&output)
        })
    }
}

/// Create a join task for heterogeneous branch outputs.
///
/// The join function receives `BranchOutputs<C>` which allows type-safe
/// retrieval of each branch's output by name.
///
/// # Example
///
/// ```rust,ignore
/// .join("combine", |outputs: BranchOutputs<MyCodec>| async move {
///     let count: u32 = outputs.get("counter")?;
///     let name: String = outputs.get("fetch_name")?;
///     Ok(format!("{} - {}", name, count))
/// })
/// ```
pub fn to_heterogeneous_join_task<F, JoinOutput, Fut, C>(func: F, codec: Arc<C>) -> UntypedCoreTask
where
    F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
    JoinOutput: Send + 'static,
    Fut: Future<Output = Result<JoinOutput, BoxError>> + Send + 'static,
    C: Codec
        + sealed::EncodeValue<JoinOutput>
        + sealed::DecodeValue<NamedBranchResults>
        + Send
        + Sync
        + 'static,
{
    to_heterogeneous_join_task_arc(Arc::new(func), codec)
}

/// Create a join task from an Arc-wrapped function.
///
/// This variant accepts an already-Arc'd function, avoiding the need
/// for the function to implement Clone.
pub fn to_heterogeneous_join_task_arc<F, JoinOutput, Fut, C>(
    func: Arc<F>,
    codec: Arc<C>,
) -> UntypedCoreTask
where
    F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
    JoinOutput: Send + 'static,
    Fut: Future<Output = Result<JoinOutput, BoxError>> + Send + 'static,
    C: Codec
        + sealed::EncodeValue<JoinOutput>
        + sealed::DecodeValue<NamedBranchResults>
        + Send
        + Sync
        + 'static,
{
    Box::new(HeterogeneousJoinTaskWrapper {
        func,
        codec,
        _phantom: PhantomData,
    })
}
