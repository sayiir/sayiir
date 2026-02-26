# Sayiir

**Durable, fast workflow engine that feels like writing normal code.** Graph-based, continuation-driven execution with a Rust core and Python/Node.js bindings — no DSL, no replay, workflows from your plain code.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-Join-7289da)](https://discord.gg/A2jWBFZsNK)

[![crates.io](https://img.shields.io/crates/v/sayiir-core.svg)](https://crates.io/crates/sayiir-core)
[![docs.rs](https://docs.rs/sayiir-core/badge.svg)](https://docs.rs/sayiir-core)
[![crates.io downloads](https://img.shields.io/crates/d/sayiir-core.svg)](https://crates.io/crates/sayiir-core)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-93450a.svg)](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html)

[![PyPI](https://img.shields.io/pypi/v/sayiir.svg)](https://pypi.org/project/sayiir/)
[![PyPI downloads](https://static.pepy.tech/badge/sayiir/month)](https://pepy.tech/project/sayiir)
[![Python](https://img.shields.io/badge/python-3.10–3.13-3776ab.svg)](https://www.python.org)

[![npm](https://img.shields.io/npm/v/sayiir.svg)](https://www.npmjs.com/package/sayiir)
[![npm downloads](https://img.shields.io/npm/dm/sayiir.svg)](https://www.npmjs.com/package/sayiir)
[![Node.js](https://img.shields.io/badge/node-18%20%7C%2020%20%7C%2022-339933.svg)](https://nodejs.org)

> Sayiir is under active development. Core functionality works. We welcome contributors, maintainers, and sponsors.

- &#129408; **Rust core** — High-performance, memory-safe workflow engine
- &#128737; **Durable** — Automatic checkpointing & crash recovery with pluggable persistence
- &#129520; **Multi-language** — Type-safe Python, TypeScript, and Rust bindings
- &#10024; **Built for developers** — Low learning curve; native language idioms, async code you already know, no DSL. No separate server or infra to deploy; get up and running in minutes. [Enterprise server](https://docs.sayiir.dev/roadmap/) in active development for when you need one
- &#9208; **Workflow control** — Cancel, pause, and resume running workflow instances
- &#128065; **Observability** — Built-in OpenTelemetry tracing and logging for full workflow visibility

---

## Why Sayiir?

&#9889;&ensp;**No replay overhead.** Resume from the last checkpoint, not from the beginning of your workflow history. JSON by default, swap to zero-copy rkyv or any binary format via the pluggable codec abstraction.

&#128274;&ensp;**No determinism constraints.** Continuation-based execution means your code can call any API, use any library — no purity rules, no sandboxing.

&#9997;&ensp;**Minimal learning curve.** Familiar language patterns with sensible defaults that get out of your way. No DSL, no YAML — just code.

#### 🐍 Python

```python
from sayiir import task, Flow, run_workflow

@task
def fetch_user(user_id: int) -> dict:
    return {"id": user_id, "name": "Alice"}

@task
def send_email(user: dict) -> str:
    return f"Sent welcome to {user['name']}"

workflow = Flow("welcome").then(fetch_user).then(send_email).build()

# Quick run — no persistence
result = run_workflow(workflow, 42)

# Or plug in a durable backend for crash recovery
from sayiir import run_durable_workflow, PostgresBackend
instance_id = f"welcome-{user_id}"
status = run_durable_workflow(workflow, instance_id, 42, backend=PostgresBackend("postgresql://localhost/sayiir"))
```

<a href="https://docs.sayiir.dev/playground/"><img src="https://img.shields.io/badge/Try_it_live-▶-00C853" alt="Try it live" height="20"></a>

#### 🦀 Rust

```rust
use sayiir_runtime::prelude::*;

#[task(timeout = "30s", retries = 3)]
async fn fetch_user(id: UserId) -> Result<User, BoxError> {
    db.get_user(id).await
}

#[task]
async fn send_email(user: User) -> Result<(), BoxError> {
    email_service.send_welcome(&user).await
}

// Compose — workflow! auto-registers all tasks
let workflow = workflow! {
    name: "welcome",
    steps: [fetch_user, send_email]
}
.unwrap();

// Quick run — no persistence
workflow.run_once(user_id).await?;

// Or plug in a durable backend for crash recovery
let runner = CheckpointingRunner::new(PostgresBackend::connect("postgres://...").await?);
let instance_id = format!("welcome-{user_id}");
runner.run(&workflow, &instance_id, user_id).await?;
```

#### <img src="https://img.shields.io/badge/Node.js-339933?logo=nodedotjs&logoColor=white&style=flat-square" height="20">

```typescript
import { task, flow, runWorkflow } from "sayiir";

const fetchUser = task("fetch-user", (id: number) => {
  return { id, name: "Alice" };
});

const sendEmail = task("send-email", (user: { id: number; name: string }) => {
  return `Sent welcome to ${user.name}`;
});

const workflow = flow<number>("welcome")
  .then(fetchUser)
  .then(sendEmail)
  .build();
// Quick run — no persistence
const result = await runWorkflow(workflow, 42);

// Or plug in a durable backend for crash recovery
import { runDurableWorkflow, PostgresBackend } from "sayiir";
const instanceId = `welcome-${42}`;
const status = runDurableWorkflow(workflow, instanceId, 42, PostgresBackend.connect("postgresql://localhost/sayiir"));
```

<a href="https://docs.sayiir.dev/playground/"><img src="https://img.shields.io/badge/Try_it_live-▶-00C853" alt="Try it live" height="20"></a>

---

## When to Use Sayiir

Sayiir is a full-featured, embeddable workflow engine — branching, loops, fork/join, signals, cancel, pause, resume, retries, timeouts — that lives inside your application as a library, not beside it as a platform.

**Sayiir shines when you:**

- Need durable workflows (order sagas, onboarding flows, ETL, data pipelines) without deploying separate infrastructure
- Want `cargo add` / `pip install` / `npm install` and a working workflow engine in minutes, not days
- Already have a Postgres (or just want in-memory for dev) and don't want to manage a separate cluster
- Value a low learning curve — write normal async code, add `@task`, ship to production

> Sayiir isn't trying to replace major workflow platforms. But for many use cases, those platforms add significant infrastructure overhead and complexity that feels like overkill. Sayiir gives you real durability and covers most workflow composition and execution scenarios with less ceremony.

---

## Documentation

**[docs.sayiir.dev](https://docs.sayiir.dev)** — Full documentation with guides, tutorials, and API reference.

**Getting Started**&ensp;
[Python](https://docs.sayiir.dev/getting-started/python/) &#183;
[Node.js](https://docs.sayiir.dev/getting-started/nodejs/) &#183;
[Rust](https://docs.sayiir.dev/getting-started/rust/)

**Learn**&ensp;
[How It Works](https://docs.sayiir.dev/concepts/how-it-works/) &#183;
[Architecture](https://docs.sayiir.dev/concepts/architecture/) &#183;
[Guides](https://docs.sayiir.dev/guides/durable-workflows/) &#183;
[Tutorials](https://docs.sayiir.dev/tutorials/order-processing-python/) &#183;
[Examples](https://github.com/sayiir/sayiir/tree/main/examples)

**Reference**&ensp;
[Python API](https://docs.sayiir.dev/reference/python-api/) &#183;
[Node.js API](https://docs.sayiir.dev/reference/nodejs-api/) &#183;
[Rust API](https://docs.sayiir.dev/reference/rust-api/) &#183;
[Observability & OpenTelemetry](https://docs.sayiir.dev/guides/observability/) &#183;
[Roadmap](https://docs.sayiir.dev/roadmap/) &#183;
[Contributing](CONTRIBUTING.md)

---

## Status

| Component | Status |
|:--|:--|
| **Rust**&ensp; sayiir-core &#183; sayiir-macros &#183; sayiir-runtime &#183; sayiir-persistence | ![stable](https://img.shields.io/badge/stable-brightgreen) |
| **Python** bindings | ![stable](https://img.shields.io/badge/stable-brightgreen) |
| **Node.js** bindings | ![stable](https://img.shields.io/badge/stable-brightgreen) |
| **PostgreSQL** backend | ![stable](https://img.shields.io/badge/stable-brightgreen)&ensp;requires PG 13+ |
| **Cloudflare Workers** | ![in progress](https://img.shields.io/badge/in%20progress-yellow) |
| **Enterprise server** | ![planned](https://img.shields.io/badge/planned-lightgrey) |

---

## Community

We're looking for **contributors**, **maintainers**, **sponsors**, and **early adopters**.

[![Discord](https://img.shields.io/badge/Discord-Join%20us-7289da?style=for-the-badge&logo=discord&logoColor=white)](https://discord.gg/A2jWBFZsNK)

[GitHub Issues](https://github.com/sayiir/sayiir/issues) — Bugs and feature requests&ensp;&#183;&ensp;PRs welcome — check `good first issue` labels&ensp;&#183;&ensp;[CONTRIBUTING.md](CONTRIBUTING.md)

## License

MIT

---

> **Stop fighting your workflow engine. Start shipping.**
