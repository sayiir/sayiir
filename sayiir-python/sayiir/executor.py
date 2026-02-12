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


def resume_workflow(
    workflow: Workflow,
    instance_id: str,
    backend: Any,
) -> Any:
    """Resume a durable workflow from a saved checkpoint.

    Args:
        workflow: The workflow definition (produced by Flow.build())
        instance_id: The instance ID used when the workflow was started
        backend: Persistence backend (must be the same one used for run)

    Returns:
        WorkflowStatus indicating the outcome
    """
    from ._sayiir import PyDurableEngine

    engine = PyDurableEngine(backend)
    return engine.resume(workflow._inner, instance_id, workflow._task_registry)


def cancel_workflow(
    instance_id: str,
    backend: Any,
    reason: str | None = None,
    cancelled_by: str | None = None,
) -> None:
    """Request cancellation of a running durable workflow.

    Args:
        instance_id: The instance ID of the workflow to cancel
        backend: Persistence backend (must be the same one used for run)
        reason: Optional reason for the cancellation
        cancelled_by: Optional identifier of who requested the cancellation
    """
    from ._sayiir import PyDurableEngine

    engine = PyDurableEngine(backend)
    engine.cancel(instance_id, reason, cancelled_by)


def pause_workflow(
    instance_id: str,
    backend: Any,
    reason: str | None = None,
    paused_by: str | None = None,
) -> None:
    """Request pausing of a running durable workflow.

    Args:
        instance_id: The instance ID of the workflow to pause
        backend: Persistence backend (must be the same one used for run)
        reason: Optional reason for the pause
        paused_by: Optional identifier of who requested the pause
    """
    from ._sayiir import PyDurableEngine

    engine = PyDurableEngine(backend)
    engine.pause(instance_id, reason, paused_by)


def unpause_workflow(
    instance_id: str,
    backend: Any,
) -> None:
    """Unpause a paused durable workflow so it can be resumed.

    Args:
        instance_id: The instance ID of the workflow to unpause
        backend: Persistence backend (must be the same one used for run)
    """
    from ._sayiir import PyDurableEngine

    engine = PyDurableEngine(backend)
    engine.unpause(instance_id)
