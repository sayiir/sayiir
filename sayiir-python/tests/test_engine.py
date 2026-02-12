"""Tests for workflow execution — both simple engine and durable engine."""

import pytest

from sayiir import (
    BackendError,
    DurableEngine,
    Flow,
    InMemoryBackend,
    TaskError,
    WorkflowEngine,
    WorkflowError,
    cancel_workflow,
    resume_workflow,
    run_durable_workflow,
    run_workflow,
    task,
)

# ── Task definitions ──────────────────────────────────────────────


@task
def double(x):
    return x * 2


@task
def add_one(x):
    return x + 1


@task
def to_string(x):
    return str(x)


@task(name="negate")
def negate(x):
    return -x


@task
def branch_a(x):
    return x + 10


@task
def branch_b(x):
    return x + 20


@task
def join_branches(data):
    return data


@task
def failing_task(_x):
    raise ValueError("intentional failure")


# ── Simple engine (no persistence) ───────────────────────────────


class TestSimpleEngine:
    def test_single_task(self):
        wf = Flow("single").then(double).build()
        assert run_workflow(wf, 21) == 42

    def test_chained_tasks(self):
        wf = Flow("chain").then(double).then(add_one).build()
        assert run_workflow(wf, 10) == 21  # (10 * 2) + 1

    def test_three_step_pipeline(self):
        wf = Flow("pipeline").then(double).then(add_one).then(to_string).build()
        assert run_workflow(wf, 5) == "11"  # str((5 * 2) + 1)

    def test_custom_task_name(self):
        wf = Flow("named").then(negate).build()
        assert run_workflow(wf, 7) == -7

    def test_fork_join(self):
        @task
        def sum_join(data):
            return data["branch_a"] + data["branch_b"]

        wf = (
            Flow("fork")
            .then(double)
            .fork()
            .branch(branch_a)
            .branch(branch_b)
            .join(sum_join)
            .build()
        )
        # input=5 → double→10 → branch_a=20, branch_b=30 → sum=50
        result = run_workflow(wf, 5)
        assert result == 50

    def test_fork_join_dict_structure(self):
        @task
        def inspect_join(data):
            assert isinstance(data, dict)
            assert "branch_a" in data
            assert "branch_b" in data
            return sorted(data.keys())

        wf = (
            Flow("fork-inspect")
            .fork()
            .branch(branch_a)
            .branch(branch_b)
            .join(inspect_join)
            .build()
        )
        result = run_workflow(wf, 1)
        assert result == ["branch_a", "branch_b"]

    def test_multi_step_branch(self):
        @task
        def step_a(x):
            return x * 10

        @task
        def step_b(x):
            return x + 1

        @task
        def collect_join(data):
            return data

        wf = (
            Flow("multi-step")
            .fork()
            .branch(step_a, step_b)  # 5 → 50 → 51
            .branch(branch_a)  # 5 → 15
            .join(collect_join)
            .build()
        )
        result = run_workflow(wf, 5)
        # Branch name is the first task's ID
        assert result["step_a"] == 51
        assert result["branch_a"] == 15

    def test_engine_direct(self):
        engine = WorkflowEngine()
        wf = Flow("direct").then(double).build()
        result = engine.run(wf._inner, 10, wf._task_registry)
        assert result == 20

    def test_string_input(self):
        @task
        def upper(s):
            return s.upper()

        wf = Flow("string").then(upper).build()
        assert run_workflow(wf, "hello") == "HELLO"

    def test_dict_input(self):
        @task
        def get_name(d):
            return d["name"]

        wf = Flow("dict").then(get_name).build()
        assert run_workflow(wf, {"name": "sayiir"}) == "sayiir"

    def test_task_error_propagates(self):
        wf = Flow("fail").then(failing_task).build()
        try:
            run_workflow(wf, 0)
            assert False, "Should have raised"
        except RuntimeError as e:
            assert "intentional failure" in str(e)

    def test_task_error_is_task_error(self):
        """TaskError is raised and is a subclass of RuntimeError."""
        wf = Flow("fail-type").then(failing_task).build()
        with pytest.raises(TaskError):
            run_workflow(wf, 0)


# ── Durable engine (with checkpointing) ─────────────────────────


