use workflow_core::codec::Codec;
use workflow_core::codec::sealed;
use workflow_core::workflow::{Workflow, WorkflowStatus};

/// A trait for executing workflows.
///
/// Different implementations can provide different execution strategies,
/// such as in-process execution, distributed execution, or execution with
/// persistence and recovery.
pub trait WorkflowRunner: Send + Sync {
    /// Run a workflow with the given input.
    ///
    /// The input type must match the input type of the first task added via `then`.
    /// Returns the workflow execution status.
    fn run<'w, C, Input, M>(
        &self,
        workflow: &'w Workflow<C, Input, M>,
        input: Input,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<WorkflowStatus>> + Send + 'w>,
    >
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::EncodeValue<Input>;
}

pub mod in_process;
