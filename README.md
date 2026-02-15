# Sayiir

**Durable, fast workflow engine that feel like writing normal code.** written in Rust, Python bindings — no DSL, worflows from your plain code.

[![crates.io](https://img.shields.io/crates/v/sayiir-core.svg)](https://crates.io/crates/sayiir-core)
[![docs.rs](https://docs.rs/sayiir-core/badge.svg)](https://docs.rs/sayiir-core)
[![crates.io downloads](https://img.shields.io/crates/d/sayiir-core.svg)](https://crates.io/crates/sayiir-core)
[![PyPI](https://img.shields.io/pypi/v/sayiir.svg)](https://pypi.org/project/sayiir/)
[![PyPI downloads](https://static.pepy.tech/badge/sayiir/month)](https://pepy.tech/project/sayiir)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-93450a.svg)](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html)
[![Python](https://img.shields.io/badge/python-3.10–3.13-3776ab.svg)](https://www.python.org)
[![Discord](https://img.shields.io/badge/Discord-Join-7289da)](https://discord.gg/MWSzsHeg)

> Sayiir is under active development. Core functionality works. We welcome contributors, maintainers, and sponsors.

---

## Why Sayiir?

**Fast by design.** Rust-native orchestration with zero-copy serialization (rkyv) and no replay overhead — resume from the last checkpoint, not from the beginning of your workflow history.

**No deterministic replay.** Sayiir checkpoints after each task and resumes from the last checkpoint — your code can call any API, use any library, with zero determinism constraints.

**A library, not a platform.** Import it, write your workflow, run it. No separate infrastructure to deploy or operate.

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

// Register and compose
let mut registry = TaskRegistry::new();
FetchUser::register(&mut registry, codec.clone(), FetchUser::new());
SendEmail::register(&mut registry, codec.clone(), SendEmail::new());

let workflow = workflow!("welcome", JsonCodec, registry,
    fetch_user => send_email
).unwrap();

runner.run(workflow.workflow(), "welcome-user-123", user_id).await?;
```

No annotations. No YAML. No separate worker processes. Just code.

---

## Documentation

**[docs.sayiir.dev](https://docs.sayiir.dev)** — Full documentation with guides, tutorials, and API reference.

| Topic | Description |
|---|---|
| [Python Quick Start](https://docs.sayiir.dev/getting-started/python/) | Install, define tasks, build durable workflows |
| [Rust Quick Start](https://docs.sayiir.dev/getting-started/rust/) | `#[task]` macro, `workflow!` DSL, CheckpointingRunner |
| [How Sayiir Works](https://docs.sayiir.dev/concepts/how-it-works/) | The 3 design decisions that make Sayiir different |
| [Architecture](https://docs.sayiir.dev/concepts/architecture/) | Hexagonal design, pluggable backends/codecs |
| [Guides](https://docs.sayiir.dev/guides/durable-workflows/) | Retries, signals, fork/join, distributed workers, Postgres |
| [Tutorials](https://docs.sayiir.dev/tutorials/order-processing-python/) | End-to-end walkthroughs with runnable examples |
| [API Reference](https://docs.sayiir.dev/reference/python-api/) | Python and Rust API docs |
| [Roadmap](ROADMAP.md) | What's planned next |
| [Contributing](CONTRIBUTING.md) | How to set up, build, test, and submit PRs |

---

## Status

| Component            | Status      |
| -------------------- | ----------- |
| sayiir-core          | ✅      |
| sayiir-macros        | ✅      |
| sayiir-runtime       | ✅      |
| sayiir-persistence   | ✅      |
| Python bindings      | ✅      |
| PostgreSQL backend   | ✅ (requires PostgreSQL 13+) |
| Cloudflare Workers   | In Progress |
| Node.js bindings     | Planned     |
| Enterprise server    | Planned     |

---

## Support the Project

We're looking for **contributors**, **maintainers**, **sponsors**, and **early adopters**.

Interested? [Join our Discord](https://discord.gg/MWSzsHeg) or open an issue.

## Community

- [Discord](https://discord.gg/MWSzsHeg) — Questions, feedback, contributions
- GitHub Issues — Bugs and feature requests
- PRs welcome — Check `good first issue` labels
- See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, build, and PR guidelines

## License

MIT

---

**Stop fighting your workflow engine. Start shipping.**
