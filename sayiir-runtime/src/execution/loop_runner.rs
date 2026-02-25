//! Shared loop iteration logic.
//!
//! Deduplicates the decode → decision → max-iterations-policy pattern that was
//! previously copy-pasted across all six execution paths (sync, async,
//! checkpointing callback, branch checkpointing, runner main, runner branch).

use std::future::Future;
use std::ops::ControlFlow;

use bytes::Bytes;
use sayiir_core::codec::LoopDecision;
use sayiir_core::error::{BoxError, CodecError, WorkflowError};
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot};
use sayiir_core::workflow::{MaxIterationsPolicy, WorkflowContinuation};
use sayiir_persistence::SnapshotStore;

use crate::error::RuntimeError;

/// Decode a loop envelope, auto-detecting binary vs JSON format.
///
/// - **Binary** (tag byte `0x00`/`0x01` + payload) — produced by Rust loop task wrappers.
/// - **JSON** (`{"_loop":"again"|"done","value":...}`) — produced by Python/JS bindings.
pub(crate) fn decode_loop_envelope(bytes: &[u8]) -> Result<(LoopDecision, Bytes), BoxError> {
    let &first = bytes.first().ok_or("empty loop envelope")?;
    match first {
        // Binary tags produced by sayiir_core::codec::encode_loop_envelope.
        0 | 1 => sayiir_core::codec::decode_loop_envelope(bytes),
        // JSON format from bindings.
        _ => {
            let v: serde_json::Value = serde_json::from_slice(bytes)?;
            let tag = v
                .get("_loop")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing or invalid '_loop' tag in LoopResult JSON")?;
            let decision: LoopDecision = tag
                .parse()
                .map_err(|_| format!("unknown loop decision tag: '{tag}'"))?;
            let inner = v
                .get("value")
                .ok_or("missing 'value' field in LoopResult JSON")?;
            Ok((decision, Bytes::from(serde_json::to_vec(inner)?)))
        }
    }
}

/// Final output of a loop (`Break` arm of `ControlFlow`).
pub(crate) struct LoopExit(pub Bytes);

/// Input for the next iteration (`Continue` arm of `ControlFlow`).
pub(crate) struct LoopNext(pub Bytes);

/// Configuration for a loop, extracted from [`WorkflowContinuation::Loop`].
pub(crate) struct LoopConfig<'a> {
    pub id: &'a str,
    pub body: &'a WorkflowContinuation,
    pub max_iterations: u32,
    pub on_max: MaxIterationsPolicy,
    pub start_iteration: u32,
}

/// Decode a loop body's raw output and resolve the iteration decision.
///
/// Returns `Break(LoopExit)` when the loop should exit (body returned
/// [`LoopDecision::Done`] or max-iterations was reached with
/// [`MaxIterationsPolicy::ExitWithLast`]).
///
/// Returns `Continue(LoopNext)` when the body returned
/// [`LoopDecision::Again`] and more iterations remain.
///
/// Returns `Err(MaxIterationsExceeded)` when max-iterations is reached
/// with [`MaxIterationsPolicy::Fail`].
pub(crate) fn resolve_loop_iteration(
    output: &Bytes,
    iteration: u32,
    cfg: &LoopConfig<'_>,
) -> Result<ControlFlow<LoopExit, LoopNext>, RuntimeError> {
    let (decision, inner) = decode_loop_envelope(output).map_err(|e| CodecError::DecodeFailed {
        task_id: cfg.id.to_string(),
        expected_type: "LoopEnvelope",
        source: e,
    })?;
    match decision {
        LoopDecision::Done => Ok(ControlFlow::Break(LoopExit(inner))),
        LoopDecision::Again => {
            if iteration + 1 >= cfg.max_iterations {
                match cfg.on_max {
                    MaxIterationsPolicy::Fail => Err(WorkflowError::MaxIterationsExceeded {
                        loop_id: cfg.id.to_string(),
                        max_iterations: cfg.max_iterations,
                    }
                    .into()),
                    MaxIterationsPolicy::ExitWithLast => Ok(ControlFlow::Break(LoopExit(inner))),
                }
            } else {
                Ok(ControlFlow::Continue(LoopNext(inner)))
            }
        }
    }
}

/// Optional snapshot bookkeeping for loop iterations.
///
/// Default methods are no-ops, used by non-checkpointing executors.
#[allow(unused_variables)]
pub(crate) trait LoopHooks: Send {
    /// Clear body task results from the snapshot after each iteration.
    fn clear_body_tasks(&mut self, body: &WorkflowContinuation) {}

