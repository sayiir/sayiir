//! Shared workflow execution logic.
//!
//! Provides generic execution functions that can be used by different runners
//! (in-process, Python bindings, etc.) by supplying task execution callbacks.

pub(crate) mod control_flow;
mod executors;
mod fork;
mod helpers;
mod lifecycle;
pub(crate) mod loop_runner;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::absurd_extreme_comparisons,
    clippy::useless_conversion
)]
mod tests;

// Re-export everything so consumers (runner/distributed.rs, lib.rs, etc.)
// continue to use `crate::execution::*` paths unchanged.

// ── helpers ─────────────────────────────────────────────────────────────
pub(crate) use helpers::{
    ResumeParkedPosition, branch_execute_or_skip_task, check_guards, execute_or_skip_task,
    retry_with_checkpoint, set_deadline_if_needed,
};
pub(crate) use loop_runner::decode_loop_envelope;

// ── fork ────────────────────────────────────────────────────────────────
pub use fork::serialize_branch_results;
pub(crate) use fork::{
    ForkBranchOutcome, JoinResolution, collect_cached_branches, resolve_join, settle_fork_outcome,
};

// ── executors ───────────────────────────────────────────────────────────
pub use executors::{
    execute_continuation_async, execute_continuation_sync, execute_continuation_with_checkpointing,
};

// ── lifecycle ───────────────────────────────────────────────────────────
pub use lifecycle::{
    PrepareRunOutcome, ResumeOutcome, check_existing_instance, finalize_execution,
    get_resume_input, prepare_resume, prepare_run,
};
