# Sayiir

**Durable workflows that feel like writing normal code.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-Join-7289da)](https://discord.gg/MWSzsHeg)

> **Early Stage Project** — Sayiir is under active development. Core functionality works but APIs may change. We welcome contributors, maintainers, and sponsors.

---

## Why Sayiir?

Most workflow engines force you to learn their mental model, their DSL, their way of thinking. You end up writing code *for the engine* instead of writing code *for your business*.

**Sayiir is different.** Write async Rust. That's it. Your existing code, your existing patterns, your existing tests—they all just work.

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

No annotations. No YAML. No separate worker processes. Just code.

**One problem, solved well.** Sayiir is an orchestrator, not a compute engine. It coordinates steps, handles retries, and checkpoints progress—while the actual heavy lifting (ETL, data pipelines, GPU training, API calls) runs in external systems you call from your tasks. We don't try to be a platform, a UI, or a kitchen sink. We make your workflows durable and let you focus on your business logic.

**Why Rust? Why not just Python, Go or Node.js?** The core runtime is written in Rust for safety, performance, and correctness—the properties that matter most for infrastructure that runs your critical business processes. But Sayiir is not a Rust-only tool. Our goal is to bring durable workflows to Python, Go and Node.js developers through native bindings, so you get Rust's reliability without leaving your ecosystem.

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
| Panic-safe execution           | Stable |
| Pluggable storage backends     | Stable |
| Distributed worker pools       | Stable |
| Claim-based task distribution  | Stable |
| Zero-copy serialization (rkyv) | Stable |

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

### Single Process with Checkpointing

Best for: Single-node deployments, crash recovery, simple setup.

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

### Distributed Workers

Best for: Horizontal scaling, high throughput, fault tolerance across machines.

```rust
use sayiir::{PooledWorker, PostgresBackend};
use std::time::Duration;

let backend = PostgresBackend::new(pool);
let worker = PooledWorker::new("worker-1", backend, registry)
    .with_claim_ttl(Some(Duration::from_secs(300)))
    .with_heartbeat_interval(Some(Duration::from_secs(120)));

// Start polling - tasks are automatically distributed across workers
worker.start_polling(Duration::from_secs(1), workflows).await?;
```

### DAG Workflows (Fork/Join)

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

### Task Registry (Reusable Activities)

Build libraries of reusable tasks and compose them:

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
├──────────────────────────────────────────────────────────────────┤
│                         Sayiir Runtime                           │
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
- **Security** — mTLS, RBAC, secrets management

---

## Use Cases

Every enterprise runs on workflows—whether they call them that or not. Order processing, user onboarding, payment reconciliation, data pipelines, compliance checks. These are all multi-step processes that need to be reliable, observable, and recoverable when things go wrong.

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

## Status

Sayiir is under active development. Core is stable, some features are in progress.

| Component            | Status      |
| -------------------- | ----------- |
| workflow-core        | Stable      |
| workflow-runtime     | Stable      |
| workflow-persistence | Stable      |
| PostgreSQL backend   | In Progress |
| Cloudflare Workers   | In Progress |
| Python bindings      | Planned     |
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
