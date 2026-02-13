"""Task decorator for annotating workflow task implementations."""

import inspect
from collections.abc import Callable
from typing import Any, TypeVar, get_type_hints, overload

from ._sayiir import PyRetryPolicy, PyTaskMetadata

T = TypeVar("T")


@overload
def task(func: Callable[..., T]) -> Callable[..., T]: ...


@overload
def task(
    func: None = None,
    *,
    name: str | None = None,
    timeout_secs: float | None = None,
    retries: PyRetryPolicy | None = None,
    tags: list[str] | None = None,
    description: str | None = None,
) -> Callable[[Callable[..., T]], Callable[..., T]]: ...


def task(
    func: Callable[..., T] | None = None,
    *,
    name: str | None = None,
    timeout_secs: float | None = None,
    retries: PyRetryPolicy | None = None,
    tags: list[str] | None = None,
    description: str | None = None,
) -> Callable[..., T] | Callable[[Callable[..., T]], Callable[..., T]]:
    """Annotate a function as a workflow task.

    Attaches task metadata directly to the function without wrapping it.
    The Flow builder reads these attributes when constructing workflows.

        @task
        def process(x: int) -> int:
            return x * 2

        @task(name="custom_name", timeout_secs=30, retries=RetryPolicy())
        def slow_task(x: int) -> int:
            return x * 2
    """

    def decorator(fn: Callable[..., Any]) -> Callable[..., Any]:
        task_id = name or fn.__name__
        fn._task_id = task_id  # type: ignore[attr-defined]
        fn._metadata = PyTaskMetadata(  # type: ignore[attr-defined]
            display_name=task_id,
            description=description,
            timeout_secs=timeout_secs,
            retries=retries,
            tags=tags,
        )

        # Store type annotations for optional Pydantic integration
        try:
            hints = get_type_hints(fn)
        except Exception:
            hints = {}

        params = list(inspect.signature(fn).parameters.keys())
        fn._input_type = hints.get(params[0]) if params else None  # type: ignore[attr-defined]
        fn._output_type = hints.get("return")  # type: ignore[attr-defined]

        return fn

    if func is not None:
        return decorator(func)
    return decorator
