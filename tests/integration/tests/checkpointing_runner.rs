#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ctx, setup};
use sayiir_core::codec::{Decoder, Encoder};
use sayiir_core::error::BoxError;
use sayiir_core::task::BranchOutputs;
use sayiir_core::workflow::{WorkflowBuilder, WorkflowStatus};
use sayiir_persistence::{SignalStore, SnapshotStore};
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;
use sayiir_runtime::{
    CheckpointingRunner, ConflictPolicy, PooledWorker, RuntimeError, WorkflowClient,
};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn run_single_task() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    let status = runner.run(&workflow, "inst-1", 5u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_completed());
}

#[tokio::test]
async fn run_chained_tasks() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .then("double", |i: u32| async move { Ok(i * 2) })
        .then("sub_three", |i: u32| async move { Ok(i - 3) })
        .build()
        .unwrap();

    let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
    // 10+1=11, 11*2=22, 22-3=19
    assert!(matches!(status, WorkflowStatus::Completed));

    let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_completed());
    // Completed state stores final output, not intermediate task results
    assert!(snapshot.final_output_bytes().is_some());
}

#[tokio::test]
async fn resume_completed_is_noop() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("task", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // Run to completion
    let status = runner.run(&workflow, "inst-1", 1u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    // Resume should be a no-op returning Completed
    let status = runner.resume(&workflow, "inst-1").await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

#[tokio::test]
async fn resume_after_simulated_crash() {
    let (_c, backend, url) = setup().await;
    let runner1 = CheckpointingRunner::new(backend);

    // Workflow: step1 → delay → step2
    // Run parks at the delay (checkpoint saved). "Crash" by dropping runner1,
    // create a fresh backend from the same DB, resume picks up from checkpoint.
    let workflow = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .delay("wait", std::time::Duration::from_millis(1))
        .then("step2", |i: u32| async move { Ok(i * 2) })
        .build()
        .unwrap();

    // Run — step1 completes, parks at delay
    let status = runner1.run(&workflow, "inst-1", 10u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Waiting { .. }));

    // Verify step1 result was checkpointed (snapshot is still InProgress)
    let snapshot = runner1.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_in_progress());
    assert!(snapshot.get_task_result("step1").is_some());

    // "Crash" — drop runner1, build new backend to same DB
    drop(runner1);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let pool2 = PgPool::connect(&url).await.unwrap();
    let backend2 = PostgresBackend::<JsonCodec>::connect_with(pool2)
        .await
        .unwrap();
    let runner2 = CheckpointingRunner::new(backend2);

    // Resume on runner2 — delay expired, step1 is skipped, step2 runs
    let status = runner2.resume(&workflow, "inst-1").await.unwrap();
    assert!(
        matches!(status, WorkflowStatus::Completed),
        "Expected Completed after crash-resume, got {status:?}"
    );
}

#[tokio::test]
async fn cancel_and_resume() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .then("step2", |i: u32| async move {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            Ok(i * 2)
        })
        .build()
        .unwrap();

    // Set up snapshot in-progress and request cancellation
    let input_bytes = Arc::new(JsonCodec).encode(&1u32).unwrap();
    let mut snapshot = sayiir_core::snapshot::WorkflowSnapshot::with_initial_input(
        "inst-cancel".into(),
        workflow.definition_hash().to_string(),
        input_bytes,
    );
    snapshot.update_position(sayiir_core::snapshot::ExecutionPosition::AtTask {
        task_id: "step1".into(),
    });
    runner.backend().save_snapshot(&snapshot).await.unwrap();

    // Signal cancellation via WorkflowClient
    let client = WorkflowClient::from_shared(Arc::clone(runner.backend()));
    client
        .cancel(
            "inst-cancel",
            Some("testing cancel".into()),
            Some("test-suite".into()),
        )
        .await
        .unwrap();

    // Resume — should detect and apply the cancel signal
    let status = runner.resume(&workflow, "inst-cancel").await.unwrap();
    match status {
        WorkflowStatus::Cancelled {
            reason,
            cancelled_by,
        } => {
            assert_eq!(reason, Some("testing cancel".into()));
            assert_eq!(cancelled_by, Some("test-suite".into()));
        }
        other => panic!("Expected Cancelled, got {other:?}"),
    }
}

