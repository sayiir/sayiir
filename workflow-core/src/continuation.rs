use crate::task::UntypedCoreTask;
use bytes::Bytes;

/// A continuation is a value that can be used to resume a workflow.
pub enum Continuation {
    Done(Bytes),
    Task {
        name: String,
        func: UntypedCoreTask,
        next: Box<Continuation>,
    },
    Fork {
        branches: Vec<Continuation>,
        join: Box<Continuation>,
    },
}
