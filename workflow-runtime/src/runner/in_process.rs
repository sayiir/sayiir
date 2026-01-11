use super::WorkflowRunner;
use bytes::Bytes;
use futures::future;
use std::sync::Arc;
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
                WorkflowContinuation::Fork { branches, join } => {
                    // Execute all branches in parallel by spawning each as a separate task
                    let branch_handles: Vec<_> = branches
                        .iter()
                        .map(|branch| {
                            let branch = Arc::clone(branch);
                            let branch_input = input.clone();
                            tokio::task::spawn(async move {
                                Self::execute_continuation(&branch, branch_input).await
                            })
                        })
                        .collect();

                    // Wait for all branches to complete in parallel
                    // Spawned tasks can run on different threads for true parallelism
                    let branch_results = future::try_join_all(branch_handles)
                        .await?
                        .into_iter()
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    // If there's a join continuation, pass the collected results to it
                    // The join task will receive Bytes containing serialized branch results
                    match join {
                        Some(join_continuation) => {
                            // Serialize branch results using length-prefixed format
                            let join_input = Self::serialize_branch_results(&branch_results)?;
                            Self::execute_continuation(join_continuation, join_input).await
                        }
                        None => {
                            // No join task, return the last branch result (or empty if no branches)
                            Ok(branch_results.last().cloned().unwrap_or_default())
                        }
                    }
                }
            }
        })
    }

    /// Serialize branch results into a format that can be passed to the join task.
    ///
    /// Uses a simple length-prefixed format:
    /// - 4 bytes: number of branches (u32, little-endian)
    /// - For each branch:
    ///   - 4 bytes: length of branch result (u32, little-endian)
    ///   - N bytes: branch result data
    fn serialize_branch_results(branch_results: &[Bytes]) -> anyhow::Result<Bytes> {
        use std::io::Write;

        let mut buffer = Vec::new();

        // Write number of branches
        buffer.write_all(&(branch_results.len() as u32).to_le_bytes())?;

        // Write each branch result with length prefix
        for result in branch_results {
            buffer.write_all(&(result.len() as u32).to_le_bytes())?;
            buffer.write_all(result.as_ref())?;
        }

        Ok(Bytes::from(buffer))
    }
}
