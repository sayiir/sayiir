"""Integration tests for distributed Worker against a real PostgreSQL backend.

Uses testcontainers to spin up a throwaway Postgres instance automatically.
Requires Docker to be running. Skipped if Docker is unavailable.

Usage:
    pytest sayiir-python/tests/test_worker_integration.py -v
"""

import time
import uuid

import pytest

from sayiir import (
    Flow,
    PostgresBackend,
    Worker,
    WorkerHandle,
    resume_workflow,
    run_durable_workflow,
    task,
)

tc = pytest.importorskip("testcontainers", reason="testcontainers not installed")
from testcontainers.postgres import PostgresContainer  # noqa: E402

# ── Task definitions ─────────────────────────────────────────────


@task
def double(x):
    return x * 2


@task
def add_one(x):
    return x + 1


@task
def to_string(x):
    return str(x)


@task
def identity(x):
    return x


# ── Fixtures ─────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def postgres_url():
    """Start a Postgres 17 container for the entire test module."""
    with PostgresContainer("postgres:17-alpine", driver=None) as pg:
        yield pg.get_connection_url()


@pytest.fixture
def backend(postgres_url):
    return PostgresBackend(postgres_url)


# ── Helpers ──────────────────────────────────────────────────────


def uid(prefix: str = "test") -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def poll_until_terminal(workflow, instance_id, backend, *, timeout=10.0):
    """Resume a workflow in a loop until it reaches a terminal status."""
    deadline = time.monotonic() + timeout
    status = None
    while time.monotonic() < deadline:
        status = resume_workflow(workflow, instance_id, backend)
        if status.status in ("completed", "failed", "cancelled"):
            return status
        time.sleep(0.1)
    raise TimeoutError(
        f"Workflow {instance_id} did not reach terminal status within {timeout}s "
        f"(last status: {status.status if status else 'unknown'})"
    )


def start_worker(backend, workflows, *, poll_interval=0.1) -> WorkerHandle:
    """Create and start a worker with a fast poll interval."""
    worker = Worker(uid("worker"), backend, poll_interval=poll_interval)
    return worker.start(workflows)


# ── Tests ────────────────────────────────────────────────────────


class TestWorkerIntegration:
    def test_worker_executes_single_task(self, backend):
        wf = Flow("single").then(double).build()
        iid = uid("single")

        status = run_durable_workflow(wf, iid, 21, backend)
        assert status.status == "in_progress"

        handle = start_worker(backend, [wf])
        try:
            result = poll_until_terminal(wf, iid, backend)
            assert result.status == "completed"
            assert result.output == 42
        finally:
            handle.shutdown()
            handle.join()

    def test_worker_executes_chained_tasks(self, backend):
        wf = Flow("chain").then(double).then(add_one).then(to_string).build()
        iid = uid("chain")

        run_durable_workflow(wf, iid, 5, backend)

        handle = start_worker(backend, [wf])
        try:
            result = poll_until_terminal(wf, iid, backend)
            assert result.status == "completed"
            assert result.output == "11"  # str((5 * 2) + 1)
        finally:
            handle.shutdown()
            handle.join()

    def test_worker_handles_multiple_workflows(self, backend):
        wf = Flow("multi").then(double).build()
        iid_a = uid("multi-a")
        iid_b = uid("multi-b")

        run_durable_workflow(wf, iid_a, 10, backend)
        run_durable_workflow(wf, iid_b, 20, backend)

        handle = start_worker(backend, [wf])
        try:
            result_a = poll_until_terminal(wf, iid_a, backend)
            result_b = poll_until_terminal(wf, iid_b, backend)
            assert result_a.status == "completed"
            assert result_a.output == 20
            assert result_b.status == "completed"
            assert result_b.output == 40
        finally:
            handle.shutdown()
            handle.join()

    def test_worker_cancel_via_handle(self, backend):
        wf = (
            Flow("cancel")
            .then(identity)
            .wait_for_signal("approval")
            .then(identity)
            .build()
        )
        iid = uid("cancel")

        run_durable_workflow(wf, iid, "start", backend)

        handle = start_worker(backend, [wf])
        try:
            # Let the worker pick up and reach the signal wait
            time.sleep(0.5)
            handle.cancel_workflow(iid, reason="test cancel")

            result = poll_until_terminal(wf, iid, backend)
            assert result.status == "cancelled"
        finally:
            handle.shutdown()
            handle.join()

    def test_worker_pause_unpause_via_handle(self, backend):
        wf = (
            Flow("pause")
            .then(identity)
            .wait_for_signal("gate")
            .then(identity)
            .build()
        )
        iid = uid("pause")

        run_durable_workflow(wf, iid, "data", backend)

        handle = start_worker(backend, [wf])
        try:
            time.sleep(0.5)
            handle.pause_workflow(iid, reason="test pause")

            # Verify paused
            status = resume_workflow(wf, iid, backend)
            assert status.status == "paused"

            # Unpause and send signal so it can complete
            handle.unpause_workflow(iid)
            handle.send_signal(iid, "gate", "go")

            result = poll_until_terminal(wf, iid, backend)
            assert result.status == "completed"
        finally:
            handle.shutdown()
            handle.join()

    def test_worker_send_signal_via_handle(self, backend):
        wf = (
            Flow("signal")
            .then(identity)
            .wait_for_signal("approval")
            .then(identity)
            .build()
        )
        iid = uid("signal")

        run_durable_workflow(wf, iid, "input", backend)

        handle = start_worker(backend, [wf])
        try:
            time.sleep(0.5)
            handle.send_signal(iid, "approval", {"approved": True})

            result = poll_until_terminal(wf, iid, backend)
            assert result.status == "completed"
            assert result.output == {"approved": True}
        finally:
            handle.shutdown()
            handle.join()

    def test_worker_graceful_shutdown(self, backend):
        wf = Flow("shutdown").then(double).build()

        handle = start_worker(backend, [wf])
        handle.shutdown()
        handle.join()  # Should return without hanging or crashing
