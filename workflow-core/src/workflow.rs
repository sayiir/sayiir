use crate::codec::Codec;
use crate::codec::sealed;
use crate::context::WorkflowContext;
use crate::task::{
    BranchOutputs, ErasedBranch, UntypedCoreTask, branch, to_core_task, to_heterogeneous_join_task,
};
use bytes::Bytes;
use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;

/// Error returned when building a workflow fails.
#[derive(Debug)]
pub enum WorkflowBuildError {
    /// A duplicate task ID was found.
    DuplicateId(String),
}

impl std::fmt::Display for WorkflowBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowBuildError::DuplicateId(id) => write!(f, "Duplicate task id: '{}'", id),
        }
    }
}

impl std::error::Error for WorkflowBuildError {}

/// A continuation is a value that can be used to resume a workflow.
pub enum WorkflowContinuation {
    Done(Bytes),
    Task {
        id: String,
        func: UntypedCoreTask,
        next: Option<Box<WorkflowContinuation>>,
    },
    Fork {
        branches: Box<[Arc<WorkflowContinuation>]>,
        join: Option<Box<WorkflowContinuation>>,
    },
}

impl WorkflowContinuation {
    /// Find the first duplicate ID in this continuation tree, if any.
    fn find_duplicate_id(&self) -> Option<String> {
        fn collect(cont: &WorkflowContinuation, seen: &mut HashSet<String>) -> Option<String> {
            match cont {
                WorkflowContinuation::Done(_) => None,
                WorkflowContinuation::Task { id, next, .. } => {
                    if !seen.insert(id.clone()) {
                        return Some(id.clone());
                    }
                    next.as_ref().and_then(|n| collect(n, seen))
                }
                WorkflowContinuation::Fork { branches, join } => branches
                    .iter()
                    .find_map(|b| collect(b, seen))
                    .or_else(|| join.as_ref().and_then(|j| collect(j, seen))),
            }
        }
        collect(self, &mut HashSet::new())
    }
}

/// The status of a workflow execution.
#[derive(Debug)]
pub enum WorkflowStatus {
    /// The workflow completed successfully.
    Completed,
    /// The workflow failed with an error.
    Failed(anyhow::Error),
}

/// Marker type for empty workflow (no tasks yet).
pub struct Empty;

/// Marker type indicating the workflow has at least one task.
pub struct HasTasks;

pub struct WorkflowBuilder<C, Input, Output, M = (), State = Empty> {
    context: WorkflowContext<C, M>,
    continuation: Option<WorkflowContinuation>,
    _phantom: PhantomData<(Input, Output, State)>,
}

/// A built workflow that can be executed.
pub struct Workflow<C, Input, M = ()> {
    context: WorkflowContext<C, M>,
    continuation: WorkflowContinuation,
    _phantom: PhantomData<Input>,
}

impl<C, Input, M> WorkflowBuilder<C, Input, Input, M, Empty> {
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
            context: ctx,
            continuation: None,
            _phantom: PhantomData,
        }
    }
}

/// Methods available on any WorkflowBuilder state.
impl<C, Input, Output, M, S> WorkflowBuilder<C, Input, Output, M, S> {
    /// Add a sequential task to the workflow.
    pub fn then<F, Fut, NewOutput>(
        self,
        id: &str,
        func: F,
    ) -> WorkflowBuilder<C, Input, NewOutput, M, HasTasks>
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        NewOutput: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<NewOutput>> + Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<NewOutput>,
    {
        let codec = Arc::clone(&self.context.codec);
        let task = to_core_task(func, codec);

        let new_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: task,
            next: None,
        };

        let continuation = match self.continuation {
            Some(mut existing) => {
                WorkflowBuilder::<C, Input, Output, M, HasTasks>::append_to_chain(
                    &mut existing,
                    new_task,
                );
                Some(existing)
            }
            None => Some(new_task),
        };