#[tokio::test]
async fn pause_unpause_resume() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .then("step2", |i: u32| async move { Ok(i * 2) })
        .build()
        .unwrap();

    // Create an in-progress snapshot
    let input_bytes = Arc::new(JsonCodec).encode(&5u32).unwrap();
    let mut snapshot = sayiir_core::snapshot::WorkflowSnapshot::with_initial_input(
        "inst-pause".into(),
        workflow.definition_hash().to_string(),
        input_bytes,
    );
    snapshot.update_position(sayiir_core::snapshot::ExecutionPosition::AtTask {
        task_id: "step1".into(),
    });
    runner.backend().save_snapshot(&snapshot).await.unwrap();

    // Request pause via WorkflowClient
    let client = WorkflowClient::from_shared(Arc::clone(runner.backend()));
    client
        .pause("inst-pause", Some("maintenance".into()), Some("ops".into()))
        .await
        .unwrap();

    // Resume should detect pause
    let status = runner.resume(&workflow, "inst-pause").await.unwrap();
    match &status {
        WorkflowStatus::Paused { reason, paused_by } => {
            assert_eq!(reason.as_deref(), Some("maintenance"));
            assert_eq!(paused_by.as_deref(), Some("ops"));
        }
        other => panic!("Expected Paused, got {other:?}"),
    }

    // Unpause via WorkflowClient
    client.unpause("inst-pause").await.unwrap();

    // Resume should now continue execution
    let status = runner.resume(&workflow, "inst-pause").await.unwrap();
    assert!(
        matches!(status, WorkflowStatus::Completed),
        "Expected Completed after unpause, got {status:?}"
    );
}

#[tokio::test]
async fn delay_returns_waiting_then_completes() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .delay("short_wait", std::time::Duration::from_millis(1))
        .then("step2", |i: u32| async move { Ok(i * 2) })
        .build()
        .unwrap();

    // Run — should park at the delay
    let status = runner.run(&workflow, "inst-1", 10u32).await.unwrap();
    match &status {
        WorkflowStatus::Waiting { delay_id, .. } => {
            assert_eq!(delay_id, "short_wait");
        }
        other => panic!("Expected Waiting, got {other:?}"),
    }

    // step1 should be completed
    let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.get_task_result("step1").is_some());

    // Wait for delay to expire
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Resume — delay expired, should complete
    let status = runner.resume(&workflow, "inst-1").await.unwrap();
    assert!(
        matches!(status, WorkflowStatus::Completed),
        "Expected Completed after delay expired, got {status:?}"
    );

    let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_completed());
}

#[tokio::test]
async fn fork_join() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("prepare", |i: u32| async move { Ok(i) })
        .branches(|b| {
            b.add("double", |i: u32| async move { Ok(i * 2) });
            b.add("add_ten", |i: u32| async move { Ok(i + 10) });
        })
        .join("combine", |outputs: BranchOutputs<JsonCodec>| async move {
            let doubled: u32 = outputs.get_by_id("double")?;
            let added: u32 = outputs.get_by_id("add_ten")?;
            Ok(doubled + added)
        })
        .build()
        .unwrap();

    let status = runner.run(&workflow, "inst-1", 5u32).await.unwrap();
    // prepare: 5, double: 10, add_ten: 15, combine: 10+15=25
    assert!(matches!(status, WorkflowStatus::Completed));

    let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_completed());
    assert!(snapshot.final_output_bytes().is_some());
}

