"""Task implementations for the AI research agent workflow."""

from __future__ import annotations

import json
import re
from datetime import datetime, timezone
from pathlib import Path

import arxiv
import httpx
from ddgs import DDGS
from ollama import chat

from sayiir import LoopResult, RetryPolicy, task

from models import QualityAssessment, ResearchFindings, ResearchQuery, ResearchReport, SourceResult

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

RETRY = 3
DRAFTS_DIR = Path("reports/drafts")

SYSTEM_PROMPT = """\
You are a research analyst. Given source materials from web search, \
Wikipedia, and academic papers, write a comprehensive research report \
in Markdown format.

Structure your report as:

## Key Findings
(3-5 bullet points summarizing the most important discoveries)

## Detailed Analysis
(In-depth discussion synthesizing all sources)

## Academic Perspectives
(Insights from academic papers, if available)

## Sources
(Numbered list of all sources with URLs)

Write clearly and concisely. Cite sources inline using [n] notation \
matching the numbered source list."""

# ---------------------------------------------------------------------------
# Parse task — validates input
# ---------------------------------------------------------------------------


@task(description="Validate and parse the research query")
def parse_query(raw: dict) -> dict:
    query = ResearchQuery.model_validate(raw)
    return query.model_dump()


# ---------------------------------------------------------------------------
# Search tasks — run in parallel via fork/join
# ---------------------------------------------------------------------------


@task(
    timeout="30s",
    retries=RETRY,
    tags=["search", "external"],
    description="Search the web via DuckDuckGo",
)
def search_web(query: dict) -> dict:
    q = ResearchQuery.model_validate(query)
    raw_results = DDGS().text(q.topic, max_results=q.max_sources_per_provider)
    results = [
        SourceResult(
            source="duckduckgo",
            title=r.get("title", "Untitled"),
            snippet=r.get("body", "")[:500],
            url=r.get("href", ""),
        ).model_dump()
        for r in raw_results
    ]
    return {"query": query, "results": results}


@task(
    timeout="30s",
    retries=RETRY,
    tags=["search", "external"],
    description="Search Wikipedia via REST API",
)
def search_wikipedia(query: dict) -> dict:
    q = ResearchQuery.model_validate(query)
    headers = {"User-Agent": "SayiirResearchAgent/1.0 (https://github.com/sayiir/sayiir)"}
    with httpx.Client(timeout=20, headers=headers) as http:
        resp = http.get(
            "https://en.wikipedia.org/w/rest.php/v1/search/page",
            params={"q": q.topic, "limit": q.max_sources_per_provider},
        )
        resp.raise_for_status()
        pages = resp.json().get("pages", [])

    results: list[dict] = []
    for page in pages:
        title = page.get("title", "Untitled")
        snippet = re.sub(r"<[^>]+>", "", page.get("excerpt", ""))
        key = page.get("key", title.replace(" ", "_"))
        results.append(
            SourceResult(
                source="wikipedia",
                title=title,
                snippet=snippet[:500],
                url=f"https://en.wikipedia.org/wiki/{key}",
            ).model_dump()
        )
    return {"query": query, "results": results}


@task(
    timeout="30s",
    retries=RETRY,
    tags=["search", "external"],
    description="Search academic papers on arxiv",
)
def search_arxiv(query: dict) -> dict:
    q = ResearchQuery.model_validate(query)
    client = arxiv.Client()
    search = arxiv.Search(
        query=q.topic,
        max_results=q.max_sources_per_provider,
        sort_by=arxiv.SortCriterion.Relevance,
    )
    results = [
        SourceResult(
            source="arxiv",
            title=paper.title,
            snippet=paper.summary[:500],
            url=str(paper.entry_id),
        ).model_dump()
        for paper in client.results(search)
    ]
    return {"query": query, "results": results}


# ---------------------------------------------------------------------------
# Join task + helper functions
# ---------------------------------------------------------------------------