        WorkflowBuilder {
            continuation,
            context: self.context,
            _phantom: PhantomData,
        }
    }

    /// Fork the workflow into multiple parallel branches with heterogeneous outputs.
    ///
    /// Each branch receives the same input (the current workflow's output) and executes in parallel.
    /// Branches can return different types. After all branches complete, use `join()` to combine
    /// the results using `BranchOutputs` for type-safe named access.
    ///
    /// # Type Safety
    ///
    /// After calling `branches`, you must call `join` before you can call `then` or `build`.
    /// The `join` function receives `BranchOutputs<C>` which allows type-safe retrieval
    /// of each branch's output by name.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// workflow
    ///     .then("prepare", |input| async { Ok(input) })
    ///     .branches(|b| {
    ///         b.add("count", |i: u32| async move { Ok(i * 2) });
    ///         b.add("name", |i: u32| async move { Ok(format!("item_{}", i)) });
    ///         b.add("ratio", |i: u32| async move { Ok(i as f64 / 100.0) });
    ///     })
    ///     .join("combine", |outputs: BranchOutputs<_>| async move {
    ///         let count: u32 = outputs.get("count")?;
    ///         let name: String = outputs.get("name")?;
    ///         let ratio: f64 = outputs.get("ratio")?;
    ///         Ok(format!("{}: {} ({})", name, count, ratio))
    ///     })
    /// ```
    pub fn branches<F>(self, f: F) -> ForkBuilder<C, Input, Output, M>
    where
        F: FnOnce(&mut BranchCollector<C, Output>),
        C: Codec,
    {
        let codec = Arc::clone(&self.context.codec);
        let mut collector = BranchCollector {
            codec,
            branches: Vec::new(),
            _phantom: PhantomData,
        };
        f(&mut collector);

        ForkBuilder {
            context: self.context,
            continuation: self.continuation,
            branches: collector.branches,
            _phantom: PhantomData,
        }
    }

    /// Fork the workflow into multiple parallel branches (low-level API).
    ///
    /// Returns a `ForkBuilder` for adding branches one at a time with `.branch()`.
    /// For most cases, prefer using [`branches`](Self::branches) instead.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// workflow
    ///     .fork()
    ///     .branch("count", |i: u32| async move { Ok(i * 2) })
    ///     .branch("name", |i: u32| async move { Ok(format!("item_{}", i)) })
    ///     .join("combine", |outputs| async move { ... })
    /// ```
    pub fn fork(self) -> ForkBuilder<C, Input, Output, M> {
        ForkBuilder {
            context: self.context,
            continuation: self.continuation,
            branches: Vec::new(),
            _phantom: PhantomData,
        }
    }
}

/// Methods only available when the workflow has at least one task.
impl<C, Input, Output, M> WorkflowBuilder<C, Input, Output, M, HasTasks> {
    /// Build the workflow into an executable workflow.
    ///
    /// # Errors
    ///
    /// Returns an error if duplicate task IDs are found.
    pub fn build(self) -> Result<Workflow<C, Input, M>, WorkflowBuildError>
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
        let continuation = self
            .continuation
            .expect("HasTasks state guarantees task exists");

        if let Some(dup) = continuation.find_duplicate_id() {
            return Err(WorkflowBuildError::DuplicateId(dup));
        }

        Ok(Workflow {
            continuation,
            context: self.context,
            _phantom: PhantomData,
        })
    }

    /// Append a new task to the end of the continuation chain.
    fn append_to_chain(continuation: &mut WorkflowContinuation, new_task: WorkflowContinuation) {
        match continuation {
            WorkflowContinuation::Task { next, .. } => match next {
                Some(next_box) => Self::append_to_chain(next_box, new_task),
                None => *next = Some(Box::new(new_task)),
            },
            WorkflowContinuation::Done(_) => *continuation = new_task,
            WorkflowContinuation::Fork { join, .. } => match join {
                Some(join_box) => Self::append_to_chain(join_box, new_task),
                None => *continuation = new_task,
            },
        }
    }
}

/// Collector for adding branches in a closure.
///
/// Used by [`WorkflowBuilder::branches`] to collect multiple branches.
pub struct BranchCollector<C, Input> {
    codec: Arc<C>,
    branches: Vec<ErasedBranch>,
    _phantom: PhantomData<Input>,
}

impl<C, Input> BranchCollector<C, Input> {
    /// Add a branch to the fork.
    ///
    /// Each branch receives the same input and can return a different output type.
    /// Duplicate IDs are checked at `build()` time.
    pub fn add<F, Fut, BranchOutput>(&mut self, id: &str, func: F)
    where
        F: Fn(Input) -> Fut + Send + Sync + 'static,
        Input: Send + 'static,
        BranchOutput: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<BranchOutput>> + Send + 'static,
        C: Codec + sealed::DecodeValue<Input> + sealed::EncodeValue<BranchOutput>,
    {
        let erased = branch(id, func).erase(Arc::clone(&self.codec));
        self.branches.push(erased);
    }
}

/// Builder for constructing fork branches fluently.
///
/// Created by calling `.fork()` on a `WorkflowBuilder`. Add branches with `.branch()`,
/// then complete with `.join()`.
pub struct ForkBuilder<C, Input, Output, M> {
    context: WorkflowContext<C, M>,
    continuation: Option<WorkflowContinuation>,
    branches: Vec<ErasedBranch>,
    _phantom: PhantomData<(Input, Output)>,
}

impl<C, Input, Output, M> ForkBuilder<C, Input, Output, M> {
    /// Add a branch to the fork.
    ///
    /// Each branch receives the same input and can return a different output type.
    /// Branch outputs are collected and passed to the join function as `BranchOutputs`.
    /// Duplicate IDs are checked at `build()` time.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// workflow
    ///     .fork()
    ///     .branch("double", |i: u32| async move { Ok(i * 2) })
    ///     .branch("name", |i: u32| async move { Ok(format!("item_{}", i)) })
    ///     .join("combine", |outputs| async move { ... })
    /// ```
    pub fn branch<F, Fut, BranchOutput>(mut self, id: &str, func: F) -> Self
    where
        F: Fn(Output) -> Fut + Send + Sync + 'static,
        Output: Send + 'static,
        BranchOutput: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<BranchOutput>> + Send + 'static,
        C: Codec + sealed::DecodeValue<Output> + sealed::EncodeValue<BranchOutput>,
    {
        let codec = Arc::clone(&self.context.codec);
        let erased = branch(id, func).erase(codec);
        self.branches.push(erased);
        self
    }

