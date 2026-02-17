import { task, flow, runWorkflow } from "sayiir";

const fetchUser = task("fetch-user", (id: number) => {
  return { id, name: "Alice" };
});

const sendEmail = task("send-email", (user: { id: number; name: string }) => {
  return `Sent welcome to ${user.name}`;
});

const workflow = flow<number>("welcome")
  .then(fetchUser)
  .then(sendEmail)
  .build();

const result = await runWorkflow(workflow, 42);
console.log(result); // "Sent welcome to Alice"