#[tokio::test]
async fn task_failure_persists() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .then("explode", |_i: u32| async move {
            Err::<u32, BoxError>("boom".into())
        })
        .build()
        .unwrap();

    let status = runner.run(&workflow, "inst-1", 1u32).await.unwrap();
    match &status {
        WorkflowStatus::Failed(msg) => {
            assert!(msg.contains("boom"), "Expected 'boom' in error, got: {msg}");
        }
        other => panic!("Expected Failed, got {other:?}"),
    }

    // Snapshot is persisted as failed in Postgres
    let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_failed());
    // Failed state preserves the error message
    match &snapshot.state {
        sayiir_core::snapshot::WorkflowSnapshotState::Failed { error } => {
            assert!(
                error.contains("boom"),
                "Expected 'boom' in error, got: {error}"
            );
        }
        other => panic!("Expected Failed state, got {other:?}"),
    }
}

// ─── 11. multi_stage_scatter_gather ───────────────────────────────────────────
//
// A two-stage scatter-gather pipeline inspired by MapReduce / distributed
// aggregation. Exercises:
//   - Sequential chaining between fork-join stages
//   - 4-way parallel fork (scatter) + join (reduce)
//   - 2-way fork after an intermediate transform
//   - End-to-end numerical verification across the full pipeline
//
// Pipeline:
//
//   validate(n=10)
//     ├── fibonacci(10)  = 55
//     ├── triangular(10) = 55
//     ├── square(10)     = 100
//     └── double(10)     = 20
//   reduce: 55 + 55 + 100 + 20 = 230
//   transform: 230 + 1 = 231
//     ├── halve:   231 / 2   = 115
//     └── modulo:  231 % 256 = 231
//   recombine: 115 * 1000 + 231 = 115_231
//   finalize:  115_231 * 3      = 345_693

fn fibonacci(n: u64) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n {
        let tmp = a + b;
        a = b;
        b = tmp;
    }
    a
}

#[tokio::test]
async fn multi_stage_scatter_gather() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        // ── Stage 0: validate ─────────────────────────────────────────
        .then("validate", |n: u64| async move {
            assert!(n <= 20, "input too large for test");
            Ok(n)
        })
        // ── Stage 1: 4-way scatter ───────────────────────────────────
        .branches(|b| {
            b.add("fibonacci", |n: u64| async move { Ok(fibonacci(n)) });
            b.add("triangular", |n: u64| async move { Ok(n * (n + 1) / 2) });
            b.add("square", |n: u64| async move { Ok(n * n) });
            b.add("double", |n: u64| async move { Ok(n * 2) });
        })
        .join("reduce", |outputs: BranchOutputs<JsonCodec>| async move {
            let fib: u64 = outputs.get_by_id("fibonacci")?;
            let tri: u64 = outputs.get_by_id("triangular")?;
            let sq: u64 = outputs.get_by_id("square")?;
            let dbl: u64 = outputs.get_by_id("double")?;
            Ok(fib + tri + sq + dbl)
        })
        // ── Stage 2: intermediate transform ──────────────────────────
        .then("transform", |sum: u64| async move { Ok(sum + 1) })
        // ── Stage 3: 2-way split ─────────────────────────────────────
        .branches(|b| {
            b.add("halve", |v: u64| async move { Ok(v / 2) });
            b.add("modulo", |v: u64| async move { Ok(v % 256) });
        })
        .join(
            "recombine",
            |outputs: BranchOutputs<JsonCodec>| async move {
                let half: u64 = outputs.get_by_id("halve")?;
                let rem: u64 = outputs.get_by_id("modulo")?;
                Ok(half * 1000 + rem)
            },
        )
        // ── Stage 4: finalize ────────────────────────────────────────
        .then("finalize", |v: u64| async move { Ok(v * 3) })
        .build()
        .unwrap();

    let status = runner.run(&workflow, "inst-1", 10u64).await.unwrap();
    assert!(
        matches!(status, WorkflowStatus::Completed),
        "Expected Completed, got {status:?}"
    );

    // Verify the final output matches our hand-computed expectation
    let snapshot = runner.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_completed());

    let output_bytes = snapshot.final_output_bytes().unwrap();
    let result: u64 = serde_json::from_slice(&output_bytes).unwrap();

    // fib(10)=55, tri=55, sq=100, dbl=20 → sum=230
    // transform=231, halve=115, mod=231, recombine=115_231, finalize=345_693
    assert_eq!(
        result, 345_693,
        "Pipeline produced {result}, expected 345693"
    );
}

