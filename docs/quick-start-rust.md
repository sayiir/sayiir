# Quick Start: Rust

All examples assume `sayiir_runtime::prelude::*` is in scope:

```rust
use sayiir_runtime::prelude::*;
// Re-exports: WorkflowBuilder, CheckpointingRunner, PooledWorker,
// WorkerHandle, InMemoryBackend, JsonCodec, TaskRegistry, etc.
```

---

## Durable single-process workflow

Run a workflow in one process with automatic checkpointing. If the process
crashes, resume from the last completed task. Use `InMemoryBackend` for
development or `PostgresBackend` for production.

```rust
use sayiir_runtime::prelude::*;

let backend = InMemoryBackend::new();
let runner = CheckpointingRunner::new(backend);

let workflow = WorkflowBuilder::new(ctx)
    .then("step_1", |input: String| async move {
        Ok(format!("processed: {}", input))
    })
    .then("step_2", |data: String| async move {
        Ok(data.to_uppercase())
    })
    .build();

// Runs to completion, checkpointing after each task
let status = runner.run(&workflow, "instance-001", "hello".to_string()).await?;

// After a crash: pick up where it left off
let status = runner.resume(&workflow, "instance-001").await?;
```

### With Postgres

Swap the backend to persist state in PostgreSQL — the workflow code is
unchanged.

```rust
use sayiir_runtime::prelude::*;
use sayiir_postgres::PostgresBackend;

// Connects and runs migrations automatically
let backend = PostgresBackend::<JsonCodec>::connect("postgresql://localhost/sayiir").await?;
let runner = CheckpointingRunner::new(backend);

let workflow = WorkflowBuilder::new(ctx)
    .then("step_1", |input: String| async move {
        Ok(format!("processed: {}", input))
    })
    .then("step_2", |data: String| async move {
        Ok(data.to_uppercase())
    })
    .build();

let status = runner.run(&workflow, "instance-001", "hello".to_string()).await?;

// Later (or in a new process after a crash), resume from the last checkpoint
let status = runner.resume(&workflow, "instance-001").await?;
```

---

## Distributed workers

Scale out by running multiple worker processes against a shared Postgres
database. Workers coordinate automatically via row-level locks — each
worker claims, executes, and checkpoints tasks independently.

A distributed setup needs two registries:

- **`TaskRegistry`** — maps task IDs (e.g. `"step_1"`) to their
  implementations (the actual functions). Each worker needs this so it
  knows *how* to execute a task.
- **`WorkflowRegistry`** — maps workflow definition hashes to workflow
  definitions (the DAG structure). This tells the worker *which* workflows
  it can run and in what order tasks are chained.

Since `TaskRegistry` is not `Clone` (it holds boxed trait objects), the
usual pattern is a builder function that both the workflow definition and
the worker call independently.

```rust
use sayiir_runtime::prelude::*;
use sayiir_runtime::WorkflowRegistry;
use sayiir_postgres::PostgresBackend;
use std::sync::Arc;
use std::time::Duration;

let url = "postgresql://localhost/sayiir";
let codec = Arc::new(JsonCodec);

// Builder function — produces the same registry on every call
fn build_task_registry(codec: Arc<JsonCodec>) -> TaskRegistry {
    let mut r = TaskRegistry::new();
    r.register_fn("step_1", codec, |input: String| async move {
        Ok(format!("processed: {}", input))
    });
    r
}

// Build the workflow using registered tasks
let workflow = WorkflowBuilder::new(ctx)
    .with_existing_registry(build_task_registry(codec.clone()))
    .then_registered::<String>("step_1")
    .build()?;

// WorkflowRegistry: map definition hashes to workflow definitions
let workflows: WorkflowRegistry<_, _, _> = vec![
    (workflow.definition_hash().to_string(), Arc::new(workflow)),
];

// Start a worker with its own TaskRegistry
let backend = PostgresBackend::<JsonCodec>::connect(url).await?;
let worker = PooledWorker::new("worker-1", backend, build_task_registry(codec))
    .with_claim_ttl(Some(Duration::from_secs(300)));
let handle = worker.spawn(Duration::from_secs(1), workflows);

// Submit work from a separate runner (own connection)
let backend = PostgresBackend::<JsonCodec>::connect(url).await?;
let runner = CheckpointingRunner::new(backend);
runner.run(&workflow, "order-42", "hello".to_string()).await?;

// Shut down gracefully
handle.shutdown();
handle.join().await?;
```

