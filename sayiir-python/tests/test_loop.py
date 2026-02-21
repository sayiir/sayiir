"""Tests for loop functionality — LoopResult and Flow.loop()."""

import pytest

from sayiir import (
    Flow,
    LoopResult,
    WorkflowError,
    run_durable_workflow,
    run_workflow,
    task,
)

# ── LoopResult tests ──────────────────────────────────────────────


class TestLoopResult:
    def test_again_creation(self):
        """LoopResult.again creates an 'again' result."""
        result = LoopResult.again(42)
        assert result.is_again is True
        assert result.is_done is False
        assert result.value == 42

    def test_done_creation(self):
        """LoopResult.done creates a 'done' result."""
        result = LoopResult.done("final")
        assert result.is_done is True
        assert result.is_again is False
        assert result.value == "final"

    def test_to_dict_again(self):
        """LoopResult.again serializes to wire format."""
        result = LoopResult.again(42)
        assert result.to_dict() == {"_loop": "again", "value": 42}

    def test_to_dict_done(self):
        """LoopResult.done serializes to wire format."""
        result = LoopResult.done("final")
        assert result.to_dict() == {"_loop": "done", "value": "final"}

    def test_repr(self):
        """LoopResult has readable repr strings."""
        again = LoopResult.again(42)
        done = LoopResult.done("final")
        assert repr(again) == "LoopResult.again(42)"
        assert repr(done) == "LoopResult.done('final')"


# ── Simple engine loop tests ──────────────────────────────────────


class TestLoopSimple:
    def test_loop_done_immediately(self):
        """Loop body returns Done on first call."""

        @task
        def immediate_done(x):
            return LoopResult.done(x * 2)

        wf = Flow("immediate").loop(immediate_done).build()
        assert run_workflow(wf, 21) == 42

    def test_loop_three_iterations(self):
        """Counter increments, Done after 3 iterations."""

        @task
        def countdown(n):
            if n <= 0:
                return LoopResult.done(0)
            return LoopResult.again(n - 1)

        wf = Flow("countdown").loop(countdown).build()
        assert run_workflow(wf, 3) == 0

    def test_loop_max_iterations_fail(self):
        """Always returns Again, max_iterations=3, should raise WorkflowError."""

        @task
        def infinite_loop(x):
            return LoopResult.again(x + 1)

        wf = (
            Flow("infinite")
            .loop(infinite_loop, max_iterations=3, on_max="fail")
            .build()
        )
        with pytest.raises(WorkflowError):
            run_workflow(wf, 0)

    def test_loop_max_iterations_exit_with_last(self):
        """Always Again, on_max='exit_with_last', returns last value."""

        @task
        def infinite_loop(x):
            return LoopResult.again(x + 1)

        wf = (
            Flow("exit-last")
            .loop(infinite_loop, max_iterations=3, on_max="exit_with_last")
            .build()
        )
        # Starts at 0: iteration 1 → 1, iteration 2 → 2, iteration 3 → 3, exits
        assert run_workflow(wf, 0) == 3

    def test_loop_in_chain(self):
        """Loop in a pipeline with setup and finalize tasks."""

        @task
        def setup(x):
            return x * 2

        @task
        def countdown(n):
            if n <= 1:
                return LoopResult.done(0)
            return LoopResult.again(n - 1)

        @task
        def finalize(x):
            return x + 100

        wf = Flow("chained").then(setup).loop(countdown).then(finalize).build()
        # Input 3 → setup → 6 → countdown (6→5→4→3→2→1→0) → finalize → 100
        assert run_workflow(wf, 3) == 100

    def test_loop_with_dict_state(self):
        """Loop body maintains dict state across iterations."""

        @task
        def accumulator(state):
            count = state.get("count", 0)
            total = state.get("total", 0)
            if count >= 3:
                return LoopResult.done(total)
            return LoopResult.again({"count": count + 1, "total": total + count})

        wf = Flow("dict-state").loop(accumulator, max_iterations=10).build()
        result = run_workflow(wf, {"count": 0, "total": 0})
        # count=0,total=0 → 1,0 → 2,1 → 3,3
        assert result == 3

    def test_loop_with_string_state(self):
        """Loop body processes string state."""

        @task
        def string_builder(s):
            if len(s) >= 5:
                return LoopResult.done(s)
            return LoopResult.again(s + "x")

        wf = Flow("string-loop").loop(string_builder).build()
        assert run_workflow(wf, "") == "xxxxx"

    def test_loop_custom_name(self):
        """Loop with custom task name."""

        @task
        def body(x):
            if x <= 0:
                return LoopResult.done(100)
            return LoopResult.again(x - 1)

        wf = Flow("named-loop").loop(body, name="custom_loop_body").build()
        assert "custom_loop_body" in wf._task_registry
        assert run_workflow(wf, 2) == 100

    def test_loop_zero_max_iterations_rejected(self):
        """Loop with max_iterations=0 is rejected at build time."""
        from sayiir import LoopResult

        @task
        def always_again(x: int) -> LoopResult:
            return LoopResult.again(x + 1)

        with pytest.raises(ValueError, match="max_iterations must be at least 1"):
            Flow("zero-loop").loop(always_again, max_iterations=0).build()


