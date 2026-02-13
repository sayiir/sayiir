# Quick Start: Python

## Installation

```bash
cd sayiir-python
uv venv && source .venv/bin/activate
maturin develop
pip install -e ".[dev]"
```

---

## Simple workflow

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

---

## Durable workflow (survives crashes)

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

### With Postgres

Pass a `PostgresBackend` to persist workflow state in PostgreSQL — everything
else stays the same.

```python
from sayiir import task, Flow, run_durable_workflow, PostgresBackend

@task(timeout_secs=30)
def process_order(order_id: int) -> dict:
    return {"order_id": order_id, "status": "processed"}

@task
def send_confirmation(order: dict) -> str:
    return f"Confirmed order {order['order_id']}"

workflow = Flow("order").then(process_order).then(send_confirmation).build()

# Connects and runs migrations automatically
backend = PostgresBackend("postgresql://localhost/sayiir")
status = run_durable_workflow(workflow, "order-123", 42, backend=backend)
print(status.output)
```

---

## Durable delays (survive restarts)

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

---

## Automatic retries with exponential backoff

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

---

## Parallel execution (fork/join)

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

---

## Pydantic integration (automatic validation)

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