// ─── 12. signal_parks_then_resumes_with_payload ──────────────────────────────
//
// step1 → wait_for_signal("approval") → step2
// Run parks at signal. Send event. Resume picks up event payload.

#[tokio::test]
async fn signal_parks_then_resumes_with_payload() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .wait_for_signal("sig_approval", "approval", None)
        .then("step2", |i: u32| async move { Ok(i * 10) })
        .build()
        .unwrap();

    // Run — step1 completes, parks at signal
    let status = runner.run(&workflow, "inst-sig-1", 5u32).await.unwrap();
    match &status {
        WorkflowStatus::AwaitingSignal { signal_name, .. } => {
            assert_eq!(signal_name, "approval");
        }
        other => panic!("Expected AwaitingSignal, got {other:?}"),
    }

    // Verify step1 completed
    let snapshot = runner.backend().load_snapshot("inst-sig-1").await.unwrap();
    assert!(snapshot.get_task_result("step1").is_some());

    // Send the signal with a payload
    let payload = Arc::new(JsonCodec).encode(&42u32).unwrap();
    runner
        .backend()
        .send_event("inst-sig-1", "approval", payload)
        .await
        .unwrap();

    // Resume — signal consumed, step2 runs with signal payload
    let status = runner.resume(&workflow, "inst-sig-1").await.unwrap();
    assert!(
        matches!(status, WorkflowStatus::Completed),
        "Expected Completed after signal resume, got {status:?}"
    );

    let snapshot = runner.backend().load_snapshot("inst-sig-1").await.unwrap();
    assert!(snapshot.state.is_completed());

    // Final output: signal payload 42 → step2(42*10) = 420
    let output = snapshot.final_output_bytes().unwrap();
    let result: u32 = serde_json::from_slice(&output).unwrap();
    assert_eq!(result, 420, "Expected 420, got {result}");
}

// ─── 13. signal_buffered_before_park ─────────────────────────────────────────
//
// Send the signal BEFORE the workflow reaches the wait_for_signal node.
// The executor should consume it immediately without parking.

#[tokio::test]
async fn signal_buffered_before_park() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .wait_for_signal("sig_early", "early_signal", None)
        .then("step1", |i: u32| async move { Ok(i + 100) })
        .build()
        .unwrap();

    // Buffer the signal before running
    let payload = Arc::new(JsonCodec).encode(&7u32).unwrap();
    runner
        .backend()
        .send_event("inst-sig-2", "early_signal", payload)
        .await
        .unwrap();

    // Run — signal already buffered, should consume immediately and continue
    let status = runner.run(&workflow, "inst-sig-2", 0u32).await.unwrap();
    assert!(
        matches!(status, WorkflowStatus::Completed),
        "Expected Completed (buffered signal), got {status:?}"
    );

    let snapshot = runner.backend().load_snapshot("inst-sig-2").await.unwrap();
    assert!(snapshot.state.is_completed());

    // Output: signal payload 7 → step1(7+100) = 107
    let output = snapshot.final_output_bytes().unwrap();
    let result: u32 = serde_json::from_slice(&output).unwrap();
    assert_eq!(result, 107, "Expected 107, got {result}");
}

// ─── 10. definition_hash_mismatch_on_resume ──────────────────────────────────

