use crate::serialization::{TaskInput, TaskOutput};
use anyhow::Result;
use bytes::Bytes;
use std::future::Future;
use std::sync::Arc;

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A core task is a task that can be run by the workflow runtime.
///
pub trait CoreTask: Send + Sync {
    fn run(&self, input: Bytes) -> BoxFuture<'static, Result<Bytes>>;
}

/// Internal wrapper that implements CoreTask for async functions
struct TaskFnWrapper<F, I, O, Fut> {
    func: Arc<F>,
    _phantom: std::marker::PhantomData<fn(I) -> (O, Fut)>,
}

impl<F, I, O, Fut> TaskFnWrapper<F, I, O, Fut> {
    fn new(func: F) -> Self {
        Self {
            func: Arc::new(func),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<F, I, O, Fut, E> CoreTask for TaskFnWrapper<F, I, O, Fut>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: TaskInput,
    O: TaskOutput,
    Fut: Future<Output = std::result::Result<O, E>> + Send + 'static,
    E: Into<anyhow::Error>,
{
    fn run(&self, input: Bytes) -> BoxFuture<'static, Result<Bytes>> {
        let func = Arc::clone(&self.func);
        let fut = async move {
            let input_value = I::from_bytes(input)?;
            let output = func(input_value).await.map_err(Into::into)?;
            output.to_bytes()
        };
        Box::pin(fut)
    }
}

/// Create a CoreTask from any async function.
///
/// The function can return any `Result<O, E>` where `E` can be converted to `anyhow::Error`.
///
/// # Example
/// ```
/// use workflow_core::task::{CoreTask, task};
/// use workflow_core::serialization::json::Json;
/// use workflow_core::serialization::TaskInput;
/// use bytes::Bytes;
///
/// async fn async_function(input: Json<String>) -> std::result::Result<Json<i32>, anyhow::Error> {
///     let len = input.as_ref().len() as i32;
///     // Construct Json using from_bytes (in real usage, this comes from task input)
///     let json_bytes = Bytes::from(serde_json::to_vec(&len)?);
///     Ok(Json::from_bytes(json_bytes)?)
/// }
/// let _task = task(async_function);
/// ```
pub fn task<F, I, O, Fut, E>(func: F) -> impl CoreTask
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: TaskInput,
    O: TaskOutput,
    Fut: Future<Output = std::result::Result<O, E>> + Send + 'static,
    E: Into<anyhow::Error>,
{
    TaskFnWrapper::new(func)
}
