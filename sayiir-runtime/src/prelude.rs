//! Convenience re-exports for common usage.
//!
//! ```rust
//! use sayiir_runtime::prelude::*;
//! ```

// Runtime types
pub use crate::{
    CheckpointingRunner, InProcessRunner, PooledWorker, PooledWorkerBuilder, RuntimeError,
    WorkerHandle, WorkflowRunExt, WorkflowRunner,
};

// Codecs
#[cfg(feature = "json")]
pub use crate::serialization::JsonCodec;
#[cfg(feature = "rkyv")]
pub use crate::serialization::RkyvCodec;

// Codec traits
pub use sayiir_core::codec::{Decoder, Encoder, EnvelopeCodec};

// Core workflow types (from sayiir-core)
pub use sayiir_core::branch_key::BranchKey;
pub use sayiir_core::branch_results::NamedBranchResults;
pub use sayiir_core::context::WorkflowContext;
pub use sayiir_core::deps::{Deps, DepsBuilder, DepsInjectable, MissingDep};
pub use sayiir_core::error::BoxError;
pub use sayiir_core::registry::TaskRegistry;
pub use sayiir_core::workflow::{Workflow, WorkflowBuilder, WorkflowStatus};

// Persistence (from sayiir-persistence)
pub use sayiir_persistence::{InMemoryBackend, PersistentBackend};

// Macros (from sayiir-macros)
#[cfg(feature = "macros")]
pub use sayiir_macros::{BranchKey, task, workflow};