    /// Join the results from all branches.
    ///
    /// The join function receives `BranchOutputs<C>` which provides type-safe
    /// access to each branch's output by id.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// workflow
    ///     .fork()
    ///     .branch("count", |i: u32| async move { Ok(i * 2) })
    ///     .branch("name", |i: u32| async move { Ok(format!("n{}", i)) })
    ///     .join("combine", |outputs: BranchOutputs<_>| async move {
    ///         let count: u32 = outputs.get("count")?;
    ///         let name: String = outputs.get("name")?;
    ///         Ok(format!("{}: {}", name, count))
    ///     })
    /// ```
    pub fn join<F, Fut, JoinOutput>(
        self,
        id: &str,
        func: F,
    ) -> WorkflowBuilder<C, Input, JoinOutput, M, HasTasks>
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        JoinOutput: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<JoinOutput>> + Send + 'static,
        C: Codec + sealed::EncodeValue<JoinOutput> + Send + Sync + 'static,
    {
        let codec = Arc::clone(&self.context.codec);
        let join_task_fn = to_heterogeneous_join_task(func, codec);

        // Create continuation for each branch
        let branch_continuations: Vec<Arc<WorkflowContinuation>> = self
            .branches
            .into_iter()
            .map(|b| {
                Arc::new(WorkflowContinuation::Task {
                    id: b.id,
                    func: b.task,
                    next: None,
                })
            })
            .collect();

        let join_task = WorkflowContinuation::Task {
            id: id.to_string(),
            func: join_task_fn,
            next: None,
        };

        let fork_continuation = WorkflowContinuation::Fork {
            branches: branch_continuations.into_boxed_slice(),
            join: Some(Box::new(join_task)),
        };

        let continuation = match self.continuation {
            Some(mut existing) => {
                WorkflowBuilder::<C, Input, JoinOutput, M, HasTasks>::append_to_chain(
                    &mut existing,
                    fork_continuation,
                );
                Some(existing)
            }
            None => Some(fork_continuation),
        };

        WorkflowBuilder {
            continuation,
            context: self.context,
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
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Verify the workflow was built successfully
        // The workflow can be executed using a WorkflowRunner from workflow-runtime
        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_workflow_with_metadata() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new("test_metadata"));
        let workflow: Workflow<DummyCodec, u32, &str> = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        assert_eq!(**workflow.metadata(), "test_metadata");
    }

    #[test]
    fn test_task_order() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("first", |i: u32| async move { Ok(i + 1) })
            .then("second", |i: u32| async move { Ok(i + 2) })
            .then("third", |i: u32| async move { Ok(i + 3) })
            .build()
            .unwrap();

        // Verify the continuation chain structure
        // Tasks should be linked in order: first -> second -> third
        let mut current = workflow.continuation();
        let mut task_ids = Vec::new();

        loop {
            match current {
                crate::workflow::WorkflowContinuation::Task { id, next, .. } => {
                    task_ids.push(id.clone());
                    match next {
                        Some(next_box) => current = next_box.as_ref(),
                        None => break,
                    }
                }
                _ => break,
            }
        }

        // Tasks should execute in the order they were added
        assert_eq!(task_ids, vec!["first", "second", "third"]);
    }

    #[test]
    fn test_heterogeneous_fork_join_compiles() {
        use crate::context::WorkflowContext;
        use crate::task::BranchOutputs;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
        // This test verifies that the heterogeneous fork-join API compiles correctly.
        // Each branch can return a different type thanks to type erasure.
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                // Returns u32
                b.add("count", |i: u32| async move { Ok(i * 2) });
                // Returns String - heterogeneous output type!
                b.add("name", |i: u32| async move { Ok(format!("item_{}", i)) });
                // Returns f64 - another different type!
                b.add("ratio", |i: u32| async move { Ok(i as f64 / 100.0) });
            })
            .join("combine", |outputs: BranchOutputs<DummyCodec>| async move {
                // In a real workflow with a proper codec, you would:
                // let count: u32 = outputs.get("count")?;
                // let name: String = outputs.get("name")?;
                // let ratio: f64 = outputs.get("ratio")?;
                // For this test, just verify the API compiles
                let _ = outputs.len();
                Ok(format!("combined {} branches", outputs.len()))
            })
            .then("final", |s: String| async move { Ok(s.len() as u32) })
            .build()
            .unwrap();

        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_duplicate_branch_id_returns_error() {
        use crate::context::WorkflowContext;
        use crate::workflow::WorkflowBuildError;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(Arc::new(DummyCodec), Arc::new(()));
        let result = WorkflowBuilder::<_, u32, _>::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                b.add("count", |i: u32| async move { Ok(i * 2) });
                b.add("count", |i: u32| async move { Ok(i * 3) }); // Duplicate!
            })
            .join("combine", |_outputs| async move { Ok(0u32) })
            .build();

        assert!(matches!(
            result,
            Err(WorkflowBuildError::DuplicateId(id)) if id == "count"
        ));
    }
}
