"""Order processing pipeline with validation, payment, and parallel inventory check."""

from sayiir import Flow, run_durable_workflow, task


@task
def validate_order(order: dict) -> dict:
    if order["amount"] <= 0:
        raise ValueError("Invalid amount")
    return {**order, "validated": True}


@task(timeout="30s", retries=3)
def charge_payment(order: dict) -> dict:
    # In production: call Stripe, Square, etc.
    return {**order, "payment_id": "pay_123", "charged": True}


@task
def check_inventory(order: dict) -> dict:
    # In production: query inventory service
    return {**order, "in_stock": True}


@task
def finalize(results: dict) -> str:
    payment = results["charge_payment"]
    inventory = results["check_inventory"]
    return (
        f"Order {payment['order_id']} complete: "
        f"paid={payment['charged']}, stock={inventory['in_stock']}"
    )


# Build workflow: validate → (charge || inventory) → finalize
workflow = (
    Flow("order-processing")
    .then(validate_order)
    .fork()
    .branch(charge_payment)
    .branch(check_inventory)
    .join(finalize)
    .build()
)

order = {"order_id": 1, "amount": 99.99}
status = run_durable_workflow(workflow, "order-1", order)
print(status.output)
