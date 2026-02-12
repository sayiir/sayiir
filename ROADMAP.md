# Roadmap

This document outlines where Sayiir is, where it's going, and why — informed by what the durable execution space actually needs.

---

## Current State

### What Works Today

**Rust Core (Stable)**

| Feature | Status |
|---|---|
| Durable task execution with automatic checkpointing | Stable |
| Crash recovery and deterministic resume | Stable |
| Fork/join parallelism with heterogeneous branch outputs | Stable |
| Distributed worker pools with claim-based task distribution | Stable |
| Pluggable storage backends (`PersistentBackend` trait) | Stable |
| Pluggable codecs (rkyv zero-copy, JSON, custom) | Stable |
| Task registry for serializable workflows | Stable |
| Workflow serialization with definition hash validation | Stable |
| Durable delay/timer primitives (`sleep` between steps) | Stable |
| Panic-safe execution | Stable |
| `WorkflowContext` with task-local metadata access | Stable |
| InMemory backend (development/testing) | Stable |

**Python Bindings (Stable)**

| Feature | Status |
|---|---|
| `@task` decorator with metadata (timeout, tags, description) | Done |
| Fluent `Flow` builder API (`.then()`, `.fork()`, `.branch()`, `.join()`) | Done |
| Simple execution (`run_workflow`) | Done |
| Durable execution with checkpointing (`run_durable_workflow`) | Done |
| Resume and cancel from Python | Done |
| Fork/join with multi-step branches | Done |
| Pydantic integration (automatic validation/serialization) | Done |
| Type stubs (`.pyi`) and PEP 561 compliance | Done |
| Async task support (via `asyncio.run()`) | Done |
| Durable delays (`.delay()` with `timedelta` support) | Done |
| `InMemoryBackend` exposed to Python | Done |
| `WorkflowStatus` with error/cancellation details | Done |

---

## Phase 0 — Python Bindings Polish

The Python SDK is the first language binding and the template for all future bindings. Getting this right matters.

**API Completeness**

- [x] `WorkflowStatus.output` — carry workflow result through durable engine
- [x] `is_in_progress()` method on `WorkflowStatus`
- [x] `resume_workflow` / `cancel_workflow` module-level helpers
- [x] Custom exception hierarchy (`WorkflowError`, `TaskError`, `BackendError`)
- [x] Updated type stubs (`.pyi`) with output, exceptions, `is_in_progress`
- [x] Resume returns decoded output for `AlreadyTerminal(Completed)`

**Documentation & Examples**

- [ ] Python package README (PyPI landing page)
- [ ] Quickstart guide with real-world examples
- [ ] API reference with comprehensive docstrings
- [ ] Error handling guide (what exceptions, when, why)
- [ ] Fork/join patterns cookbook
- [ ] Pydantic integration guide

**API Refinements**

- [ ] Native async/await execution path (no `asyncio.run()` workaround — works in Jupyter, existing event loops)
- [ ] Better error messages for common mistakes (missing `@task`, wrong input types)
- [ ] Workflow composition (reuse sub-flows as steps in larger flows)

**Testing & Quality**

- [ ] Expand test coverage for edge cases (timeout enforcement, concurrent instances)
- [ ] CI pipeline for Python bindings (maturin build + pytest across Python 3.10-3.13)
- [ ] Publish to PyPI via GitHub Actions

---

## Phase 1 — Production Readiness

The features every team needs before they'll trust Sayiir with real workloads.

### PostgreSQL Backend

The #1 blocker for production adoption. Nobody ships with InMemory.

- [ ] Schema design (workflows, snapshots, claims, heartbeats)
- [ ] Connection pooling (deadpool-postgres or sqlx)
- [ ] Migration tooling (embedded migrations via refinery or sqlx)
- [ ] Expose to Python bindings

### Retry Policies

Every competitor has this. Table stakes.

