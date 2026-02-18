//! Common imports for workflow authors.
//!
//! ```rust
//! use sayiir_core::prelude::*;
//! ```
//!
//! This re-exports the types needed to define and build workflows.
//! Codec implementers should additionally import from [`codec`](crate::codec)
//! (`Encoder`, `Decoder`, and `sealed::*`).

pub use crate::branch_key::BranchKey;
pub use crate::codec::{Codec, EnvelopeCodec};
pub use crate::context::WorkflowContext;
pub use crate::error::{BoxError, BuildError, WorkflowError};
pub use crate::registry::TaskRegistry;
pub use crate::task::{BranchEnvelope, BranchOutputs, CoreTask, TaskMetadata, fn_task};
pub use crate::workflow::{SerializableWorkflow, Workflow, WorkflowBuilder, key_fn_id};
