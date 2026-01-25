"""Flow builder for constructing workflows in Python.

This module provides a Pythonic wrapper around the Rust FlowBuilder,
offering a fluent API for defining sequential and parallel task execution.
"""

from typing import TYPE_CHECKING, Any, Callable, Literal, Optional

from ._sayiir import PyFlowBuilder, PyTaskMetadata

if TYPE_CHECKING:
    from ._sayiir import PyWorkflow


class Flow:
    """Builder for constructing workflows with a fluent API.

    The Flow class provides a high-level interface for defining workflows
    as a sequence of tasks, with support for parallel execution via fork/join.

    Example:
        >>> from sayiir import task, Flow
        >>>
        >>> @task
        ... def step1(x: int) -> int:
        ...     return x + 1
        >>>
        >>> @task
        ... def step2(x: int) -> int:
        ...     return x * 2
        >>>
        >>> workflow = (
        ...     Flow("my_workflow")
        ...     .then(step1)
        ...     .then(step2)
        ...     .build()
        ... )

    For objects that aren't JSON-serializable, use pickle:
        >>> workflow = (
        ...     Flow("my_workflow", serializer="pickle")
        ...     .then(task_with_complex_objects)
        ...     .build()
        ... )
    """

    def __init__(
        self,
        name: str = "workflow",
        serializer: Optional[Literal["json", "pickle"]] = None,
    ) -> None:
        """Create a new Flow builder.

        Args:
            name: The workflow name for identification and logging.
            serializer: Serialization format - "json" (default) or "pickle".
                       Use "pickle" for objects that aren't JSON-serializable.
                       WARNING: Only use "pickle" with trusted data sources.
        """
        self._builder = PyFlowBuilder(name, serializer)

    def then(self, task_func: Callable[..., Any]) -> "Flow":
        """Add a sequential task to the workflow.

        The task receives the output of the previous task as input
        (or the workflow input if this is the first task).

        Args:
            task_func: A function decorated with @task.

        Returns:
            Self for method chaining.

        Raises:
            AttributeError: If task_func is not decorated with @task.

        Example:
            >>> flow = Flow("example").then(fetch_data).then(process)
        """
        task_id = getattr(task_func, "_task_id", task_func.__name__)
        metadata = getattr(task_func, "_metadata", None)
        self._builder.then(task_id, metadata)
        return self

    def fork(self) -> "ForkBuilder":
        """Start a fork for parallel branch execution.

        Use fork() to run multiple tasks concurrently. Each branch
        executes in parallel and their outputs are collected for
        the join task.

        Returns:
            A ForkBuilder for adding parallel branches.

        Example:
            >>> workflow = (
            ...     Flow("parallel")
            ...     .fork()
            ...         .branch("a", branch_a_task)
            ...         .branch("b", branch_b_task)
            ...     .join(combine_task)
            ...     .build()
            ... )
        """
        return ForkBuilder(self._builder.fork(), self)

    def build(self) -> "PyWorkflow":
        """Finalize and return the workflow.

        Returns:
            A PyWorkflow object ready for execution.

        Raises:
            RuntimeError: If the workflow definition is invalid.
        """
        return self._builder.build()


class ForkBuilder:
    """Builder for fork/join parallel execution patterns.

    This class is created by calling Flow.fork() and provides methods
    for adding parallel branches and joining them with a combining task.
    """

    def __init__(self, rust_builder: Any, parent_flow: Flow) -> None:
        """Initialize the fork builder.

        Args:
            rust_builder: The underlying Rust PyForkBuilder.
            parent_flow: The parent Flow for returning after join.
        """
        self._builder = rust_builder
        self._parent = parent_flow

    def branch(
        self,
        name: str,
        task_func: Callable[..., Any],
        metadata: Optional[PyTaskMetadata] = None,
    ) -> "ForkBuilder":
        """Add a parallel branch to the fork.

        Each branch runs concurrently with other branches. The branch
        name is used to access the output in the join task.

        Args:
            name: Identifier for this branch (used in join task).
            task_func: A function decorated with @task.
            metadata: Optional task metadata override.

        Returns:
            Self for method chaining.

        Example:
            >>> fork = Flow("example").fork()
            >>> fork.branch("fast", quick_task).branch("slow", slow_task)
        """
        task_id = getattr(task_func, "_task_id", task_func.__name__)
        task_metadata = metadata or getattr(task_func, "_metadata", None)
        self._builder.branch(name, task_id, task_metadata)
        return self

    def join(
        self,
        task_func: Callable[..., Any],
        metadata: Optional[PyTaskMetadata] = None,
    ) -> Flow:
        """Join all branches with a combining task.

        The join task receives a dictionary mapping branch names to
        their outputs. After joining, you can continue adding tasks
        to the workflow.

        Args:
            task_func: A function decorated with @task that combines outputs.
            metadata: Optional task metadata override.

        Returns:
            The parent Flow for continued chaining.

        Example:
            >>> @task
            ... def combine(outputs: dict) -> str:
            ...     return f"a={outputs['a']}, b={outputs['b']}"
            >>>
            >>> workflow = (
            ...     Flow("parallel")
            ...     .fork()
            ...         .branch("a", task_a)
            ...         .branch("b", task_b)
            ...     .join(combine)
            ...     .build()
            ... )
        """
        task_id = getattr(task_func, "_task_id", task_func.__name__)
        task_metadata = metadata or getattr(task_func, "_metadata", None)
        self._builder.join(task_id, task_metadata)
        return self._parent
