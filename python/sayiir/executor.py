"""Task executor for Python-side task execution.

This module provides the TaskExecutor class which polls the Rust
task channel and executes Python tasks in an asyncio event loop.
"""

import asyncio
from typing import Any, Callable, Dict, Optional

from ._sayiir import TaskChannel

from .decorators import get_task_registry


class TaskExecutor:
    """Executor that runs Python tasks requested by the Rust orchestrator.

    The TaskExecutor polls a TaskChannel for task requests, looks up
    the corresponding Python callable in the registry, executes it
    (handling both sync and async functions), and submits the result
    back to the channel.

    This class bridges the gap between Rust workflow orchestration
    and Python task execution.

    Example:
        >>> from sayiir import TaskExecutor, WorkflowEngine
        >>>
        >>> engine = WorkflowEngine()
        >>> executor = TaskExecutor(engine.get_channel())
        >>>
        >>> async def main():
        ...     # Start executor in background
        ...     executor_task = asyncio.create_task(executor.run())
        ...     # Run workflow
        ...     result = await engine.run(my_workflow, instance_id="run-1", input=data)
        ...     # Stop executor
        ...     executor.stop()
        ...     await executor_task
    """

    def __init__(
        self,
        channel: TaskChannel,
        task_registry: Optional[Dict[str, Callable[..., Any]]] = None,
    ) -> None:
        """Create a new TaskExecutor.

        Args:
            channel: The TaskChannel for receiving requests and sending responses.
            task_registry: Optional custom task registry. If not provided,
                          uses the global registry from @task decorators.
        """
        self._channel = channel
        self._tasks = task_registry if task_registry is not None else get_task_registry()
        self._running = False

    async def run(self) -> None:
        """Main executor loop - polls channel and executes tasks.

        This coroutine runs continuously until stop() is called,
        polling for task requests and executing them.

        The executor handles both synchronous and asynchronous task
        functions automatically.
        """
        self._running = True
        while self._running:
            task_request = self._channel.poll_task()

            if task_request is None:
                # No pending tasks, yield control briefly
                await asyncio.sleep(0.001)
                continue

            request_id = task_request.request_id
            task_id = task_request.task_id

            try:
                # Look up the task function
                if task_id not in self._tasks:
                    self._channel.submit_error(
                        request_id,
                        f"Task '{task_id}' not found in registry"
                    )
                    continue

                func = self._tasks[task_id]
                input_data = task_request.get_input()

                # Execute the task (handle both sync and async)
                if asyncio.iscoroutinefunction(func):
                    result = await func(input_data)
                else:
                    result = func(input_data)

                # Submit successful result
                self._channel.submit_result(request_id, result)

            except Exception as e:
                # Submit error
                self._channel.submit_error(request_id, str(e))

    def stop(self) -> None:
        """Stop the executor loop.

        After calling stop(), the executor will finish processing
        any current task and then exit the run() loop.
        """
        self._running = False

    @property
    def is_running(self) -> bool:
        """Check if the executor is currently running.

        Returns:
            True if the executor loop is active, False otherwise.
        """
        return self._running


async def run_with_executor(
    engine: Any,
    workflow: Any,
    instance_id: str,
    input_data: Any,
    task_registry: Optional[Dict[str, Callable[..., Any]]] = None,
) -> Any:
    """Convenience function to run a workflow with automatic executor management.

    This function creates a TaskExecutor, starts it in the background,
    runs the workflow, and cleans up the executor when done.

    Args:
        engine: The WorkflowEngine instance.
        workflow: The workflow to run.
        instance_id: Unique identifier for this workflow run.
        input_data: Input data for the workflow.
        task_registry: Optional custom task registry.

    Returns:
        The workflow output.

    Example:
        >>> from sayiir import WorkflowEngine, run_with_executor
        >>>
        >>> engine = WorkflowEngine()
        >>> result = await run_with_executor(
        ...     engine,
        ...     my_workflow,
        ...     instance_id="run-1",
        ...     input_data={"url": "https://api.example.com"}
        ... )
    """
    channel = engine.get_channel()
    executor = TaskExecutor(channel, task_registry)

    # Start executor in background
    executor_task = asyncio.create_task(executor.run())

    try:
        # Run the workflow
        result = await engine.run(workflow, instance_id, input_data)
        return result
    finally:
        # Stop and wait for executor
        executor.stop()
        await executor_task
