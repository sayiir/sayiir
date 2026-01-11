use super::WorkflowRunner;
use bytes::Bytes;
use workflow_core::codec::Codec;
use workflow_core::codec::sealed;
use workflow_core::context::with_context;
use workflow_core::workflow::{Workflow, WorkflowContinuation, WorkflowStatus};

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
/// let ctx = WorkflowContext::new(Arc::new(RkyvCodec), Arc::new(()));
/// let workflow = WorkflowBuilder::new(ctx)
///     .then("test", |i: u32| async move { Ok(i + 1) })
///     .build();
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
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<WorkflowStatus>> + Send + 'w>,
    >
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::EncodeValue<Input>,
    {
        let context = workflow.context().clone();
        let continuation = workflow.continuation();
        let codec = context.codec.clone();
        Box::pin(async move {
            with_context(context, || async move {
                let input_bytes = codec.encode(&input)?;
                match Self::execute_continuation(continuation, input_bytes).await {
                    Ok(_) => Ok(WorkflowStatus::Completed),
                    Err(e) => Ok(WorkflowStatus::Failed(e)),
                }
            })
            .await
        })
    }
}

impl InProcessRunner {
    fn execute_continuation(
        continuation: &WorkflowContinuation,
        input: Bytes,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Bytes>> + Send + '_>>
    {
        Box::pin(async move {
            match continuation {
                WorkflowContinuation::Done(bytes) => Ok(bytes.clone()),
                WorkflowContinuation::Task { func, next, .. } => {
                    let output = func.run(input).await?;
                    match next {
                        Some(next_continuation) => {
                            Self::execute_continuation(next_continuation, output).await
                        }
                        None => Ok(output),
                    }
                }
                WorkflowContinuation::Fork { .. } => {
                    // TODO: Implement fork execution
                    Err(anyhow::anyhow!("Fork not yet implemented"))
                }
            }
        })
    }
}
