"""Standalone script to send an approval signal.

In production, this would be called from a web API, Slack bot, etc.
"""

from sayiir import PostgresBackend, send_signal

# Use the same backend as the workflow
backend = PostgresBackend("postgresql://localhost/sayiir")

send_signal(
    instance_id="exp-001",
    signal_name="manager_approval",
    payload={"approver": "Bob", "decision": "approved"},
    backend=backend,
)

print("Approval signal sent!")
