"""Flow builder for constructing workflows."""

import functools
from collections.abc import Callable
from datetime import timedelta
from typing import TYPE_CHECKING, Any

from ._sayiir import PyFlowBuilder, PyTaskMetadata

if TYPE_CHECKING:
    from ._sayiir import PyWorkflow


def _maybe_wrap_pydantic(task_func: Callable[..., Any]) -> Callable[..., Any]:
    """Wrap a task with Pydantic validation if annotations are models.

    - If ``_input_type`` is a Pydantic model, the raw input is
      validated via ``model_validate``.
    - If ``_output_type`` is a Pydantic model and the return value
      is a model instance, it is serialized via ``model_dump``.
    - If Pydantic is not installed or annotations are not models,
      returns the task unchanged.
    """
    try:
        from pydantic import BaseModel  # pyright: ignore[reportMissingImports]
    except ImportError:
        return task_func

    input_type = getattr(task_func, "_input_type", None)
    output_type = getattr(task_func, "_output_type", None)

    validate_input = isinstance(input_type, type) and issubclass(input_type, BaseModel)
    dump_output = isinstance(output_type, type) and issubclass(output_type, BaseModel)

    if not validate_input and not dump_output:
        return task_func

    @functools.wraps(task_func)
    def wrapper(data: Any) -> Any:
        if validate_input:
            data = input_type.model_validate(data)  # type: ignore[union-attr]
        result = task_func(data)
        if dump_output and isinstance(result, BaseModel):
            result = result.model_dump()
        return result

    # Preserve task attributes on the wrapper
    for attr in ("_task_id", "_metadata", "_input_type", "_output_type"):
        val = getattr(task_func, attr, None)
        if val is not None:
            setattr(wrapper, attr, val)

    return wrapper


def _resolve_task_id(
    task_func: Callable[..., Any],
    *,
    name: str | None = None,
    lambda_counter: int = 0,
) -> tuple[str, int]:
    """Determine task id and return (task_id, updated_counter)."""
    if name is not None:
        return name, lambda_counter
    task_id = getattr(task_func, "_task_id", None)
    if task_id is not None:
        return task_id, lambda_counter
    fn_name = getattr(task_func, "__name__", "<lambda>")
    if fn_name == "<lambda>":
        task_id = f"lambda_{lambda_counter}"
        return task_id, lambda_counter + 1
    return fn_name, lambda_counter


def _register_task(
    task_func: Callable[..., Any],
    registry: dict[str, Callable[..., Any]],
    *,
    name: str | None = None,
    lambda_counter: int = 0,
) -> tuple[str, PyTaskMetadata | None, int]:
    """Extract task id/metadata and register the wrapped task."""
    task_id, lambda_counter = _resolve_task_id(
        task_func, name=name, lambda_counter=lambda_counter
    )
    metadata = getattr(task_func, "_metadata", None)
    registry[task_id] = _maybe_wrap_pydantic(task_func)
    return task_id, metadata, lambda_counter


class Workflow:
    """A compiled workflow with its task registry.

    Produced by Flow.build(). Carries both the Rust-side workflow definition
    and the Python-side task registry so execution is self-contained.
    """

    def __init__(
        self,
        inner: "PyWorkflow",
        task_registry: dict[str, Callable[..., Any]],
    ):
        self._inner = inner
        self._task_registry = task_registry

    @property
    def workflow_id(self) -> str:
        return self._inner.workflow_id

    @property
    def definition_hash(self) -> str:
        return self._inner.definition_hash


