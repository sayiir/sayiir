from sayiir import task, Flow, run_workflow

@task(timeout="5s", retries=3)
def fetch_data(query: str) -> str:
    return f"Result for: {query}"

workflow = Flow("search").then(fetch_data).build()
print(run_workflow(workflow, "sayiir"))
