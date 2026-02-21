//! Common imports for workflow authors.
//!
//! ```rust
//! use sayiir_core::prelude::*;
//! ```
//!
//! This re-exports the types needed to define and build workflows,
//! including `Encoder`/`Decoder` traits and `NamedBranchResults` for fork/join.
//! Codec implementers should additionally import from [`codec`](crate::codec)
//! (`sealed::*`).

pub use crate::branch_key::BranchKey;
pub use crate::branch_results::NamedBranchResults;
pub use crate::codec::{Codec, Decoder, Encoder, EnvelopeCodec};
pub use crate::context::WorkflowContext;
pub use crate::error::{BoxError, BuildError, BuildErrors, CodecError, WorkflowError};
pub use crate::registry::TaskRegistry;
pub use crate::task::{
    BranchEnvelope, BranchOutputs, CoreTask, RegisterableTask, TaskMetadata, fn_task,
};
pub use crate::workflow::{SerializableWorkflow, Workflow, WorkflowBuilder, key_fn_id};