class TestDurableEngine:
    def test_single_task_completed(self):
        wf = Flow("durable-single").then(double).build()
        status = run_durable_workflow(wf, "inst-1", 21)
        assert status.is_completed()

    def test_chained_tasks_completed(self):
        wf = Flow("durable-chain").then(double).then(add_one).build()
        status = run_durable_workflow(wf, "inst-2", 10)
        assert status.is_completed()

    def test_fork_join_completed(self):
        @task
        def sum_join_durable(data):
            return data["branch_a"] + data["branch_b"]

        wf = (
            Flow("durable-fork")
            .then(double)
            .fork()
            .branch(branch_a)
            .branch(branch_b)
            .join(sum_join_durable)
            .build()
        )
        status = run_durable_workflow(wf, "inst-3", 5)
        assert status.is_completed()

    def test_status_repr(self):
        wf = Flow("repr").then(double).build()
        status = run_durable_workflow(wf, "inst-repr", 1)
        assert "Completed" in repr(status)

    def test_failed_status(self):
        wf = Flow("durable-fail").then(failing_task).build()
        status = run_durable_workflow(wf, "inst-fail", 0)
        assert status.is_failed()
        assert "intentional failure" in status.error

    def test_explicit_backend(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        wf = Flow("explicit").then(double).build()
        status = engine.run(wf._inner, "inst-explicit", 21, wf._task_registry)
        assert status.is_completed()

    def test_resume_completed_is_noop(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        wf = Flow("resume-noop").then(double).build()

        # Run to completion
        status = engine.run(wf._inner, "inst-resume", 10, wf._task_registry)
        assert status.is_completed()

        # Resume should return completed immediately
        status = engine.resume(wf._inner, "inst-resume", wf._task_registry)
        assert status.is_completed()

    def test_resume_failed_is_noop(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        wf = Flow("resume-failed").then(failing_task).build()

        status = engine.run(wf._inner, "inst-fail2", 0, wf._task_registry)
        assert status.is_failed()

        status = engine.resume(wf._inner, "inst-fail2", wf._task_registry)
        assert status.is_failed()

    def test_resume_not_found_raises(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        wf = Flow("ghost").then(double).build()

        try:
            engine.resume(wf._inner, "nonexistent", wf._task_registry)
            assert False, "Should have raised"
        except RuntimeError as e:
            assert "not found" in str(e).lower()

    def test_cancel_not_found_raises(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)

        try:
            engine.cancel("nonexistent")
            assert False, "Should have raised"
        except RuntimeError as e:
            assert "not found" in str(e).lower()

    def test_different_instances_independent(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        wf = Flow("multi").then(double).build()

        s1 = engine.run(wf._inner, "a", 10, wf._task_registry)
        s2 = engine.run(wf._inner, "b", 20, wf._task_registry)
        assert s1.is_completed()
        assert s2.is_completed()

    def test_workflow_status_fields(self):
        wf = Flow("fields").then(double).build()
        status = run_durable_workflow(wf, "inst-fields", 1)
        assert status.status == "completed"
        assert status.error is None
        assert status.reason is None
        assert status.cancelled_by is None


# ── Output value tests ───────────────────────────────────────────


class TestDurableOutput:
    def test_durable_output_value(self):
        """status.output carries the workflow result."""
        wf = Flow("output").then(double).build()
        status = run_durable_workflow(wf, "inst-out", 21)
        assert status.is_completed()
        assert status.output == 42

    def test_durable_output_chained(self):
        """Output reflects the last task in the chain."""
        wf = Flow("output-chain").then(double).then(add_one).build()
        status = run_durable_workflow(wf, "inst-out-chain", 10)
        assert status.is_completed()
        assert status.output == 21  # (10 * 2) + 1

    def test_durable_output_none_on_failure(self):
        """Failed workflows have output=None."""
        wf = Flow("output-fail").then(failing_task).build()
        status = run_durable_workflow(wf, "inst-out-fail", 0)
        assert status.is_failed()
        assert status.output is None

    def test_durable_output_on_resume(self):
        """Resume of completed workflow still has output."""
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        wf = Flow("output-resume").then(double).build()

        status = engine.run(wf._inner, "inst-out-resume", 21, wf._task_registry)
        assert status.output == 42

        # Resume should also carry the output
        status = engine.resume(wf._inner, "inst-out-resume", wf._task_registry)
        assert status.is_completed()
        assert status.output == 42


# ── Helper function tests ────────────────────────────────────────


class TestHelpers:
    def test_resume_helper(self):
        backend = InMemoryBackend()
        wf = Flow("resume-helper").then(double).build()

        # Run first
        status = run_durable_workflow(wf, "inst-rh", 10, backend=backend)
        assert status.is_completed()
        assert status.output == 20

        # Resume via helper
        status = resume_workflow(wf, "inst-rh", backend)
        assert status.is_completed()
        assert status.output == 20

    def test_cancel_helper(self):
        backend = InMemoryBackend()
        wf = Flow("cancel-helper").then(double).build()

        # Run first to create a snapshot
        run_durable_workflow(wf, "inst-ch", 10, backend=backend)

        # Cancel should not raise for a completed workflow... but cancel of
        # nonexistent should raise BackendError
        with pytest.raises(RuntimeError):
            cancel_workflow("nonexistent", backend)


# ── Exception hierarchy tests ────────────────────────────────────


class TestExceptions:
    def test_workflow_error_is_runtime_error(self):
        assert issubclass(WorkflowError, RuntimeError)

    def test_task_error_is_workflow_error(self):
        assert issubclass(TaskError, WorkflowError)
        assert issubclass(TaskError, RuntimeError)

    def test_backend_error_is_workflow_error(self):
        assert issubclass(BackendError, WorkflowError)
        assert issubclass(BackendError, RuntimeError)

    def test_simple_engine_raises_task_error(self):
        wf = Flow("exc-task").then(failing_task).build()
        with pytest.raises(TaskError):
            run_workflow(wf, 0)

    def test_cancel_nonexistent_raises_backend_error(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        with pytest.raises(BackendError):
            engine.cancel("nonexistent")

    def test_resume_nonexistent_raises_workflow_error(self):
        backend = InMemoryBackend()
        engine = DurableEngine(backend)
        wf = Flow("exc-resume").then(double).build()
        with pytest.raises(WorkflowError):
            engine.resume(wf._inner, "nonexistent", wf._task_registry)


# ── Edge-case / validation tests ─────────────────────────────────


class TestValidation:
    def test_branch_with_no_tasks_raises(self):
        with pytest.raises(ValueError, match="at least one task"):
            Flow("bad").fork().branch()

    def test_fork_with_no_branches_raises(self):
        with pytest.raises(ValueError, match="at least one branch"):
            Flow("bad").fork().join(join_branches).build()
