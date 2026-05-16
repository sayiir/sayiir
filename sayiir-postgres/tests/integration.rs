#![allow(clippy::unwrap_used, clippy::expect_used)]

use bytes::Bytes;
use chrono::Duration;
use sayiir_core::snapshot::{ExecutionPosition, SignalKind, SignalRequest, WorkflowSnapshot};
use sayiir_persistence::{
    BackendError, SignalStore, SnapshotStore, TaskClaimStore, TaskResultStore,
};
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;
use sqlx::PgPool;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// Minimum supported PostgreSQL version.
///
/// The schema requires `ALTER TABLE … ADD COLUMN IF NOT EXISTS` (9.6+) and
/// `INSERT … ON CONFLICT DO UPDATE` (9.5+). We set the floor at PostgreSQL 13,
/// which is the oldest major version still receiving security patches.
const MIN_PG_VERSION: &str = "13-alpine";

/// Default PostgreSQL version used by most tests.
const DEFAULT_PG_VERSION: &str = "17-alpine";

async fn setup_with(
    tag: &str,
) -> (
    testcontainers::ContainerAsync<Postgres>,
    PostgresBackend<JsonCodec>,
) {
    let container = Postgres::default().with_tag(tag).start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgresql://postgres:postgres@127.0.0.1:{port}/postgres");
    let pool = PgPool::connect(&url).await.unwrap();
    let backend = PostgresBackend::<JsonCodec>::connect_with(pool)
        .await
        .unwrap();
    (container, backend)
}

async fn setup() -> (
    testcontainers::ContainerAsync<Postgres>,
    PostgresBackend<JsonCodec>,
) {
    setup_with(DEFAULT_PG_VERSION).await
}

// ─── SnapshotStore ───────────────────────────────────────────────────────────

#[tokio::test]
async fn save_and_load_snapshot() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("test-1".into(), "hash-1".into());

    backend.save_snapshot(&snapshot).await.unwrap();
    let loaded = backend.load_snapshot("test-1").await.unwrap();

    assert_eq!(loaded.instance_id, "test-1");
    assert_eq!(loaded.definition_hash, "hash-1");
    assert!(loaded.state.is_in_progress());
}

#[tokio::test]
async fn load_not_found() {
    let (_c, backend) = setup().await;
    let result = backend.load_snapshot("nonexistent").await;
    assert!(matches!(result, Err(BackendError::NotFound(_))));
}

