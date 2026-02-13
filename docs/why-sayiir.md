# Why Sayiir

Most workflow engines force you to learn their mental model, their DSL, their way of thinking. You end up writing code *for the engine* instead of writing code *for your business*.

**Sayiir is different.** Write async Rust or plain Python. That's it. Your existing code, your existing patterns, your existing tests — they all just work.

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

---

## How We Compare

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

---

## What Developers Actually Want

After studying these solutions, the pattern is clear:

- **Flexibility** — No DSLs, no determinism constraints, no artificial splits between "workflow" and "activity" code
- **Get durability for free** — Checkpointing and recovery without restructuring your logic
- **Scale without infrastructure** — A library that works in a single process or across a cluster, without requiring a platform team
- **No lock-in** — MIT license, pluggable backends, standard async patterns that transfer to any runtime
