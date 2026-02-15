"""Expense approval workflow with signals and timeouts."""

from datetime import timedelta

from sayiir import (
    Flow,
    InMemoryBackend,
    resume_workflow,
    run_durable_workflow,
    send_signal,
    task,
)


@task
def submit_expense(expense: dict) -> dict:
    print(f"Expense submitted: ${expense['amount']} by {expense['employee']}")
    return {**expense, "status": "pending_approval"}


@task
def process_approved(approval: dict) -> str:
    approver = approval.get("approver", "unknown")
    return f"Expense approved by {approver} — processing reimbursement"


workflow = (
    Flow("expense-approval")
    .then(submit_expense)
    .wait_for_signal("manager_approval", timeout=timedelta(hours=48))
    .then(process_approved)
    .build()
)

backend = InMemoryBackend()

# Submit expense — parks at signal
print("Submitting expense...")
status = run_durable_workflow(
    workflow,
    "exp-001",
    {"employee": "Alice", "amount": 250.00},
    backend=backend,
)
print(f"Status after submit: {status}")

# Manager approves
print("\nManager approving...")
send_signal(
    "exp-001",
    "manager_approval",
    {"approver": "Bob", "decision": "approved"},
    backend=backend,
)

# Resume — processes approval
print("Resuming workflow...")
status = resume_workflow(workflow, "exp-001", backend=backend)
print(f"Final result: {status.output}")