    /// Called when the loop exits (Done or `ExitWithLast`).
    ///
    /// Checkpointing implementations cache the final output in the snapshot
    /// so that resume after a crash can skip the completed loop.
    fn on_loop_exit(
        &mut self,
        loop_id: &str,
        output: &Bytes,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        async { Ok(()) }
    }

    /// Checkpoint iteration progress (set iteration counter, optionally
    /// update position, save snapshot).
    fn on_iteration_progress(
        &mut self,
        loop_id: &str,
        next_iteration: u32,
        body: &WorkflowContinuation,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        async { Ok(()) }
    }
}

/// No-op hooks for executors that don't checkpoint.
pub(crate) struct NoHooks;
impl LoopHooks for NoHooks {}

/// Snapshot-aware hooks for checkpointing executors.
///
/// When `track_position` is `true` (main executor path), updates
/// [`ExecutionPosition::InLoop`] and saves the snapshot after each
/// iteration. When `false` (branch executor path), only updates the
/// iteration counter without position tracking.
pub(crate) struct CheckpointingLoopHooks<'a, B> {
    pub snapshot: &'a mut WorkflowSnapshot,
    pub backend: &'a B,
    pub track_position: bool,
}

impl<B: SnapshotStore> LoopHooks for CheckpointingLoopHooks<'_, B> {
    fn clear_body_tasks(&mut self, body: &WorkflowContinuation) {
        let body_ser = body.to_serializable();
        for tid in &body_ser.task_ids() {
            self.snapshot.remove_task_result(tid);
        }
    }

    async fn on_loop_exit(&mut self, loop_id: &str, output: &Bytes) -> Result<(), RuntimeError> {
        self.snapshot.clear_loop_iteration(loop_id);
        self.snapshot
            .mark_task_completed(loop_id.to_string(), output.clone());
        self.backend.save_snapshot(self.snapshot).await?;
        Ok(())
    }

    async fn on_iteration_progress(
        &mut self,
        loop_id: &str,
        next_iteration: u32,
        body: &WorkflowContinuation,
    ) -> Result<(), RuntimeError> {
        self.snapshot.set_loop_iteration(loop_id, next_iteration);
        if self.track_position {
            self.snapshot.update_position(ExecutionPosition::InLoop {
                loop_id: loop_id.to_string(),
                iteration: next_iteration,
                next_task_id: Some(body.first_task_id().to_string()),
            });
        }
        // Always persist: the cleared body task results and updated iteration
        // counter must reach the backend so that nested executors (e.g.
        // execute_branch_with_checkpointing) that load a fresh snapshot
        // don't see stale cached body results from the previous iteration.
        self.backend.save_snapshot(self.snapshot).await?;
        Ok(())
    }
}

/// Execute a loop body repeatedly, resolving each iteration's decision.
///
/// Generic over body execution (async closure) and snapshot hooks.
/// The sync executor uses [`resolve_loop_iteration`] directly instead.
#[tracing::instrument(
    name = "loop",
    skip_all,
    fields(loop_id = %cfg.id),
)]
pub(crate) async fn run_loop_async<F, Fut, H>(
    cfg: &LoopConfig<'_>,
    initial_input: Bytes,
    execute_body: F,
    hooks: &mut H,
) -> Result<Bytes, RuntimeError>
where
    F: Fn(Bytes) -> Fut,
    Fut: Future<Output = Result<Bytes, RuntimeError>>,
    H: LoopHooks,
{
    tracing::debug!("starting loop execution");
    let mut loop_input = initial_input;

    for iteration in cfg.start_iteration..cfg.max_iterations {
        let output = execute_body(loop_input.clone()).await?;

        hooks.clear_body_tasks(cfg.body);

        match resolve_loop_iteration(&output, iteration, cfg)? {
            ControlFlow::Break(LoopExit(inner)) => {
                hooks.on_loop_exit(cfg.id, &inner).await?;
                return Ok(inner);
            }
            ControlFlow::Continue(LoopNext(inner)) => {
                hooks
                    .on_iteration_progress(cfg.id, iteration + 1, cfg.body)
                    .await?;
                loop_input = inner;
            }
        }
    }

    Err(WorkflowError::MaxIterationsExceeded {
        loop_id: cfg.id.to_string(),
        max_iterations: cfg.max_iterations,
    }
    .into())
}
