"""LoopResult type for controlling loop iteration."""

from enum import StrEnum
from typing import Any, Generic, TypeVar

T = TypeVar("T")


class OnMax(StrEnum):
    """Policy when a loop reaches its maximum iteration count.

    Mirrors the Rust ``MaxIterationsPolicy`` enum.
    """

    FAIL = "fail"
    """Fail the workflow with a ``MaxIterationsExceeded`` error."""

    EXIT_WITH_LAST = "exit_with_last"
    """Exit the loop with the last iteration's output."""


class LoopResult(Generic[T]):
    """Result from a loop body task.

    Return ``LoopResult.again(value)`` to continue iterating with the
    new value, or ``LoopResult.done(value)`` to exit the loop.

    The value serializes as ``{"_loop": "again"|"done", "value": ...}``
    matching the Rust ``LoopResult<T>`` serde format.
    """

    __slots__ = ("_tag", "_value")

    def __init__(self, tag: str, value: Any) -> None:
        self._tag = tag
        self._value = value

    @classmethod
    def again(cls, value: T) -> "LoopResult[T]":
        """Continue the loop with a new value."""
        return cls("again", value)

    @classmethod
    def done(cls, value: T) -> "LoopResult[T]":
        """Exit the loop with a final value."""
        return cls("done", value)

    @property
    def is_done(self) -> bool:
        return self._tag == "done"

    @property
    def is_again(self) -> bool:
        return self._tag == "again"

    @property
    def value(self) -> T:
        return self._value  # type: ignore[return-value]

    def to_dict(self) -> dict[str, Any]:
        """Serialize to the wire format expected by the engine."""
        return {"_loop": self._tag, "value": self._value}

    def __repr__(self) -> str:
        return f"LoopResult.{self._tag}({self._value!r})"