#[tokio::test]
async fn definition_hash_mismatch_on_resume() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow_v1 = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // Create an in-progress snapshot with v1's hash
    let input_bytes = Arc::new(JsonCodec).encode(&5u32).unwrap();
    let mut snapshot = sayiir_core::snapshot::WorkflowSnapshot::with_initial_input(
        "inst-mismatch".into(),
        workflow_v1.definition_hash().to_string(),
        input_bytes,
    );
    snapshot.update_position(sayiir_core::snapshot::ExecutionPosition::AtTask {
        task_id: "step1".into(),
    });
    runner.backend().save_snapshot(&snapshot).await.unwrap();

    // Build a different workflow (v2 — different hash)
    let workflow_v2 = WorkflowBuilder::new(ctx())
        .then("step1", |i: u32| async move { Ok(i + 1) })
        .then("step2", |i: u32| async move { Ok(i * 2) })
        .build()
        .unwrap();

    // Resume with v2 against v1's snapshot → hash mismatch
    let result = runner.resume(&workflow_v2, "inst-mismatch").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("mismatch"),
        "Expected 'mismatch' in error, got: {err_msg}"
    );
}

// ─── 11. task_version_change_causes_hash_mismatch ────────────────────────────

#[tokio::test]
async fn task_version_change_causes_hash_mismatch() {
    use sayiir_core::task::TaskMetadata;

    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    // v1: task with version "1.0"
    let workflow_v1 = WorkflowBuilder::new(ctx())
        .with_registry()
        .then("process", |i: u32| async move { Ok(i + 1) })
        .with_metadata(TaskMetadata {
            version: Some("1.0".into()),
            ..Default::default()
        })
        .build()
        .unwrap();

    // Create an in-progress snapshot with v1's hash
    let input_bytes = Arc::new(JsonCodec).encode(&5u32).unwrap();
    let mut snapshot = sayiir_core::snapshot::WorkflowSnapshot::with_initial_input(
        "inst-version".into(),
        workflow_v1.definition_hash().to_string(),
        input_bytes,
    );
    snapshot.update_position(sayiir_core::snapshot::ExecutionPosition::AtTask {
        task_id: "process".into(),
    });
    runner.backend().save_snapshot(&snapshot).await.unwrap();

    // v2: same structure, same task ID, but different version
    let workflow_v2 = WorkflowBuilder::new(ctx())
        .with_registry()
        .then("process", |i: u32| async move { Ok(i + 1) })
        .with_metadata(TaskMetadata {
            version: Some("2.0".into()),
            ..Default::default()
        })
        .build()
        .unwrap();

    // Hashes must differ
    assert_ne!(
        workflow_v1.definition_hash(),
        workflow_v2.definition_hash(),
        "Changing task version should change the definition hash"
    );

    // Resume with v2 against v1's snapshot → hash mismatch
    let result = runner.resume(workflow_v2.workflow(), "inst-version").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("mismatch"),
        "Expected 'mismatch' in error, got: {err_msg}"
    );
}

// ─── 12. task_version_none_vs_some_causes_hash_mismatch ──────────────────────

#[tokio::test]
async fn task_version_none_vs_some_causes_hash_mismatch() {
    use sayiir_core::task::TaskMetadata;

    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    // v1: no version set
    let workflow_v1 = WorkflowBuilder::new(ctx())
        .then("process", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // v2: same structure but version added
    let workflow_v2 = WorkflowBuilder::new(ctx())
        .with_registry()
        .then("process", |i: u32| async move { Ok(i + 1) })
        .with_metadata(TaskMetadata {
            version: Some("1.0".into()),
            ..Default::default()
        })
        .build()
        .unwrap();

    assert_ne!(
        workflow_v1.definition_hash(),
        workflow_v2.definition_hash(),
        "Adding a version where there was none should change the hash"
    );

    // Create snapshot with v1
    let input_bytes = Arc::new(JsonCodec).encode(&5u32).unwrap();
    let mut snapshot = sayiir_core::snapshot::WorkflowSnapshot::with_initial_input(
        "inst-version-none".into(),
        workflow_v1.definition_hash().to_string(),
        input_bytes,
    );
    snapshot.update_position(sayiir_core::snapshot::ExecutionPosition::AtTask {
        task_id: "process".into(),
    });
    runner.backend().save_snapshot(&snapshot).await.unwrap();

    // Resume with v2 → mismatch
    let result = runner
        .resume(workflow_v2.workflow(), "inst-version-none")
        .await;
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("mismatch"),
        "Expected definition mismatch error"
    );
}

