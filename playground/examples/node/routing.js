const { task, flow, runDurableWorkflow, InMemoryBackend } = require("sayiir");

const classify = task("classify", (ticket) => {
  return ticket.type === "invoice" ? "billing" : "tech";
});

const handleBilling = task("handle-billing", (ticket) => {
  return `Billing handled: ticket #${ticket.id}`;
});

const handleTech = task("handle-tech", (ticket) => {
  return `Tech resolved: ticket #${ticket.id}`;
});

const workflow = flow("support")
  .route(classify, ["billing", "tech"])
    .branch("billing", handleBilling)
    .branch("tech", handleTech)
  .done()
  .build();

const backend = new InMemoryBackend();
const status = runDurableWorkflow(workflow, "run-1", { id: 42, type: "invoice" }, backend);
console.log(status.output);
