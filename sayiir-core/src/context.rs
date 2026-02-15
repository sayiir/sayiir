use std::sync::Arc;

/// Workflow execution context that provides access to metadata and codec.
///
/// This context is always available as a plain struct. When the `tokio` feature
/// is enabled, it can also be stored in task-local storage and retrieved via
/// the [`sayiir_ctx!`] macro.
pub struct WorkflowContext<C, M> {
    /// The unique workflow identifier.
    pub workflow_id: Arc<str>,
    /// The codec used for serialization/deserialization.
    pub codec: Arc<C>,
    /// Immutable metadata attached to the workflow.
    pub metadata: Arc<M>,
}

impl<C, M> Clone for WorkflowContext<C, M> {
    fn clone(&self) -> Self {
        Self {
            workflow_id: Arc::clone(&self.workflow_id),
            codec: Arc::clone(&self.codec),
            metadata: Arc::clone(&self.metadata),
        }
    }
}

impl<C, M> WorkflowContext<C, M> {
    /// Create a new workflow context.
    pub fn new(workflow_id: impl Into<Arc<str>>, codec: Arc<C>, metadata: Arc<M>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            codec,
            metadata,
        }
    }

    #[must_use]
    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    #[must_use]
    pub fn codec(&self) -> Arc<C> {
        self.codec.clone()
    }

    #[must_use]
    pub fn metadata(&self) -> Arc<M> {
        self.metadata.clone()
    }
}

// ── Task-local context storage (requires tokio) ─────────────────────────

#[cfg(feature = "tokio")]
mod task_local_ctx {
    use super::{Arc, WorkflowContext};
    use crate::codec::Codec;

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
                        Arc::clone(&arc.workflow_id),
                        Arc::clone(&arc.codec),
                        Arc::clone(&arc.metadata),
                    )
                })
        }
    }

    tokio::task_local! {
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
    #[must_use]
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
}

#[cfg(feature = "tokio")]
pub use task_local_ctx::{get_context, with_context};

/// Macro to access the workflow context from within a task.
///
/// Requires the `tokio` feature. Returns `Option<WorkflowContext<C, M>>` —
/// `None` if called outside of workflow execution context.
///
/// Usage:
/// ```rust,ignore
/// if let Some(ctx) = sayiir_ctx!() {
///     let metadata = ctx.metadata();
///     let codec = ctx.codec();
/// }
/// ```
#[cfg(feature = "tokio")]
#[macro_export]
macro_rules! sayiir_ctx {
    () => {
        $crate::context::get_context()
    };
}
