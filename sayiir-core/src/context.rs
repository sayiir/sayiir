//! Workflow execution context.
//!
//! [`WorkflowContext`] carries the workflow ID, codec, and user-supplied
//! metadata through every task execution.
//!
//! [`TaskExecutionContext`] provides read-only access to workflow and task
//! metadata from within running tasks. It is set automatically by the
//! runtime and can be retrieved via [`get_task_context()`] or the
//! [`task_context!`](crate::task_context) macro.

use std::sync::Arc;

use crate::task::TaskMetadata;

/// Execution context available to a running task.
///
/// Provides read-only access to workflow and task metadata. Accessible
/// from within task functions via task-local storage (Rust) or
/// language-specific context APIs (Python/Node.js).
#[derive(Clone, Debug)]
pub struct TaskExecutionContext {
    /// The workflow definition identifier.
    pub workflow_id: Arc<str>,
    /// The workflow instance identifier.
    pub instance_id: Arc<str>,
    /// The current task identifier.
    pub task_id: Arc<str>,
    /// Task metadata (timeout, retry policy, version, etc.).
    pub metadata: TaskMetadata,
    /// Optional JSON-encoded workflow-level metadata.
    pub workflow_metadata_json: Option<Arc<str>>,
}

/// Workflow execution context that provides access to metadata and codec.
///
/// This context is always available as a plain struct used during workflow
/// building and by the runner for codec/metadata access.
pub struct WorkflowContext<C, M> {
    /// The unique workflow identifier.
    pub workflow_id: Arc<str>,
    /// The codec used for serialization/deserialization.
    pub codec: Arc<C>,
    /// Immutable metadata attached to the workflow.
    pub metadata: Arc<M>,
    /// Optional JSON-encoded workflow-level metadata for task context.
    pub metadata_json: Option<Arc<str>>,
}

impl<C, M> Clone for WorkflowContext<C, M> {
    fn clone(&self) -> Self {
        Self {
            workflow_id: Arc::clone(&self.workflow_id),
            codec: Arc::clone(&self.codec),
            metadata: Arc::clone(&self.metadata),
            metadata_json: self.metadata_json.clone(),
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
            metadata_json: None,
        }
    }

    /// Returns the workflow identifier.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    /// Returns a clone of the codec `Arc`.
    #[must_use]
    pub fn codec(&self) -> Arc<C> {
        self.codec.clone()
    }

    /// Returns a clone of the metadata `Arc`.
    #[must_use]
    pub fn metadata(&self) -> Arc<M> {
        self.metadata.clone()
    }
}

use std::cell::RefCell;

std::thread_local! {
    /// Thread-local fallback for `TaskExecutionContext`.
    ///
    /// Used by sync executor paths (Python GIL, Node.js main thread) where
    /// tokio task-locals are not available.
    static THREAD_LOCAL_TASK_CTX: RefCell<Option<TaskExecutionContext>> = const { RefCell::new(None) };
}

/// Set the task execution context in thread-local storage for the duration
/// of the closure. Clears the context when the closure returns (even on panic).
pub fn with_thread_local_task_context<R>(ctx: TaskExecutionContext, f: impl FnOnce() -> R) -> R {
    THREAD_LOCAL_TASK_CTX.with(|cell| {
        let prev = cell.borrow_mut().replace(ctx);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        *cell.borrow_mut() = prev;
        match result {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        }
    })
}

/// Get the task execution context from thread-local storage.
#[must_use]
pub fn get_thread_local_task_context() -> Option<TaskExecutionContext> {
    THREAD_LOCAL_TASK_CTX.with(|cell| cell.borrow().clone())
}

// ── Task-local context storage (requires tokio) ─────────────────────────

#[cfg(feature = "tokio")]
mod task_local_ctx {
    use super::TaskExecutionContext;

    tokio::task_local! {
        /// Task-local storage for task execution context.
        static TASK_EXEC_CTX: Option<TaskExecutionContext>;
    }

    /// Set the task execution context in task-local storage and execute the future.
    pub async fn with_task_context<F: std::future::Future>(
        ctx: TaskExecutionContext,
        fut: F,
    ) -> F::Output {
        TASK_EXEC_CTX.scope(Some(ctx), fut).await
    }

    /// Get the task execution context from task-local storage.
    ///
    /// Tries the tokio task-local first, then falls back to the thread-local.
    #[must_use]
    pub fn get_task_context() -> Option<TaskExecutionContext> {
        TASK_EXEC_CTX
            .try_with(std::clone::Clone::clone)
            .ok()
            .flatten()
            .or_else(super::get_thread_local_task_context)
    }
}

#[cfg(feature = "tokio")]
pub use task_local_ctx::{get_task_context, with_task_context};

/// Get the task execution context (non-tokio fallback).
///
/// Delegates to thread-local storage only.
#[cfg(not(feature = "tokio"))]
#[must_use]
pub fn get_task_context() -> Option<TaskExecutionContext> {
    get_thread_local_task_context()
}

/// Macro to access the task execution context from within a task.
///
/// Returns `Option<TaskExecutionContext>` — `None` if called outside of
/// task execution context.
///
/// Usage:
/// ```rust,ignore
/// if let Some(ctx) = task_context!() {
///     println!("workflow: {}, task: {}", ctx.workflow_id, ctx.task_id);
/// }
/// ```
#[macro_export]
macro_rules! task_context {
    () => {
        $crate::context::get_task_context()
    };
}

#[cfg(all(test, feature = "tokio"))]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::task::TaskMetadata;

    fn make_task_ctx() -> TaskExecutionContext {
        TaskExecutionContext {
            workflow_id: Arc::from("wf-1"),
            instance_id: Arc::from("inst-1"),
            task_id: Arc::from("task-a"),
            metadata: TaskMetadata::default(),
            workflow_metadata_json: None,
        }
    }

    #[test]
    fn thread_local_roundtrip() {
        assert!(get_thread_local_task_context().is_none());

        let ctx = make_task_ctx();
        let result = with_thread_local_task_context(ctx.clone(), || {
            let inner = get_thread_local_task_context().unwrap();
            assert_eq!(&*inner.workflow_id, "wf-1");
            assert_eq!(&*inner.instance_id, "inst-1");
            assert_eq!(&*inner.task_id, "task-a");
            42
        });
        assert_eq!(result, 42);

        // Cleared after scope
        assert!(get_thread_local_task_context().is_none());
    }

    #[test]
    fn thread_local_restores_on_panic() {
        let ctx = make_task_ctx();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_thread_local_task_context(ctx, || {
                panic!("boom");
            })
        }));
        assert!(result.is_err());
        assert!(get_thread_local_task_context().is_none());
    }

    #[test]
    fn task_local_roundtrip() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(get_task_context().is_none());

            let ctx = make_task_ctx();
            let inner = with_task_context(ctx, async {
                let c = get_task_context().unwrap();
                assert_eq!(&*c.task_id, "task-a");
                c
            })
            .await;

            assert_eq!(&*inner.workflow_id, "wf-1");
        });
    }

    #[test]
    fn task_local_falls_back_to_thread_local() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // Set only thread-local, no task-local — should still find it
            let ctx = make_task_ctx();
            let result = with_thread_local_task_context(ctx, get_task_context);
            assert!(result.is_some());
            assert_eq!(&*result.unwrap().instance_id, "inst-1");
        });
    }

    #[test]
    fn macro_works() {
        let ctx = make_task_ctx();
        with_thread_local_task_context(ctx, || {
            let c = task_context!().unwrap();
            assert_eq!(&*c.task_id, "task-a");
        });
    }
}
