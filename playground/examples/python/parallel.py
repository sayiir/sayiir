from sayiir import task, Flow, run_workflow

@task
def validate_payment(order: dict) -> dict:
    return {**order, "payment": "valid"}

@task
def check_inventory(order: dict) -> dict:
    return {**order, "stock": "available"}

@task
def finalize(results: dict) -> str:
    return f"Order complete: {results}"

workflow = (
    Flow("checkout")
    .fork()
        .branch(validate_payment)
        .branch(check_inventory)
    .join(finalize)
    .build()
)
print(run_workflow(workflow, {"id": 1, "item": "Widget"}))
