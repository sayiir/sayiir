"""Type stubs for the Rust extension module."""

from typing import Any

from .loop_result import OnMax

class PyRetryPolicy:
    """Retry policy for tasks."""

    max_retries: int
    initial_delay_secs: float
    backoff_multiplier: float

    def __init__(
        self,
        max_retries: int = 2,
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
        version: str | None = None,
        priority: int | None = None,
    ) -> None: ...

class PyFlowBuilder:
    """Workflow builder."""

    def __init__(self, name: str) -> None: ...
    def next_lambda_id(self) -> str: ...
    def then(self, task_id: str, metadata: PyTaskMetadata | None = None) -> None: ...
    def delay(self, delay_id: str, seconds: float) -> None: ...
    def wait_for_signal(
        self,
        signal_id: str,
        signal_name: str,
        timeout_secs: float | None = None,
    ) -> None: ...
    def add_fork(
        self,
        branches: list[list[tuple[str, PyTaskMetadata | None]]],
        join_id: str,
        join_metadata: PyTaskMetadata | None = None,
    ) -> None: ...
    def set_metadata_json(self, json: str) -> None: ...
    def add_branch(
        self,
        branches: list[tuple[str, list[tuple[str, PyTaskMetadata | None]]]],
        default: list[tuple[str, PyTaskMetadata | None]] | None = None,
    ) -> str: ...
    def add_loop(
        self,
        body_task_id: str,
        body_metadata: PyTaskMetadata | None = None,
        max_iterations: int = 10,
        on_max: str | OnMax = "fail",
    ) -> str: ...
    def add_child_workflow(
        self,
        child_id: str,
        child_builder: PyFlowBuilder,
    ) -> None: ...
    def build(self) -> PyWorkflow: ...

class NodeInfo:
    """Metadata about a single node in the workflow DAG."""

    id: str
    kind: str
    predecessor_id: str | None
    timeout_secs: float | None
    retry_policy: PyRetryPolicy | None
    priority: int | None

    def __repr__(self) -> str: ...

class PyWorkflow:
    """Compiled workflow definition."""

    @property
    def workflow_id(self) -> str: ...
    @property
    def definition_hash(self) -> str: ...
    @property
    def metadata_json(self) -> str | None: ...
    def iter_nodes(self) -> list[NodeInfo]: ...

class PyWorkflowStatus:
    """Workflow execution status."""

    status: str
    error: str | None
    reason: str | None
    cancelled_by: str | None
    paused_by: str | None
    output: Any | None
    wake_at: str | None
    delay_id: str | None
    signal_id: str | None
    signal_name: str | None

    def is_completed(self) -> bool: ...
    def is_failed(self) -> bool: ...
    def is_cancelled(self) -> bool: ...
    def is_in_progress(self) -> bool: ...
    def is_paused(self) -> bool: ...
    def is_waiting(self) -> bool: ...
    def is_awaiting_signal(self) -> bool: ...
    def __repr__(self) -> str: ...

class PyWorkflowEngine:
    """Workflow engine — Rust orchestrates, Python provides task implementations."""

    def __init__(self) -> None: ...
    def run(
        self,
        workflow: PyWorkflow,
        input: Any,
        task_registry: dict[str, Any],
    ) -> Any: ...
    def __repr__(self) -> str: ...

class PyInMemoryBackend:
    """In-memory persistence backend for workflow snapshots."""

    def __init__(self) -> None: ...

class PyPostgresBackend:
    """PostgreSQL persistence backend for workflow snapshots."""

    def __init__(self, url: str) -> None: ...
    def __repr__(self) -> str: ...

class PyDurableEngine:
    """Durable workflow engine with checkpointing, cancellation, and resume."""

    def __init__(
        self,
        backend: PyInMemoryBackend | PyPostgresBackend,
        conflict_policy: str | None = None,
    ) -> None: ...
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
    def pause(
        self,
        instance_id: str,
        reason: str | None = None,
        paused_by: str | None = None,
    ) -> None: ...
    def send_signal(
        self,
        instance_id: str,
        signal_name: str,
        payload: Any,
    ) -> None: ...
    def unpause(self, instance_id: str) -> None: ...
    def __repr__(self) -> str: ...

class PyWorker:
    """Distributed workflow worker."""

    def __init__(
        self,
        worker_id: str,
        backend: PyInMemoryBackend | PyPostgresBackend,
        poll_interval_secs: float = 5.0,
        claim_ttl_secs: float = 300.0,
        tags: list[str] | None = None,
    ) -> None: ...
    def start(
        self,
        workflows: list[tuple[PyWorkflow, dict[str, Any]]],
    ) -> PyWorkerHandle: ...
    def __repr__(self) -> str: ...

class PyWorkerHandle:
    """Handle for controlling a running worker."""

    def shutdown(self) -> None: ...
    def join(self) -> None: ...
    def __repr__(self) -> str: ...

class PyWorkflowClient:
    """Client for submitting and controlling workflow instances."""

    def __new__(
        cls,
        backend: PyInMemoryBackend | PyPostgresBackend,
        conflict_policy: str | None = None,
    ) -> PyWorkflowClient: ...
    def submit(
        self,
        workflow: Any,
        instance_id: str,
        input: Any,
    ) -> PyWorkflowStatus: ...
    def cancel(
        self,
        instance_id: str,
        reason: str | None = None,
        cancelled_by: str | None = None,
    ) -> None: ...
    def pause(
        self,
        instance_id: str,
        reason: str | None = None,
        paused_by: str | None = None,
    ) -> None: ...
    def unpause(self, instance_id: str) -> None: ...
    def send_signal(
        self,
        instance_id: str,
        signal_name: str,
        payload: Any,
    ) -> None: ...
    def status(self, instance_id: str) -> PyWorkflowStatus: ...
    def get_task_result(self, instance_id: str, task_id: str) -> Any | None: ...
    def __repr__(self) -> str: ...

class PyTaskExecutionContext:
    """Task execution context available from within a running task."""

    workflow_id: str
    instance_id: str
    task_id: str
    metadata: PyTaskMetadata
    workflow_metadata: dict[str, Any] | None

    def __repr__(self) -> str: ...

def get_task_context() -> PyTaskExecutionContext | None:
    """Get the current task execution context. Returns None outside of a task."""
    ...

def init_tracing() -> None:
    """Initialize the tracing subscriber.

    Sets up a ``tracing-subscriber`` registry with an ``fmt`` layer for
    console output and an optional ``tracing-opentelemetry`` layer for OTLP
    export (enabled when ``OTEL_EXPORTER_OTLP_ENDPOINT`` is set).

    Idempotent — calling multiple times is safe.
    """
    ...

def shutdown_tracing() -> None:
    """Flush and shut down the OpenTelemetry tracer provider.

    Call before process exit to ensure all pending spans are exported.
    """
    ...

# Exception hierarchy
class WorkflowError(RuntimeError):
    """Base exception for Sayiir workflow errors."""

    ...

class TaskError(WorkflowError):
    """A task failed during execution."""

    ...

class BackendError(WorkflowError):
    """A persistence backend operation failed."""

    ...

class InstanceAlreadyExistsError(WorkflowError):
    """A workflow instance with this ID already exists."""

    ...
