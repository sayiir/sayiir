"""Client for submitting and controlling workflow instances.

The WorkflowClient does **not** execute tasks — it only creates initial
snapshots and stores lifecycle signals. A :class:`~sayiir.Worker` picks up
and executes the work.

Example::

    from sayiir import Flow, InMemoryBackend, WorkflowClient, task

    @task
    def step(x):
        return x + 1

    wf = Flow("my-wf").then(step).build()
    backend = InMemoryBackend()
    client = WorkflowClient(backend)
    status = client.submit(wf, "run-1", 42)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from ._sayiir import PyWorkflowClient as _PyWorkflowClient

if TYPE_CHECKING:
    from .flow import Workflow


class WorkflowClient:
    """Client for submitting and controlling workflow instances.

    Args:
        backend: Either ``InMemoryBackend()`` or ``PostgresBackend(url)``.
        conflict_policy: What to do when an ``instance_id`` already exists.
            One of ``"fail"`` (default), ``"use_existing"``, or
            ``"terminate_existing"``.
    """

    def __init__(
        self,
        backend: Any,
        *,
        conflict_policy: str | None = None,
    ) -> None:
        self._inner = _PyWorkflowClient(backend, conflict_policy)

    def submit(
        self,
        workflow: Workflow,
        instance_id: str,
        input: Any,
    ) -> Any:
        """Submit a workflow for execution (does not run tasks).

        Creates an initial snapshot so a :class:`~sayiir.Worker` can pick it up.

        Returns:
            A :class:`~sayiir.WorkflowStatus` indicating the outcome.
        """
        return self._inner.submit(workflow._inner, instance_id, input)

    def cancel(
        self,
        instance_id: str,
        *,
        reason: str | None = None,
        cancelled_by: str | None = None,
    ) -> None:
        """Request cancellation of a workflow instance."""
        self._inner.cancel(instance_id, reason, cancelled_by)

    def pause(
        self,
        instance_id: str,
        *,
        reason: str | None = None,
        paused_by: str | None = None,
    ) -> None:
        """Request pausing of a workflow instance."""
        self._inner.pause(instance_id, reason, paused_by)

    def unpause(self, instance_id: str) -> None:
        """Unpause a paused workflow instance."""
        self._inner.unpause(instance_id)

    def send_signal(self, instance_id: str, signal_name: str, payload: Any) -> None:
        """Send an external signal to a workflow instance."""
        self._inner.send_signal(instance_id, signal_name, payload)

    def get_task_result(self, instance_id: str, task_id: str) -> Any | None:
        """Get a single task result from a workflow instance.

        Returns the deserialized task output, or ``None`` if the task was
        never executed.  For completed/failed workflows the result is
        recovered from the backend's history or cache.
        """
        return self._inner.get_task_result(instance_id, task_id)

    def status(self, instance_id: str) -> Any:
        """Get the current status of a workflow instance.

        Returns:
            A :class:`~sayiir.WorkflowStatus`.
        """
        return self._inner.status(instance_id)