// ─── 13. same_task_version_allows_resume ─────────────────────────────────────

#[tokio::test]
async fn same_task_version_allows_resume() {
    use sayiir_core::task::TaskMetadata;

    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    // Run workflow to completion with version "1.0"
    let workflow = WorkflowBuilder::new(ctx())
        .with_registry()
        .then("process", |i: u32| async move { Ok(i + 1) })
        .with_metadata(TaskMetadata {
            version: Some("1.0".into()),
            ..Default::default()
        })
        .build()
        .unwrap();

    let status = runner
        .run(workflow.workflow(), "inst-same-ver", 10u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    // Resume with same version — should succeed (already completed)
    let workflow_same = WorkflowBuilder::new(ctx())
        .with_registry()
        .then("process", |i: u32| async move { Ok(i + 1) })
        .with_metadata(TaskMetadata {
            version: Some("1.0".into()),
            ..Default::default()
        })
        .build()
        .unwrap();

    assert_eq!(
        workflow.definition_hash(),
        workflow_same.definition_hash(),
        "Same version should produce same hash"
    );

    let status = runner
        .resume(workflow_same.workflow(), "inst-same-ver")
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── ConflictPolicy tests ─────────────────────────────────────────────────

#[tokio::test]
async fn run_fail_policy_rejects_duplicate() {
    let (_c, backend, _url) = setup().await;
    let runner = CheckpointingRunner::new(backend).with_conflict_policy(ConflictPolicy::Fail);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // First run succeeds
    let status = runner.run(&workflow, "inst-dup", 5u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    // Second run with same instance_id should fail
    let err = runner.run(&workflow, "inst-dup", 5u32).await.unwrap_err();
    assert!(
        matches!(err, RuntimeError::InstanceAlreadyExists(ref id) if id == "inst-dup"),
        "expected InstanceAlreadyExists error, got: {err}"
    );
}

#[tokio::test]
async fn run_use_existing_returns_completed() {
    let (_c, backend, _url) = setup().await;
    let backend = backend.clone();
    let runner =
        CheckpointingRunner::new(backend.clone()).with_conflict_policy(ConflictPolicy::UseExisting);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // First run: 5 + 1 = 6
    let status = runner.run(&workflow, "inst-reuse", 5u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    // Second run with different input — UseExisting should NOT re-execute
    let status = runner.run(&workflow, "inst-reuse", 99u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    // The stored output should still be 6 (from the first run), not 100
    let snapshot = backend.load_snapshot("inst-reuse").await.unwrap();
    let output_bytes = snapshot.state.completed_output().unwrap().clone();
    let output: u32 = JsonCodec.decode(output_bytes).unwrap();
    assert_eq!(
        output, 6,
        "UseExisting should not re-execute; stored output should remain 6"
    );
}

#[tokio::test]
async fn run_terminate_existing_restarts() {
    let (_c, backend, _url) = setup().await;
    let backend = backend.clone();
    let runner = CheckpointingRunner::new(backend.clone())
        .with_conflict_policy(ConflictPolicy::TerminateExisting);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // First run: 5 + 1 = 6
    let status = runner.run(&workflow, "inst-restart", 5u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    let snapshot = backend.load_snapshot("inst-restart").await.unwrap();
    let first_output: u32 = JsonCodec
        .decode(snapshot.state.completed_output().unwrap().clone())
        .unwrap();
    assert_eq!(first_output, 6);

    // Second run with different input — TerminateExisting should re-execute: 10 + 1 = 11
    let status = runner.run(&workflow, "inst-restart", 10u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    let snapshot = backend.load_snapshot("inst-restart").await.unwrap();
    let second_output: u32 = JsonCodec
        .decode(snapshot.state.completed_output().unwrap().clone())
        .unwrap();
    assert_eq!(
        second_output, 11,
        "TerminateExisting should re-execute with new input; expected 11, got {second_output}"
    );
}

// ─── WorkflowClient tests ────────────────────────────────────────────────────

// ─── client: submit creates snapshot ─────────────────────────────────────────

#[tokio::test]
async fn client_submit_creates_snapshot() {
    let (_c, backend, _url) = setup().await;
    let client = WorkflowClient::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    let (status, output) = client.submit(&workflow, "inst-1", 5u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::InProgress));
    assert!(output.is_none());

    // Verify snapshot was created
    let snapshot = client.backend().load_snapshot("inst-1").await.unwrap();
    assert!(!snapshot.state.is_completed());
}

// ─── client: Fail policy rejects duplicate ───────────────────────────────────

#[tokio::test]
async fn client_submit_fail_policy_rejects_duplicate() {
    let (_c, backend, _url) = setup().await;
    let client = WorkflowClient::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // First submit succeeds
    client.submit(&workflow, "inst-1", 5u32).await.unwrap();

    // Second submit with default Fail policy should error
    let result = client.submit(&workflow, "inst-1", 10u32).await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::InstanceAlreadyExists(_)
    ));
}

// ─── client: UseExisting returns status ──────────────────────────────────────

#[tokio::test]
async fn client_submit_use_existing_returns_status() {
    let (_c, backend, _url) = setup().await;
    let client = WorkflowClient::new(backend).with_conflict_policy(ConflictPolicy::UseExisting);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // First submit
    client.submit(&workflow, "inst-1", 5u32).await.unwrap();

    // Second submit with UseExisting should return current status
    let (status, _output) = client.submit(&workflow, "inst-1", 10u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::InProgress));
}

// ─── client: TerminateExisting recreates ─────────────────────────────────────

#[tokio::test]
async fn client_submit_terminate_existing_restarts() {
    let (_c, backend, _url) = setup().await;
    let client =
        WorkflowClient::new(backend).with_conflict_policy(ConflictPolicy::TerminateExisting);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // First submit
    client.submit(&workflow, "inst-1", 5u32).await.unwrap();

    // Second submit with TerminateExisting should succeed (recreate)
    let (status, _output) = client.submit(&workflow, "inst-1", 10u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::InProgress));
}

// ─── client: cancel stores signal ────────────────────────────────────────────

#[tokio::test]
async fn client_cancel_stores_signal() {
    let (_c, backend, _url) = setup().await;
    let client = WorkflowClient::new(backend);

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .build()
        .unwrap();

    // Submit first
    client.submit(&workflow, "inst-1", 5u32).await.unwrap();

    // Cancel
    client
        .cancel("inst-1", Some("testing".into()), Some("admin".into()))
        .await
        .unwrap();

    // Verify signal was stored
    use sayiir_core::snapshot::SignalKind;
    let req = client
        .backend()
        .get_signal("inst-1", SignalKind::Cancel)
        .await
        .unwrap();
    assert!(req.is_some());
    assert_eq!(req.unwrap().reason, Some("testing".into()));
}

// ─── client: submit then runner executes ─────────────────────────────────────

#[tokio::test]
async fn client_submit_then_runner_executes() {
    let (_c, backend, _url) = setup().await;
    let client = WorkflowClient::from_shared(Arc::new(backend));

    let workflow = WorkflowBuilder::new(ctx())
        .then("add_one", |i: u32| async move { Ok(i + 1) })
        .then("double", |i: u32| async move { Ok(i * 2) })
        .build()
        .unwrap();

    // Submit via client
    let (status, _) = client.submit(&workflow, "inst-1", 5u32).await.unwrap();
    assert!(matches!(status, WorkflowStatus::InProgress));

    // Resume via CheckpointingRunner (simulating what a worker does)
    let runner = CheckpointingRunner::from_shared(Arc::clone(client.backend()));
    let status = runner.resume(&workflow, "inst-1").await.unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    // Verify output
    let snapshot = client.backend().load_snapshot("inst-1").await.unwrap();
    assert!(snapshot.state.is_completed());
    let codec = Arc::new(JsonCodec);
    let output: u32 = codec
        .decode(snapshot.state.completed_output().unwrap().clone())
        .unwrap();
    assert_eq!(output, 12); // (5+1)*2
}

// ─── multi-worker: two workers collaborate on workflow instances ──────────────

/// Two PooledWorkers running in parallel against the same Postgres backend,
/// collaboratively processing multiple workflow instances submitted via
/// WorkflowClient.
#[tokio::test]
async fn two_workers_collaborate_on_workflows() {
    let (_c, _backend, url) = setup().await;

    // Each worker needs its own backend connection (just like production).
    let backend_w1 = PostgresBackend::<JsonCodec>::connect(&url).await.unwrap();
    let backend_w2 = PostgresBackend::<JsonCodec>::connect(&url).await.unwrap();
    let backend_client = PostgresBackend::<JsonCodec>::connect(&url).await.unwrap();

    let workflow = WorkflowBuilder::new(ctx())
        .then("step_a", |i: u32| async move { Ok(i + 1) })
        .then("step_b", |i: u32| async move { Ok(i * 2) })
        .then("step_c", |i: u32| async move { Ok(i + 10) })
        .build()
        .unwrap();

    let wf = Arc::new(workflow);
    let def_hash = wf.definition_hash().to_string();

    // Submit 6 workflow instances via the client.
    let client = WorkflowClient::new(backend_client);
    for i in 0u32..6 {
        let (status, _) = client
            .submit(wf.as_ref(), format!("inst-{i}"), i)
            .await
            .unwrap();
        assert!(matches!(status, WorkflowStatus::InProgress));
    }

    // Spawn two workers with short poll intervals.
    let registry1 = sayiir_core::registry::TaskRegistry::new();
    let registry2 = sayiir_core::registry::TaskRegistry::new();

    let workflows1 = vec![(def_hash.clone(), Arc::clone(&wf))];
    let workflows2 = vec![(def_hash.clone(), Arc::clone(&wf))];

    let worker1 = PooledWorker::new("worker-1", backend_w1, registry1)
        .with_claim_ttl(Some(Duration::from_secs(30)));
    let worker2 = PooledWorker::new("worker-2", backend_w2, registry2)
        .with_claim_ttl(Some(Duration::from_secs(30)));

    let handle1 = worker1.spawn(Duration::from_millis(100), workflows1);
    let handle2 = worker2.spawn(Duration::from_millis(100), workflows2);

    // Poll until all instances are completed (or timeout).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if tokio::time::Instant::now() > deadline {
            panic!("Timed out waiting for all workflow instances to complete");
        }

        let mut all_done = true;
        for i in 0u32..6 {
            let snapshot = client
                .backend()
                .load_snapshot(&format!("inst-{i}"))
                .await
                .unwrap();
            if !snapshot.state.is_completed() {
                all_done = false;
                break;
            }
        }
        if all_done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Verify outputs: input i → (i+1)*2+10
    let codec = Arc::new(JsonCodec);
    for i in 0u32..6 {
        let snapshot = client
            .backend()
            .load_snapshot(&format!("inst-{i}"))
            .await
            .unwrap();
        assert!(
            snapshot.state.is_completed(),
            "inst-{i} should be completed"
        );
        let output: u32 = codec
            .decode(snapshot.state.completed_output().unwrap().clone())
            .unwrap();
        let expected = (i + 1) * 2 + 10;
        assert_eq!(
            output, expected,
            "inst-{i}: expected {expected}, got {output}"
        );
    }

    // Shutdown both workers.
    handle1.shutdown();
    handle2.shutdown();
    tokio::time::timeout(Duration::from_secs(5), handle1.join())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle2.join())
        .await
        .unwrap()
        .unwrap();
}