class ForkBuilder:
    """Builder for parallel workflow branches."""

    def __init__(self, flow: "Flow"):
        self._flow = flow
        self._branches: list[list[tuple[str, Callable[..., Any]]]] = []

    def branch(
        self, *task_funcs: Callable[..., Any], name: str | None = None
    ) -> "ForkBuilder":
        """Add a branch with one or more chained tasks.

        Each positional argument is a task function. When multiple tasks are
        given they form a pipeline within the branch: the output of each task
        feeds into the next.

        The branch name in the join dict is the first task's ID.

        Args:
            *task_funcs: One or more task callables. Accepts ``@task``-decorated
                functions, plain functions, or lambdas.
            name: Override the task ID of the **first** task in the branch.
                Useful for lambdas or when the same function appears in
                multiple branches.

        Examples::

            .branch(step_a)                                # single-task branch
            .branch(step_a, step_b, step_c)                # multi-step branch
            .branch(lambda x: x + 1, name="increment")    # lambda with name

        Raises:
            ValueError: If no task functions are provided.
        """
        if not task_funcs:
            raise ValueError("branch() requires at least one task function")
        chain: list[tuple[str, Callable[..., Any]]] = []
        for i, func in enumerate(task_funcs):
            task_name = name if i == 0 else None
            task_id, _, self._flow._lambda_counter = _register_task(
                func,
                self._flow._task_registry,
                name=task_name,
                lambda_counter=self._flow._lambda_counter,
            )
            chain.append((task_id, func))
        self._branches.append(chain)
        return self

    def join(self, task_func: Callable[..., Any], *, name: str | None = None) -> "Flow":
        """Join branches with a combining task.

        Args:
            task_func: The join callable that receives a dict of branch results.
                Accepts ``@task``-decorated functions, plain functions, or lambdas.
            name: Override the task ID. Useful for lambdas or when the same
                function is used as a join in multiple forks.
        """
        task_id, metadata, self._flow._lambda_counter = _register_task(
            task_func,
            self._flow._task_registry,
            name=name,
            lambda_counter=self._flow._lambda_counter,
        )
        branches: list[list[tuple[str, PyTaskMetadata | None]]] = [
            [(name, getattr(func, "_metadata", None)) for name, func in chain]
            for chain in self._branches
        ]
        self._flow._builder.add_fork(branches, task_id, metadata)
        return self._flow


class Flow:
    """Workflow builder with fluent API.

    Each Flow instance maintains its own task registry, making workflows
    self-contained and independent of global state.

    Example:
        @task
        def double(x: int) -> int:
            return x * 2

        workflow = Flow("my-pipeline").then(double).build()
        result = run_workflow(workflow, 21)
    """

    def __init__(self, name: str = "workflow"):
        self._name = name
        self._builder = PyFlowBuilder(name)
        self._task_registry: dict[str, Callable[..., Any]] = {}
        self._lambda_counter: int = 0

    def then(self, task_func: Callable[..., Any], *, name: str | None = None) -> "Flow":
        """Add a sequential task to the workflow pipeline.

        Accepts any callable: ``@task``-decorated functions, plain functions,
        or lambdas. The callable receives the output of the previous step
        and returns the input for the next.

        Args:
            task_func: The task to execute. Can be:
                - A ``@task``-decorated function (uses its task ID and metadata)
                - A plain function (uses ``__name__`` as the task ID)
                - A lambda (auto-assigned ``lambda_0``, ``lambda_1``, etc.)
            name: Override the task ID. Useful for lambdas or when the same
                function is used multiple times in a pipeline.

        Returns:
            The Flow instance for chaining.

        Examples::

            @task
            def double(x: int) -> int:
                return x * 2

            Flow("example")
                .then(double)                          # @task-decorated
                .then(lambda x: x + 1, name="add_one") # lambda with name
                .then(str.upper)                        # plain callable
                .build()
        """
        task_id, metadata, self._lambda_counter = _register_task(
            task_func,
            self._task_registry,
            name=name,
            lambda_counter=self._lambda_counter,
        )
        self._builder.then(task_id, metadata)
        return self

    def delay(self, name: str, duration: "float | timedelta") -> "Flow":
        """Add a durable delay. No workers held during the delay."""
        if isinstance(duration, timedelta):
            duration = duration.total_seconds()
        self._builder.delay(name, duration)
        return self

    def fork(self) -> "ForkBuilder":
        """Start a fork for parallel execution."""
        return ForkBuilder(self)

    def build(self) -> Workflow:
        """Build the workflow."""
        return Workflow(self._builder.build(), self._task_registry)
