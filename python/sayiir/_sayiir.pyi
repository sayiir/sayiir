"""Type stubs for the Rust extension module."""

from typing import Any

class PyRetryPolicy:
    """Retry policy for tasks."""

    max_attempts: int
    initial_delay_secs: float
    backoff_multiplier: float

    def __init__(
        self,
        max_attempts: int = 3,
        initial_delay_secs: float = 1.0,
        backoff_multiplier: float = 2.0,
    ) -> None: ...

class PyTaskMetadata:
    """Task metadata."""

    def __init__(
        self,
        display_name: str | None = None,
        description: str | None = None,
        timeout_secs: float | None = None,
        retries: PyRetryPolicy | None = None,
        tags: list[str] | None = None,
    ) -> None: ...

class PyFlowBuilder:
    """Workflow builder."""

    def __init__(self, name: str) -> None: ...
    def then(self, task_id: str, metadata: PyTaskMetadata | None = None) -> None: ...
    def add_fork(
        self,
        branches: list[list[tuple[str, PyTaskMetadata | None]]],
        join_id: str,
        join_metadata: PyTaskMetadata | None = None,
    ) -> None: ...
    def build(self) -> PyWorkflow: ...

class PyWorkflow:
    """Compiled workflow definition."""

    @property
    def workflow_id(self) -> str: ...
    @property
    def definition_hash(self) -> str: ...

class PyWorkflowStatus:
    """Workflow execution status."""

    status: str
    error: str | None
    reason: str | None
    cancelled_by: str | None

    def is_completed(self) -> bool: ...
    def is_failed(self) -> bool: ...
    def is_cancelled(self) -> bool: ...

class PyWorkflowEngine:
    """Workflow engine — Rust orchestrates, Python provides task implementations."""

    def __init__(self) -> None: ...
    def run(
        self,
        workflow: PyWorkflow,
        input: Any,
        task_registry: dict[str, Any],
    ) -> Any: ...

class PyInMemoryBackend:
    """In-memory persistence backend for workflow snapshots."""

    def __init__(self) -> None: ...

class PyDurableEngine:
    """Durable workflow engine with checkpointing, cancellation, and resume."""

    def __init__(self, backend: PyInMemoryBackend) -> None: ...
    def run(
        self,
        workflow: PyWorkflow,
        instance_id: str,
        input: Any,
        task_registry: dict[str, Any],
    ) -> PyWorkflowStatus: ...
    def resume(
        self,
        workflow: PyWorkflow,
        instance_id: str,
        task_registry: dict[str, Any],
    ) -> PyWorkflowStatus: ...
    def cancel(
        self,
        instance_id: str,
        reason: str | None = None,
        cancelled_by: str | None = None,
    ) -> None: ...
