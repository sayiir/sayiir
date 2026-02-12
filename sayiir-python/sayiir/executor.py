"""Workflow execution utilities.

The actual execution logic is in Rust. This module provides simple helpers.
"""

from typing import Any

from .flow import Workflow


def run_workflow(workflow: Workflow, input_data: Any) -> Any:
    """Run a workflow to completion (no persistence).

    Args:
        workflow: The workflow to run (produced by Flow.build())
        input_data: Input to the first task

    Returns:
        The workflow result

    Example:
        @task
        def double(x: int) -> int:
            return x * 2

        workflow = Flow("test").then(double).build()
        result = run_workflow(workflow, 21)
        print(result)  # 42
    """
    from ._sayiir import PyWorkflowEngine

    engine = PyWorkflowEngine()
    return engine.run(workflow._inner, input_data, workflow._task_registry)


def run_durable_workflow(
    workflow: Workflow,
    instance_id: str,
    input_data: Any,
    backend: Any = None,
) -> Any:
    """Run a workflow with checkpointing and durability.

    Args:
        workflow: The workflow to run (produced by Flow.build())
        instance_id: Unique identifier for this execution instance
        input_data: Input to the first task
        backend: Persistence backend (defaults to InMemoryBackend)

    Returns:
        WorkflowStatus indicating the outcome

    Example:
        @task
        def double(x: int) -> int:
            return x * 2

        workflow = Flow("test").then(double).build()
        status = run_durable_workflow(workflow, "run-1", 21)
        assert status.is_completed()
    """
    from ._sayiir import PyDurableEngine, PyInMemoryBackend

    if backend is None:
        backend = PyInMemoryBackend()
    engine = PyDurableEngine(backend)
    return engine.run(workflow._inner, instance_id, input_data, workflow._task_registry)
