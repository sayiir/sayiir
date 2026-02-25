"""Sayiir workflow library — Python bindings.

All orchestration runs in Rust. Python provides task implementations.
"""

from ._sayiir import (
    BackendError,
    TaskError,
    WorkflowError,
    get_task_context,
    init_tracing,
    shutdown_tracing,
)
from ._sayiir import PyDurableEngine as DurableEngine
from ._sayiir import PyFlowBuilder as FlowBuilder
from ._sayiir import PyInMemoryBackend as InMemoryBackend
from ._sayiir import PyPostgresBackend as PostgresBackend
from ._sayiir import PyRetryPolicy as RetryPolicy
from ._sayiir import PyTaskExecutionContext as TaskExecutionContext
from ._sayiir import PyTaskMetadata as TaskMetadata
from ._sayiir import PyWorkflowEngine as WorkflowEngine
from ._sayiir import PyWorkflowStatus as WorkflowStatus
from .decorators import parse_duration, task
from .executor import (
    cancel_workflow,
    pause_workflow,
    resume_workflow,
    run_durable_workflow,
    run_workflow,
    send_signal,
    unpause_workflow,
)
from .flow import BranchBuilder, Flow, ForkBuilder, Workflow
from .loop_result import LoopResult, OnMax
from .worker import Worker, WorkerHandle

__all__ = [
    "BackendError",
    "BranchBuilder",
    "DurableEngine",
    "Flow",
    "FlowBuilder",
    "ForkBuilder",
    "LoopResult",
    "OnMax",
    "InMemoryBackend",
    "PostgresBackend",
    "RetryPolicy",
    "TaskError",
    "TaskExecutionContext",
    "TaskMetadata",
    "get_task_context",
    "Workflow",
    "WorkflowEngine",
    "WorkflowError",
    "WorkflowStatus",
    "cancel_workflow",
    "pause_workflow",
    "resume_workflow",
    "run_durable_workflow",
    "run_workflow",
    "send_signal",
    "unpause_workflow",
    "parse_duration",
    "task",
    "Worker",
    "WorkerHandle",
    "init_tracing",
    "shutdown_tracing",
]

__version__ = "0.3.0"
