//! Module for workflow runners.
//!
//! A workflow runner is responsible for executing workflows.
//! Different implementations can provide different execution strategies,
//! such as in-process execution, distributed execution, or execution with
//! persistence and recovery.

use sayiir_core::codec::Codec;
use sayiir_core::codec::sealed;
use sayiir_core::workflow::{Workflow, WorkflowStatus};
use std::future::Future;

use crate::error::RuntimeError;

/// A trait for executing workflows.
///
/// Different implementations can provide different execution strategies,
/// such as in-process execution, distributed execution, or execution with
/// persistence and recovery.
///
/// Note: This trait uses `impl Future` which makes it non-object-safe.
/// If you need dynamic dispatch, wrap implementations in an enum.
pub trait WorkflowRunner: Send + Sync {
    /// Run a workflow with the given input.
    ///
    /// The input type must match the input type of the first task added via `then`.
    /// Returns the workflow execution status.
    fn run<'w, C, Input, M>(
        &self,
        workflow: &'w Workflow<C, Input, M>,
        input: Input,
    ) -> impl Future<Output = Result<WorkflowStatus, RuntimeError>> + Send + 'w
    where
        Input: Send + 'static,
        M: Send + Sync + 'static,
        C: Codec + sealed::EncodeValue<Input>;
}

pub mod distributed;
pub mod in_process;