@task(description="Merge results from all search providers")
def merge_sources(branches: dict) -> dict:
    """Join task — receives dict keyed by branch task name.

    Each branch returns {"query": {...}, "results": [...]}.
    We extract the query from the first branch and flatten all results.
    """
    all_sources: list[dict] = []
    query_dict: dict = {}

    for branch_output in branches.values():
        if not query_dict and "query" in branch_output:
            query_dict = branch_output["query"]
        all_sources.extend(branch_output.get("results", []))

    return ResearchFindings(
        topic=query_dict.get("topic", "Unknown"),
        depth=query_dict.get("depth", "detailed"),
        sources=[SourceResult.model_validate(s) for s in all_sources],
    ).model_dump()


def _synthesize(findings_dict: dict) -> dict:
    """Call a local LLM via Ollama to synthesize all sources into a report.

    Also saves a draft JSON file so the approval sender can reference it.
    """
    findings = ResearchFindings.model_validate(findings_dict)

    # Build context from all sources
    source_lines: list[str] = []
    for i, src in enumerate(findings.sources, 1):
        source_lines.append(
            f"[{i}] ({src.source}) {src.title}\n    URL: {src.url}\n    {src.snippet}"
        )
    source_text = "\n\n".join(source_lines)

    response = chat(
        model="llama3.2",
        messages=[
            {"role": "system", "content": SYSTEM_PROMPT},
            {
                "role": "user",
                "content": (
                    f"Research topic: {findings.topic}\n\n"
                    f"Source materials:\n{source_text}\n\n"
                    f"Write a {'brief' if findings.depth == 'brief' else 'comprehensive'} "
                    f"research report based on these sources."
                ),
            },
        ],
    )

    report = ResearchReport(
        topic=findings.topic,
        report_markdown=response.message.content or "",
        sources=findings.sources,
        generated_at=datetime.now(timezone.utc),
    )
    return report.model_dump(mode="json")


# Quality gate — LLM self-assessment

QUALITY_PROMPT = """\
You are a research quality reviewer. Given the report below, assess its quality.

Respond with EXACTLY one JSON object (no markdown, no extra text):
{"verdict": "<publish|revise|insufficient>", "confidence": <0.0-1.0>, "reason": "<one sentence>"}

Rules:
- "publish": report is well-sourced, coherent, and covers the topic adequately
- "revise": report has substance but needs refinement (e.g. weak structure, missing analysis)
- "insufficient": fewer than 3 sources or the report is too superficial to be useful"""


def _assess_quality(report_dict: dict) -> dict:
    """Ask the LLM to evaluate its own report and return a routing verdict."""
    report = ResearchReport.model_validate(report_dict)

    response = chat(
        model="llama3.2",
        messages=[
            {"role": "system", "content": QUALITY_PROMPT},
            {"role": "user", "content": report.report_markdown},
        ],
    )

    raw = json.loads(response.message.content or "{}")
    assessment = QualityAssessment(
        verdict=raw["verdict"],
        confidence=raw.get("confidence", 0.5),
        reason=raw.get("reason", "No reason provided"),
        report=report,
    )

    print(f"\n  Quality assessment: {assessment.verdict} "
          f"(confidence: {assessment.confidence:.0%}) — {assessment.reason}")

    return assessment.model_dump(mode="json")


def _revise_report(report_dict: dict, reason: str) -> dict:
    """Ask the LLM to improve the report based on the quality feedback."""
    report = ResearchReport.model_validate(report_dict)

    response = chat(
        model="llama3.2",
        messages=[
            {"role": "system", "content": SYSTEM_PROMPT},
            {
                "role": "user",
                "content": (
                    f"Your previous report on '{report.topic}' received this feedback:\n"
                    f"  {reason}\n\n"
                    f"Here is the original report:\n{report.report_markdown}\n\n"
                    f"Please revise and improve the report, addressing the feedback."
                ),
            },
        ],
    )

    revised = ResearchReport(
        topic=report.topic,
        report_markdown=response.message.content or "",
        sources=report.sources,
    )
    print("\n  Report revised based on quality feedback.")
    return revised.model_dump(mode="json")


