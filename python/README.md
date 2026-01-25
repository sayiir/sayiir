# Sayiir - Python Workflow Library

A high-performance workflow orchestration library with Rust core and Pythonic API.

## Installation

```bash
pip install sayiir
```

Or build from source with maturin:

```bash
cd python
maturin develop
```

## Quick Start

```python
from sayiir import task, flow, Flow, WorkflowEngine, run_with_executor

# Define tasks
@task
async def fetch_data(url: str) -> dict:
    async with aiohttp.ClientSession() as session:
        async with session.get(url) as resp:
            return await resp.json()

@task
def process(data: dict) -> str:
    return data["result"].upper()

# Build workflow
@flow
def my_pipeline():
    return Flow("pipeline").then(fetch_data).then(process).build()

# Run
async def main():
    engine = WorkflowEngine()
    result = await run_with_executor(
        engine,
        my_pipeline(),
        instance_id="run-123",
        input_data="https://api.example.com"
    )
    print(result)
```

## Features

- **High Performance**: Rust-based workflow engine with Python bindings
- **Durable Execution**: Automatic checkpointing and crash recovery
- **Parallel Execution**: Fork/join patterns for concurrent task execution
- **Pythonic API**: Familiar decorators and async/await support
- **Type Safety**: Full type annotations and IDE support
- **Flexible Serialization**: JSON (default) or pickle for complex objects

## Fork/Join Example

```python
@task
async def branch_a(x: int) -> int:
    return x * 2

@task
async def branch_b(x: int) -> int:
    return x + 10

@task
def combine(outputs: dict) -> str:
    return f"a={outputs['a']}, b={outputs['b']}"

@flow
def parallel_flow():
    return (
        Flow("parallel")
        .fork()
            .branch("a", branch_a)
            .branch("b", branch_b)
        .join(combine)
        .build()
    )
```

## Persistence

```python
from sayiir import WorkflowEngine, InMemoryBackend

# Use in-memory backend (default)
engine = WorkflowEngine()

# Or provide custom backend
backend = InMemoryBackend()
engine = WorkflowEngine(backend)

# Resume after crash
result = await engine.resume(my_pipeline(), instance_id="run-123")

# Cancel workflow
engine.cancel(instance_id="run-123", reason="User requested")
```

## Serialization Options

By default, sayiir uses JSON for serialization, which is safe, human-readable,
and interoperable. For objects that aren't JSON-serializable (custom classes,
numpy arrays, etc.), you can use pickle:

```python
# Use pickle for complex objects
workflow = Flow("my_workflow", serializer="pickle").then(task1).build()

# Or at the engine level
engine = WorkflowEngine(serializer="pickle")
```

**Warning**: Only use pickle with trusted data sources. Deserializing untrusted
pickle data can execute arbitrary code.

## License

MIT
