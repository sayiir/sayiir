use crate::codec::{Codec, sealed};
use anyhow::Result;
use bytes::Bytes;
use std::future::Future;
use std::io::Read;
use std::marker::PhantomData;
use std::sync::Arc;

/// Deserialize branch results from length-prefixed format.
///
/// Format:
/// - 4 bytes: number of branches (u32, little-endian)
/// - For each branch:
///   - 4 bytes: length of branch result (u32, little-endian)
///   - N bytes: branch result data
pub fn deserialize_branch_results(bytes: Bytes) -> Result<Vec<Bytes>> {
    let mut reader = bytes.as_ref();
    let mut results = Vec::new();

    // Read number of branches
    let mut branch_count_bytes = [0u8; 4];
    reader.read_exact(&mut branch_count_bytes)?;
    let branch_count = u32::from_le_bytes(branch_count_bytes) as usize;

    // Read each branch result
    for _ in 0..branch_count {
        // Read length
        let mut length_bytes = [0u8; 4];
        reader.read_exact(&mut length_bytes)?;
        let length = u32::from_le_bytes(length_bytes) as usize;

        // Read data
        let mut data = vec![0u8; length];
        reader.read_exact(&mut data)?;
        results.push(Bytes::from(data));
    }

    Ok(results)
}

/// A core task is a task that can be run by the workflow runtime.
///
pub trait CoreTask: Send + Sync {
    type Input;
    type Output;
    type Future: Future<Output = Result<Self::Output>> + Send;

    /// Run the task with the given input and return the output.
    fn run(&self, input: Self::Input) -> Self::Future;
}

/// A boxed core task that can be used to run a task without knowing the input and output types.
pub type UntypedCoreTask = Box<
    dyn CoreTask<
            Input = Bytes,
            Output = Bytes,
            Future = std::pin::Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>,
        > + Send
        + Sync,
>;

/// Internal wrapper that implements `CoreTask<Input = Bytes, Output = Bytes>` for async functions.
struct UntypedTaskFnWrapper<F, I, O, Fut, C> {
    func: Arc<F>,
    codec: Arc<C>,
    _phantom: std::marker::PhantomData<fn(I) -> (O, Fut)>,
}

impl<F, I, O, Fut, C> CoreTask for UntypedTaskFnWrapper<F, I, O, Fut, C>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: Send + 'static,
    O: Send + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
    C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
{
    type Input = Bytes;
    type Output = Bytes;
    type Future = std::pin::Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>;

    fn run(&self, input: Bytes) -> Self::Future {
        let func = Arc::clone(&self.func);
        let codec = Arc::clone(&self.codec);
        Box::pin(async move {
            let decoded_input = codec.decode::<I>(input)?;
            let output = func(decoded_input).await?;
            codec.encode(&output)
        })
    }
}

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
    Fut: Future<Output = Result<O>> + Send + 'static,
    C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
{
    Box::new(UntypedTaskFnWrapper {
        func: Arc::new(func),
        codec,
        _phantom: std::marker::PhantomData,
    })
}

/// A boxed async function for use in fork branches.
///
/// This type alias enables heterogeneous branch closures in `fork()`.
pub type BoxedBranchFn<I, O> = Box<
    dyn Fn(I) -> std::pin::Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync,
>;

/// A named branch for use with `fork()`.
pub struct Branch<I, O> {
    /// The name of the branch.
    pub name: String,
    /// The boxed async function to execute.
    pub func: BoxedBranchFn<I, O>,
}

/// Create a named branch for use with `fork()`.
///
/// This helper boxes the closure to allow different closures in the same fork.
///
/// # Example
///
/// ```rust,ignore
/// workflow
///     .fork(vec![
///         branch("double", |i: i32| async move { Ok(i * 2) }),
///         branch("triple", |i: i32| async move { Ok(i * 3) }),
///         branch("square", |i: i32| async move { Ok(i * i) }),
///     ])
///     .join("combine", |results: Vec<i32>| async move {
///         Ok(results.into_iter().sum())
///     })
/// ```
pub fn branch<F, Fut, I, O>(name: &str, f: F) -> Branch<I, O>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
    I: 'static,
    O: 'static,
{
    Branch {
        name: name.to_string(),
        func: Box::new(move |i| Box::pin(f(i))),
    }
}

