const { task, flow, branch, runDurableWorkflow, InMemoryBackend } = require("sayiir");

const validatePayment = task("validate-payment", (order) => {
  return { ...order, payment: "valid" };
});

const checkInventory = task("check-inventory", (order) => {
  return { ...order, stock: "available" };
});

const finalize = task("finalize", (results) => {
  return `Order complete: ${JSON.stringify(results)}`;
});

const workflow = flow("checkout")
  .fork([branch("payment", validatePayment), branch("inventory", checkInventory)])
  .join("finalize", finalize)
  .build();

const backend = new InMemoryBackend();
const status = runDurableWorkflow(workflow, "run-1", { id: 1, item: "Widget" }, backend);
console.log(status.output);
