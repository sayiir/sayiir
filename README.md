# Sayiir

**Durable, fast workflow engine that feels like writing normal code.** Rust core with Python and Node.js bindings — no DSL, workflows from your plain code.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-Join-7289da)](https://discord.gg/MWSzsHeg)

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

---

## Why Sayiir?

&#9889;&ensp;**Fast by design.** Rust-native orchestration with zero-copy serialization (rkyv) and no replay overhead — resume from the last checkpoint, not from the beginning of your workflow history.

&#128274;&ensp;**No deterministic replay.** Sayiir checkpoints after each task and resumes from the last checkpoint — your code can call any API, use any library, with zero determinism constraints.

&#128230;&ensp;**A library, not a platform.** Import it, write your workflow, run it. No separate infrastructure to deploy or operate.

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
result = run_workflow(workflow, 42)
```

<a href="https://gitpod.io/#https://github.com/sayiir/sayiir/tree/main/examples/hello-world-py"><img src="https://img.shields.io/badge/Try_it-Gitpod-FFAE33?logo=gitpod&logoColor=white" alt="Try in Gitpod" height="20"></a>

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
let workflow = workflow!("welcome", JsonCodec, TaskRegistry::new(),
    fetch_user => send_email
).unwrap();

runner.run(workflow.workflow(), "welcome-user-123", user_id).await?;
```

<a href="https://gitpod.io/#https://github.com/sayiir/sayiir/tree/main/examples/hello-world-rs"><img src="https://img.shields.io/badge/Try_it-Gitpod-FFAE33?logo=gitpod&logoColor=white" alt="Try in Gitpod" height="20"></a>

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

const result = await runWorkflow(workflow, 42);
```

<a href="https://gitpod.io/#https://github.com/sayiir/sayiir/tree/main/examples/hello-world-node"><img src="https://img.shields.io/badge/Try_it-Gitpod-FFAE33?logo=gitpod&logoColor=white" alt="Try in Gitpod" height="20"></a>

No annotations. No YAML. No separate worker processes. Just code.

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
[Tutorials](https://docs.sayiir.dev/tutorials/order-processing-python/)

**Reference**&ensp;
[Python API](https://docs.sayiir.dev/reference/python-api/) &#183;
[Node.js API](https://docs.sayiir.dev/reference/nodejs-api/) &#183;
[Rust API](https://docs.sayiir.dev/reference/rust-api/) &#183;
[Roadmap](ROADMAP.md) &#183;
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

[![Discord](https://img.shields.io/badge/Discord-Join%20us-7289da?style=for-the-badge&logo=discord&logoColor=white)](https://discord.gg/MWSzsHeg)

[GitHub Issues](https://github.com/sayiir/sayiir/issues) — Bugs and feature requests&ensp;&#183;&ensp;PRs welcome — check `good first issue` labels&ensp;&#183;&ensp;[CONTRIBUTING.md](CONTRIBUTING.md)

## License

MIT

---

> **Stop fighting your workflow engine. Start shipping.**
