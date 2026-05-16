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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn done_is_done() {
        let r = LoopResult::Done(42);
        assert!(r.is_done());
        assert!(!r.is_again());
    }

    #[test]
    fn again_is_again() {
        let r = LoopResult::Again(42);
        assert!(r.is_again());
        assert!(!r.is_done());
    }

    #[test]
    fn into_inner_done() {
        assert_eq!(LoopResult::Done("hello").into_inner(), "hello");
    }

    #[test]
    fn into_inner_again() {
        assert_eq!(LoopResult::Again(99).into_inner(), 99);
    }

    #[test]
    fn into_decision_done() {
        let (decision, val) = LoopResult::Done(10).into_decision();
        assert_eq!(decision, LoopDecision::Done);
        assert_eq!(val, 10);
    }

    #[test]
    fn into_decision_again() {
        let (decision, val) = LoopResult::Again(5).into_decision();
        assert_eq!(decision, LoopDecision::Again);
        assert_eq!(val, 5);
    }

    #[test]
    fn serde_round_trip_done() {
        let r = LoopResult::Done(42u32);
        let json = serde_json::to_string(&r).unwrap();
        let back: LoopResult<u32> = serde_json::from_str(&json).unwrap();
        assert!(back.is_done());
        assert_eq!(back.into_inner(), 42);
    }

    #[test]
    fn serde_round_trip_again() {
        let r = LoopResult::Again("next".to_string());
        let json = serde_json::to_string(&r).unwrap();
        let back: LoopResult<String> = serde_json::from_str(&json).unwrap();
        assert!(back.is_again());
        assert_eq!(back.into_inner(), "next");
    }
}
