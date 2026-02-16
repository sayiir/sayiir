"""Distributed worker for processing workflows across multiple processes.

The Worker polls a backend for available tasks, claims them, and executes
them using the registered task functions. Multiple workers can run across
machines or processes, all polling the same backend.

Example::

    from sayiir import Flow, PostgresBackend, task
    from sayiir.worker import Worker

    @task
    def process_order(order):
        ...

    wf = Flow("orders").then(process_order).build()
    backend = PostgresBackend("postgresql://localhost/sayiir")
    worker = Worker("worker-1", backend)
    handle = worker.start([wf])

    # In shutdown handler:
    handle.shutdown()
    handle.join()
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from ._sayiir import PyWorker as _PyWorker

if TYPE_CHECKING:
    from ._sayiir import PyWorkerHandle as _PyWorkerHandle
    from .flow import Workflow


class WorkerHandle:
    """Handle for controlling a running worker.

    Obtained from :meth:`Worker.start`. Use :meth:`shutdown` to request
    graceful shutdown and :meth:`join` to wait for completion.
    """

    def __init__(self, inner: _PyWorkerHandle) -> None:
        self._inner = inner

    def shutdown(self) -> None:
        """Request a graceful shutdown. Non-blocking."""
        self._inner.shutdown()

    def join(self) -> None:
        """Wait for the worker to finish. Call :meth:`shutdown` first.

        Releases the GIL while waiting.
        """
        self._inner.join()

    def cancel_workflow(
        self,
        instance_id: str,
        *,
        reason: str | None = None,
        cancelled_by: str | None = None,
    ) -> None:
        """Request cancellation of a workflow instance."""
        self._inner.cancel_workflow(instance_id, reason, cancelled_by)

    def pause_workflow(
        self,
        instance_id: str,
        *,
        reason: str | None = None,
        paused_by: str | None = None,
    ) -> None:
        """Request pausing of a workflow instance."""
        self._inner.pause_workflow(instance_id, reason, paused_by)

    def unpause_workflow(self, instance_id: str) -> None:
        """Unpause a paused workflow instance."""
        self._inner.unpause_workflow(instance_id)

    def send_signal(self, instance_id: str, signal_name: str, payload: Any) -> None:
        """Send an external signal to a workflow instance."""
        self._inner.send_signal(instance_id, signal_name, payload)


class Worker:
    """Distributed workflow worker.

    Args:
        worker_id: Unique identifier for this worker node.
        backend: Either ``InMemoryBackend()`` or ``PostgresBackend(url)``.
        poll_interval: Seconds between polls for available tasks.
        claim_ttl: Task claim TTL in seconds.
    """

    def __init__(
        self,
        worker_id: str,
        backend: Any,
        *,
        poll_interval: float = 5.0,
        claim_ttl: float = 300.0,
    ) -> None:
        self._inner = _PyWorker(worker_id, backend, poll_interval, claim_ttl)

    def start(self, workflows: list[Workflow]) -> WorkerHandle:
        """Start the worker and return a handle for lifecycle control.

        Args:
            workflows: List of compiled :class:`~sayiir.Workflow` objects.
                Each workflow's task registry is used to look up task
                functions when executing claimed tasks.

        Returns:
            A :class:`WorkerHandle` for shutdown and lifecycle control.
        """
        pairs = [(wf._inner, wf._task_registry) for wf in workflows]
        return WorkerHandle(self._inner.start(pairs))
