# sayiir-postgres

PostgreSQL persistence backend for the [Sayiir](https://github.com/sayiir/sayiir) durable workflow engine.

## Overview

Provides [`PostgresBackend`](https://docs.rs/sayiir-postgres/latest/sayiir_postgres/struct.PostgresBackend.html), a production-grade implementation of `SnapshotStore`, `SignalStore`, and `TaskClaimStore` backed by PostgreSQL via [sqlx](https://crates.io/crates/sqlx).

## Features

- **Codec-generic** — Serialise snapshots with any codec (JSON for debuggability, rkyv/bincode for speed). The data column is always `BYTEA`.
- **ACID transactions** — Composite signal operations use `SELECT … FOR UPDATE` for true atomicity.
- **Snapshot history** — Every checkpoint is appended to an immutable history table for debugging and auditing.
- **Observability-ready** — Indexed metadata columns (`status`, `current_task_id`, `completed_task_count`, `error`, timestamps) plus a denormalised `sayiir_workflow_tasks` table enable monitoring without deserialising blobs.
- **Distributed task claiming** — TTL-based claims with expired-claim replacement and soft worker bias.

## Quick Start

```rust
use sayiir_postgres::PostgresBackend;
use sayiir_runtime::serialization::JsonCodec;
use sayiir_persistence::SnapshotStore;
use sayiir_core::snapshot::WorkflowSnapshot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = PostgresBackend::<JsonCodec>::connect("postgresql://localhost/sayiir").await?;

    let snapshot = WorkflowSnapshot::new("order-123".to_string(), "hash-abc".to_string());
    backend.save_snapshot(&snapshot).await?;

    let loaded = backend.load_snapshot("order-123").await?;
    Ok(())
}
```

## Schema

`PostgresBackend::connect()` runs migrations automatically. The schema consists of:

| Table | Purpose |
|---|---|
| `sayiir_workflows` | Current snapshot per workflow instance |
| `sayiir_workflow_history` | Immutable append-only snapshot history |
| `sayiir_workflow_tasks` | Denormalised task metadata for querying |
| `sayiir_task_claims` | Distributed task claim tracking |
| `sayiir_workflow_events` | Event log for workflow lifecycle |

## PostgreSQL Version Support

Minimum supported version: **PostgreSQL 13**. Integration tests run against both 13 and 17.

## Documentation

Full API docs are available on [docs.rs](https://docs.rs/sayiir-postgres).

## License

MIT
