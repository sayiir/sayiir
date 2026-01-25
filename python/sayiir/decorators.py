"""Decorators for defining workflow tasks and flows.

This module provides the @task and @flow decorators for registering
Python functions as workflow tasks.
"""

from functools import wraps
from typing import Callable, Dict, List, Optional, TypeVar, Any

from ._sayiir import PyTaskMetadata as TaskMetadata

# Type variables for generic decorator typing
F = TypeVar("F", bound=Callable[..., Any])

# Global registry of task functions
_task_registry: Dict[str, Callable[..., Any]] = {}


def get_task_registry() -> Dict[str, Callable[..., Any]]:
    """Get the global task registry.

    Returns:
        Dict mapping task IDs to their callable implementations.
    """
    return _task_registry


def task(
    name: Optional[str] = None,
    retries: int = 0,
    timeout: Optional[float] = None,
    tags: Optional[List[str]] = None,
) -> Callable[[F], F]:
    """Decorator to register a function as a workflow task.

    Tasks are the fundamental units of work in a workflow. Each task
    receives input, performs some computation, and returns output.
    Tasks can be either synchronous or asynchronous.

    Args:
        name: Optional task identifier. Defaults to the function name.
        retries: Number of retry attempts on failure (default 0).
        timeout: Maximum execution time in seconds (None = no timeout).
        tags: Optional list of tags for categorization.

    Returns:
        The decorated function with task metadata attached.

    Example:
        >>> @task
        ... def simple_task(x: int) -> int:
        ...     return x * 2

        >>> @task(retries=3, timeout=30.0)
        ... async def fetch_data(url: str) -> dict:
        ...     async with aiohttp.ClientSession() as session:
        ...         async with session.get(url) as resp:
        ...             return await resp.json()

        >>> @task(name="custom_name", tags=["network", "io"])
        ... def named_task(x: str) -> str:
        ...     return x.upper()
    """
    def decorator(func: F) -> F:
        task_id = name or func.__name__
        _task_registry[task_id] = func

        @wraps(func)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            return func(*args, **kwargs)

        # Attach metadata to the wrapper
        wrapper._task_id = task_id  # type: ignore[attr-defined]
        wrapper._metadata = TaskMetadata(  # type: ignore[attr-defined]
            retries=retries,
            timeout=timeout,
            tags=tags or [],
        )
        return wrapper  # type: ignore[return-value]

    # Handle @task vs @task()
    if callable(name):
        # Called as @task without parentheses
        func = name
        name = None
        return decorator(func)  # type: ignore[arg-type]

    return decorator


def flow(name: Optional[str] = None) -> Callable[[F], F]:
    """Decorator to define a workflow.

    A flow function should return a built Workflow object created
    using the Flow builder API.

    Args:
        name: Optional workflow name. Defaults to the function name.

    Returns:
        The decorated function.

    Example:
        >>> @flow
        ... def my_pipeline():
        ...     return (
        ...         Flow("pipeline")
        ...         .then(fetch_data)
        ...         .then(process)
        ...         .build()
        ...     )

        >>> @flow(name="data_processing")
        ... def custom_named_flow():
        ...     return Flow("custom").then(task1).then(task2).build()
    """
    def decorator(func: F) -> F:
        flow_name = name or func.__name__

        @wraps(func)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            return func(*args, **kwargs)

        wrapper._flow_name = flow_name  # type: ignore[attr-defined]
        return wrapper  # type: ignore[return-value]

    # Handle @flow vs @flow()
    if callable(name):
        func = name
        name = None
        return decorator(func)  # type: ignore[arg-type]

    return decorator
