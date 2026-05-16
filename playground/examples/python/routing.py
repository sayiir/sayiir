from sayiir import task, Flow, run_workflow

@task
def classify(ticket: dict) -> str:
    return "billing" if ticket["type"] == "invoice" else "tech"

@task
def handle_billing(ticket: dict) -> str:
    return f"Billing handled: ticket #{ticket['id']}"

@task
def handle_tech(ticket: dict) -> str:
    return f"Tech resolved: ticket #{ticket['id']}"

workflow = (
    Flow("support")
    .route(classify, keys=["billing", "tech"])
        .branch("billing", handle_billing)
        .branch("tech", handle_tech)
    .done()
    .build()
)
print(run_workflow(workflow, {"id": 42, "type": "invoice"}))
