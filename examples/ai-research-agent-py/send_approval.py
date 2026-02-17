"""Send an approval signal to a running research agent workflow.

In production with PostgresBackend, this runs as a separate process
(e.g. triggered by a web API, Slack bot, or CLI).

Usage:
    python send_approval.py reports/drafts/battery-technology.json
    python send_approval.py reports/drafts/battery-technology.json --reject
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from sayiir import PostgresBackend, send_signal


def main() -> None:
    parser = argparse.ArgumentParser(description="Approve or reject a research report")
    parser.add_argument("draft_path", help="Path to the draft JSON file")
    parser.add_argument(
        "--reject",
        action="store_true",
        help="Reject the report instead of approving",
    )
    parser.add_argument(
        "--instance-id",
        required=True,
        help="Workflow instance ID to send the signal to",
    )
    parser.add_argument(
        "--db-url",
        default="postgresql://localhost/sayiir",
        help="PostgreSQL connection URL (default: postgresql://localhost/sayiir)",
    )
    args = parser.parse_args()

    draft_path = Path(args.draft_path)
    if not draft_path.exists():
        print(f"ERROR: Draft file not found: {draft_path}", file=sys.stderr)
        sys.exit(1)

    if args.reject:
        print(f"Rejecting report for workflow {args.instance_id}...")
        # Send an empty/rejection payload — save_report will see no valid report
        send_signal(
            args.instance_id,
            "human_approval",
            {"rejected": True},
            backend=PostgresBackend(args.db_url),
        )
        print("Rejection signal sent.")
    else:
        report_dict = json.loads(draft_path.read_text())
        print(f"Approving report for workflow {args.instance_id}...")
        send_signal(
            args.instance_id,
            "human_approval",
            report_dict,
            backend=PostgresBackend(args.db_url),
        )
        print("Approval signal sent! The workflow will resume and save the report.")


if __name__ == "__main__":
    main()
