# Sayiir

**Durable workflows for Python, powered by a Rust runtime.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/sayiir/sayiir/blob/main/LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Discord](https://img.shields.io/badge/Discord-Join-7289da)](https://discord.gg/MWSzsHeg)

Write plain Python functions. Sayiir makes them durable — automatic checkpointing, crash recovery, and parallel execution with zero infrastructure.

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
# "Sent welcome to Alice"
```

No DSL. No YAML. No determinism constraints. No infrastructure to deploy.

## Why Sayiir?

- **No replay, no determinism rules** — Unlike Temporal, Restate, and other replay-based engines, Sayiir checkpoints after each task and resumes from the last checkpoint. Your tasks can call any API, use any library, read the clock, generate random values. No restrictions.
- **A library, not a platform** — `pip install sayiir` and write workflows. No server cluster, no separate services. Optional PostgreSQL for production persistence.
- **Rust core** — All orchestration, checkpointing, and execution runs in Rust via PyO3. You write Python; Rust handles the hard parts.
- **Pydantic integration** — Automatic input validation and output serialization for `BaseModel` types.
- **Type-safe** — Full type stubs (`.pyi`) and PEP 561 `py.typed` marker. Works with mypy and pyright.

## Installation

```bash
pip install sayiir
```

**From source (development):**

```bash
git clone https://github.com/sayiir/sayiir.git
cd sayiir/sayiir-python
pip install -e ".[dev]"
```

Requires a Rust toolchain (`rustup`) for building from source.

## Quickstart

### Inline lambdas — zero boilerplate

```python
from sayiir import Flow, run_workflow

workflow = (
    Flow("pipeline")
    .then(lambda x: x * 2)
    .then(lambda x: x + 1)
    .then(lambda x: str(x))
    .build()
)
result = run_workflow(workflow, 5)
# "11"  (5 * 2 = 10, 10 + 1 = 11, str(11))
```

No decorators, no registration — just pass any callable. Use `@task` when you need metadata (retries, timeouts, tags) or explicit naming.

### Sequential workflow

```python
from sayiir import task, Flow, run_workflow

@task
def double(x: int) -> int:
    return x * 2

@task
def add_ten(x: int) -> int:
    return x + 10

workflow = Flow("math").then(double).then(add_ten).build()
result = run_workflow(workflow, 5)
# 20  (5 * 2 = 10, 10 + 10 = 20)
```

### Durable workflow (survives crashes)

```python
from sayiir import task, Flow, run_durable_workflow

@task(timeout_secs=30)
def process_order(order_id: int) -> dict:
    return {"order_id": order_id, "status": "processed"}

@task
def send_confirmation(order: dict) -> str:
    return f"Confirmed order {order['order_id']}"

workflow = Flow("order").then(process_order).then(send_confirmation).build()

# Checkpoints after each task — resumes from last checkpoint on crash
status = run_durable_workflow(workflow, "order-123", 42)
print(status.output)           # "Confirmed order 42"
print(status.is_completed())   # True
```

### PostgreSQL persistence

```python
from sayiir import task, Flow, PostgresBackend, run_durable_workflow

@task
def process(x: int) -> int:
    return x * 2

workflow = Flow("persistent").then(process).build()

# Auto-runs migrations on first connect
backend = PostgresBackend("postgresql://localhost/sayiir")
status = run_durable_workflow(workflow, "run-001", 21, backend=backend)
```

### Retry policy

```python
from sayiir import task, RetryPolicy

@task(retries=RetryPolicy(max_retries=3, initial_delay_secs=0.5, backoff_multiplier=2.0))
def flaky_call(url: str) -> dict:
    return requests.get(url).json()
```

### Parallel execution (fork/join)

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

### Multi-step branches

```python
workflow = (
    Flow("pipeline")
    .fork()
        .branch(fetch_data, transform, validate)  # 3-step branch
        .branch(fetch_metadata)                    # 1-step branch
    .join(merge_results)
    .build()
)
```

### Pydantic integration

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
# Automatic validation on input, serialization on output
```

### Task metadata

```python
@task(
    name="Process Payment",
    timeout_secs=60,
    tags=["payments", "critical"],
    description="Charges the customer's payment method",
)
def process_payment(order: dict) -> dict:
    ...
```

## API Reference

### Decorators

- **`@task`** — Mark a function as a workflow task. Optional params: `name`, `timeout_secs`, `retries`, `tags`, `description`.

### Flow Builder

- **`Flow(name)`** — Create a new workflow builder.
- **`.then(task_fn, *, name=None)`** — Append a task to the workflow. Accepts `@task`-decorated functions, plain functions, or lambdas. Use `name` to set an explicit task ID.
- **`.fork()`** — Start parallel branches. Returns a `ForkBuilder`.
- **`.branch(task_fn, ...)`** — Add a branch (one or more chained tasks).
- **`.join(task_fn)`** — Merge parallel branches. Join function receives `dict[str, value]`.
- **`.build()`** — Finalize and return a `Workflow`.

### Execution

- **`run_workflow(workflow, input)`** — Execute a workflow in-memory. Returns the final output.
- **`run_durable_workflow(workflow, instance_id, input, backend=None)`** — Execute with checkpointing. Returns a `WorkflowStatus`.
- **`resume_workflow(workflow, instance_id, backend)`** — Resume a workflow from its last checkpoint.
- **`cancel_workflow(instance_id, backend, reason=None, cancelled_by=None)`** — Cancel a running workflow.
- **`pause_workflow(instance_id, backend, reason=None, paused_by=None)`** — Pause a running workflow.
- **`unpause_workflow(instance_id, backend)`** — Unpause a paused workflow.

### WorkflowStatus

- **`.output`** — The final output value (if completed).
- **`.status`** — `"completed"`, `"failed"`, `"cancelled"`, or `"in_progress"`.
- **`.is_completed()`** / **`.is_failed()`** / **`.is_cancelled()`** / **`.is_paused()`** / **`.is_in_progress()`** — Status checks.
- **`.error`** — Error message (if failed).
- **`.reason`** / **`.cancelled_by`** — Cancellation details.

### Retry

- **`RetryPolicy(max_retries=2, initial_delay_secs=1.0, backoff_multiplier=2.0)`** — Exponential backoff retry policy for tasks.

### Backends

- **`InMemoryBackend()`** — In-memory storage for development and testing (default).
- **`PostgresBackend(url)`** — PostgreSQL persistence. Auto-runs migrations on first connect.

## Architecture

```
Your Python code          Sayiir (Rust)              Storage
┌──────────────┐    ┌─────────────────────┐    ┌──────────────┐
│  @task       │───>│  Orchestration      │───>│  Checkpoint  │
│  functions   │    │  Checkpointing      │    │  after each  │
│              │<───│  Crash recovery     │<───│  task        │
└──────────────┘    │  Fork/join          │    └──────────────┘
                    │  Serialization      │
                    └─────────────────────┘
```

Python provides task implementations. Rust handles everything else: building the execution graph, running tasks in order, checkpointing results, recovering from crashes, and managing parallel branches.

The project follows hexagonal architecture — the core domain has zero infrastructure dependencies, all dependencies flow inward, and every integration point (storage, serialization, execution) is a swappable trait-based adapter.

## Requirements

- Python 3.10+
- Optional: `pydantic >= 2.0` for automatic model validation

## License

MIT

## Links

- [GitHub](https://github.com/sayiir/sayiir)
- [Discord](https://discord.gg/MWSzsHeg)
- [Roadmap](https://github.com/sayiir/sayiir/blob/main/ROADMAP.md)
