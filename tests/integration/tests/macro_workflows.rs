#![allow(clippy::unwrap_used, clippy::expect_used)]

use sayiir_core::LoopResult;
use sayiir_core::context::WorkflowContext;
use sayiir_core::error::BoxError;
use sayiir_core::registry::TaskRegistry;
use sayiir_core::task::CoreTask;
use sayiir_core::workflow::{WorkflowBuilder, WorkflowStatus};
use sayiir_macros::{BranchKey, task, workflow};
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

// ─── Task with display_name and description ──────────────────────────────────

#[task(display_name = "Add Ten", description = "Adds 10 to the input")]
async fn described_task(input: u32) -> Result<u32, BoxError> {
    Ok(input + 10)
}

// ─── Custom error type ───────────────────────────────────────────────────────

#[derive(Debug)]
struct MyError(String);

impl std::fmt::Display for MyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MyError: {}", self.0)
    }
}

impl std::error::Error for MyError {}

#[task]
async fn fallible_custom_err(input: u32) -> Result<u32, MyError> {
    if input == 0 {
        Err(MyError("zero not allowed".into()))
    } else {
        Ok(input + 1)
    }
}

// ─── Infallible task ─────────────────────────────────────────────────────────

#[task]
async fn infallible_add(input: u32) -> u32 {
    input + 42
}

// ─── 1. #[task] basic — struct generation, CoreTask impl ────────────────────

#[test]
fn task_basic_struct_generation() {
    let task = AddTenTask::new();
    assert_eq!(AddTenTask::task_id(), "add_ten");

    let fut = task.run(5u32);
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fut)
        .unwrap();
    assert_eq!(result, 15);
}

#[test]
fn task_with_metadata() {
    assert_eq!(DoubleTask::task_id(), "double");
    let meta = DoubleTask::metadata();
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
    let task = MultiplyTask::new(mul);

    let fut = task.run(7u32);
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fut)
        .unwrap();
    assert_eq!(result, 21);
}

#[test]
fn task_custom_id() {
    assert_eq!(ProcessTask::task_id(), "process_order");
}

#[test]
fn task_display_name_and_description() {
    let meta = DescribedTaskTask::metadata();
    assert_eq!(meta.display_name.as_deref(), Some("Add Ten"));
    assert_eq!(meta.description.as_deref(), Some("Adds 10 to the input"));
}

#[test]
fn original_fn_preserved() {
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(add_ten(5u32))
        .unwrap();
    assert_eq!(result, 15);
}

#[test]
fn task_custom_error_type_ok() {
    let task = FallibleCustomErrTask::new();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(task.run(5u32))
        .unwrap();
    assert_eq!(result, 6);
}

#[test]
fn task_custom_error_type_err() {
    let task = FallibleCustomErrTask::new();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(task.run(0u32));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("zero not allowed"));
}

#[test]
fn task_infallible_return() {
    let task = InfallibleAddTask::new();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(task.run(8u32))
        .unwrap();
    assert_eq!(result, 50);
}

// ─── 2. #[task] registration with codec ──────────────────────────────────────

#[test]
fn task_register_into_registry() {
    let codec = Arc::new(JsonCodec);
    let mut registry = TaskRegistry::new();
    AddTenTask::register(&mut registry, codec.clone(), AddTenTask::new());
    DoubleTask::register(&mut registry, codec, DoubleTask::new());

    assert!(registry.contains("add_ten"));
    assert!(registry.contains("double"));
    assert_eq!(registry.len(), 2);

    let meta = registry.get_metadata("double").unwrap();
    assert!(meta.timeout.is_some());
}

#[tokio::test]
async fn task_macro_with_then_task() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let codec = Arc::new(JsonCodec);
    let ctx = WorkflowContext::new("macro-test", codec, Arc::new(()));
    let workflow = WorkflowBuilder::new(ctx)
        .with_registry()
        .then_task::<AddTenTask>()
        .then_task::<DoubleTask>()
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

// ─── 3b. Dynamic pattern: then_registered (for runtime-determined task IDs) ──

