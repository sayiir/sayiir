//! Convenience re-exports for common usage.
//!
//! ```rust
//! use sayiir_runtime::prelude::*;
//! ```

// Runtime types
pub use crate::{
    CheckpointingRunner, InProcessRunner, PooledWorker, RuntimeError, WorkerHandle, WorkflowRunner,
};

// Codecs
#[cfg(feature = "json")]
pub use crate::serialization::JsonCodec;
#[cfg(feature = "rkyv")]
pub use crate::serialization::RkyvCodec;

// Core workflow types (from sayiir-core)
pub use sayiir_core::context::WorkflowContext;
pub use sayiir_core::registry::TaskRegistry;
pub use sayiir_core::workflow::{Workflow, WorkflowBuilder, WorkflowStatus};

// Persistence (from sayiir-persistence)
pub use sayiir_persistence::{InMemoryBackend, PersistentBackend};