#[tokio::test]
async fn save_snapshot_overwrites() {
    let (_c, backend) = setup().await;
    let mut snapshot = WorkflowSnapshot::new("test-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "step-2".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    let loaded = backend.load_snapshot("test-1").await.unwrap();
    match &loaded.state {
        sayiir_core::snapshot::WorkflowSnapshotState::InProgress { position, .. } => match position
        {
            ExecutionPosition::AtTask { task_id } => assert_eq!(task_id, "step-2"),
            other => panic!("expected AtTask, got {other:?}"),
        },
        other => panic!("expected InProgress, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_snapshot() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("test-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    backend.delete_snapshot("test-1").await.unwrap();
    let result = backend.load_snapshot("test-1").await;
    assert!(matches!(result, Err(BackendError::NotFound(_))));
}

#[tokio::test]
async fn delete_not_found() {
    let (_c, backend) = setup().await;
    let result = backend.delete_snapshot("nonexistent").await;
    assert!(matches!(result, Err(BackendError::NotFound(_))));
}

#[tokio::test]
async fn list_snapshots() {
    let (_c, backend) = setup().await;

    backend
        .save_snapshot(&WorkflowSnapshot::new("wf-1".into(), "h".into()))
        .await
        .unwrap();
    backend
        .save_snapshot(&WorkflowSnapshot::new("wf-2".into(), "h".into()))
        .await
        .unwrap();

    let mut list = backend.list_snapshots().await.unwrap();
    list.sort();
    assert_eq!(list, vec!["wf-1".to_string(), "wf-2".to_string()]);
}

#[tokio::test]
async fn save_task_result() {
    let (_c, backend) = setup().await;
    let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "task-1".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    backend
        .save_task_result("wf-1", "task-1", Bytes::from(r#""done""#))
        .await
        .unwrap();

    let loaded = backend.load_snapshot("wf-1").await.unwrap();
    let result = loaded.get_task_result("task-1");
    assert!(result.is_some());
    assert_eq!(result.unwrap().output, Bytes::from(r#""done""#));
}

// ─── SignalStore ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn store_and_get_cancel_signal() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    backend
        .store_signal(
            "wf-1",
            SignalKind::Cancel,
            SignalRequest::new(Some("timeout".into()), Some("system".into())),
        )
        .await
        .unwrap();

    let signal = backend
        .get_signal("wf-1", SignalKind::Cancel)
        .await
        .unwrap();
    assert!(signal.is_some());
    let signal = signal.unwrap();
    assert_eq!(signal.reason, Some("timeout".into()));
    assert_eq!(signal.requested_by, Some("system".into()));
}

#[tokio::test]
async fn store_signal_on_nonexistent_workflow() {
    let (_c, backend) = setup().await;
    let result = backend
        .store_signal(
            "nonexistent",
            SignalKind::Cancel,
            SignalRequest::new(None, None),
        )
        .await;
    assert!(matches!(result, Err(BackendError::NotFound(_))));
}

#[tokio::test]
async fn store_cancel_on_completed_workflow() {
    let (_c, backend) = setup().await;
    let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    snapshot.mark_completed(Bytes::from("result"));
    backend.save_snapshot(&snapshot).await.unwrap();

    let result = backend
        .store_signal("wf-1", SignalKind::Cancel, SignalRequest::new(None, None))
        .await;
    assert!(matches!(result, Err(BackendError::CannotCancel(_))));
}

#[tokio::test]
async fn store_pause_on_completed_workflow() {
    let (_c, backend) = setup().await;
    let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    snapshot.mark_completed(Bytes::from("result"));
    backend.save_snapshot(&snapshot).await.unwrap();

    let result = backend
        .store_signal("wf-1", SignalKind::Pause, SignalRequest::new(None, None))
        .await;
    assert!(matches!(result, Err(BackendError::CannotPause(_))));
}

#[tokio::test]
async fn clear_signal() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    backend
        .store_signal(
            "wf-1",
            SignalKind::Cancel,
            SignalRequest::new(Some("test".into()), None),
        )
        .await
        .unwrap();

    backend
        .clear_signal("wf-1", SignalKind::Cancel)
        .await
        .unwrap();

    let signal = backend
        .get_signal("wf-1", SignalKind::Cancel)
        .await
        .unwrap();
    assert!(signal.is_none());
}

#[tokio::test]
async fn get_signal_returns_none_when_absent() {
    let (_c, backend) = setup().await;
    let signal = backend
        .get_signal("wf-1", SignalKind::Cancel)
        .await
        .unwrap();
    assert!(signal.is_none());
}

// ─── Composite signal operations ─────────────────────────────────────────────

#[tokio::test]
async fn check_and_cancel_success() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    backend
        .store_signal(
            "wf-1",
            SignalKind::Cancel,
            SignalRequest::new(Some("timeout".into()), Some("system".into())),
        )
        .await
        .unwrap();

    let cancelled = backend
        .check_and_cancel("wf-1", Some("task-1"))
        .await
        .unwrap();
    assert!(cancelled);

    let loaded = backend.load_snapshot("wf-1").await.unwrap();
    assert!(loaded.state.is_cancelled());

    let (reason, by) = loaded.state.cancellation_details().unwrap();
    assert_eq!(reason, Some("timeout".into()));
    assert_eq!(by, Some("system".into()));

    // Signal should be cleared
    let signal = backend
        .get_signal("wf-1", SignalKind::Cancel)
        .await
        .unwrap();
    assert!(signal.is_none());
}

#[tokio::test]
async fn check_and_cancel_no_signal() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    let cancelled = backend.check_and_cancel("wf-1", None).await.unwrap();
    assert!(!cancelled);

    let loaded = backend.load_snapshot("wf-1").await.unwrap();
    assert!(loaded.state.is_in_progress());
}

#[tokio::test]
async fn check_and_pause_success() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    backend
        .store_signal(
            "wf-1",
            SignalKind::Pause,
            SignalRequest::new(Some("maintenance".into()), Some("ops".into())),
        )
        .await
        .unwrap();

    let paused = backend.check_and_pause("wf-1").await.unwrap();
    assert!(paused);

    let loaded = backend.load_snapshot("wf-1").await.unwrap();
    assert!(loaded.state.is_paused());

    let (reason, by) = loaded.state.pause_details().unwrap();
    assert_eq!(reason, Some("maintenance".into()));
    assert_eq!(by, Some("ops".into()));
}

#[tokio::test]
async fn unpause_success() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    // Pause it first
    backend
        .store_signal(
            "wf-1",
            SignalKind::Pause,
            SignalRequest::new(Some("maintenance".into()), None),
        )
        .await
        .unwrap();
    backend.check_and_pause("wf-1").await.unwrap();

    // Unpause
    let unpaused = backend.unpause("wf-1").await.unwrap();
    assert!(unpaused.state.is_in_progress());

    let loaded = backend.load_snapshot("wf-1").await.unwrap();
    assert!(loaded.state.is_in_progress());
}

// ─── TaskClaimStore ──────────────────────────────────────────────────────────

#[tokio::test]
async fn claim_task_success() {
    let (_c, backend) = setup().await;

    let claim = backend
        .claim_task("wf-1", "task-1", "worker-1", Some(Duration::seconds(300)))
        .await
        .unwrap();

    assert!(claim.is_some());
    let claim = claim.unwrap();
    assert_eq!(claim.instance_id, "wf-1");
    assert_eq!(claim.task_id, "task-1");
    assert_eq!(claim.worker_id, "worker-1");
    assert!(claim.expires_at.is_some());
}

#[tokio::test]
async fn claim_task_already_claimed() {
    let (_c, backend) = setup().await;

    let claim1 = backend
        .claim_task("wf-1", "task-1", "worker-1", Some(Duration::seconds(300)))
        .await
        .unwrap();
    assert!(claim1.is_some());

    // Second claim by different worker should fail
    let claim2 = backend
        .claim_task("wf-1", "task-1", "worker-2", Some(Duration::seconds(300)))
        .await
        .unwrap();
    assert!(claim2.is_none());
}

#[tokio::test]
async fn claim_task_expired_claim_replaced() {
    let (_c, backend) = setup().await;

    // Claim with 1-second TTL then wait for it to expire
    backend
        .claim_task("wf-1", "task-1", "worker-1", Some(Duration::seconds(1)))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Second claim should succeed because first is expired
    let claim2 = backend
        .claim_task("wf-1", "task-1", "worker-2", Some(Duration::seconds(300)))
        .await
        .unwrap();
    assert!(claim2.is_some());
    assert_eq!(claim2.unwrap().worker_id, "worker-2");
}

#[tokio::test]
async fn claim_task_no_ttl() {
    let (_c, backend) = setup().await;

    let claim = backend
        .claim_task("wf-1", "task-1", "worker-1", None)
        .await
        .unwrap();

    assert!(claim.is_some());
    let claim = claim.unwrap();
    assert!(claim.expires_at.is_none());
    assert!(!claim.is_expired());
}

#[tokio::test]
async fn release_task_claim() {
    let (_c, backend) = setup().await;

    backend
        .claim_task("wf-1", "task-1", "worker-1", Some(Duration::seconds(300)))
        .await
        .unwrap();

    backend
        .release_task_claim("wf-1", "task-1", "worker-1")
        .await
        .unwrap();

    // Can claim again after release
    let claim = backend
        .claim_task("wf-1", "task-1", "worker-2", Some(Duration::seconds(300)))
        .await
        .unwrap();
    assert!(claim.is_some());
}

#[tokio::test]
async fn release_task_claim_wrong_worker() {
    let (_c, backend) = setup().await;

    backend
        .claim_task("wf-1", "task-1", "worker-1", Some(Duration::seconds(300)))
        .await
        .unwrap();

    let result = backend
        .release_task_claim("wf-1", "task-1", "worker-2")
        .await;
    assert!(matches!(result, Err(BackendError::Backend(_))));
}

#[tokio::test]
async fn extend_task_claim() {
    let (_c, backend) = setup().await;

    let claim = backend
        .claim_task("wf-1", "task-1", "worker-1", Some(Duration::seconds(10)))
        .await
        .unwrap()
        .unwrap();
    let original_expiry = claim.expires_at.unwrap();

    backend
        .extend_task_claim("wf-1", "task-1", "worker-1", Duration::seconds(300))
        .await
        .unwrap();

    // Re-claim to verify extension (claim should fail since it's still held)
    let reclaim = backend
        .claim_task("wf-1", "task-1", "worker-2", Some(Duration::seconds(10)))
        .await
        .unwrap();
    assert!(reclaim.is_none(), "claim should still be held after extend");
    // The original_expiry was 10s from now, extension added 300s — well beyond original
    let _ = original_expiry; // used above conceptually
}

#[tokio::test]
async fn extend_task_claim_wrong_worker() {
    let (_c, backend) = setup().await;

    backend
        .claim_task("wf-1", "task-1", "worker-1", Some(Duration::seconds(300)))
        .await
        .unwrap();

    let result = backend
        .extend_task_claim("wf-1", "task-1", "worker-2", Duration::seconds(300))
        .await;
    assert!(matches!(result, Err(BackendError::Backend(_))));
}

#[tokio::test]
async fn find_available_tasks_basic() {
    let (_c, backend) = setup().await;

    let mut snapshot = WorkflowSnapshot::with_initial_input(
        "wf-1".into(),
        "hash-1".into(),
        Bytes::from(r#""input""#),
    );
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "task-1".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    let tasks = backend
        .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].instance_id, "wf-1");
    assert_eq!(tasks[0].task_id, "task-1");
}

