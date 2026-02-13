# Architecture

```text
┌──────────────────────────────────────────────────────────────────┐
│                        Your Application                          │
│               (Rust, Python, or future bindings)                 │
├──────────────────────────────────────────────────────────────────┤
│  Language Bindings (thin layer — workflow definition only)        │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                        │
│  │  Rust    │  │  Python  │  │  Node.js │                        │
│  │ (native) │  │  (PyO3)  │  │ (planned)│                        │
│  └──────────┘  └──────────┘  └──────────┘                        │
├──────────────────────────────────────────────────────────────────┤
│                    Sayiir Runtime (Rust)                          │
│  ┌─────────────────────┐  ┌──────────────────────────────────┐   │
│  │ CheckpointingRunner │  │          PooledWorker            │   │
│  │  (single process)   │  │  (distributed, multi-machine)    │   │
│  └─────────────────────┘  └──────────────────────────────────┘   │
├──────────────────────────────────────────────────────────────────┤
│                      Persistence Layer                           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────────┐     │
│  │ InMemory │  │ Postgres │  │  Custom  │  │  Enterprise   │     │
│  │ Backend  │  │ Backend  │  │ Backend  │  │  gRPC Server  │     │
│  └──────────┘  └──────────┘  └──────────┘  └───────────────┘     │
└──────────────────────────────────────────────────────────────────┘
```

**One problem, solved well.** Sayiir is an orchestrator, not a compute engine. It coordinates steps, handles retries, and checkpoints progress — while the actual heavy lifting (ETL, data pipelines, GPU training, API calls) runs in external systems you call from your tasks. We don't try to be a platform, a UI, or a kitchen sink. We make your workflows durable and let you focus on your business logic.

**Why Rust?** The core runtime is written in Rust for safety, performance, and correctness — the properties that matter most for infrastructure that runs your critical business processes. But Sayiir is not a Rust-only tool. Python bindings are available today, with TypeScript and Go planned — so you get Rust's reliability without leaving your ecosystem. The binding is a thin layer: you write task functions in your language, Rust handles all orchestration, checkpointing, and execution.

---

## Hexagonal Design

Sayiir's internals follow hexagonal (ports & adapters) architecture. The core domain (`sayiir-core`) has **zero infrastructure dependencies** — pure business logic. All dependencies flow inward:

```
core ← persistence ← runtime ← language bindings
```

Every integration point is a trait-based port with swappable adapters:

- **`Codec`** — rkyv, JSON, or your own serializer
- **`PersistentBackend`** — InMemory, PostgreSQL, or your own storage
- **`CoreTask`** — closures, registry lookups, or your own execution model
- **`WorkflowRunner`** — single-process, distributed, or your own topology

This isn't accidental. It means you can swap any layer without touching the others. Test with InMemory, deploy with PostgreSQL. Prototype with JSON, optimize with rkyv, or any custom Codec of your choice (protobuf, avro ..). Run single-process locally, distribute across machines in production. Same workflow code, different adapters.

---

## Pluggable Codecs

Serialization is pluggable — bring your own format or use the built-in options:

```rust
// Zero-copy for maximum performance (default)
let codec = RkyvCodec::new();

// Human-readable for debugging
let codec = JsonCodec::new();

// Custom format (implement Codec trait)
let codec = MyCustomCodec::new();
```

- **rkyv** (default) — Zero-copy deserialization for maximum performance
- **JSON** — Human-readable, enable with `--features json`
- **Custom** — Implement the `Codec` trait for any format (Protobuf, MessagePack, etc.)

---

## Pluggable Storage Backends

```rust
// In-memory (testing)
let backend = InMemoryBackend::new();

// PostgreSQL (production)
let backend = PostgresBackend::new(pool);

// Custom (bring your own)
impl PersistentBackend for MyBackend { ... }
```

- **InMemory** — For testing
- **PostgreSQL** — For production
- **Custom** — Implement the `PersistentBackend` trait for anything else (Redis, DynamoDB, SQLite, Cloudflare Durable Objects)

---

## Performance

Designed to scale to **hundreds of thousands of concurrent activities**:

- **Zero-copy deserialization** with rkyv codec
- **Minimal coordination** — workers claim tasks independently
- **Per-task checkpointing** — fine-grained durability
- **No global locks** — optimistic concurrency

---

## Distributed Retry Resilience

When a task fails in distributed mode, Sayiir uses **soft worker affinity** to prefer retrying on a different worker. This improves resilience against worker-local failures — corrupted caches, unhealthy dependencies, resource exhaustion, or environment-specific bugs.

### How it works

1. A worker executes a task and it fails (error, timeout, or panic)
2. The worker records the retry in the snapshot, tagging itself as the `last_failed_worker`
3. The task claim is released, making the task available for any worker to pick up
4. When workers poll for available tasks, the backend sorts results so that tasks which did **not** fail on the requesting worker come first

This is a **soft bias**, not a hard exclusion. If the failed worker is the only one available, it will still pick up the task — no work is left stranded. But when multiple workers are polling, tasks naturally migrate away from the worker that failed them.

### Why soft affinity over hard exclusion

- **No starvation** — A single-worker deployment still retries normally
- **Self-healing** — Transient worker issues resolve without manual intervention; the task moves to a healthy worker while the original recovers
- **No configuration** — The bias is automatic; no retry routing rules to maintain
- **Distributed fault isolation** — If Worker A has a bad network path to an external service, retries on Worker B bypass the issue entirely without any operator awareness
