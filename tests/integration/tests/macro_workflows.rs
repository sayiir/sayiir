#![allow(clippy::unwrap_used, clippy::expect_used)]

use sayiir_core::context::WorkflowContext;
use sayiir_core::error::BoxError;
use sayiir_core::registry::TaskRegistry;
use sayiir_core::task::CoreTask;
use sayiir_core::workflow::{WorkflowBuilder, WorkflowStatus};
use sayiir_macros::{task, workflow};
use sayiir_persistence::SnapshotStore;
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::CheckpointingRunner;
use sayiir_runtime::serialization::JsonCodec;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

// ─── Setup ───────────────────────────────────────────────────────────────────

async fn setup() -> (
    testcontainers::ContainerAsync<Postgres>,
    PostgresBackend<JsonCodec>,
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
    (container, backend)
}

// ─── Task definitions ────────────────────────────────────────────────────────

#[task]
async fn add_ten(input: u32) -> Result<u32, BoxError> {
    Ok(input + 10)
}

#[task(timeout = "5s", retries = 2, backoff = "50ms")]
async fn double(input: u32) -> Result<u32, BoxError> {
    Ok(input * 2)
}

#[derive(Debug, Clone)]
struct Multiplier {
    factor: u32,
}

#[task]
async fn multiply(input: u32, #[inject] mul: Arc<Multiplier>) -> Result<u32, BoxError> {
    Ok(input * mul.factor)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: u64,
    amount: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receipt {
    order_id: u64,
    total: u32,
}

#[task(id = "process_order")]
async fn process(order: Order) -> Result<Receipt, BoxError> {
    Ok(Receipt {
        order_id: order.id,
        total: order.amount * 2,
    })
}

// ─── 1. #[task] basic — struct generation, CoreTask impl ────────────────────

#[test]
fn task_basic_struct_generation() {
    let task = AddTen::new();
    assert_eq!(AddTen::task_id(), "add_ten");

    let fut = task.run(5u32);
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fut)
        .unwrap();
    assert_eq!(result, 15);
}

#[test]
fn task_with_metadata() {
    assert_eq!(Double::task_id(), "double");
    let meta = Double::metadata();
    assert!(meta.timeout.is_some());
    assert_eq!(
        meta.timeout.unwrap(),
        std::time::Duration::from_millis(5000)
    );
    assert!(meta.retries.is_some());
    let retry = meta.retries.unwrap();
    assert_eq!(retry.max_retries, 2);
    assert_eq!(retry.initial_delay, std::time::Duration::from_millis(50));
    assert_eq!(retry.backoff_multiplier, 2.0);
}

#[test]
fn task_with_inject() {
    let mul = Arc::new(Multiplier { factor: 3 });
    let task = Multiply::new(mul);

    let fut = task.run(7u32);
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fut)
        .unwrap();
    assert_eq!(result, 21);
}

#[test]
fn task_custom_id() {
    assert_eq!(Process::task_id(), "process_order");
}

#[test]
fn original_fn_preserved() {
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(add_ten(5u32))
        .unwrap();
    assert_eq!(result, 15);
}

// ─── 2. #[task] registration with codec ──────────────────────────────────────

#[test]
fn task_register_into_registry() {
    let codec = Arc::new(JsonCodec);
    let mut registry = TaskRegistry::new();
    AddTen::register(&mut registry, codec.clone(), AddTen::new());
    Double::register(&mut registry, codec, Double::new());

    assert!(registry.contains("add_ten"));
    assert!(registry.contains("double"));
    assert_eq!(registry.len(), 2);

    let meta = registry.get_metadata("double").unwrap();
    assert!(meta.timeout.is_some());
}

// ─── 3. End-to-end with CheckpointingRunner ─────────────────────────────────

#[tokio::test]
async fn task_macro_with_checkpointing_runner() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let codec = Arc::new(JsonCodec);
    let mut registry = TaskRegistry::new();
    AddTen::register(&mut registry, codec.clone(), AddTen::new());
    Double::register(&mut registry, codec.clone(), Double::new());

    let ctx = WorkflowContext::new("macro-test", codec, Arc::new(()));
    let workflow = WorkflowBuilder::new(ctx)
        .with_existing_registry(registry)
        .then_registered::<<AddTen as CoreTask>::Output>(AddTen::task_id())
        .unwrap()
        .then_registered::<<Double as CoreTask>::Output>(Double::task_id())
        .unwrap()
        .build()
        .unwrap();

    // 5 + 10 = 15, 15 * 2 = 30
    let status = runner
        .run(workflow.workflow(), "macro-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));

    let snapshot = runner
        .backend()
        .load_snapshot("macro-inst-1")
        .await
        .unwrap();
    assert!(snapshot.state.is_completed());
}

// ─── 4. workflow! macro — linear pipeline ────────────────────────────────────

#[tokio::test]
async fn workflow_macro_linear() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let codec = Arc::new(JsonCodec);
    let mut registry = TaskRegistry::new();
    AddTen::register(&mut registry, codec.clone(), AddTen::new());
    Double::register(&mut registry, codec.clone(), Double::new());

    let workflow = workflow!("linear-test", JsonCodec, registry,
        add_ten => double
    )
    .unwrap();

    // 5 + 10 = 15, 15 * 2 = 30
    let status = runner
        .run(workflow.workflow(), "linear-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── 5. workflow! macro — inline task ────────────────────────────────────────

#[tokio::test]
async fn workflow_macro_inline_task() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let codec = Arc::new(JsonCodec);
    let mut registry = TaskRegistry::new();
    Double::register(&mut registry, codec.clone(), Double::new());

    let workflow = workflow!("inline-test", JsonCodec, registry,
        add_five(x: u32) { x + 5 }
        => double
    )
    .unwrap();

    // 10 + 5 = 15, 15 * 2 = 30
    let status = runner
        .run(workflow.workflow(), "inline-inst-1", 10u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── 6. workflow! macro — fork-join ──────────────────────────────────────────

#[task]
async fn branch_a(input: u32) -> Result<bytes::Bytes, BoxError> {
    use sayiir_core::codec::Encoder;
    let codec = JsonCodec;
    let result = input + 100;
    codec.encode(&result)
}

#[task]
async fn branch_b(input: u32) -> Result<bytes::Bytes, BoxError> {
    use sayiir_core::codec::Encoder;
    let codec = JsonCodec;
    let result = input * 100;
    codec.encode(&result)
}

#[task]
async fn join_branches(
    results: sayiir_core::branch_results::NamedBranchResults,
) -> Result<String, BoxError> {
    Ok(format!("joined-{}", results.len()))
}

#[tokio::test]
async fn workflow_macro_fork_join() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let codec = Arc::new(JsonCodec);
    let mut registry = TaskRegistry::new();
    AddTen::register(&mut registry, codec.clone(), AddTen::new());
    BranchA::register(&mut registry, codec.clone(), BranchA::new());
    BranchB::register(&mut registry, codec.clone(), BranchB::new());
    JoinBranches::register(&mut registry, codec.clone(), JoinBranches::new());

    let workflow = workflow!("fork-join-test", JsonCodec, registry,
        add_ten
        => branch_a || branch_b
        => join_branches
    )
    .unwrap();

    let status = runner
        .run(workflow.workflow(), "fork-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}
