use crate::codec::{Codec, sealed};
use anyhow::Result;
use bytes::Bytes;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

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

/// Special wrapper for join tasks that receive Bytes directly (passthrough).
struct JoinTaskWrapper<F, O, Fut, C> {
    func: Arc<F>,
    codec: Arc<C>,
    _phantom: PhantomData<fn(Bytes) -> (O, Fut)>,
}

impl<F, O, Fut, C> CoreTask for JoinTaskWrapper<F, O, Fut, C>
where
    F: Fn(Bytes) -> Fut + Send + Sync + 'static,
    O: Send + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
    C: Codec + sealed::EncodeValue<O>,
{
    type Input = Bytes;
    type Output = Bytes;
    type Future = std::pin::Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>;

    fn run(&self, input: Bytes) -> Self::Future {
        let func = Arc::clone(&self.func);
        let codec = Arc::clone(&self.codec);
        Box::pin(async move {
            // Pass Bytes through without decoding
            let output = func(input).await?;
            codec.encode(&output)
        })
    }
}

/// Create a join task that receives Bytes directly (for fork/join operations).
///
/// Unlike `to_core_task`, this doesn't decode the input since Bytes is already
/// the serialized form (from fork branch results).
pub fn to_join_task<F, O, Fut, C>(func: F, codec: Arc<C>) -> UntypedCoreTask
where
    F: Fn(Bytes) -> Fut + Send + Sync + 'static,
    O: Send + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
    C: Codec + sealed::EncodeValue<O>,
{
    Box::new(JoinTaskWrapper {
        func: Arc::new(func),
        codec,
        _phantom: PhantomData,
    })
}
