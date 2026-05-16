const { task, flow, runWorkflow } = require("sayiir");

const fetchUser = task("fetch-user", (id) => {
  return { id, name: "Alice" };
});

const sendEmail = task("send-email", (user) => {
  return `Sent welcome to ${user.name}`;
});

const workflow = flow("welcome")
  .then(fetchUser)
  .then(sendEmail)
  .build();

runWorkflow(workflow, 42).then(console.log);
