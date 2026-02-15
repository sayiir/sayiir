# sayiir-core

Core types and traits for the [Sayiir](https://github.com/sayiir/sayiir) durable workflow engine.

## Overview

This crate defines the foundational abstractions that every other `sayiir-*` crate builds on. It is intentionally **runtime-agnostic** — no persistence, no execution strategy, just pure workflow modelling.

## Key Types

| Type / Trait | Purpose |
|---|---|
| `Workflow` / `SerializableWorkflow` | Continuation tree describing *what* to execute |
| `WorkflowBuilder` | Fluent API for assembling sequential, forked, and joined pipelines |
| `CoreTask` | Trait every task implements (input → output, with metadata) |
| `Codec` | Pluggable serialization (JSON, rkyv, …) |
| `WorkflowContext` | Per-execution context (codec, workflow ID, user metadata) |
| `WorkflowSnapshot` | Checkpoint of in-flight state (completed tasks + outputs) |
| `TaskRegistry` | Name → factory map for cross-process workflow deserialization |

## Architecture

```
sayiir-core          ← this crate (types & traits only)
  ↑
sayiir-persistence   (SnapshotStore, SignalStore, TaskClaimStore)
  ↑
sayiir-runtime       (CheckpointingRunner, PooledWorker, execution loop)
  ↑
sayiir-macros        (#[task], workflow! — optional convenience)
```

## Quick Example

```rust
use sayiir_core::prelude::*;
use std::sync::Arc;

fn main() -> Result<(), WorkflowError> {
    let ctx = WorkflowContext::new("order-pipeline", Arc::new(my_codec), Arc::new(()));

    let wf = WorkflowBuilder::new(ctx)
        .then("validate", |order: String| async move { Ok(order) })
        .then("charge", |order: String| async move { Ok(42u64) })
        .build()?;

    Ok(())
}
```

Most users will depend on `sayiir-runtime` (which re-exports this crate) rather than using `sayiir-core` directly.

## Documentation

Full API docs are available on [docs.rs](https://docs.rs/sayiir-core).

## License

MIT