#[tokio::test]
async fn task_macro_with_then_registered() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    // Use then_registered when task IDs are determined at runtime,
    // e.g. loaded from config, or when composing from a shared registry.
    let codec = Arc::new(JsonCodec);
    let mut registry = TaskRegistry::new();
    AddTenTask::register(&mut registry, codec.clone(), AddTenTask::new());
    DoubleTask::register(&mut registry, codec.clone(), DoubleTask::new());

    let ctx = WorkflowContext::new("macro-test-dyn", codec, Arc::new(()));
    let workflow = WorkflowBuilder::new(ctx)
        .with_existing_registry(registry)
        .then_registered::<<AddTenTask as CoreTask>::Output>(AddTenTask::task_id())
        .then_registered::<<DoubleTask as CoreTask>::Output>(DoubleTask::task_id())
        .build()
        .unwrap();

    // 5 + 10 = 15, 15 * 2 = 30
    let status = runner
        .run(workflow.workflow(), "macro-inst-dyn-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── 4. workflow! macro — linear pipeline ────────────────────────────────────

#[tokio::test]
async fn workflow_macro_linear() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "linear-test",
        codec: JsonCodec,
        steps: [add_ten, double]
    }
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

    let workflow = workflow! {
        name: "inline-test",
        codec: JsonCodec,
        steps: [
            add_five(x: u32) { Ok(x + 5) },
            double,
        ]
    }
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

    let workflow = workflow! {
        name: "fork-join-test",
        codec: JsonCodec,
        steps: [add_ten, branch_a || branch_b, join_branches]
    }
    .unwrap();

    let status = runner
        .run(workflow.workflow(), "fork-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── 7. workflow! macro — signal step ────────────────────────────────────────

#[tokio::test]
async fn workflow_macro_signal() {
    use sayiir_persistence::SignalStore;

    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "signal-test",
        codec: JsonCodec,
        steps: [add_ten, signal "approval", double]
    }
    .unwrap();

    // 5 + 10 = 15, then parks at signal
    let status = runner
        .run(workflow.workflow(), "signal-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::AwaitingSignal { .. }));

    // Send the signal with the current value as payload
    let payload: bytes::Bytes = serde_json::to_vec(&15u32).unwrap().into();
    runner
        .backend()
        .send_event("signal-inst-1", "approval", payload)
        .await
        .unwrap();

    // Resume — consumes the signal, then 15 * 2 = 30
    let status = runner
        .resume(workflow.workflow(), "signal-inst-1")
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── 8. workflow! macro — route ──────────────────────────────────────────

#[task]
async fn classify(input: u32) -> Result<String, BoxError> {
    if input >= 100 {
        Ok("big".to_string())
    } else {
        Ok("small".to_string())
    }
}

#[task]
async fn handle_big(input: u32) -> Result<u32, BoxError> {
    Ok(input / 10)
}

#[task]
async fn handle_small(input: u32) -> Result<u32, BoxError> {
    Ok(input * 10)
}

#[task]
async fn handle_default(input: u32) -> Result<u32, BoxError> {
    Ok(input)
}

#[tokio::test]
async fn workflow_macro_route() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "branch-on-test",
        codec: JsonCodec,
        steps: [
            add_ten,
            route classify {
                "big" => [handle_big],
                "small" => [handle_small],
                _ => [handle_default],
            },
        ]
    }
    .unwrap();

    // Input 5 → add_ten → 15 → classify → "small" → handle_small → 150
    let status = runner
        .run(workflow.workflow(), "branch-on-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

#[task]
async fn unwrap_envelope(
    envelope: sayiir_core::task::BranchEnvelope<u32>,
) -> Result<u32, BoxError> {
    Ok(envelope.result)
}

#[tokio::test]
async fn workflow_macro_route_then_next() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "branch-on-next-test",
        codec: JsonCodec,
        steps: [
            add_ten,
            route classify {
                "big" => [handle_big],
                "small" => [handle_small],
            },
            unwrap_envelope,
            double,
        ]
    }
    .unwrap();

    // Input 5 → add_ten → 15 → classify → "small" → handle_small → 150
    // → unwrap_envelope → 150 → double → 300
    let status = runner
        .run(workflow.workflow(), "branch-on-next-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── 9. workflow! macro — typed route with BranchKey enum ────────────────

#[derive(BranchKey)]
enum Size {
    Big,
    Small,
}

#[tokio::test]
async fn workflow_macro_typed_route() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "typed-route-test",
        codec: JsonCodec,
        steps: [
            add_ten,
            route classify -> Size {
                Big => [handle_big],
                Small => [handle_small],
                _ => [handle_default],
            },
        ]
    }
    .unwrap();

    // Input 5 → add_ten → 15 → classify → "small" → handle_small → 150
    let status = runner
        .run(workflow.workflow(), "typed-route-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

#[tokio::test]
async fn workflow_macro_typed_route_then_next() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "typed-route-next-test",
        codec: JsonCodec,
        steps: [
            add_ten,
            route classify -> Size {
                Big => [handle_big],
                Small => [handle_small],
            },
            unwrap_envelope,
            double,
        ]
    }
    .unwrap();

    // Input 5 → add_ten → 15 → classify → "small" → handle_small → 150
    // → unwrap_envelope → 150 → double → 300
    let status = runner
        .run(workflow.workflow(), "typed-route-next-inst-1", 5u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

// ─── 10. workflow! macro — loop ───────────────────────────────────────────

#[task]
async fn accumulate(input: u32) -> Result<LoopResult<u32>, BoxError> {
    let next = input + 1;
    if next >= 5 {
        Ok(LoopResult::Done(next))
    } else {
        Ok(LoopResult::Again(next))
    }
}

#[tokio::test]
async fn workflow_macro_loop() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "loop-test",
        codec: JsonCodec,
        steps: [loop accumulate 10]
    }
    .unwrap();

    // Input 0 → accumulate → Again(1) → Again(2) → … → Done(5)
    let status = runner
        .run(workflow.workflow(), "loop-inst-1", 0u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

#[tokio::test]
async fn workflow_macro_loop_exit_with_last() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    // Body always returns Again; max 3 iterations with exit_with_last
    let workflow = workflow! {
        name: "loop-exit-last-test",
        codec: JsonCodec,
        steps: [loop accumulate 3 exit_with_last]
    }
    .unwrap();

    // Input 0 → Again(1) → Again(2) → Again(3) → max reached, exit with 3
    let status = runner
        .run(workflow.workflow(), "loop-exit-inst-1", 0u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}

#[tokio::test]
async fn workflow_macro_loop_in_pipeline() {
    let (_c, backend) = setup().await;
    let runner = CheckpointingRunner::new(backend);

    let workflow = workflow! {
        name: "loop-pipeline-test",
        codec: JsonCodec,
        steps: [add_ten, loop accumulate 20, double]
    }
    .unwrap();

    // Input 0 → add_ten → 10 → accumulate: 10+1=11 ≥5 → Done(11) → double → 22
    let status = runner
        .run(workflow.workflow(), "loop-pipe-inst-1", 0u32)
        .await
        .unwrap();
    assert!(matches!(status, WorkflowStatus::Completed));
}
