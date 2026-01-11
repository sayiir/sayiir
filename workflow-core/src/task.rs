use anyhow::Result;
use bytes::Bytes;
use std::future::Future;
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

/// Internal wrapper that implements `CoreTask` for async functions.
struct TaskFnWrapper<F, I, O, Fut> {
    func: Arc<F>,
    _phantom: std::marker::PhantomData<fn(I) -> (O, Fut)>,
}

impl<F, I, O, Fut, E> CoreTask for TaskFnWrapper<F, I, O, Fut>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: Send + 'static,
    O: Send + 'static,
    Fut: Future<Output = std::result::Result<O, E>> + Send + 'static,
    E: Into<anyhow::Error>,
{
    type Input = I;
    type Output = O;
    type Future = std::pin::Pin<Box<dyn Future<Output = Result<O>> + Send>>;

    fn run(&self, input: I) -> Self::Future {
        let func = Arc::clone(&self.func);
        Box::pin(async move { func(input).await.map_err(Into::into) })
    }
}

/// Create a new task from any function.
///
/// The function must be Send + Sync + 'static and return a Future that resolves to a Result.
/// Both input and output types must be Send for type erasure to work.
pub fn task<F, I, O, Fut>(func: F) -> impl CoreTask<Input = I, Output = O>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: Send + 'static,
    O: Send + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
{
    TaskFnWrapper {
        func: Arc::new(func),
        _phantom: std::marker::PhantomData,
    }
}
