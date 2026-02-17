#![allow(clippy::unwrap_used, clippy::expect_used)]

use sayiir_core::codec::Encoder;
use sayiir_core::context::WorkflowContext;
use sayiir_core::error::BoxError;
use sayiir_core::task::BranchOutputs;
use sayiir_core::workflow::{WorkflowBuilder, WorkflowStatus};
use sayiir_persistence::{SignalStore, SnapshotStore};
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::CheckpointingRunner;
use sayiir_runtime::serialization::JsonCodec;
use sqlx::PgPool;
use std::sync::Arc;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

async fn setup() -> (
    testcontainers::ContainerAsync<Postgres>,
    PostgresBackend<JsonCodec>,
    String,
) {
    let container = Postgres::default()
        .with_tag("17-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgresql://postgres:postgres@127.0.0.1:{port}/postgres");
    let pool = PgPool::connect(&url).await.unwrap();
    let backend = PostgresBackend::<JsonCodec>::connect_with(pool)
        .await
        .unwrap();
    (container, backend, url)
}

fn ctx() -> WorkflowContext<JsonCodec, ()> {
    WorkflowContext::new("test-wf", Arc::new(JsonCodec), Arc::new(()))
}

// ─── 1. run_single_task ──────────────────────────────────────────────────────

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

// ─── 2. run_chained_tasks ────────────────────────────────────────────────────

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

// ─── 3. resume_completed_is_noop ─────────────────────────────────────────────

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

// ─── 4. resume_after_simulated_crash ─────────────────────────────────────────

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

// ─── 5. cancel_and_resume ────────────────────────────────────────────────────

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

    // Signal cancellation
    runner
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

// ─── 6. pause_unpause_resume ─────────────────────────────────────────────────

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

    // Request pause
    runner
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

    // Unpause
    let unpaused = runner.unpause("inst-pause").await.unwrap();
    assert!(unpaused.state.is_in_progress());

    // Resume should now continue execution
    let status = runner.resume(&workflow, "inst-pause").await.unwrap();
    assert!(
        matches!(status, WorkflowStatus::Completed),
        "Expected Completed after unpause, got {status:?}"
    );
}

// ─── 7. delay_returns_waiting_then_completes ─────────────────────────────────

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

// ─── 8. fork_join ────────────────────────────────────────────────────────────

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
            let doubled: u32 = outputs.get("double")?;
            let added: u32 = outputs.get("add_ten")?;
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

// ─── 9. task_failure_persists ────────────────────────────────────────────────

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
            let fib: u64 = outputs.get("fibonacci")?;
            let tri: u64 = outputs.get("triangular")?;
            let sq: u64 = outputs.get("square")?;
            let dbl: u64 = outputs.get("double")?;
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
                let half: u64 = outputs.get("halve")?;
                let rem: u64 = outputs.get("modulo")?;
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
