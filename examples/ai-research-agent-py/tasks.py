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

from sayiir import RetryPolicy, task

from models import ResearchFindings, ResearchQuery, ResearchReport, SourceResult

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
# Join + synthesis tasks
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


@task(
    timeout="2m",
    retries=RetryPolicy(max_retries=2, initial_delay_secs=2.0),
    tags=["llm"],
    description="Synthesize sources into a research report using a local LLM",
)
def synthesize(findings_dict: dict) -> dict:
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
        report_markdown=response.message.content,
        sources=findings.sources,
        generated_at=datetime.now(timezone.utc),
    )
    report_dict = report.model_dump(mode="json")

    # Save draft for the approval sender to reference
    DRAFTS_DIR.mkdir(parents=True, exist_ok=True)
    slug = re.sub(r"[^\w\s-]", "", report.topic.lower())
    slug = re.sub(r"[\s]+", "-", slug).strip("-")[:60]
    draft_path = DRAFTS_DIR / f"{slug}.json"
    draft_path.write_text(json.dumps(report_dict, indent=2))

    # Print draft for human review
    print("\n" + "=" * 60)
    print("DRAFT REPORT — review and approve to save")
    print("=" * 60)
    print(report.report_markdown)
    print("=" * 60)
    print(f"\nDraft saved to: {draft_path}")
    print("To approve, run:")
    print(f"  python -m ai_research_agent.send_approval {draft_path}")
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
