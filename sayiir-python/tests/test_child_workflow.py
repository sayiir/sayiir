"""Tests for child workflow composition (then_flow)."""

from sayiir import Flow, run_workflow, task


@task
def double(x):
    return x * 2


@task
def add_one(x):
    return x + 1


@task
def add_ten(x):
    return x + 10


@task
def failing_task(_x):
    raise ValueError("child failed!")


class TestThenFlow:
    def test_basic_composition(self):
        child = Flow("child").then(double).build()
        parent = Flow("parent").then(add_one).then_flow(child).build()
        # 5 + 1 = 6, child: 6 * 2 = 12
        assert run_workflow(parent, 5) == 12

    def test_output_flows_through(self):
        child = Flow("child").then(add_ten).build()
        parent = (
            Flow("parent").then(add_one).then_flow(child).then(double).build()
        )
        # 5 + 1 = 6, child: 6 + 10 = 16, then 16 * 2 = 32
        assert run_workflow(parent, 5) == 32

    def test_error_propagation(self):
        child = Flow("child").then(failing_task).build()
        parent = Flow("parent").then(add_one).then_flow(child).build()
        try:
            run_workflow(parent, 5)
            assert False, "Expected error"
        except Exception as e:
            assert "child failed!" in str(e)

    def test_multi_step_child(self):
        child = Flow("child").then(add_ten).then(double).build()
        parent = Flow("parent").then(add_one).then_flow(child).build()
        # 5 + 1 = 6, child: (6 + 10) * 2 = 32
        assert run_workflow(parent, 5) == 32

    def test_task_registry_merge(self):
        child = Flow("child").then(double).build()
        parent = Flow("parent").then(add_one).then_flow(child).build()
        assert "double" in parent._task_registry
        assert "add_one" in parent._task_registry
