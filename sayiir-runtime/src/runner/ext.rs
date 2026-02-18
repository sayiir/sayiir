use super::WorkflowRunner;
use crate::error::RuntimeError;
use crate::runner::in_process::InProcessRunner;
use sayiir_core::codec::sealed;
use sayiir_core::codec::{Codec, EnvelopeCodec};
use sayiir_core::workflow::{Workflow, WorkflowStatus};

/// Extension trait providing convenience methods on [`Workflow`].
pub trait WorkflowRunExt<C, Input, M> {
    /// Run the workflow once in-process without persistence.
    ///
    /// Uses [`InProcessRunner`] internally — no backend, no instance ID.
    /// Ideal for quick testing and simple scripts.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use sayiir_runtime::prelude::*;
    /// # use sayiir_core::error::BoxError;
    /// # async fn example() -> Result<(), BoxError> {
    /// let ctx = WorkflowContext::new("demo", std::sync::Arc::new(JsonCodec), std::sync::Arc::new(()));
    /// let workflow = WorkflowBuilder::new(ctx)
    ///     .then("greet", |name: String| async move { Ok(format!("Hello, {name}!")) })
    ///     .build()?;
    ///
    /// let status = workflow.run_once("World".to_string()).await?;
    /// # Ok(())
    /// # }
    /// ```
    fn run_once(
        &self,
        input: Input,
    ) -> impl std::future::Future<Output = Result<WorkflowStatus, RuntimeError>> + Send + '_
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + EnvelopeCodec + sealed::EncodeValue<Input>;
}

impl<C, Input, M> WorkflowRunExt<C, Input, M> for Workflow<C, Input, M> {
    fn run_once(
        &self,
        input: Input,
    ) -> impl std::future::Future<Output = Result<WorkflowStatus, RuntimeError>> + Send + '_
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + EnvelopeCodec + sealed::EncodeValue<Input>,
    {
        InProcessRunner.run(self, input)
    }
}
