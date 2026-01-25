"""Sayiir - A workflow orchestration library with Rust performance.

This package provides a Pythonic API for defining and running durable workflows
backed by a high-performance Rust engine.

Example:
    >>> from sayiir import task, flow, Flow, WorkflowEngine
    >>>
    >>> @task
    ... async def fetch_data(url: str) -> dict:
    ...     async with aiohttp.ClientSession() as session:
    ...         async with session.get(url) as resp:
    ...             return await resp.json()
    >>>
    >>> @task
    ... def process(data: dict) -> str:
    ...     return data["result"].upper()
    >>>
    >>> @flow
    ... def my_pipeline():
    ...     return Flow("pipeline").then(fetch_data).then(process).build()
    >>>
    >>> async def main():
    ...     engine = WorkflowEngine()
    ...     result = await engine.run(my_pipeline(), input="https://api.example.com")
    ...     print(result)
"""

from ._sayiir import (
    TaskChannel,
    TaskRequest,
    PyFlowBuilder as FlowBuilder,
    PyForkBuilder as ForkBuilder,
    PyTaskMetadata as TaskMetadata,
    PyWorkflow as Workflow,
    PyWorkflowEngine as WorkflowEngine,
    PyInMemoryBackend as InMemoryBackend,
)

from .decorators import task, flow
from .flow import Flow
from .executor import TaskExecutor, run_with_executor

__all__ = [
    # Rust types
    "TaskChannel",
    "TaskRequest",
    "FlowBuilder",
    "ForkBuilder",
    "TaskMetadata",
    "Workflow",
    "WorkflowEngine",
    "InMemoryBackend",
    # Python decorators
    "task",
    "flow",
    # Python classes
    "Flow",
    "TaskExecutor",
    "run_with_executor",
]

__version__ = "0.1.0"
