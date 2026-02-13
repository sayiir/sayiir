# Sayiir

**Durable workflows that feel like writing normal code.**

[![CI](https://github.com/sayiir/sayiir/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/sayiir/sayiir/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-edition_2024-93450a.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Python](https://img.shields.io/badge/python-3.10–3.13-3776ab.svg)](https://www.python.org)
[![Discord](https://img.shields.io/badge/Discord-Join-7289da)](https://discord.gg/MWSzsHeg)

> **Early Stage Project** — Sayiir is under active development. Core functionality works but APIs may change. We welcome contributors, maintainers, and sponsors.

---

## Why Sayiir?

Most workflow engines force you to learn their mental model, their DSL, their way of thinking. You end up writing code *for the engine* instead of writing code *for your business*.

**Sayiir is different.** Write async Rust or plain Python. That's it. Your existing code, your existing patterns, your existing tests — they all just work.

```rust
let workflow = WorkflowBuilder::new(ctx)
    .then("fetch_user", |id: UserId| async move {
        db.get_user(id).await
    })
    .then("send_email", |user: User| async move {
        email_service.send_welcome(&user).await
    })
    .build();

// Run it. Resume it after crashes. Scale it across machines.
runner.run(&workflow, "welcome-user-123", user_id).await?;
```

```python
from sayiir import task, Flow, run_workflow

@task
def fetch_user(user_id: int) -> dict:
    return db.get_user(user_id)

@task
def send_email(user: dict) -> str:
    return email_service.send_welcome(user)

workflow = Flow("welcome").then(fetch_user).then(send_email).build()
result = run_workflow(workflow, 42)
```

No annotations. No YAML. No separate worker processes. Just code.

---

## How Sayiir Is Different

### No deterministic replay

This is the big one. Temporal, Restate, Azure Durable Functions, and most durable execution engines use **replay-based recovery**: when a workflow resumes, they re-execute your code from the beginning and skip completed steps. This requires your workflow code to be **deterministic** — no system time, no random values, no direct I/O, no side effects outside SDK-approved APIs.

Developers consistently report this as the #1 source of production incidents, versioning nightmares, and onboarding friction.

**Sayiir doesn't replay.** It checkpoints after each task and resumes from the last checkpoint. Your tasks can call any API, use any library, read the clock, generate random values — there are no determinism constraints. When a process crashes, Sayiir loads the last snapshot and picks up from where it left off. No re-execution. No replay storms. No versioning headaches.

### A library, not a platform

Temporal requires a multi-service cluster (frontend, history, matching, workers) plus a database. Airflow needs a scheduler, webserver, workers, and database. Even "lightweight" options like Inngest and Hatchet run a centralized server.

**Sayiir is a library you import.** Add it as a dependency, write your workflow, run it. Works in a single process, across a cluster, or on serverless — no separate infrastructure to deploy, monitor, or operate. Your application *is* the workflow engine.

### Rust core, thin language bindings

Temporal recognized this was the right architecture — their newer Python, TypeScript, and .NET SDKs all wrap a shared Rust core (`sdk-core`). Sayiir was built this way from day one.

The Rust runtime handles all orchestration, checkpointing, serialization, and execution. Language bindings are thin: you define tasks in your language, Rust does everything else. This means every language gets the same performance, correctness, and safety guarantees — because they share the same battle-tested core.

### Hexagonal architecture

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

### Pluggable everything

- **Storage backends** — InMemory for testing, PostgreSQL for production, or implement the `PersistentBackend` trait for anything else (Redis, DynamoDB, SQLite, Cloudflare Durable Objects)
- **Codecs** — rkyv (zero-copy, default), JSON (human-readable), or bring your own (Protobuf, MessagePack)
- **No lock-in** — MIT licensed, standard async patterns, portable across any runtime

### How we compare

| | Sayiir | Temporal | Cadence | Restate | Inngest | DBOS | Windmill | Elsa |
|---|---|---|---|---|---|---|---|---|
| **Architecture** | Library (embedded) | Server cluster | Server cluster | Single binary server | Centralized server | Library + Postgres | Platform (server + workers) | Library or server (.NET) |
| **Recovery model** | Checkpoint & resume | Deterministic replay | Deterministic replay | Journal replay | Step replay | Checkpoint & resume | Step retry | State persistence |
| **Determinism required** | No | Yes | Yes | Yes | Yes | No | No | No |
| **Infrastructure** | None (library) | Multi-service + DB | Multi-service + DB | Single binary | Server | Postgres | Postgres + workers | None (library) or DB |
| **Rust core** | Native | Migrating to | No (Go) | Written in | No | No | Backend in Rust | No (.NET) |
| **Language SDKs** | Rust, Python | Go, Java, TS, Python, .NET | Go, Java | TS, Java, Python, Go, Rust | TS, Python, Go | Python, TS, Go, Java | Python, TS, Go, Bash, +20 | .NET (C#) |
| **License** | MIT | MIT | Apache 2.0 | BSL | SSPL | MIT | AGPLv3 | MIT |
| **Self-host complexity** | Zero | High | High | Low | Medium | Low | Medium | Low |

---

## The Problem with Existing Solutions

### The Fundamental Issues

Most workflow engines fall into one of three traps:

1. **The Abstraction Tax** — They force you to learn a new mental model, new primitives, new APIs. Your existing async code doesn't work; you must rewrite it for the engine. The more powerful the engine, the steeper the learning curve.

2. **The Infrastructure Burden** — They require multiple services (scheduler, workers, web UI, database, message broker) before you can run a single workflow. What should be a library becomes a platform you must operate.

3. **The Lock-in Problem** — They use proprietary DSLs, cloud-specific languages, or execution models that don't transfer. Migration means rewriting everything.

Sayiir avoids all three: it's a library (not a platform), it runs your existing async code (not a new DSL), and it's MIT-licensed with pluggable backends (no lock-in).

### Platform-Specific Analysis

#### Temporal

The most sophisticated option, but sophistication has costs:

- **Deterministic replay is powerful but treacherous** — Your workflow code must be deterministic: no system time, no random values, no direct I/O. Violations cause replay failures and production incidents. Teams report that "as time passed, we faced more and more problems" with versioning and replay.
- **Activities vs workflows is a false dichotomy** — You must split your logic into "workflow code" (deterministic, replay-safe) and "activities" (side effects). This artificial distinction adds cognitive load and boilerplate.
- **Expect a month before productivity** — The learning curve is real. Temporal's own documentation acknowledges this isn't the right fit if your team "doesn't have strong software engineering experience."
- **Heavy infrastructure** — Requires their server, a database (PostgreSQL/Cassandra), optionally Elasticsearch. Self-hosting is non-trivial; their cloud offering is the path of least resistance.

#### Cadence

Temporal's predecessor, built at Uber:

- **Same replay model, smaller ecosystem** — Cadence uses the same deterministic replay and event sourcing approach as Temporal (its creators went on to build Temporal). All the same determinism constraints apply — no random values, no system time, no direct I/O in workflow code.
- **Go and Java only** — No Python, TypeScript, or .NET SDKs. If your team isn't writing Go or Java, Cadence isn't an option.
- **Same infrastructure weight as Temporal** — Requires a multi-service cluster (frontend, history, matching, workers) plus Cassandra or MySQL. Self-hosting complexity is comparable to Temporal.
- **Internal focus** — Cadence was built for Uber's internal needs and is maintained with that priority. Community adoption and third-party ecosystem are significantly smaller than Temporal's.
- **Frozen in time** — While Temporal has evolved with new SDKs, cloud offering, and features, Cadence has remained largely stable. Stability is a feature for existing users, but new adopters get fewer capabilities and less community support.

#### Apache Airflow

The incumbent for data pipelines, showing its age:

- **Batch-first, event-second** — Built for daily scheduled jobs, not event-driven workflows. Real-time use cases require workarounds.
- **DAG parsing overhead** — Workflows are discovered by parsing Python files on a schedule. Large DAG counts cause scheduler performance issues.
- **4+ services minimum** — Scheduler, webserver, workers, and database—all required before your first workflow runs. Local development needs 4-8GB RAM.
- **XCom limitations** — Data passing between tasks uses XCom, which has size limits and requires external storage for anything substantial.
- **Onboarding struggles** — The community's own survey cited "lack of best practices" and "no easy option to launch" as top grievances.

#### Prefect

Modern Python-native alternative, but with trade-offs:

- **Cloud-first business model** — The open-source server is intentionally limited; the real value is in Prefect Cloud. This shapes what features get attention.
- **Momentum considerations** — Evaluate the current release cadence, roadmap, and community activity to ensure they align with your long-term needs.
- **No native data lineage** — Logs and statuses are centralized, but cross-workflow visibility requires custom instrumentation.
- **Python-only** — Great for Python shops, but no path to other languages.

#### Dagster

Asset-centric approach with strong developer experience:

- **Mental model shift required** — The "software-defined assets" paradigm is powerful but unfamiliar. Teams need time to internalize asset-centric thinking.
- **Smaller ecosystem** — Fewer integrations than Airflow; enterprises may find it restrictive.
- **More initial setup** — Compared to Prefect, more configuration required before productivity.
- **Python-only** — Same limitation as Prefect.

#### AWS Step Functions

Serverless but with significant constraints:

- **JSON state machine language** — Not code. You define workflows in Amazon States Language (JSON/YAML), limiting expressiveness and making version control awkward.
- **256KB payload limit** — Data between states is capped; larger payloads require S3 indirection.
- **Cost explosion at scale** — $0.025 per 1,000 state transitions sounds cheap until your workflow has nested loops. Teams report single executions costing $450+.
- **50-100ms latency per transition** — Each state adds overhead. Sub-second workflows are impractical.
- **Complete vendor lock-in** — Moving to another cloud means rewriting all orchestration.

#### Netflix Conductor

Microservice orchestration with JSON DSL:

- **No enforceable data contracts** — Task workers can return anything; type safety requires external testing frameworks.
- **DSL readability issues** — The JSON workflow definitions become unwieldy for complex logic.
- **No native scheduling** — Despite being a workflow engine, scheduled execution isn't built-in.
- **Maintenance uncertainty** — Netflix built Maestro as their next-generation orchestrator, signaling Conductor's future is unclear.

#### Windmill

A platform that combines process scheduling with workflow features:

- **Feature freakshow** — Scripts, flows, apps, schedules, webhooks, approval steps, error handlers, REST API builder, variables, resources, groups, folders, workers, worker groups... The feature list keeps growing, but the core workflow primitives get lost in the noise. It's unclear what problem Windmill is trying to solve exceptionally well.
- **Subprocess-based execution model** — Windmill spawns subprocesses and captures their output. This contrasts with library-first durable workflow engines like Temporal or Sayiir, where workflows are expressed directly in application code rather than via external scripts managed by a platform.
- **AGPLv3 licensing** — It adds further constraints for commercial use.
- **Platform, not a library** — You don't import Windmill; you deploy it. Your workflows live in their UI, their database, their execution model. **Self-hosted doesn't mean portable**. How you structure scripts, how you pass data, how you handle errors, how you deploy workers—Windmill has strong opinions, and your code must conform.

#### Elsa Workflows

.NET workflow library with visual designer:

- **Library architecture — but .NET only** — Elsa can be embedded as a NuGet package, which is the right architectural choice. But it's exclusively .NET/C#. No path to Python, Go, or TypeScript. If your stack isn't .NET, Elsa doesn't exist for you.
- **Visual designer focus** — Elsa's strength is its Blazor-based workflow designer (Elsa Studio). Workflows can be defined in code, JSON, or visually. This makes it closer to a BPM tool than a developer-first workflow library.
- **No durable execution** — Elsa persists workflow state and supports long-running workflows with bookmarks/signals, but it's not a durable execution engine. There's no automatic checkpointing of arbitrary code — you design workflows as activity graphs, not as regular application code.
- **Ecosystem limitations** — Smaller community than Temporal or Airflow. Enterprise support is available through ELSA-X, but the ecosystem of integrations and production battle-testing is limited.

### What Developers Actually Want

After studying these solutions, the pattern is clear:

- **Flexibility** — No DSLs, no determinism constraints, no artificial splits between "workflow" and "activity" code
- **Get durability for free** — Checkpointing and recovery without restructuring your logic
- **Scale without infrastructure** — A library that works in a single process or across a cluster, without requiring a platform team
- **No lock-in** — MIT license, pluggable backends, standard async patterns that transfer to any runtime

---

## Features

### Core (Open Source)

| Feature                        | Status |
| ------------------------------ | ------ |
| Durable task execution         | Stable |
| Automatic checkpointing        | Stable |
| Fork/join parallelism          | Stable |
| Crash recovery and resume      | Stable |
| Pause and resume workflows     | Stable |
| Panic-safe execution           | Stable |
| Pluggable storage backends     | Stable |
| Durable timers/delays          | Stable |
| Automatic retries with backoff | Stable |
| Distributed worker pools       | Stable |
| Claim-based task distribution  | Stable |
| Zero-copy serialization (rkyv) | Stable |

### Python Bindings

| Feature | Status |
|---|---|
| `@task` decorator with metadata | Done |
| Fluent `Flow` builder API | Done |
| Durable execution with checkpointing | Done |
| Resume, cancel, pause and unpause | Done |
| Fork/join with multi-step branches | Done |
| Durable delays (`.delay()`) | Done |
| Pydantic integration (automatic validation) | Done |
| Type stubs and PEP 561 compliance | Done |
| Async task support | Done |

### Pluggable Codecs

Serialization is pluggable—bring your own format or use the built-in options:

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

### Pluggable Storage Backends

```rust
// In-memory (testing)
let backend = InMemoryBackend::new();

// PostgreSQL (production)
let backend = PostgresBackend::new(pool);

// Custom (bring your own)
impl PersistentBackend for MyBackend { ... }
```

---

## Quick Start

### Python

Install with:

```bash
cd python && pip install -e ".[dev]"
```

**Simple workflow:**

```python
from sayiir import task, Flow, run_workflow

@task
def fetch_user(user_id: int) -> dict:
    return {"id": user_id, "name": "Alice"}

@task
def send_email(user: dict) -> str:
    return f"Sent welcome to {user['name']}"

workflow = Flow("welcome").then(fetch_user).then(send_email).build()
result = run_workflow(workflow, 42)
```

**Durable workflow (survives crashes):**

```python
from sayiir import task, Flow, run_durable_workflow, InMemoryBackend

@task(timeout_secs=30)
def process_order(order_id: int) -> dict:
    return {"order_id": order_id, "status": "processed"}

@task
def send_confirmation(order: dict) -> str:
    return f"Confirmed order {order['order_id']}"

workflow = Flow("order").then(process_order).then(send_confirmation).build()

# Checkpoints after each task — resume from last checkpoint on crash
status = run_durable_workflow(workflow, "order-123", 42)
print(status.output)
```

**Durable delays (survive restarts):**

```python
from datetime import timedelta
from sayiir import task, Flow, run_durable_workflow

@task
def fetch_data(url: str) -> dict:
    return {"url": url, "data": "..."}

@task
def process_data(data: dict) -> str:
    return f"processed {data['url']}"

workflow = (
    Flow("pipeline")
    .then(fetch_data)
    .delay("wait_1h", timedelta(hours=1))  # durable — no worker held
    .then(process_data)
    .build()
)
status = run_durable_workflow(workflow, "job-1", "https://api.example.com")
```

**Automatic retries with exponential backoff:**

```python
from sayiir import task, Flow, RetryPolicy, run_durable_workflow

@task(timeout_secs=10, retries=RetryPolicy(max_retries=2, initial_delay_secs=1.0, backoff_multiplier=2.0))
def call_api(url: str) -> dict:
    return requests.get(url).json()  # retries on failure or timeout: 1s, 2s, 4s

@task
def process(data: dict) -> str:
    return f"processed {len(data)} items"

workflow = Flow("resilient").then(call_api).then(process).build()
status = run_durable_workflow(workflow, "job-1", "https://api.example.com/data")
```

Retries are durable — if the process crashes mid-backoff, the retry count and next-retry time are recovered from the checkpoint. Timeouts also trigger retries: a task that times out is treated the same as a task that throws an error.

**Parallel execution (fork/join):**

```python
from sayiir import task, Flow, run_workflow

@task
def validate_payment(order: dict) -> dict:
    return {"payment": "valid"}

@task
def check_inventory(order: dict) -> dict:
    return {"stock": "available"}

@task
def finalize(results: dict) -> str:
    return f"Order complete: {results}"

workflow = (
    Flow("checkout")
    .fork()
        .branch(validate_payment)
        .branch(check_inventory)
    .join(finalize)
    .build()
)
result = run_workflow(workflow, {"order_id": 1})
```

**Pydantic integration (automatic validation):**

```python
from pydantic import BaseModel
from sayiir import task, Flow, run_workflow

class OrderInput(BaseModel):
    order_id: int
    amount: float

class OrderResult(BaseModel):
    status: str
    message: str

@task
def process(order: OrderInput) -> OrderResult:
    return OrderResult(status="ok", message=f"Processed ${order.amount}")

workflow = Flow("typed").then(process).build()
result = run_workflow(workflow, {"order_id": 1, "amount": 99.99})
```

### Rust

**Single process with checkpointing:**

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

**Distributed workers:**

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

**Durable delays:**

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

**Automatic retries with exponential backoff:**

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

**DAG workflows (fork/join):**

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

**Task registry (reusable activities):**

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

---

## Architecture

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

## Deployment

### Open Source

| Platform           | Status      | Notes                     |
| ------------------ | ----------- | ------------------------- |
| Bare metal / VMs   | Ready       | Any Linux/macOS/Windows   |
| Kubernetes         | Ready       | StatefulSet or Deployment |
| AWS ECS / Fargate  | Ready       | Container-based           |
| AWS Lambda         | Ready       | With external persistence |
| Cloudflare Workers | In Progress | Via Durable Objects       |

### Enterprise (Planned)

For teams that need more:

- **Managed Control Plane** — Scalable gRPC server on Kubernetes
- **Web UI** — Workflow visualization, debugging, manual interventions
- **Audit Logging** — Complete execution history for compliance
- **Time-Critical Tasks** — Hard deadline enforcement with SLA guarantees, automatic escalation on breach
- **Worker Pools** — Isolated execution environments per tenant/workload
- **Code Sandboxing** — Secure execution of untrusted or tenant-provided code with resource limits and isolation
- **Auto-scaling** — Dynamic worker provisioning based on queue depth
- **Security** — mTLS worker authentication, RBAC, payload-level encryption, snapshot integrity verification (HMAC), secure credential passing (Vault, AWS Secrets Manager)

---

## Use Cases

Every enterprise runs on workflows — whether they call them that or not. Order processing, user onboarding, payment reconciliation, data pipelines, compliance checks. These are all multi-step processes that need to be reliable, observable, and recoverable when things go wrong.

Sayiir is built for teams that need these guarantees without the complexity of traditional workflow engines.

### Fintech & Payments

- Payment processing pipelines with retry and reconciliation
- KYC/AML verification workflows
- Transaction monitoring and fraud detection
- Multi-step onboarding flows

### E-commerce & Marketplaces

- Order fulfillment orchestration
- Inventory synchronization across channels
- Refund and return processing
- Seller payout workflows

### SaaS & B2B

- User provisioning and deprovisioning
- Subscription lifecycle management
- Data pipeline orchestration
- Multi-tenant job scheduling

### Healthcare & Compliance

- Patient data processing with audit trails
- Insurance claim workflows
- Regulatory reporting pipelines
- Document processing and approvals

### Data & ML Pipelines

- ETL/ELT workflow orchestration
- ML training pipelines (prep → train → evaluate → deploy)
- Batch inference scheduling
- Feature engineering workflows
- Data quality and validation checks

### AI Agents

- Multi-step agent orchestration with checkpointing
- Tool call durability — resume after failures without re-running LLM calls
- Human-in-the-loop approval chains
- Long-running agent loops (hours/days) with crash recovery
- No determinism constraints — LLM calls are inherently non-deterministic, and that's fine

### Infrastructure & DevOps

- CI/CD pipeline orchestration
- Infrastructure provisioning workflows
- Incident response automation
- Scheduled maintenance tasks

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

---

## Status

Sayiir is under active development. Core is stable, some features are in progress.

| Component            | Status      |
| -------------------- | ----------- |
| sayiir-core          | Stable      |
| sayiir-runtime       | Stable      |
| sayiir-persistence   | Stable      |
| Python bindings      | Stable      |
| PostgreSQL backend   | In Progress |
| Cloudflare Workers   | In Progress |
| Node.js bindings     | Planned     |
| Enterprise server    | Planned     |

See [ROADMAP.md](./ROADMAP.md) for details.

---

## Support the Project

Sayiir is an early-stage open source project. We're building the workflow engine we wish existed.

**We're looking for:**

- **Contributors** — Storage backends, language bindings, documentation
- **Maintainers** — Join the core team, review PRs, guide direction
- **Sponsors** — Fund development of enterprise features
- **Early adopters** — Try it, break it, tell us what's missing

Interested? [Join our Discord](https://discord.gg/MWSzsHeg) or open an issue.

---

## Community

- [Discord](https://discord.gg/MWSzsHeg) — Questions, feedback, contributions
- GitHub Issues — Bugs and feature requests
- PRs welcome — Check `good first issue` labels

---

## License

MIT

---

**Stop fighting your workflow engine. Start shipping.**
