use crate::codec::Codec;
use crate::codec::sealed;
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

pub struct WorkflowBuilder<C, Input, Output> {
    codec: Arc<C>,
    continuation: Option<WorkflowContinuation>,
    _phantom: PhantomData<(Input, Output)>,
}

/// A built workflow that can be executed.
pub struct Workflow<C, Input> {
    codec: Arc<C>,
    continuation: WorkflowContinuation,
    _phantom: PhantomData<Input>,
}

impl<C, Input, Output> WorkflowBuilder<C, Input, Output> {
    pub fn with_codec(codec: C) -> Self
    where
        C: Codec,
    {
        Self {
            codec: Arc::new(codec),
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
        let task = to_core_task(func, Arc::clone(&self.codec));

        Self {
            continuation: Some(WorkflowContinuation::Task {
                name: name.to_string(),
                func: task,
                next: self.continuation.map(Box::new),
            }),
            codec: self.codec,
            _phantom: PhantomData,
        }
    }

    /// Build the workflow into an executable workflow.
    ///
    /// # Panics
    ///
    /// Panics if no tasks have been added to the workflow (i.e., `then` was never called).
    pub fn build(self) -> Workflow<C, Input>
    where
        Input: Send + 'static,
        Output: Send + 'static,
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

            codec: self.codec,
            _phantom: PhantomData,
        }
    }
}

impl<C, Input> Workflow<C, Input> {
    /// Get a reference to the codec used by this workflow.
    pub fn codec(&self) -> &Arc<C> {
        &self.codec
    }

    /// Get a reference to the continuation of this workflow.
    pub fn continuation(&self) -> &WorkflowContinuation {
        &self.continuation
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
        let workflow = WorkflowBuilder::with_codec(DummyCodec)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build();

        // Verify the workflow was built successfully
        // The workflow can be executed using a WorkflowRunner from workflow-runtime
        let _workflow_ref = &workflow;
    }
}
