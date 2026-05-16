const { task, flow, runWorkflow } = require("sayiir");

const fetchData = task("fetch-data", (query) => {
  return `Result for: ${query}`;
}, { timeout: "5s", retries: 3 });

const workflow = flow("search").then(fetchData).build();
runWorkflow(workflow, "sayiir").then(console.log);
