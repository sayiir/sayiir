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
                match Self::execute_continuation(continuation, input_bytes).await {
                    Ok(_) => Ok(WorkflowStatus::Completed),
                    Err(e) => Ok(WorkflowStatus::Failed(e)),
                }
            })
            .await
        }
    }
}

impl InProcessRunner {
    /// Execute a continuation chain iteratively (no recursion, no boxing).
    ///
    /// Uses a loop instead of recursion to avoid boxing the future.
    /// Fork branches are spawned as separate tasks (they have their own stacks).
    #[allow(clippy::manual_async_fn)]
    fn execute_continuation(
        continuation: &WorkflowContinuation,
        input: Bytes,
    ) -> impl std::future::Future<Output = anyhow::Result<Bytes>> + Send + '_ {
        async move {
            let mut current = continuation;
            let mut current_input = input;

            loop {
                match current {
                    WorkflowContinuation::Task { func, next, .. } => {
                        let output = func.run(current_input).await?;
                        match next {
                            Some(next_continuation) => {
                                current = next_continuation;
                                current_input = output;
                            }
                            None => return Ok(output),
                        }
                    }
                    WorkflowContinuation::Fork { branches, join } => {
                        let branch_handles: Vec<_> = branches
                            .iter()
                            .map(|branch| {
                                let id = match branch.as_ref() {
                                    WorkflowContinuation::Task { id, .. } => id.clone(),
                                    WorkflowContinuation::Fork { .. } => String::from("unnamed"),
                                };
                                let branch = Arc::clone(branch);
                                let branch_input = current_input.clone();
                                tokio::task::spawn(Self::execute_branch(branch, branch_input, id))
                            })
                            .collect();

                        let branch_results: Vec<(String, Bytes)> =
                            future::try_join_all(branch_handles)
                                .await?
                                .into_iter()
                                .collect::<anyhow::Result<Vec<_>>>()?;

                        match join {
                            Some(join_continuation) => {
                                let join_input =
                                    Self::serialize_named_branch_results(&branch_results)?;
                                current = join_continuation;
                                current_input = join_input;
                            }
                            None => {
                                return Ok(branch_results
                                    .last()
                                    .map(|(_, b)| b.clone())
                                    .unwrap_or_default());
                            }
                        }
                    }
                }
            }
        }
    }

    /// Execute a branch (for spawned tasks) - takes ownership of Arc.
    #[allow(clippy::manual_async_fn)]
    fn execute_branch(
        continuation: Arc<WorkflowContinuation>,
        input: Bytes,
        id: String,
    ) -> impl std::future::Future<Output = anyhow::Result<(String, Bytes)>> + Send {
        async move {
            let result = Self::execute_continuation(&continuation, input).await?;
            Ok((id, result))
        }
    }

    /// Serialize named branch results into a format that can be passed to the join task.
    ///
    /// Uses a length-prefixed format with names:
    /// - 4 bytes: number of branches (u32, little-endian)
    /// - For each branch:
    ///   - 4 bytes: name length (u32, little-endian)
    ///   - N bytes: name (UTF-8)
    ///   - 4 bytes: data length (u32, little-endian)
    ///   - M bytes: data
    #[allow(clippy::cast_possible_truncation)]
    fn serialize_named_branch_results(branch_results: &[(String, Bytes)]) -> anyhow::Result<Bytes> {
        use std::io::Write;

        let mut buffer = Vec::new();

        // Safe: we never have more than u32::MAX branches in practice
        buffer.write_all(&(branch_results.len() as u32).to_le_bytes())?;

        // Write each branch result with name and length prefix
        for (name, data) in branch_results {
            // Write name length and name
            let name_bytes = name.as_bytes();
            buffer.write_all(&(name_bytes.len() as u32).to_le_bytes())?;
            buffer.write_all(name_bytes)?;

            // Write data length and data
            buffer.write_all(&(data.len() as u32).to_le_bytes())?;
            buffer.write_all(data.as_ref())?;
        }

        Ok(Bytes::from(buffer))
    }
}
