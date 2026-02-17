"""Pydantic models for the AI research agent workflow."""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Literal

from pydantic import BaseModel, Field


class ResearchQuery(BaseModel):
    """Input to the research workflow."""

    topic: str
    depth: Literal["brief", "detailed"] = "detailed"
    max_sources_per_provider: int = Field(default=3, ge=1, le=10)


class SourceResult(BaseModel):
    """A single result from any search provider."""

    source: str  # "duckduckgo", "wikipedia", "arxiv"
    title: str
    snippet: str
    url: str


class ResearchFindings(BaseModel):
    """Merged results from all search providers, ready for synthesis."""

    topic: str
    depth: Literal["brief", "detailed"]
    sources: list[SourceResult]


class ResearchReport(BaseModel):
    """Final synthesized report produced by the LLM."""

    topic: str
    report_markdown: str
    sources: list[SourceResult]
    generated_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