/// Convert a Branch to an UntypedCoreTask.
pub fn branch_to_core_task<I, O, C>(branch: Branch<I, O>, codec: Arc<C>) -> UntypedCoreTask
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

    impl<I, O, C> CoreTask for ArcBranchWrapper<I, O, C>
    where
        I: Send + 'static,
        O: Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
    {
        type Input = Bytes;
        type Output = Bytes;
        type Future = std::pin::Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>;

        fn run(&self, input: Bytes) -> Self::Future {
            let codec = Arc::clone(&self.codec);
            let func = Arc::clone(&self.func);
            Box::pin(async move {
                let decoded_input = codec.decode::<I>(input)?;
                let output = func(decoded_input).await?;
                codec.encode(&output)
            })
        }
    }

    Box::new(ArcBranchWrapper {
        func,
        codec,
        _phantom: PhantomData,
    })
}

/// Typed join wrapper that deserializes branch results automatically.
///
/// This wrapper receives serialized branch results, deserializes each to the
/// expected `BranchOutput` type, and passes a `Vec<BranchOutput>` to the user function.
// Type alias to reduce complexity for clippy
type TypedJoinPhantom<BranchOutput, JoinOutput, Fut> =
    PhantomData<fn(Vec<BranchOutput>) -> (JoinOutput, Fut)>;

struct TypedJoinTaskWrapper<F, BranchOutput, JoinOutput, Fut, C> {
    func: Arc<F>,
    codec: Arc<C>,
    _phantom: TypedJoinPhantom<BranchOutput, JoinOutput, Fut>,
}

impl<F, BranchOutput, JoinOutput, Fut, C> CoreTask
    for TypedJoinTaskWrapper<F, BranchOutput, JoinOutput, Fut, C>
where
    F: Fn(Vec<BranchOutput>) -> Fut + Send + Sync + 'static,
    BranchOutput: Send + 'static,
    JoinOutput: Send + 'static,
    Fut: Future<Output = Result<JoinOutput>> + Send + 'static,
    C: Codec + sealed::DecodeValue<BranchOutput> + sealed::EncodeValue<JoinOutput>,
{
    type Input = Bytes;
    type Output = Bytes;
    type Future = std::pin::Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>;

    fn run(&self, input: Bytes) -> Self::Future {
        let func = Arc::clone(&self.func);
        let codec = Arc::clone(&self.codec);
        Box::pin(async move {
            let branch_bytes = deserialize_branch_results(input)?;

            // Decode each branch result to typed value
            let typed_results: Vec<BranchOutput> = branch_bytes
                .into_iter()
                .map(|b| codec.decode::<BranchOutput>(b))
                .collect::<Result<Vec<_>>>()?;

            let output = func(typed_results).await?;
            codec.encode(&output)
        })
    }
}

/// Create a typed join task that automatically deserializes branch results.
///
/// The join function receives `Vec<BranchOutput>` instead of raw `Bytes`,
/// providing type safety at the fork-join boundary.
pub fn to_typed_join_task<F, BranchOutput, JoinOutput, Fut, C>(
    func: F,
    codec: Arc<C>,
) -> UntypedCoreTask
where
    F: Fn(Vec<BranchOutput>) -> Fut + Send + Sync + 'static,
    BranchOutput: Send + 'static,
    JoinOutput: Send + 'static,
    Fut: Future<Output = Result<JoinOutput>> + Send + 'static,
    C: Codec + sealed::DecodeValue<BranchOutput> + sealed::EncodeValue<JoinOutput>,
{
    Box::new(TypedJoinTaskWrapper {
        func: Arc::new(func),
        codec,
        _phantom: PhantomData,
    })
}
