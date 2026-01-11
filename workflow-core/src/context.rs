use crate::codec::Codec;
use std::sync::Arc;
use tokio::task_local;

/// Workflow execution context that provides access to metadata and codec.
///
/// This context is stored in tokio task-local storage and is accessible
/// from within any task execution via the `sayiir_ctx!` macro.
///
pub struct WorkflowContext<C, M> {
    /// The unique workflow identifier.
    pub workflow_id: String,
    /// The codec used for serialization/deserialization.
    pub codec: Arc<C>,
    /// Immutable metadata attached to the workflow.
    pub metadata: Arc<M>,
}

impl<C, M> Clone for WorkflowContext<C, M> {
    fn clone(&self) -> Self {
        Self {
            workflow_id: self.workflow_id.clone(),
            codec: Arc::clone(&self.codec),
            metadata: Arc::clone(&self.metadata),
        }
    }
}

impl<C, M> WorkflowContext<C, M> {
    /// Create a new workflow context.
    pub fn new(workflow_id: impl Into<String>, codec: Arc<C>, metadata: Arc<M>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            codec,
            metadata,
        }
    }

    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    pub fn codec(&self) -> Arc<C> {
        self.codec.clone()
    }

    pub fn metadata(&self) -> Arc<M> {
        self.metadata.clone()
    }
}

/// Type-erased workflow context for task-local storage.
struct ErasedContext {
    inner: Arc<dyn std::any::Any + Send + Sync>,
}

impl ErasedContext {
    fn new<C, M>(ctx: WorkflowContext<C, M>) -> Self
    where
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(ctx) as Arc<dyn std::any::Any + Send + Sync>,
        }
    }

    fn downcast<C, M>(&self) -> Option<WorkflowContext<C, M>>
    where
        C: Codec + 'static,
        M: Send + Sync + 'static,
    {
        self.inner
            .clone()
            .downcast::<WorkflowContext<C, M>>()
            .ok()
            .map(|arc| {
                WorkflowContext::new(
                    arc.workflow_id.clone(),
                    Arc::clone(&arc.codec),
                    Arc::clone(&arc.metadata),
                )
            })
    }
}

task_local! {
    /// Task-local storage for workflow context.
    static WORKFLOW_CTX: Option<ErasedContext>;
}

/// Set the workflow context in task-local storage and execute the future.
///
/// This should be called by the runner before executing tasks.
pub async fn with_context<C, M, F, Fut>(ctx: WorkflowContext<C, M>, f: F) -> Fut::Output
where
    C: Codec + 'static,
    M: Send + Sync + 'static,
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    WORKFLOW_CTX.scope(Some(ErasedContext::new(ctx)), f()).await
}

/// Get the workflow context from task-local storage.
///
/// This is used internally by the `sayiir_ctx!` macro.
pub fn get_context<C, M>() -> Option<WorkflowContext<C, M>>
where
    C: Codec + 'static,
    M: Send + Sync + 'static,
{
    WORKFLOW_CTX
        .try_with(|ctx_opt| ctx_opt.as_ref()?.downcast())
        .ok()
        .flatten()
}

/// Macro to access the workflow context from within a task.
///
/// Returns `Option<WorkflowContext<C, M>>` - `None` if called outside of workflow execution context.
///
/// Usage:
/// ```rust,ignore
/// if let Some(ctx) = sayiir_ctx!() {
///     let metadata = ctx.metadata();
///     let codec = ctx.codec();
/// }
/// ```
#[macro_export]
macro_rules! sayiir_ctx {
    () => {
        $crate::context::get_context()
    };
}
