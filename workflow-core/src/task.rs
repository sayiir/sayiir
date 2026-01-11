use crate::codec::{Codec, sealed};
use anyhow::Result;
use bytes::Bytes;
use std::collections::HashMap;
use std::future::Future;
use std::io::Read;
use std::marker::PhantomData;
use std::sync::Arc;

/// Deserialize named branch results from length-prefixed format.
///
/// Format:
/// - 4 bytes: number of branches (u32, little-endian)
/// - For each branch:
///   - 4 bytes: name length (u32, little-endian)
///   - N bytes: name (UTF-8)
///   - 4 bytes: data length (u32, little-endian)
///   - M bytes: data
pub fn deserialize_named_branch_results(bytes: Bytes) -> Result<HashMap<String, Bytes>> {
    let mut reader = bytes.as_ref();
    let mut results = HashMap::new();

    // Read number of branches
    let mut branch_count_bytes = [0u8; 4];
    reader.read_exact(&mut branch_count_bytes)?;
    let branch_count = u32::from_le_bytes(branch_count_bytes) as usize;

    // Read each branch result
    for _ in 0..branch_count {
        // Read name length
        let mut name_len_bytes = [0u8; 4];
        reader.read_exact(&mut name_len_bytes)?;
        let name_len = u32::from_le_bytes(name_len_bytes) as usize;

        // Read name
        let mut name_bytes = vec![0u8; name_len];
        reader.read_exact(&mut name_bytes)?;
        let name = String::from_utf8(name_bytes)?;

        // Read data length
        let mut data_len_bytes = [0u8; 4];
        reader.read_exact(&mut data_len_bytes)?;
        let data_len = u32::from_le_bytes(data_len_bytes) as usize;

        // Read data
        let mut data = vec![0u8; data_len];
        reader.read_exact(&mut data)?;
        results.insert(name, Bytes::from(data));
    }

    Ok(results)
}

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
    /// Create a new BranchOutputs from raw data.
    pub fn new(outputs: HashMap<String, Bytes>, codec: Arc<C>) -> Self {
        Self { outputs, codec }
    }

    /// Get the names of all branches.
    pub fn branch_names(&self) -> impl Iterator<Item = &str> {
        self.outputs.keys().map(|s| s.as_str())
    }

    /// Check if a branch exists.
    pub fn contains(&self, name: &str) -> bool {
        self.outputs.contains_key(name)
    }

    /// Get the number of branches.
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    /// Check if there are no branches.
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }
}

impl<C: Codec> BranchOutputs<C> {
    /// Get a branch output by name, deserializing to the requested type.
    ///
    /// Returns an error if the branch doesn't exist or deserialization fails.
    pub fn get<T>(&self, name: &str) -> Result<T>
    where
        C: sealed::DecodeValue<T>,
    {
        let bytes = self
            .outputs
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Branch '{}' not found", name))?;

        self.codec.decode(bytes.clone())
    }
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

/// A boxed async function for use in fork branches (internal).
type BoxedBranchFn<I, O> =
    Box<dyn Fn(I) -> std::pin::Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync>;

/// A branch for use with `fork()` (internal).
pub(crate) struct Branch<I, O> {
    id: String,
    func: BoxedBranchFn<I, O>,
}

/// Create a branch (internal helper used by ForkBuilder).
pub(crate) fn branch<F, Fut, I, O>(id: &str, f: F) -> Branch<I, O>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
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

/// Convert a Branch to an UntypedCoreTask (internal).
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
    Fut: Future<Output = Result<JoinOutput>> + Send + 'static,
    C: Codec + sealed::EncodeValue<JoinOutput> + Send + Sync + 'static,
{
    type Input = Bytes;
    type Output = Bytes;
    type Future = std::pin::Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>;

    fn run(&self, input: Bytes) -> Self::Future {
        let func = Arc::clone(&self.func);
        let codec = Arc::clone(&self.codec);
        Box::pin(async move {
            let named_results = deserialize_named_branch_results(input)?;
            let branch_outputs = BranchOutputs::new(named_results, codec.clone());

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
    Fut: Future<Output = Result<JoinOutput>> + Send + 'static,
    C: Codec + sealed::EncodeValue<JoinOutput> + Send + Sync + 'static,
{
    Box::new(HeterogeneousJoinTaskWrapper {
        func: Arc::new(func),
        codec,
        _phantom: PhantomData,
    })
}
