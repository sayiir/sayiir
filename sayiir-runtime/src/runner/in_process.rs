use super::WorkflowRunner;
use crate::execution::execute_continuation_async;
use sayiir_core::codec::Codec;
use sayiir_core::codec::sealed;
use sayiir_core::context::with_context;
use sayiir_core::workflow::{Workflow, WorkflowStatus};

/// A workflow runner that executes workflows in-process.
///
/// This is an in-process implementation that executes workflows synchronously
/// by following the continuation chain.
///
/// # Example
///
/// ```rust,no_run
/// # use sayiir_runtime::{InProcessRunner, WorkflowRunner};
/// # use sayiir_core::workflow::WorkflowBuilder;
/// # use sayiir_core::context::WorkflowContext;
/// # use sayiir_runtime::serialization::RkyvCodec;
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use crate::serialization::JsonCodec;
    use std::sync::Arc;
    use sayiir_core::context::WorkflowContext;
    use sayiir_core::task::BranchOutputs;
    use sayiir_core::workflow::WorkflowBuilder;

    fn ctx() -> WorkflowContext<JsonCodec, ()> {
        WorkflowContext::new("test-workflow", Arc::new(JsonCodec), Arc::new(()))
    }

    #[tokio::test]
    async fn test_single_task() {
        let workflow = WorkflowBuilder::new(ctx())
            .then("add_one", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        let runner = InProcessRunner;
        let status = runner.run(&workflow, 5u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));
    }

    #[tokio::test]
    async fn test_chained_tasks() {
        let workflow = WorkflowBuilder::new(ctx())
            .then("add_one", |i: u32| async move { Ok(i + 1) })
            .then("double", |i: u32| async move { Ok(i * 2) })
            .then("to_string", |i: u32| async move { Ok(i.to_string()) })
            .build()
            .unwrap();

        let runner = InProcessRunner;
        let status = runner.run(&workflow, 10u32).await.unwrap();
        // 10 + 1 = 11, 11 * 2 = 22, "22"
        assert!(matches!(status, WorkflowStatus::Completed));
    }

    #[tokio::test]
    async fn test_task_failure_returns_failed_status() {
        let workflow = WorkflowBuilder::new(ctx())
            .then("fail", |_i: u32| async move {
                Err::<u32, _>(anyhow::anyhow!("intentional failure"))
            })
            .build()
            .unwrap();

        let runner = InProcessRunner;
        let status = runner.run(&workflow, 1u32).await.unwrap();
        match status {
            WorkflowStatus::Failed(e) => {
                assert!(e.to_string().contains("intentional failure"));
            }
            _ => panic!("Expected Failed status"),
        }
    }

    #[tokio::test]
    async fn test_fork_join() {
        let workflow = WorkflowBuilder::new(ctx())
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                b.add("double", |i: u32| async move { Ok(i * 2) });
                b.add("add_ten", |i: u32| async move { Ok(i + 10) });
            })
            .join("combine", |outputs: BranchOutputs<JsonCodec>| async move {
                let doubled: u32 = outputs.get("double")?;
                let added: u32 = outputs.get("add_ten")?;
                Ok(doubled + added)
            })
            .build()
            .unwrap();

        let runner = InProcessRunner;
        let status = runner.run(&workflow, 5u32).await.unwrap();
        // prepare: 5, double: 10, add_ten: 15, combine: 10+15=25
        assert!(matches!(status, WorkflowStatus::Completed));
    }

    #[tokio::test]
    async fn test_failure_in_chain_propagates() {
        let workflow = WorkflowBuilder::new(ctx())
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("fail_step", |_i: u32| async move {
                Err::<u32, _>(anyhow::anyhow!("step2 failed"))
            })
            .then("step3", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        let runner = InProcessRunner;
        let status = runner.run(&workflow, 1u32).await.unwrap();
        match status {
            WorkflowStatus::Failed(e) => {
                assert!(e.to_string().contains("step2 failed"));
            }
            _ => panic!("Expected Failed status"),
        }
    }

    #[tokio::test]
    async fn test_with_custom_metadata() {
        let ctx = WorkflowContext::new(
            "meta-workflow",
            Arc::new(JsonCodec),
            Arc::new("my-metadata".to_string()),
        );
        let workflow = WorkflowBuilder::new(ctx)
            .then("task", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        assert_eq!(workflow.workflow_id(), "meta-workflow");
        assert_eq!(**workflow.metadata(), "my-metadata");

        let runner = InProcessRunner;
        let status = runner.run(&workflow, 1u32).await.unwrap();
        assert!(matches!(status, WorkflowStatus::Completed));
    }
}