To add capacity, start more processes with a different `worker_id`
pointing at the same database.

---

## Lifecycle operations

Control running workflows via the runner. These operations write signals
that are picked up at the next checkpoint boundary.

```rust
// Cancel a running workflow
runner.cancel("instance-001", Some("no longer needed".into()), None).await?;

// Pause — the workflow stops at the next checkpoint
runner.pause("instance-001", Some("maintenance window".into()), None).await?;

// Unpause — allows the workflow to be resumed
runner.unpause("instance-001").await?;

// Resume after unpause
runner.resume(&workflow, "instance-001").await?;
```

---

## Durable delays

Delays are persisted — the worker is released while the timer runs. After
a crash the remaining delay is recalculated from the checkpoint.

```rust
use std::time::Duration;

let workflow = WorkflowBuilder::new(ctx)
    .then("fetch", |input: String| async move {
        Ok(fetch_data(&input).await?)
    })
    .delay("wait_24h", Duration::from_secs(86400))
    .then("process", |data: Data| async move {
        Ok(process(data).await?)
    })
    .build();
```

---

## Automatic retries with exponential backoff

```rust
use sayiir_core::task::{TaskMetadata, RetryPolicy};

let workflow = WorkflowBuilder::new(ctx)
    .with_registry()
    .then("call_api", |url: String| async move {
        Ok(reqwest::get(&url).await?.json::<serde_json::Value>().await?)
    })
    .with_metadata(TaskMetadata {
        timeout: Some(Duration::from_secs(10)),
        retries: Some(RetryPolicy {
            max_retries: 2,
            initial_delay: Duration::from_secs(1),
            backoff_multiplier: 2.0,
        }),
        ..Default::default()
    })
    .then("process", |data: serde_json::Value| async move {
        Ok(format!("processed {} keys", data.as_object().map_or(0, |o| o.len())))
    })
    .build()?;
```

Retries use exponential backoff (`delay = initial_delay * multiplier^attempt`). The retry count and next-retry time are persisted in the snapshot, so retries survive crashes. Timeouts also trigger retries — a timed-out task is retried the same as a failed one.

---

## DAG workflows (fork/join)

```rust
let workflow = WorkflowBuilder::new(ctx)
    .then("fetch_order", fetch_order)
    .fork(|fork| {
        fork.branch("validate_payment", validate_payment)
            .branch("check_inventory", check_inventory)
            .branch("calculate_shipping", calculate_shipping)
    })
    .join("finalize_order", |results| async move {
        // All branches complete before this runs
        let payment = results.get("validate_payment")?;
        let inventory = results.get("check_inventory")?;
        let shipping = results.get("calculate_shipping")?;
        Ok(Order::finalize(payment, inventory, shipping))
    })
    .build();
```

---

## Task registry (reusable activities)

```rust
// Domain module with reusable tasks
fn payments_registry(codec: Arc<C>) -> TaskRegistry {
    TaskRegistry::new()
        .register_fn("payments::charge", codec.clone(), charge_card)
        .register_fn("payments::refund", codec.clone(), refund)
}

// Compose workflows from registered + inline tasks
let workflow = WorkflowBuilder::new(ctx)
    .with_existing_registry(payments_registry(codec))
    .then_registered::<PaymentResult>("payments::charge")
    .then("custom_logic", |r| async move { /* inline */ })
    .build()?;
```
