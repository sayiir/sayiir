use super::WorkflowRunner;
use crate::execution::execute_continuation_async;
use workflow_core::codec::Codec;
use workflow_core::codec::sealed;
use workflow_core::context::with_context;
use workflow_core::workflow::{Workflow, WorkflowStatus};

/// A workflow runner that executes workflows in-process.
///
/// This is an in-process implementation that executes workflows synchronously
/// by following the continuation chain.
///
/// # Example
///
/// ```rust,no_run
/// # use workflow_runtime::{InProcessRunner, WorkflowRunner};
/// # use workflow_core::workflow::WorkflowBuilder;
/// # use workflow_core::context::WorkflowContext;
/// # use workflow_runtime::serialization::RkyvCodec;
/// # use std::sync::Arc;
/// # async fn example() -> anyhow::Result<()> {
/// let ctx = WorkflowContext::new("my-workflow", Arc::new(RkyvCodec), Arc::new(()));
/// let workflow = WorkflowBuilder::new(ctx)
///     .then("test", |i: u32| async move { Ok(i + 1) })
///     .build()?;
/// let runner = InProcessRunner::default();
/// let status = runner.run(&workflow, 1).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct InProcessRunner;

impl WorkflowRunner for InProcessRunner {
    fn run<'w, C, Input, M>(
        &self,
        workflow: &'w Workflow<C, Input, M>,
        input: Input,
    ) -> impl std::future::Future<Output = anyhow::Result<WorkflowStatus>> + Send + 'w
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::EncodeValue<Input>,
    {
        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let codec = context.codec.clone();
        async move {
            with_context(context, || async move {
                let input_bytes = codec.encode(&input)?;
                match execute_continuation_async(continuation, input_bytes).await {
                    Ok(_) => Ok(WorkflowStatus::Completed),
                    Err(e) => Ok(WorkflowStatus::Failed(e)),
                }
            })
            .await
        }
    }
}