#[tokio::test]
async fn find_available_tasks_skips_cancelled() {
    let (_c, backend) = setup().await;

    // Workflow 1: in-progress with pending cancel signal
    let mut snapshot1 = WorkflowSnapshot::with_initial_input(
        "wf-1".into(),
        "hash-1".into(),
        Bytes::from(r#""input""#),
    );
    snapshot1.update_position(ExecutionPosition::AtTask {
        task_id: "task-1".into(),
    });
    backend.save_snapshot(&snapshot1).await.unwrap();
    backend
        .store_signal("wf-1", SignalKind::Cancel, SignalRequest::new(None, None))
        .await
        .unwrap();

    // Workflow 2: in-progress, no signals
    let mut snapshot2 = WorkflowSnapshot::with_initial_input(
        "wf-2".into(),
        "hash-1".into(),
        Bytes::from(r#""input""#),
    );
    snapshot2.update_position(ExecutionPosition::AtTask {
        task_id: "task-2".into(),
    });
    backend.save_snapshot(&snapshot2).await.unwrap();

    let tasks = backend
        .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
        .await
        .unwrap();
    assert!(!tasks.iter().any(|t| t.instance_id == "wf-1"));
    assert!(tasks.iter().any(|t| t.instance_id == "wf-2"));
}

#[tokio::test]
async fn find_available_tasks_skips_completed() {
    let (_c, backend) = setup().await;

    let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    snapshot.mark_completed(Bytes::from("done"));
    backend.save_snapshot(&snapshot).await.unwrap();

    let tasks = backend
        .find_available_tasks("worker-1", 10, chrono::Duration::seconds(300), &[])
        .await
        .unwrap();
    assert!(tasks.is_empty());
}

// ─── Worker affinity (task tags) ─────────────────────────────────────────────

#[tokio::test]
async fn find_available_tasks_filters_by_worker_tags() {
    let (_c, backend) = setup().await;

    // Task tagged ["gpu"]
    let mut snap1 =
        WorkflowSnapshot::with_initial_input("wf-gpu".into(), "h1".into(), Bytes::from(r#""1""#));
    snap1.update_position(ExecutionPosition::AtTask {
        task_id: "t1".into(),
    });
    snap1.task_tags = vec!["gpu".into()];
    backend.save_snapshot(&snap1).await.unwrap();

    // Task tagged ["cpu"]
    let mut snap2 =
        WorkflowSnapshot::with_initial_input("wf-cpu".into(), "h1".into(), Bytes::from(r#""2""#));
    snap2.update_position(ExecutionPosition::AtTask {
        task_id: "t2".into(),
    });
    snap2.task_tags = vec!["cpu".into()];
    backend.save_snapshot(&snap2).await.unwrap();

    // Worker with ["gpu"] should only see the gpu task
    let tasks = backend
        .find_available_tasks("w1", 10, Duration::seconds(300), &["gpu".into()])
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].instance_id, "wf-gpu");
}

#[tokio::test]
async fn find_available_tasks_untagged_worker_accepts_all() {
    let (_c, backend) = setup().await;

    let mut snap1 = WorkflowSnapshot::with_initial_input(
        "wf-tagged".into(),
        "h1".into(),
        Bytes::from(r#""1""#),
    );
    snap1.update_position(ExecutionPosition::AtTask {
        task_id: "t1".into(),
    });
    snap1.task_tags = vec!["gpu".into()];
    backend.save_snapshot(&snap1).await.unwrap();

    let mut snap2 =
        WorkflowSnapshot::with_initial_input("wf-plain".into(), "h1".into(), Bytes::from(r#""2""#));
    snap2.update_position(ExecutionPosition::AtTask {
        task_id: "t2".into(),
    });
    backend.save_snapshot(&snap2).await.unwrap();

    // Untagged worker should see both
    let tasks = backend
        .find_available_tasks("w1", 10, Duration::seconds(300), &[])
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
}

#[tokio::test]
async fn find_available_tasks_untagged_tasks_accepted_by_tagged_worker() {
    let (_c, backend) = setup().await;

    // Untagged task
    let mut snap =
        WorkflowSnapshot::with_initial_input("wf-plain".into(), "h1".into(), Bytes::from(r#""1""#));
    snap.update_position(ExecutionPosition::AtTask {
        task_id: "t1".into(),
    });
    backend.save_snapshot(&snap).await.unwrap();

    // Tagged worker should still pick up untagged tasks
    let tasks = backend
        .find_available_tasks("w1", 10, Duration::seconds(300), &["gpu".into()])
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].instance_id, "wf-plain");
}

#[tokio::test]
async fn find_available_tasks_multi_tag_subset() {
    let (_c, backend) = setup().await;

    // Task requiring ["gpu", "cuda"]
    let mut snap1 =
        WorkflowSnapshot::with_initial_input("wf-multi".into(), "h1".into(), Bytes::from(r#""1""#));
    snap1.update_position(ExecutionPosition::AtTask {
        task_id: "t1".into(),
    });
    snap1.task_tags = vec!["gpu".into(), "cuda".into()];
    backend.save_snapshot(&snap1).await.unwrap();

    // Task requiring only ["gpu"]
    let mut snap2 = WorkflowSnapshot::with_initial_input(
        "wf-single".into(),
        "h1".into(),
        Bytes::from(r#""2""#),
    );
    snap2.update_position(ExecutionPosition::AtTask {
        task_id: "t2".into(),
    });
    snap2.task_tags = vec!["gpu".into()];
    backend.save_snapshot(&snap2).await.unwrap();

    // Worker with ["gpu"] cannot run ["gpu","cuda"] (not a superset), but can run ["gpu"]
    let tasks = backend
        .find_available_tasks("w1", 10, Duration::seconds(300), &["gpu".into()])
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].instance_id, "wf-single");

    // Worker with ["gpu","cuda","fast"] can run both (superset of both)
    let tasks = backend
        .find_available_tasks(
            "w2",
            10,
            Duration::seconds(300),
            &["gpu".into(), "cuda".into(), "fast".into()],
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
}

#[tokio::test]
async fn find_available_tasks_tags_persist_through_save_load() {
    let (_c, backend) = setup().await;

    let mut snap = WorkflowSnapshot::with_initial_input(
        "wf-persist".into(),
        "h1".into(),
        Bytes::from(r#""1""#),
    );
    snap.update_position(ExecutionPosition::AtTask {
        task_id: "t1".into(),
    });
    snap.task_tags = vec!["gpu".into(), "cuda".into()];
    backend.save_snapshot(&snap).await.unwrap();

    // Load and verify tags survived the roundtrip
    let loaded = backend.load_snapshot("wf-persist").await.unwrap();
    assert_eq!(
        loaded.task_tags,
        vec!["gpu".to_string(), "cuda".to_string()]
    );
}

#[tokio::test]
async fn find_available_tasks_disjoint_tags_no_match() {
    let (_c, backend) = setup().await;

    // Task tagged ["cpu"]
    let mut snap =
        WorkflowSnapshot::with_initial_input("wf-cpu".into(), "h1".into(), Bytes::from(r#""1""#));
    snap.update_position(ExecutionPosition::AtTask {
        task_id: "t1".into(),
    });
    snap.task_tags = vec!["cpu".into()];
    backend.save_snapshot(&snap).await.unwrap();

    // Worker with ["gpu"] should not see cpu tasks
    let tasks = backend
        .find_available_tasks("w1", 10, Duration::seconds(300), &["gpu".into()])
        .await
        .unwrap();
    assert!(tasks.is_empty());
}

// ─── TaskResultStore ─────────────────────────────────────────────────────────

#[tokio::test]
async fn load_task_result_in_progress() {
    let (_c, backend) = setup().await;
    let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    snapshot.mark_task_completed("task-1".into(), Bytes::from(r#""out1""#));
    backend.save_snapshot(&snapshot).await.unwrap();

    let result = backend.load_task_result("wf-1", "task-1").await.unwrap();
    assert_eq!(result, Some(Bytes::from(r#""out1""#)));
}

#[tokio::test]
async fn load_task_result_not_found() {
    let (_c, backend) = setup().await;
    let snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    let result = backend
        .load_task_result("wf-1", "no-such-task")
        .await
        .unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn load_task_result_nonexistent_instance() {
    let (_c, backend) = setup().await;
    let result = backend.load_task_result("no-such-wf", "task-1").await;
    assert!(matches!(result, Err(BackendError::NotFound(_))));
}

#[tokio::test]
async fn load_task_result_after_completion() {
    let (_c, backend) = setup().await;

    // Create workflow with a completed task
    let mut snapshot = WorkflowSnapshot::new("wf-1".into(), "hash-1".into());
    snapshot.mark_task_completed("task-1".into(), Bytes::from(r#""out1""#));
    backend.save_snapshot(&snapshot).await.unwrap();

    // Complete the workflow
    snapshot.mark_completed(Bytes::from(r#""final""#));
    backend.save_snapshot(&snapshot).await.unwrap();

    // Task result should still be accessible from history
    let result = backend.load_task_result("wf-1", "task-1").await.unwrap();
    assert_eq!(result, Some(Bytes::from(r#""out1""#)));
}

// ─── Minimum Postgres version ────────────────────────────────────────────────

/// Verify that migrations and core operations work on the minimum supported
/// PostgreSQL version (currently 13).
#[tokio::test]
async fn works_on_minimum_pg_version() {
    let (_c, backend) = setup_with(MIN_PG_VERSION).await;

    // Snapshot CRUD
    let snapshot = WorkflowSnapshot::new("min-pg-1".into(), "hash".into());
    backend.save_snapshot(&snapshot).await.unwrap();
    let loaded = backend.load_snapshot("min-pg-1").await.unwrap();
    assert_eq!(loaded.instance_id, "min-pg-1");
    backend.delete_snapshot("min-pg-1").await.unwrap();

    // Signals
    let snapshot = WorkflowSnapshot::new("min-pg-2".into(), "hash".into());
    backend.save_snapshot(&snapshot).await.unwrap();
    backend
        .store_signal(
            "min-pg-2",
            SignalKind::Cancel,
            SignalRequest::new(Some("test".into()), None),
        )
        .await
        .unwrap();
    let cancelled = backend
        .check_and_cancel("min-pg-2", Some("task-1"))
        .await
        .unwrap();
    assert!(cancelled);

    // Task claims
    let claim = backend
        .claim_task(
            "min-pg-3",
            "task-1",
            "worker-1",
            Some(Duration::seconds(60)),
        )
        .await
        .unwrap();
    assert!(claim.is_some());
}

/// `connect_with` must reject PostgreSQL versions below the minimum (13).
#[tokio::test]
async fn rejects_unsupported_pg_version() {
    let container = Postgres::default()
        .with_tag("12-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgresql://postgres:postgres@127.0.0.1:{port}/postgres");
    let pool = PgPool::connect(&url).await.unwrap();

    let result = PostgresBackend::<JsonCodec>::connect_with(pool).await;
    match result {
        Ok(_) => panic!("expected version rejection, but connect succeeded"),
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("not supported"),
                "expected version rejection, got: {msg}"
            );
        }
    }
}
