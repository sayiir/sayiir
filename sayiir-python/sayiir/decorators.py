"""Task decorator for annotating workflow task implementations."""

import inspect
import re
from collections.abc import Callable
from typing import Any, TypeVar, get_type_hints, overload

from ._sayiir import PyRetryPolicy, PyTaskMetadata

T = TypeVar("T")

# Regex for human-readable durations: "30s", "5m", "2h", "100ms"
_DURATION_RE = re.compile(
    r"^\s*(\d+(?:\.\d+)?)\s*(ms|s|m|h)\s*$", re.IGNORECASE
)

_DURATION_MULTIPLIERS = {
    "ms": 0.001,
    "s": 1.0,
    "m": 60.0,
    "h": 3600.0,
}


def parse_duration(value: "str | float | int") -> float:
    """Parse a duration to seconds.

    Accepts:
        - A number (interpreted as seconds)
        - A string like ``"30s"``, ``"5m"``, ``"1h"``, ``"100ms"``

    Returns:
        Duration in seconds as a float.

    Raises:
        ValueError: If the string cannot be parsed.
    """
    if isinstance(value, (int, float)):
        return float(value)
    match = _DURATION_RE.match(value)
    if not match:
        raise ValueError(
            f'Invalid duration: "{value}". '
            f'Expected a number (seconds) or a string like "30s", "5m", "1h", "100ms".'
        )
    amount = float(match.group(1))
    unit = match.group(2).lower()
    return amount * _DURATION_MULTIPLIERS[unit]


def _resolve_retries(
    retries: "int | PyRetryPolicy | None",
) -> PyRetryPolicy | None:
    """Resolve a retries parameter to a PyRetryPolicy.

    Accepts an int shorthand (uses 1s initial delay, 2x backoff) or a full
    ``RetryPolicy`` object.
    """
    if retries is None:
        return None
    if isinstance(retries, int):
        return PyRetryPolicy(
            max_retries=retries,
            initial_delay_secs=1.0,
            backoff_multiplier=2.0,
        )
    return retries


@overload
def task(func: Callable[..., T]) -> Callable[..., T]: ...


@overload
def task(
    func: None = None,
    *,
    name: str | None = None,
    timeout: "str | float | None" = None,
    timeout_secs: float | None = None,
    retries: "int | PyRetryPolicy | None" = None,
    tags: list[str] | None = None,
    description: str | None = None,
) -> Callable[[Callable[..., T]], Callable[..., T]]: ...


@overload
def task(
    func: str,
    *,
    timeout: "str | float | None" = None,
    timeout_secs: float | None = None,
    retries: "int | PyRetryPolicy | None" = None,
    tags: list[str] | None = None,
    description: str | None = None,
) -> Callable[[Callable[..., T]], Callable[..., T]]: ...


def task(
    func: "Callable[..., T] | str | None" = None,
    *,
    name: str | None = None,
    timeout: "str | float | None" = None,
    timeout_secs: float | None = None,
    retries: "int | PyRetryPolicy | None" = None,
    tags: list[str] | None = None,
    description: str | None = None,
) -> "Callable[..., T] | Callable[[Callable[..., T]], Callable[..., T]]":
    """Annotate a function as a workflow task.

    Attaches task metadata directly to the function without wrapping it.
    The Flow builder reads these attributes when constructing workflows.

    ``retries`` accepts either an int (shorthand for max retries with 1s/2x
    backoff defaults) or a full ``RetryPolicy(...)`` object.

    ``timeout`` accepts a number (seconds) or a human-readable string like
    ``"30s"``, ``"5m"``, ``"1h"``.  The legacy ``timeout_secs`` parameter
    is still supported but ``timeout`` takes precedence.

    The first positional argument can be a string to set the task name::

        @task("fetch-user")
        def fetch_user(user_id: int) -> dict: ...

    Examples::

        @task
        def process(x: int) -> int:
            return x * 2

        @task("charge", timeout="30s", retries=3)
        def charge_card(order: dict) -> dict:
            return {**order, "charged": True}

        @task(retries=RetryPolicy(max_retries=5, initial_delay_secs=0.5))
        def flaky_task(x: int) -> int:
            return x
    """
    # Handle @task("name", ...) — first positional arg is a string
    if isinstance(func, str):
        name = func
        func = None

    # Resolve timeout: prefer `timeout` over legacy `timeout_secs`
    resolved_timeout_secs = timeout_secs
    if timeout is not None:
        resolved_timeout_secs = parse_duration(timeout)

    # Resolve retries: int shorthand or full RetryPolicy
    resolved_retries = _resolve_retries(retries)

    def decorator(fn: Callable[..., Any]) -> Callable[..., Any]:
        task_id = name or fn.__name__
        fn._task_id = task_id  # type: ignore[attr-defined]
        fn._metadata = PyTaskMetadata(  # type: ignore[attr-defined]
            display_name=task_id,
            description=description,
            timeout_secs=resolved_timeout_secs,
            retries=resolved_retries,
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

    if func is not None and callable(func):
        return decorator(func)
    return decorator
