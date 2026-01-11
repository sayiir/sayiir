use crate::codec::Codec;
use crate::codec::sealed;
use crate::context::WorkflowContext;
use crate::task::{UntypedCoreTask, to_core_task};
use bytes::Bytes;
use std::marker::PhantomData;
use std::sync::Arc;

/// A continuation is a value that can be used to resume a workflow.
pub enum WorkflowContinuation {
    Done(Bytes),
    Task {
        name: String,
        func: UntypedCoreTask,
        next: Option<Box<WorkflowContinuation>>,
    },
    Fork {
        branches: Box<[WorkflowContinuation]>,
        join: Option<Box<WorkflowContinuation>>,
    },
}

/// The status of a workflow execution.
#[derive(Debug)]
pub enum WorkflowStatus {
    /// The workflow completed successfully.
    Completed,
    /// The workflow failed with an error.
    Failed(anyhow::Error),
}

pub struct WorkflowBuilder<C, Input, Output, M = ()> {
    context: Option<WorkflowContext<C, M>>,
    continuation: Option<WorkflowContinuation>,
    _phantom: PhantomData<(Input, Output)>,
}

/// A built workflow that can be executed.
pub struct Workflow<C, Input, M = ()> {
    context: WorkflowContext<C, M>,
    continuation: WorkflowContinuation,
    _phantom: PhantomData<Input>,
}

impl<C, Input, Output, M> WorkflowBuilder<C, Input, Output, M> {
    /// Create a new workflow builder with a context object.
    ///
    /// The context contains both the codec and metadata that will be available
    /// at any execution point via the `sayiir_ctx!` macro.
    pub fn new(ctx: WorkflowContext<C, M>) -> Self
    where
        C: Codec,
        M: Send + Sync + 'static,
    {
        Self {
            context: Some(ctx),
            continuation: None,
            _phantom: PhantomData,
        }
    }

    pub fn then<F, Fut>(self, name: &str, func: F) -> Self
    where
        F: Fn(Input) -> Fut + Send + Sync + 'static,
        Input: Send + 'static,
        Output: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<Output>> + Send + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<Output>,
    {
        let codec = Arc::clone(&self.context.as_ref().expect("Context must be set").codec);
        let task = to_core_task(func, codec);

        let new_task = WorkflowContinuation::Task {
            name: name.to_string(),
            func: task,
            next: None,
        };

        let continuation = match self.continuation {
            Some(mut existing) => {
                Self::append_to_chain(&mut existing, new_task);
                Some(existing)
            }
            None => Some(new_task),
        };

        Self {
            continuation,
            context: self.context,
            _phantom: PhantomData,
        }
    }

    /// Append a new task to the end of the continuation chain.
    fn append_to_chain(continuation: &mut WorkflowContinuation, new_task: WorkflowContinuation) {
        match continuation {
            WorkflowContinuation::Task { next, .. } => {
                match next {
                    Some(next_box) => {
                        // Recursively find the end of the chain
                        Self::append_to_chain(next_box, new_task);
                    }
                    None => {
                        // This is the last task, append the new task here
                        *next = Some(Box::new(new_task));
                    }
                }
            }
            WorkflowContinuation::Done(_) => {
                // Replace Done with the new task
                *continuation = new_task;
            }
            WorkflowContinuation::Fork { join, .. } => {
                // If there's a join continuation, append to it
                // Otherwise, replace the fork with the new task
                match join {
                    Some(join_box) => {
                        Self::append_to_chain(join_box, new_task);
                    }
                    None => {
                        *continuation = new_task;
                    }
                }
            }
        }
    }

    /// Build the workflow into an executable workflow.
    ///
    /// # Panics
    ///
    /// Panics if no tasks have been added to the workflow (i.e., `then` was never called).
    pub fn build(self) -> Workflow<C, Input, M>
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
        Workflow {
            continuation: self
                .continuation
                .expect("Workflow must have at least one task"),
            context: self
                .context
                .expect("Context must be set when using WorkflowBuilder::new"),
            _phantom: PhantomData,
        }
    }
}

impl<C, Input, M> Workflow<C, Input, M> {
    /// Get a reference to the context of this workflow.
    pub fn context(&self) -> &WorkflowContext<C, M> {
        &self.context
    }

    /// Get a reference to the codec used by this workflow.
    pub fn codec(&self) -> &Arc<C> {
        &self.context.codec
    }

    /// Get a reference to the continuation of this workflow.
    pub fn continuation(&self) -> &WorkflowContinuation {
        &self.continuation
    }

    /// Get a reference to the metadata attached to this workflow.
    pub fn metadata(&self) -> &Arc<M> {
        &self.context.metadata
    }
}

#[cfg(test)]
mod tests {
    use crate::codec::{Decoder, Encoder, sealed};
    use crate::workflow::WorkflowBuilder;
    use anyhow::Result;
    use bytes::Bytes;

    struct DummyCodec;

    impl Encoder for DummyCodec {}
    impl Decoder for DummyCodec {}

    impl<Input> sealed::EncodeValue<Input> for DummyCodec {
        fn encode_value(&self, _value: &Input) -> Result<Bytes> {
            Ok(Bytes::new())
        }
    }
    impl<Output> sealed::DecodeValue<Output> for DummyCodec {
        fn decode_value(&self, _bytes: Bytes) -> Result<Output> {
            Err(anyhow::anyhow!("Not implemented"))
        }
    }

    #[test]
    fn test_workflow_build() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build();

        // Verify the workflow was built successfully
        // The workflow can be executed using a WorkflowRunner from workflow-runtime
        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_workflow_with_metadata() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new("test_metadata"));
        let workflow = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build();

        assert_eq!(**workflow.metadata(), "test_metadata");
    }

    #[test]
    fn test_task_order() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("first", |i: u32| async move { Ok(i + 1) })
            .then("second", |i: u32| async move { Ok(i + 2) })
            .then("third", |i: u32| async move { Ok(i + 3) })
            .build();

        // Verify the continuation chain structure
        // Tasks should be linked in order: first -> second -> third
        let mut current = workflow.continuation();
        let mut task_names = Vec::new();

        loop {
            match current {
                crate::workflow::WorkflowContinuation::Task { name, next, .. } => {
                    task_names.push(name.clone());
                    match next {
                        Some(next_box) => current = next_box.as_ref(),
                        None => break,
                    }
                }
                _ => break,
            }
        }

        // Tasks should execute in the order they were added
        assert_eq!(task_names, vec!["first", "second", "third"]);
    }
}