def _flag_insufficient(report_dict: dict, assessment: dict) -> dict:
    """Mark the report as insufficient — still saves but with a warning header."""
    report = ResearchReport.model_validate(report_dict)
    warning = (
        f"> **Warning:** This report was flagged as insufficient "
        f"({assessment['confidence']:.0%} confidence). Reason: {assessment['reason']}\n\n"
    )
    flagged = ResearchReport(
        topic=report.topic,
        report_markdown=warning + report.report_markdown,
        sources=report.sources,
    )
    print(f"\n  Report flagged as insufficient: {assessment['reason']}")
    return flagged.model_dump(mode="json")


# ---------------------------------------------------------------------------
# Refine loop body — replaces synthesize → assess → route
# ---------------------------------------------------------------------------


@task(
    timeout="3m",
    retries=RetryPolicy(max_retries=2, initial_delay_secs=2.0),
    tags=["llm", "loop"],
    description="Synthesize or revise report, assess quality, loop until publishable",
)
def refine_report(input_dict: dict) -> LoopResult:
    """Loop body: synthesize (first pass) or revise, then assess quality.

    Returns LoopResult.done() when publishable/insufficient, or
    LoopResult.again() to request another revision pass.
    """
    # First iteration: input is ResearchFindings (has "sources" key but no "report_markdown")
    if "sources" in input_dict and "report_markdown" not in input_dict:
        report_dict = _synthesize(input_dict)
    else:
        # Subsequent iterations: input is a report dict
        report_dict = _revise_report(input_dict, input_dict.get("_feedback", "Improve the report"))

    assessment = _assess_quality(report_dict)

    if assessment["verdict"] == "publish":
        return LoopResult.done(report_dict)
    if assessment["verdict"] == "insufficient":
        return LoopResult.done(_flag_insufficient(report_dict, assessment))

    # "revise" — feed the report back with assessment context for next iteration
    report_dict["_feedback"] = assessment["reason"]
    return LoopResult.again(report_dict)


# ---------------------------------------------------------------------------
# Draft saving — runs after refine loop, before human approval
# ---------------------------------------------------------------------------


@task(description="Save the refined draft for human review")
def save_draft(report_dict: dict) -> dict:
    """Save the report produced by the refine loop as a draft JSON."""
    report = ResearchReport.model_validate(report_dict)

    DRAFTS_DIR.mkdir(parents=True, exist_ok=True)
    slug = re.sub(r"[^\w\s-]", "", report.topic.lower())
    slug = re.sub(r"[\s]+", "-", slug).strip("-")[:60]
    draft_path = DRAFTS_DIR / f"{slug}.json"
    draft_path.write_text(json.dumps(report_dict, indent=2))

    print("\n" + "=" * 60)
    print("DRAFT REPORT — review and approve to save")
    print("=" * 60)
    print(report.report_markdown)
    print("=" * 60)
    print(f"\nDraft saved to: {draft_path}")
    print()

    return report_dict


# ---------------------------------------------------------------------------
# Post-approval task
# ---------------------------------------------------------------------------


@task(description="Save approved report to a Markdown file")
def save_report(report_dict: dict) -> str:
    """Write the approved report to disk as Markdown.

    Receives the report dict via the signal payload (the approval sender
    reads the draft JSON and passes it as the signal payload).
    """
    report = ResearchReport.model_validate(report_dict)

    reports_dir = Path("reports")
    reports_dir.mkdir(exist_ok=True)

    slug = re.sub(r"[^\w\s-]", "", report.topic.lower())
    slug = re.sub(r"[\s]+", "-", slug).strip("-")[:60]
    timestamp = report.generated_at.strftime("%Y%m%d-%H%M%S")
    filepath = reports_dir / f"{slug}-{timestamp}.md"

    lines = [
        f"# {report.topic}",
        "",
        f"*Generated: {report.generated_at}*",
        "",
        report.report_markdown,
        "",
        "---",
        "",
        "## Raw Sources",
        "",
    ]
    for i, src in enumerate(report.sources, 1):
        lines.append(f"{i}. **[{src.source}]** [{src.title}]({src.url})")
        lines.append(f"   {src.snippet[:200]}...")
        lines.append("")

    filepath.write_text("\n".join(lines))
    print(f"\nReport saved to: {filepath}")
    return str(filepath)
