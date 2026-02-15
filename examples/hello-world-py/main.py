"""Minimal Sayiir example — 10 lines."""

from sayiir import Flow, run_workflow, task


@task
def greet(name: str) -> str:
    return f"Hello, {name}!"


workflow = Flow("hello").then(greet).build()
result = run_workflow(workflow, "World")
print(result)  # Hello, World!
