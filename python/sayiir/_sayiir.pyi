"""Type stubs for the _sayiir Rust extension module."""

from typing import Any, List, Literal, Optional


class TaskRequest:
    """Request for Python to execute a task."""

    @property
    def request_id(self) -> int:
        """Unique ID for correlating request/response."""
        ...

    @property
    def task_id(self) -> str:
        """The task identifier."""
        ...

    def get_input(self) -> Any:
        """Get the input data as a Python object (deserializes using configured format)."""
        ...


class TaskChannel:
    """Communication channel between Rust orchestrator and Python executor."""

    def __init__(self, serializer: Optional[Literal["json", "pickle"]] = None) -> None:
        """Create a new TaskChannel.

        Args:
            serializer: Serialization format - "json" (default) or "pickle".
        """
        ...

    @property
    def serializer(self) -> str:
        """Get the serialization format as a string."""
        ...

    def poll_task(self) -> Optional[TaskRequest]:
        """Poll for the next task request (non-blocking, returns None if empty)."""
        ...

    def submit_result(self, request_id: int, result: Any) -> None:
        """Submit a successful task result.

        Args:
            request_id: The request ID from the TaskRequest.
            result: The result object to serialize and send back.
        """
        ...

    def submit_error(self, request_id: int, error: str) -> None:
        """Submit a task error.

        Args:
            request_id: The request ID from the TaskRequest.
            error: The error message.
        """
        ...


class PyTaskMetadata:
    """Task metadata for configuration (retries, timeout, tags)."""

    retries: int
    timeout: Optional[float]
    tags: List[str]

    def __init__(
        self,
        retries: int = 0,
        timeout: Optional[float] = None,
        tags: Optional[List[str]] = None,
    ) -> None:
        """Create task metadata.

        Args:
            retries: Number of retry attempts on failure.
            timeout: Timeout in seconds (None = no timeout).
            tags: Tags for categorization.
        """
        ...


class PyFlowBuilder:
    """Builder for constructing workflows."""

    def __init__(
        self,
        name: str,
        serializer: Optional[Literal["json", "pickle"]] = None,
    ) -> None:
        """Create a new flow builder with the given name.

        Args:
            name: The workflow name.
            serializer: Serialization format - "json" (default) or "pickle".
        """
        ...

    def then(
        self,
        task_id: str,
        metadata: Optional[PyTaskMetadata] = None,
    ) -> None:
        """Add a sequential task to the workflow."""
        ...

    def fork(self) -> "PyForkBuilder":
        """Start a fork for parallel execution."""
        ...

    def build(self) -> "PyWorkflow":
        """Build the workflow."""
        ...


class PyForkBuilder:
    """Builder for fork/join parallel execution."""

    def branch(
        self,
        name: str,
        task_id: str,
        metadata: Optional[PyTaskMetadata] = None,
    ) -> "PyForkBuilder":
        """Add a branch to the fork."""
        ...

    def join(
        self,
        task_id: str,
        metadata: Optional[PyTaskMetadata] = None,
    ) -> PyFlowBuilder:
        """Join the fork branches with a combining task."""
        ...


class PyWorkflow:
    """A built workflow ready for execution."""

    @property
    def name(self) -> str:
        """Name of the workflow."""
        ...

    def get_task_ids(self) -> List[str]:
        """Get the list of task IDs in this workflow."""
        ...


class PyInMemoryBackend:
    """In-memory persistence backend for development and testing."""

    def __init__(self) -> None:
        """Create a new in-memory backend."""
        ...

    def list_instances(self) -> List[str]:
        """List all workflow instance IDs stored in this backend."""
        ...


class PyWorkflowEngine:
    """Workflow execution engine with persistence support."""

    def __init__(
        self,
        backend: Optional[PyInMemoryBackend] = None,
        serializer: Optional[Literal["json", "pickle"]] = None,
    ) -> None:
        """Create a new workflow engine.

        Args:
            backend: Optional persistence backend (defaults to InMemoryBackend).
            serializer: Serialization format - "json" (default) or "pickle".
                       WARNING: Only use "pickle" with trusted data sources.
        """
        ...

    @property
    def serializer(self) -> str:
        """Get the serialization format as a string."""
        ...

    def get_channel(self) -> TaskChannel:
        """Get the task channel for the Python executor."""
        ...

    async def run(
        self,
        workflow: PyWorkflow,
        instance_id: str,
        input: Any,
    ) -> Any:
        """Run a workflow with the given input.

        Args:
            workflow: The workflow to run.
            instance_id: Unique identifier for this workflow run.
            input: Input data for the workflow.

        Returns:
            The workflow output on success.
        """
        ...

    async def resume(
        self,
        workflow: PyWorkflow,
        instance_id: str,
    ) -> Any:
        """Resume a workflow from a checkpoint.

        Args:
            workflow: The workflow to resume.
            instance_id: The workflow instance to resume.

        Returns:
            The workflow output on success.
        """
        ...

    def cancel(
        self,
        instance_id: str,
        reason: Optional[str] = None,
    ) -> None:
        """Cancel a running workflow.

        Args:
            instance_id: The workflow instance to cancel.
            reason: Optional reason for cancellation.
        """
        ...
