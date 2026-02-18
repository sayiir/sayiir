#![allow(clippy::unwrap_used, clippy::expect_used)]

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sayiir_core::snapshot::WorkflowSnapshot;
use sayiir_core::task::to_core_task;
use sayiir_core::workflow::WorkflowContinuation;
use sayiir_persistence::SnapshotStore;
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;
use sqlx::PgPool;
use std::sync::{Arc, OnceLock};
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

fn encode(val: u32) -> Bytes {
    Bytes::from(serde_json::to_vec(&val).unwrap())
}

fn shared_backend() -> &'static (
    tokio::runtime::Runtime,
    PostgresBackend<JsonCodec>,
    testcontainers::ContainerAsync<Postgres>,
) {
    static INSTANCE: OnceLock<(
        tokio::runtime::Runtime,
        PostgresBackend<JsonCodec>,
        testcontainers::ContainerAsync<Postgres>,
    )> = OnceLock::new();

    INSTANCE.get_or_init(|| {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (container, backend) = rt.block_on(async {
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
            (container, backend)
        });
        (rt, backend, container)
    })
}

/// Build a linear chain of N identity tasks with `func` populated.
fn linear_chain(n: usize) -> WorkflowContinuation {
    let codec = Arc::new(JsonCodec);
    let mut chain: Option<WorkflowContinuation> = None;
    for i in (0..n).rev() {
        chain = Some(WorkflowContinuation::Task {
            id: format!("task_{i}"),
            func: Some(to_core_task(
                |v: u32| async move { Ok(v + 1) },
                codec.clone(),
            )),
            timeout: None,
            retry_policy: None,
            next: chain.map(Box::new),
        });
    }
    chain.unwrap()
}

/// Build a linear chain of N tasks *without* func (for checkpointing benchmarks).
fn linear_chain_no_func(n: usize) -> WorkflowContinuation {
    let mut chain: Option<WorkflowContinuation> = None;
    for i in (0..n).rev() {
        chain = Some(WorkflowContinuation::Task {
            id: format!("task_{i}"),
            func: None,
            timeout: None,
            retry_policy: None,
            next: chain.map(Box::new),
        });
    }
    chain.unwrap()
}

/// Build a fork with N branches + join, all with `func`.
fn fork_join(n_branches: usize) -> WorkflowContinuation {
    let codec = Arc::new(JsonCodec);
    let branch_ids: Vec<String> = (0..n_branches).map(|i| format!("branch_{i}")).collect();
    let branches: Vec<Arc<WorkflowContinuation>> = branch_ids
        .iter()
        .map(|id| {
            Arc::new(WorkflowContinuation::Task {
                id: id.clone(),
                func: Some(to_core_task(
                    |v: u32| async move { Ok(v * 2) },
                    codec.clone(),
                )),
                timeout: None,
                retry_policy: None,
                next: None,
            })
        })
        .collect();

    let fork_id = WorkflowContinuation::derive_fork_id(
        &branch_ids.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    );

    WorkflowContinuation::Fork {
        id: fork_id,
        branches: branches.into_boxed_slice(),
        join: None,
    }
}

/// Build a snapshot with N completed tasks (simulating progress).
fn snapshot_with_tasks(n: usize) -> WorkflowSnapshot {
    let input = encode(0);
    let mut snapshot =
        WorkflowSnapshot::with_initial_input("bench-inst".into(), "bench-hash".into(), input);
    for i in 0..n {
        snapshot.mark_task_completed(format!("task_{i}"), encode(i as u32));
    }
    snapshot
}

// ── Group 1: Snapshot persistence round-trip (PostgresBackend) ──────────

fn snapshot_store(c: &mut Criterion) {
    let (rt, backend, _) = shared_backend();
    let mut group = c.benchmark_group("snapshot_store");

    for size in [0, 5, 50] {
        let snapshot = snapshot_with_tasks(size);

        group.bench_with_input(BenchmarkId::new("save_load", size), &snapshot, |b, snap| {
            b.to_async(rt).iter(|| async {
                backend.save_snapshot(snap).await.unwrap();
                let loaded = backend.load_snapshot(&snap.instance_id).await.unwrap();
                backend.delete_snapshot(&snap.instance_id).await.unwrap();
                loaded
            });
        });
    }

    group.finish();
}

// ── Group 2: Execution engine (pure, no Postgres) ───────────────────────

fn execution(c: &mut Criterion) {
    let (rt, _, _) = shared_backend();
    let mut group = c.benchmark_group("execution");

    // Linear chains
    for n in [5, 20] {
        let chain = linear_chain(n);
        let input = encode(0);
        group.bench_with_input(BenchmarkId::new("linear", n), &n, |b, _| {
            b.to_async(rt).iter(|| {
                sayiir_runtime::execute_continuation_async(
                    &chain,
                    input.clone(),
                    &sayiir_runtime::serialization::JsonCodec,
                )
            });
        });
    }

    // Fork/join
    for n in [2, 5, 10] {
        let fork = fork_join(n);
        let input = encode(1);
        group.bench_with_input(BenchmarkId::new("fork_join", n), &n, |b, _| {
            b.to_async(rt).iter(|| {
                sayiir_runtime::execute_continuation_async(
                    &fork,
                    input.clone(),
                    &sayiir_runtime::serialization::JsonCodec,
                )
            });
        });
    }

    group.finish();
}

// ── Group 3: Checkpointing execution (against PostgresBackend) ──────────

fn checkpointing(c: &mut Criterion) {
    let (rt, backend, _) = shared_backend();
    let mut group = c.benchmark_group("checkpointing");

    let chain = linear_chain_no_func(5);
    let input = encode(0);

    group.bench_function("linear_5_tasks", |b| {
        b.to_async(rt).iter(|| async {
            let mut snapshot = WorkflowSnapshot::with_initial_input(
                "bench-ckpt".into(),
                "bench-hash".into(),
                input.clone(),
            );

            let callback = |_id: &str, input: Bytes| async move {
                let val: u32 = serde_json::from_slice(&input)?;
                Ok(Bytes::from(serde_json::to_vec(&(val + 1))?))
            };

            let result = sayiir_runtime::execute_continuation_with_checkpointing(
                &chain,
                input.clone(),
                &mut snapshot,
                backend,
                &callback,
                &sayiir_runtime::serialization::JsonCodec,
            )
            .await
            .unwrap();

            // Clean up snapshot between iterations
            let _ = backend.delete_snapshot("bench-ckpt").await;
            result
        });
    });

    group.finish();
}

criterion_group!(benches, snapshot_store, execution, checkpointing);
criterion_main!(benches);