# ── Durable engine loop tests ─────────────────────────────────────


class TestLoopDurable:
    def test_loop_durable_basic(self):
        """Basic loop with durable engine."""

        @task
        def immediate_done(x):
            return LoopResult.done(x * 2)

        wf = Flow("durable-immediate").loop(immediate_done).build()
        status = run_durable_workflow(wf, "loop-inst-1", 21)
        assert status.is_completed()
        assert status.output == 42

    def test_loop_durable_three_iterations(self):
        """Three iterations with durable engine."""

        @task
        def countdown(n):
            if n <= 0:
                return LoopResult.done(0)
            return LoopResult.again(n - 1)

        wf = Flow("durable-countdown").loop(countdown).build()
        status = run_durable_workflow(wf, "loop-inst-2", 3)
        assert status.is_completed()
        assert status.output == 0

    def test_loop_durable_max_iterations_fail(self):
        """Durable loop hits max iterations and fails."""

        @task
        def infinite_loop(x):
            return LoopResult.again(x + 1)

        wf = (
            Flow("durable-infinite")
            .loop(infinite_loop, max_iterations=3, on_max="fail")
            .build()
        )
        status = run_durable_workflow(wf, "loop-inst-fail", 0)
        assert status.is_failed()
        assert "max" in status.error.lower() or "iteration" in status.error.lower()

    def test_loop_durable_exit_with_last(self):
        """Durable loop exits with last value."""

        @task
        def infinite_loop(x):
            return LoopResult.again(x + 1)

        wf = (
            Flow("durable-exit-last")
            .loop(infinite_loop, max_iterations=3, on_max="exit_with_last")
            .build()
        )
        status = run_durable_workflow(wf, "loop-inst-exit", 0)
        assert status.is_completed()
        assert status.output == 3

    def test_loop_durable_in_chain(self):
        """Durable loop in a pipeline."""

        @task
        def setup(x):
            return x * 2

        @task
        def countdown(n):
            if n <= 1:
                return LoopResult.done(0)
            return LoopResult.again(n - 1)

        @task
        def finalize(x):
            return x + 100

        wf = Flow("durable-chain").then(setup).loop(countdown).then(finalize).build()
        status = run_durable_workflow(wf, "loop-inst-chain", 3)
        assert status.is_completed()
        assert status.output == 100

    def test_loop_durable_dict_state(self):
        """Durable loop with dict state."""

        @task
        def accumulator(state):
            count = state.get("count", 0)
            total = state.get("total", 0)
            if count >= 3:
                return LoopResult.done(total)
            return LoopResult.again({"count": count + 1, "total": total + count})

        wf = Flow("durable-dict").loop(accumulator, max_iterations=10).build()
        status = run_durable_workflow(wf, "loop-inst-dict", {"count": 0, "total": 0})
        assert status.is_completed()
        assert status.output == 3


# ── Async loop tests ──────────────────────────────────────────────


@task
async def async_countdown(n):
    """Async task for loop testing."""
    if n <= 0:
        return LoopResult.done(0)
    return LoopResult.again(n - 1)


@task
async def async_immediate_done(x):
    """Async task that exits immediately."""
    return LoopResult.done(x * 2)


