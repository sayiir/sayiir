# Quick Start: Rust

## Single process with checkpointing

```rust
use sayiir::{CheckpointingRunner, InMemoryBackend, WorkflowBuilder};

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

// Run workflow - automatically checkpoints after each task
let result = runner.run(&workflow, "instance-001", "hello".to_string()).await?;

// If process crashes, resume from last checkpoint
let result = runner.resume(&workflow, "instance-001").await?;
```

---

## Distributed workers

```rust
use sayiir::{PooledWorker, PostgresBackend};
use std::time::Duration;

let backend = PostgresBackend::new(pool);
let worker = PooledWorker::new("worker-1", backend, registry)
    .with_claim_ttl(Some(Duration::from_secs(300)))
    .with_heartbeat_interval(Some(Duration::from_secs(120)));

// Spawn the worker - tasks are automatically distributed across workers
let handle = worker.spawn(Duration::from_secs(1), workflows);
// ... later, shut down gracefully ...
handle.shutdown();
handle.join().await?;
```

---

## Durable delays

```rust
use std::time::Duration;

let workflow = WorkflowBuilder::new(ctx)
    .then("fetch", |input: String| async move {
        Ok(fetch_data(&input).await?)
    })
    .delay("wait_24h", Duration::from_secs(86400))  // persisted, no worker held
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
