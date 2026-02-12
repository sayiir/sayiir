"""Tests for optional Pydantic integration."""

import pytest

pydantic = pytest.importorskip("pydantic")

from pydantic import BaseModel  # noqa: E402, I001

from sayiir import Flow, run_workflow, task  # noqa: E402, I001


# ── Models ───────────────────────────────────────────────────────


class UserInput(BaseModel):
    name: str
    age: int


class UserOutput(BaseModel):
    greeting: str
    is_adult: bool


# ── Task definitions ─────────────────────────────────────────────


@task
def greet_user(user: UserInput) -> UserOutput:
    return UserOutput(
        greeting=f"Hello, {user.name}!",
        is_adult=user.age >= 18,
    )


@task
def shout_greeting(data: dict) -> str:
    return data["greeting"].upper()


# ── Tests ────────────────────────────────────────────────────────


class TestPydanticIntegration:
    def test_model_in_model_out(self):
        """Pydantic model is validated on input and serialized on output."""
        wf = Flow("pydantic-roundtrip").then(greet_user).build()
        result = run_workflow(wf, {"name": "Alice", "age": 30})
        assert result == {"greeting": "Hello, Alice!", "is_adult": True}

    def test_validation_error_propagates(self):
        """Invalid input raises a validation error through the engine."""
        wf = Flow("pydantic-error").then(greet_user).build()
        with pytest.raises(RuntimeError, match="validation error"):
            run_workflow(wf, {"name": "Bob"})  # missing 'age'

    def test_chained_pydantic_then_plain(self):
        """Pydantic output feeds into a plain (non-model) task."""
        wf = (
            Flow("pydantic-chain")
            .then(greet_user)
            .then(shout_greeting)
            .build()
        )
        result = run_workflow(wf, {"name": "Eve", "age": 25})
        assert result == "HELLO, EVE!"

    def test_non_pydantic_unaffected(self):
        """Tasks without Pydantic annotations work as before."""

        @task
        def plain_double(x):
            return x * 2

        wf = Flow("no-pydantic").then(plain_double).build()
        assert run_workflow(wf, 5) == 10