class TestLoopAsync:
    def test_async_loop_simple(self):
        """Async loop body works in simple engine."""
        wf = Flow("async-loop").loop(async_countdown).build()
        assert run_workflow(wf, 3) == 0

    def test_async_loop_immediate_done(self):
        """Async loop that exits immediately."""
        wf = Flow("async-immediate").loop(async_immediate_done).build()
        assert run_workflow(wf, 21) == 42

    def test_async_loop_durable(self):
        """Async loop works in durable engine."""
        wf = Flow("async-durable-loop").loop(async_countdown).build()
        status = run_durable_workflow(wf, "async-loop-1", 3)
        assert status.is_completed()
        assert status.output == 0

    def test_mixed_sync_async_with_loop(self):
        """Mix sync and async tasks with loop."""

        @task
        def setup(x):
            return x * 2

        @task
        async def finalize(x):
            return x + 100

        wf = Flow("mixed-loop").then(setup).loop(async_countdown).then(finalize).build()
        assert run_workflow(wf, 3) == 100


# ── Loop error handling tests ─────────────────────────────────────


class TestLoopErrors:
    def test_loop_body_raises_error(self):
        """Error in loop body propagates correctly."""

        @task
        def failing_loop(x):
            if x == 2:
                raise ValueError("intentional loop error")
            if x <= 0:
                return LoopResult.done(0)
            return LoopResult.again(x - 1)

        wf = Flow("failing-loop").loop(failing_loop).build()
        from sayiir import TaskError

        with pytest.raises(TaskError, match="intentional loop error"):
            run_workflow(wf, 3)

    def test_loop_body_error_durable(self):
        """Error in loop body with durable engine."""

        @task
        def failing_loop(x):
            if x == 2:
                raise ValueError("intentional loop error")
            if x <= 0:
                return LoopResult.done(0)
            return LoopResult.again(x - 1)

        wf = Flow("failing-loop-durable").loop(failing_loop).build()
        status = run_durable_workflow(wf, "loop-fail-1", 3)
        assert status.is_failed()
        assert "intentional loop error" in status.error

    def test_loop_body_returns_non_loop_result(self):
        """Loop body that returns plain value instead of LoopResult.

        This should be handled by the engine - the wrapper converts
        LoopResult to dict but doesn't enforce that tasks return LoopResult.
        The Rust side should detect invalid loop results.
        """

        @task
        def bad_loop_body(x):
            # Returns plain value instead of LoopResult
            return x + 1

        wf = Flow("bad-loop").loop(bad_loop_body, max_iterations=2).build()
        # The engine should handle this - either by failing or by
        # treating it as some default behavior
        with pytest.raises((WorkflowError, ValueError, RuntimeError)):
            run_workflow(wf, 0)


# ── Loop edge cases ───────────────────────────────────────────────


class TestLoopEdgeCases:
    def test_loop_with_none_value(self):
        """Loop that passes None through iterations."""

        @task
        def none_handler(x):
            if x is None:
                return LoopResult.done("was_none")
            return LoopResult.again(None)

        wf = Flow("none-loop").loop(none_handler).build()
        assert run_workflow(wf, 42) == "was_none"

    def test_loop_with_zero_and_false(self):
        """Loop distinguishes between 0/False and done condition."""

        @task
        def zero_counter(x):
            if x < 0:
                return LoopResult.done(x)
            return LoopResult.again(x - 1)

        wf = Flow("zero-loop").loop(zero_counter).build()
        assert run_workflow(wf, 2) == -1

    def test_loop_with_list_state(self):
        """Loop body maintains list state."""

        @task
        def list_builder(lst):
            if len(lst) >= 3:
                return LoopResult.done(lst)
            return LoopResult.again(lst + [len(lst)])

        wf = Flow("list-loop").loop(list_builder).build()
        assert run_workflow(wf, []) == [0, 1, 2]

    def test_loop_single_iteration(self):
        """Loop that completes in exactly one iteration."""

        @task
        def single_iter(x):
            if x == 1:
                return LoopResult.done(100)
            return LoopResult.again(1)

        wf = Flow("single-iter").loop(single_iter).build()
        assert run_workflow(wf, 0) == 100
