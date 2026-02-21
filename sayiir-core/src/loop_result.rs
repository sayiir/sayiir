//! Loop iteration result type.
//!
//! [`LoopResult`] is the return type of loop body tasks. It tells the
//! runtime whether to execute another iteration (`Again`) or exit the
//! loop (`Done`).

use crate::codec::LoopDecision;

/// The result of a single loop iteration.
///
/// A task inside a `loop_task` must return `LoopResult<T>`:
///
/// - `Again(value)` — feed `value` back as input to the next iteration.
/// - `Done(value)` — exit the loop; `value` becomes the loop's output.
///
/// # Example
///
/// ```rust
/// use sayiir_core::LoopResult;
///
/// fn refine(draft: String) -> LoopResult<String> {
///     if draft.len() > 100 {
///         LoopResult::Done(draft)
///     } else {
///         LoopResult::Again(format!("{draft} ...more"))
///     }
/// }
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "_loop", content = "value")]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum LoopResult<T> {
    /// Continue looping with the wrapped value as the next iteration's input.
    #[serde(rename = "again")]
    Again(T),
    /// Exit the loop with the wrapped value as the loop's output.
    #[serde(rename = "done")]
    Done(T),
}

/// Helper trait to extract the inner type from a `LoopResult<T>`.
///
/// Used by the `workflow!` macro to resolve the output type of a loop node:
/// the loop body returns `LoopResult<T>` but the loop itself outputs `T`.
///
/// ```rust
/// use sayiir_core::loop_result::{LoopResult, LoopOutput};
///
/// // <LoopResult<u32> as LoopOutput>::Inner == u32
/// fn assert_inner<T: LoopOutput<Inner = u32>>() {}
/// assert_inner::<LoopResult<u32>>();
/// ```
pub trait LoopOutput {
    /// The inner type after unwrapping the `LoopResult` envelope.
    type Inner;
}

impl<T> LoopOutput for LoopResult<T> {
    type Inner = T;
}

impl<T> LoopResult<T> {
    /// Returns `true` if this is a `Done` variant.
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done(_))
    }

    /// Returns `true` if this is an `Again` variant.
    #[must_use]
    pub fn is_again(&self) -> bool {
        matches!(self, Self::Again(_))
    }

    /// Unwrap the inner value regardless of variant.
    #[must_use]
    pub fn into_inner(self) -> T {
        match self {
            Self::Again(v) | Self::Done(v) => v,
        }
    }

    /// Split into a [`LoopDecision`] and the inner value.
    #[must_use]
    pub fn into_decision(self) -> (LoopDecision, T) {
        match self {
            Self::Again(v) => (LoopDecision::Again, v),
            Self::Done(v) => (LoopDecision::Done, v),
        }
    }
}