- [ ] Configurable per-task: max attempts, initial delay, backoff multiplier, max delay
- [ ] Exponential backoff with jitter
- [ ] Retry-aware checkpointing (don't lose retry count on crash)
- [ ] `RetryPolicy` already defined in Python — wire it through Rust runtime

### Task Timeouts

- [ ] Per-task timeout enforcement in runtime
- [ ] Timeout cancellation with clear error propagation
- [ ] `timeout_secs` already in `@task` metadata — wire it through

---

## Phase 2 — Real-World Workflow Patterns

The features that unlock the remaining 80% of use cases: anything that waits for external input or time.

### Durable Sleep / Timers ✅

Pause a workflow for minutes, hours, or days — surviving process restarts.

- [x] `delay(duration)` as a first-class workflow primitive (Rust + Python)
- [x] Timer persisted to backend, not held in memory
- [x] Resume after timer expiry on any worker

### Signals / External Events

Pause a workflow until an external event arrives (payment confirmed, human approved, webhook received).

- [ ] `wait_for_signal(signal_name, timeout)` primitive
- [ ] `send_signal(instance_id, signal_name, payload)` API
- [ ] Enables: human-in-the-loop, payment callbacks, approval chains, webhook-driven flows

### Child Workflows

Compose workflows from other workflows.

- [ ] `run_child_workflow(workflow, input)` primitive
- [ ] Independent lifecycle (own instance ID, own checkpointing)
- [ ] Parent can wait for child or fire-and-forget

---

## Phase 3 — Ecosystem

### TypeScript / Node.js Bindings

Python + TypeScript covers ~90% of the developer market for this space.

- [ ] NAPI-RS bindings (same architecture as Python: thin layer, Rust orchestrates)
- [ ] Promise-based API
- [ ] TypeScript type definitions
- [ ] npm package

### Observability

Teams need visibility into what's running, what's stuck, and what failed.

- [ ] OpenTelemetry integration (span-per-task)
- [ ] Prometheus/OpenMetrics export (task latency, queue depth, worker utilization)
- [ ] Structured logging with correlation IDs

### Scheduling / Cron

- [ ] Cron-style recurring workflow triggers
- [ ] Backfill support
- [ ] Timezone-aware scheduling

---

## Phase 4 — Advanced Runtime

### Queuing Primitives

- [ ] Concurrency control (max N instances of workflow X)
- [ ] Rate limiting (max N tasks/second)
- [ ] Priority queues (urgent workflows first)
- [ ] Dead letter queue for permanently failed tasks

### Two-Phase Claiming

Distinguish "reserved" from "executing" for faster failure detection.

```
Available → Reserved (short TTL) → Executing (heartbeat) → Completed
                ↓ (TTL expires)
            Available (fast recovery)
```

### Worker Affinity / Task Routing

Route specific task types to specific worker pools (GPU tasks to GPU workers, etc.).

### Eternal Workflows (ContinueAsNew)

Long-running workflows that loop indefinitely (monitoring, polling, recurring processing) without unbounded state growth.

- [ ] `continue_as_new(input)` primitive — restart the workflow with fresh state
- [ ] Completed tasks from previous iteration are discarded, keeping snapshot size constant
- [ ] Iteration count and metadata carried forward
- [ ] Note: less critical than in replay-based engines (Sayiir has no growing history), but still needed for workflows that accumulate task results over thousands of iterations

### Durable Entities (Actor Model)

Stateful, addressable entities with durable state — virtual actor pattern (comparable to Azure Durable Entities, Orleans grains).

- [ ] `Entity` trait with typed state and operations
- [ ] Addressable by entity ID — send operations from workflows or external callers
- [ ] State persisted via `PersistentBackend` (same pluggable storage)
- [ ] Enables: shopping carts, counters, aggregators, device twins, session state

### Workflow Versioning

The checkpoint model makes this fundamentally easier than replay-based systems. When the workflow definition changes:

- [ ] Detect definition hash mismatch on resume
- [ ] Migration strategies: complete-in-place, drain-and-restart, version routing
- [ ] No replay storms (unlike Temporal) — just resume from last checkpoint

---

## Phase 5 — Edge & Serverless

### Cloudflare Workers

- [ ] Durable Objects as persistence backend
- [ ] Stateless worker execution model
- [ ] Cold start optimization
- [ ] WASM compilation of core runtime

### SQLite Backend

Single-binary durable execution with zero infrastructure. For CLI tools, edge functions, embedded systems.

- [ ] SQLite via rusqlite
- [ ] WAL mode for concurrent reads
- [ ] Expose to all language bindings

---

## Enterprise Features (Planned)

Commercial offering for teams that need more:

- **Managed control plane** — Scalable gRPC server, Kubernetes-native
- **Web UI** — Workflow visualization, real-time monitoring, manual interventions
- **Audit logging** — Immutable execution history for compliance
- **Time-critical tasks** — Hard deadline enforcement, SLA guarantees, automatic escalation
- **Multi-tenancy** — Isolated worker pools, resource quotas, tenant-specific config
- **Security** — mTLS, RBAC, secrets management (Vault, AWS Secrets Manager)
- **Auto-scaling** — Queue-depth-based worker provisioning, Kubernetes HPA/KEDA
- **Code sandboxing** — Secure execution of untrusted/tenant-provided code

---

## Contributing

Want to help? Check out issues labeled `good first issue` or join our [Discord](https://discord.gg/MWSzsHeg).

Areas where contributions are especially welcome:

- Storage backend implementations (PostgreSQL, SQLite, Redis)
- Language binding prototypes (TypeScript, Go)
- Documentation, examples, and tutorials
- Testing and benchmarking
