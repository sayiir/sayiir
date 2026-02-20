"""AI Research Agent — durable workflow that searches, synthesizes, and saves.

Demonstrates: fork/join, loops (iterative refinement), retries, timeouts,
signals, Pydantic models, and durable checkpointing. If the process crashes
at any point, resume picks up from the last completed step.

Usage:
    python main.py "What are the latest advances in battery technology?"
    python main.py "Quantum error correction" --depth brief

Prerequisites:
    Ollama running locally with a model pulled (e.g. ollama pull llama3.2)
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import timedelta
from pathlib import Path

from sayiir import (
    Flow,
    InMemoryBackend,
    OnMax,
    resume_workflow,
    run_durable_workflow,
    send_signal,
)

from tasks import (
    merge_sources,
    parse_query,
    refine_report,
    save_draft,
    save_report,
    search_arxiv,
    search_web,
    search_wikipedia,
)

# ---------------------------------------------------------------------------
# Workflow definition
# ---------------------------------------------------------------------------

workflow = (
    Flow("ai-research-agent")
    .then(parse_query)
    # Search three sources in parallel
    .fork()
        .branch(search_web)
        .branch(search_wikipedia)
        .branch(search_arxiv)
    .join(merge_sources)
    # Refine loop: synthesize → assess → revise (up to 3 iterations)
    .loop(refine_report, max_iterations=3, on_max=OnMax.EXIT_WITH_LAST)
    .then(save_draft)
    # Human approval before saving
    .wait_for_signal("human_approval", timeout=timedelta(hours=48))
    .then(save_report)
    .build()
)

# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(description="AI Research Agent powered by Sayiir")
    parser.add_argument("topic", help="Research topic or question")
    parser.add_argument(
        "--depth",
        choices=["brief", "detailed"],
        default="detailed",
        help="Research depth (default: detailed)",
    )
    parser.add_argument(
        "--max-sources",
        type=int,
        default=3,
        help="Max results per search provider (default: 3)",
    )
    parser.add_argument(
        "--instance-id",
        default=None,
        help="Workflow instance ID (default: auto-generated from topic)",
    )
    parser.add_argument(
        "--resume",
        action="store_true",
        help="Resume a previously started workflow instead of starting new",
    )
    args = parser.parse_args()

    # Generate a stable instance ID from the topic
    slug = args.topic.lower()[:40].replace(" ", "-")
    instance_id = args.instance_id or f"research-{slug}"

    backend = InMemoryBackend()

    if args.resume:
        print(f"Resuming workflow {instance_id}...")
        status = resume_workflow(workflow, instance_id, backend=backend)
    else:
        query = {
            "topic": args.topic,
            "depth": args.depth,
            "max_sources_per_provider": args.max_sources,
        }
        print(f"Starting research on: {args.topic}")
        print(f"Instance ID: {instance_id}")
        print(f"Depth: {args.depth}, Max sources per provider: {args.max_sources}")
        print()

        # Run the workflow — it will park at wait_for_signal after the refine loop
        status = run_durable_workflow(workflow, instance_id, query, backend=backend)

    if status.is_awaiting_signal():
        print("Workflow is waiting for human approval.")
        print()

        # Read the draft that save_draft saved
        draft_path = _find_draft(args.topic)
        if draft_path is None:
            print("ERROR: Could not find draft file. Workflow state may be corrupted.")
            sys.exit(1)

        approval = input("Approve this report? [y/N] ").strip().lower()
        if approval in ("y", "yes"):
            # Send the report as the signal payload — it becomes input to save_report
            report_dict = json.loads(draft_path.read_text())
            send_signal(instance_id, "human_approval", report_dict, backend=backend)
            status = resume_workflow(workflow, instance_id, backend=backend)
        else:
            print("Report rejected. Workflow will not be completed.")
            sys.exit(0)

    if status.is_completed():
        print(f"\nWorkflow completed! Output: {status.output}")
    elif status.is_failed():
        print(f"\nWorkflow failed: {status.error}", file=sys.stderr)
        sys.exit(1)
    else:
        print(f"\nWorkflow status: {status}")


def _find_draft(topic: str) -> Path | None:
    """Find the draft JSON file saved by the save_draft task."""
    import re

    drafts_dir = Path("reports/drafts")
    if not drafts_dir.exists():
        return None
    slug = re.sub(r"[^\w\s-]", "", topic.lower())
    slug = re.sub(r"[\s]+", "-", slug).strip("-")[:60]
    draft_path = drafts_dir / f"{slug}.json"
    return draft_path if draft_path.exists() else None


if __name__ == "__main__":
    main()
