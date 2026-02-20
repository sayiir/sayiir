"""Flow builder for constructing workflows."""

import functools
import json as _json
from collections.abc import Callable
from datetime import timedelta
from typing import TYPE_CHECKING, Any

from ._sayiir import PyFlowBuilder, PyTaskMetadata

if TYPE_CHECKING:
    from ._sayiir import PyWorkflow
    from .loop_result import OnMax

# Suffix convention for branch key-function task IDs.
# Must match ``sayiir_core::workflow::key_fn_id``.
_KEY_FN_SUFFIX = "::key_fn"


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
    builder: "PyFlowBuilder",
    *,
    name: str | None = None,
) -> str:
    """Determine task id, using the builder's lambda counter for anonymous fns."""
    if name is not None:
        return name
    task_id = getattr(task_func, "_task_id", None)
    if task_id is not None:
        return task_id
    fn_name = getattr(task_func, "__name__", "<lambda>")
    if fn_name == "<lambda>":
        return builder.next_lambda_id()
    return fn_name


def _register_task(
    task_func: Callable[..., Any],
    registry: dict[str, Callable[..., Any]],
    builder: "PyFlowBuilder",
    *,
    name: str | None = None,
) -> tuple[str, PyTaskMetadata | None]:
    """Extract task id/metadata and register the wrapped task."""
    task_id = _resolve_task_id(task_func, builder, name=name)
    metadata = getattr(task_func, "_metadata", None)
    registry[task_id] = _maybe_wrap_pydantic(task_func)
    return task_id, metadata


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

    @property
    def metadata(self) -> dict[str, Any] | None:
        """Workflow-level metadata, or ``None`` if none was provided."""
        raw = self._inner.metadata_json
        return _json.loads(raw) if raw is not None else None


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
            task_id, _ = _register_task(
                func,
                self._flow._task_registry,
                self._flow._builder,
                name=task_name,
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
        task_id, metadata = _register_task(
            task_func,
            self._flow._task_registry,
            self._flow._builder,
            name=name,
        )
        branches: list[list[tuple[str, PyTaskMetadata | None]]] = [
            [(name, getattr(func, "_metadata", None)) for name, func in chain]
            for chain in self._branches
        ]
        self._flow._builder.add_fork(branches, task_id, metadata)
        return self._flow


class BranchBuilder:
    """Builder for conditional branching.

    The ``keys`` parameter declares all valid routing keys up front.
    ``.branch()`` validates that the key is in the declared set, and
    ``.done()`` checks exhaustiveness: every declared key must have a
    corresponding branch or a default branch must be provided.
    """

    def __init__(
        self,
        flow: "Flow",
        key_fn: Callable[..., Any],
        *,
        keys: list[str],
    ):
        self._flow = flow
        self._key_fn = key_fn
        self._declared_keys = keys
        self._branches: list[tuple[str, list[tuple[str, Callable[..., Any]]]]] = []
        self._default: list[tuple[str, Callable[..., Any]]] | None = None

    def branch(
        self, key: str, *task_funcs: Callable[..., Any], name: str | None = None
    ) -> "BranchBuilder":
        """Add a named branch with one or more chained tasks.

        Args:
            key: The routing key that selects this branch. Must be one of
                the keys declared in ``route(keys=...)``.
            *task_funcs: One or more task callables forming the branch pipeline.
            name: Override the task ID of the first task in the branch.

        Raises:
            ValueError: If the key is not in the declared set.
        """
        if not task_funcs:
            raise ValueError("branch() requires at least one task function")
        if key not in self._declared_keys:
            raise ValueError(
                f"Branch key '{key}' is not in the declared keys: {self._declared_keys}"
            )
        chain: list[tuple[str, Callable[..., Any]]] = []
        for i, func in enumerate(task_funcs):
            task_name = name if i == 0 else None
            task_id, _ = _register_task(
                func,
                self._flow._task_registry,
                self._flow._builder,
                name=task_name,
            )
            chain.append((task_id, func))
        self._branches.append((key, chain))
        return self

    def default_branch(
        self, *task_funcs: Callable[..., Any], name: str | None = None
    ) -> "BranchBuilder":
        """Add a default branch for unmatched keys.

        Args:
            *task_funcs: One or more task callables forming the default pipeline.
            name: Override the task ID of the first task.
        """
        if not task_funcs:
            raise ValueError("default_branch() requires at least one task function")
        chain: list[tuple[str, Callable[..., Any]]] = []
        for i, func in enumerate(task_funcs):
            task_name = name if i == 0 else None
            task_id, _ = _register_task(
                func,
                self._flow._task_registry,
                self._flow._builder,
                name=task_name,
            )
            chain.append((task_id, func))
        self._default = chain
        return self

    def done(self) -> "Flow":
        """Finish the route and return to the Flow builder.

        Raises:
            ValueError: If declared keys are missing branches and no default
                is provided, or if orphan branches reference undeclared keys.
        """
        branched_keys = {key for key, _ in self._branches}
        declared_set = set(self._declared_keys)

        # Check for orphan branches
        orphans = branched_keys - declared_set
        if orphans:
            raise ValueError(
                f"Route: orphan branches for keys: {', '.join(sorted(orphans))}"
            )

        # Check for missing branches (when no default)
        if self._default is None:
            missing = declared_set - branched_keys
            if missing:
                raise ValueError(
                    f"Route: missing branches for keys: {', '.join(sorted(missing))}"
                )

        # Build branch data for the Rust builder
        branches: list[tuple[str, list[tuple[str, PyTaskMetadata | None]]]] = [
            (key, [(tid, getattr(func, "_metadata", None)) for tid, func in chain])
            for key, chain in self._branches
        ]
        default: list[tuple[str, PyTaskMetadata | None]] | None = None
        if self._default is not None:
            default = [
                (tid, getattr(func, "_metadata", None)) for tid, func in self._default
            ]

        # add_branch returns the generated branch ID
        branch_id = self._flow._builder.add_branch(branches, default)

        # Register the key function using the generated branch ID
        key_fn_id = f"{branch_id}{_KEY_FN_SUFFIX}"
        self._flow._task_registry[key_fn_id] = _maybe_wrap_pydantic(self._key_fn)

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

    def __init__(
        self,
        name: str = "workflow",
        *,
        metadata: dict[str, Any] | None = None,
    ):
        self._name = name
        self._builder = PyFlowBuilder(name)
        self._task_registry: dict[str, Callable[..., Any]] = {}
        if metadata is not None:
            self._builder.set_metadata_json(_json.dumps(metadata))

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
        task_id, metadata = _register_task(
            task_func,
            self._task_registry,
            self._builder,
            name=name,
        )
        self._builder.then(task_id, metadata)
        return self

    def delay(self, name: str, duration: "str | float | timedelta") -> "Flow":
        """Add a durable delay. No workers held during the delay.

        Args:
            name: Step identifier.
            duration: Delay length as seconds (number), a ``timedelta``,
                or a human-readable string (``"30s"``, ``"5m"``, ``"1h"``).
        """
        if isinstance(duration, timedelta):
            duration = duration.total_seconds()
        elif isinstance(duration, str):
            from .decorators import parse_duration

            duration = parse_duration(duration)
        self._builder.delay(name, duration)
        return self

    def wait_for_signal(
        self,
        signal_name: str,
        *,
        name: str | None = None,
        timeout: "str | float | timedelta | None" = None,
    ) -> "Flow":
        """Wait for an external signal before continuing.

        The workflow parks and releases the worker until the signal arrives
        (via ``send_signal``). An optional timeout causes the workflow to
        fail if the signal is not received in time.

        Args:
            signal_name: The named signal to wait for.
            name: Node ID for this step. Defaults to ``signal_name``.
            timeout: Optional timeout as seconds (number), ``timedelta``,
                or human-readable string (``"30s"``, ``"5m"``, ``"1h"``).
        """
        signal_id = name or signal_name
        timeout_secs: float | None = None
        if timeout is not None:
            if isinstance(timeout, timedelta):
                timeout_secs = timeout.total_seconds()
            elif isinstance(timeout, str):
                from .decorators import parse_duration

                timeout_secs = parse_duration(timeout)
            else:
                timeout_secs = timeout
        self._builder.wait_for_signal(signal_id, signal_name, timeout_secs)
        return self

    def route(
        self,
        key_fn: Callable[..., Any],
        *,
        keys: list[str],
    ) -> "BranchBuilder":
        """Start a conditional branch based on a routing key.

        The key function receives the output of the previous step and returns
        a string key. The ``keys`` parameter declares all valid routing keys
        up front. ``.branch()`` validates against this set and ``.done()``
        checks exhaustiveness.

        Args:
            key_fn: A callable that extracts a string routing key from the
                previous step's output.
            keys: All valid routing keys. Each must have a corresponding
                ``.branch()`` call or a ``.default_branch()`` must be provided.

        Returns:
            A BranchBuilder for adding named branches.

        Example::

            Flow("classify")
                .then(classify)
                .route(lambda r: r["intent"], keys=["billing", "tech"])
                    .branch("billing", handle_billing)
                    .branch("tech", handle_tech)
                    .done()
                .then(finalize)
                .build()
        """
        return BranchBuilder(self, key_fn, keys=keys)

    def loop(
        self,
        task_func: Callable[..., Any],
        *,
        max_iterations: int = 10,
        on_max: "str | OnMax" = "fail",
        name: str | None = None,
    ) -> "Flow":
        """Add a loop whose body repeats until it returns ``LoopResult.done()``.

        The body task receives the current value and must return a
        ``LoopResult``. ``LoopResult.again(new_value)`` continues the loop;
        ``LoopResult.done(final_value)`` exits.

        Args:
            task_func: The loop body callable.
            max_iterations: Maximum iterations before ``on_max`` applies.
            on_max: ``"fail"`` (default) raises an error, ``"exit_with_last"``
                exits with the most recent iteration's output.
            name: Override the task ID.

        Returns:
            The Flow instance for chaining.
        """
        from .loop_result import LoopResult

        task_id, metadata = _register_task(
            task_func,
            self._task_registry,
            self._builder,
            name=name,
        )

        # Wrap the task to serialize LoopResult to its wire format
        original_fn = self._task_registry[task_id]

        # Check if the function is async
        import inspect

        if inspect.iscoroutinefunction(original_fn):

            @functools.wraps(original_fn)
            async def async_loop_wrapper(data: Any) -> Any:
                result = await original_fn(data)
                if isinstance(result, LoopResult):
                    return result.to_dict()
                return result

            loop_wrapper = async_loop_wrapper
        else:

            @functools.wraps(original_fn)
            def sync_loop_wrapper(data: Any) -> Any:
                result = original_fn(data)
                if isinstance(result, LoopResult):
                    return result.to_dict()
                return result

            loop_wrapper = sync_loop_wrapper

        # Preserve task attributes on the wrapper
        for attr in ("_task_id", "_metadata", "_input_type", "_output_type"):
            val = getattr(original_fn, attr, None)
            if val is not None:
                setattr(loop_wrapper, attr, val)

        self._task_registry[task_id] = loop_wrapper

        self._builder.add_loop(task_id, metadata, max_iterations, on_max)
        return self

    def fork(self) -> "ForkBuilder":
        """Start a fork for parallel execution."""
        return ForkBuilder(self)

    def build(self) -> Workflow:
        """Build the workflow."""
        return Workflow(self._builder.build(), self._task_registry)
