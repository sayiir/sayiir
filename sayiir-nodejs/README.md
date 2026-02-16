# Sayiir

**Durable workflows for Node.js and TypeScript, powered by a Rust runtime.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/sayiir/sayiir/blob/main/LICENSE)
[![Node.js 18+](https://img.shields.io/badge/node-18+-339933.svg)](https://nodejs.org)
[![Discord](https://img.shields.io/badge/Discord-Join-7289da)](https://discord.gg/MWSzsHeg)

Write plain TypeScript functions. Sayiir makes them durable — automatic checkpointing, crash recovery, and parallel execution with zero infrastructure.

```typescript
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
// "Sent welcome to Alice"
```

No DSL. No YAML. No determinism constraints. No infrastructure to deploy.

## Why Sayiir?

- **No replay, no determinism rules** — Unlike Temporal, Restate, and other replay-based engines, Sayiir checkpoints after each task and resumes from the last checkpoint. Your tasks can call any API, use any library, read the clock, generate random values. No restrictions.
- **A library, not a platform** — `pnpm add sayiir` and write workflows. No server cluster, no separate services. Optional PostgreSQL for production persistence.
- **Rust core** — All orchestration, checkpointing, and execution runs in Rust via NAPI-RS. You write TypeScript; Rust handles the hard parts.
- **Type-safe** — Generic `Flow<TInput, TLast>` builder tracks types through the chain. Full inference, no manual annotations.
- **Zod integration** — Optional input/output validation with Zod schemas as a peer dependency.

## Installation

```bash
pnpm add sayiir
```

Requires Node.js 18 or higher.

## Quickstart

### Inline lambdas — zero boilerplate

```typescript
import { flow, runWorkflow } from "sayiir";

const workflow = flow<number>("pipeline")
  .then("double", (x) => x * 2)
  .then("add-one", (x) => x + 1)
  .then("stringify", (x) => String(x))
  .build();

const result = await runWorkflow(workflow, 5);
// "11"  (5 * 2 = 10, 10 + 1 = 11, String(11))
```

No decorators, no registration — just pass any function. Use `task()` when you need metadata (retries, timeouts, tags) or reusable task definitions.

### Sequential workflow

```typescript
import { task, flow, runWorkflow } from "sayiir";

const double = task("double", (x: number) => x * 2);
const addTen = task("add-ten", (x: number) => x + 10);

const workflow = flow<number>("math")
  .then(double)
  .then(addTen)
  .build();

const result = await runWorkflow(workflow, 5);
// 20  (5 * 2 = 10, 10 + 10 = 20)
```

### Durable workflow (survives crashes)

```typescript
import { task, flow, runDurableWorkflow, InMemoryBackend } from "sayiir";

const processOrder = task("process-order", (orderId: number) => {
  return { orderId, status: "processed" };
}, { timeout: "30s" });

const sendConfirmation = task("send-confirmation", (order: { orderId: number }) => {
  return `Confirmed order ${order.orderId}`;
});

const workflow = flow<number>("order")
  .then(processOrder)
  .then(sendConfirmation)
  .build();

const backend = new InMemoryBackend();

// Checkpoints after each task — resumes from last checkpoint on crash
const status = runDurableWorkflow(workflow, "order-123", 42, backend);

if (status.status === "completed") {
  console.log(status.output); // "Confirmed order 42"
}
```

### PostgreSQL persistence

```typescript
import { PostgresBackend, runDurableWorkflow } from "sayiir";

// Auto-runs migrations on first connect
const backend = PostgresBackend.connect("postgresql://localhost/sayiir");
const status = runDurableWorkflow(workflow, "run-001", 21, backend);
```

### Retry policy

```typescript
import { task } from "sayiir";

const flakyCall = task("flaky-call", (input: string) => {
  return callExternalApi(input);
}, {
  retry: { maxAttempts: 3, initialDelay: "500ms", backoffMultiplier: 2.0 },
});
```

### Parallel execution (fork/join)

```typescript
import { task, flow, branch, runWorkflow } from "sayiir";

const validatePayment = task("validate-payment", (order: { id: number }) => {
  return { payment: "valid" };
});

const checkInventory = task("check-inventory", (order: { id: number }) => {
  return { stock: "available" };
});

const workflow = flow<{ id: number }>("checkout")
  .fork([
    branch("payment", validatePayment),
    branch("inventory", checkInventory),
  ])
  .join("finalize", ([payment, inventory]) => {
    return { ...payment, ...inventory };
  })
  .build();

const result = await runWorkflow(workflow, { id: 1 });
```

### Delays and signals

```typescript
import { flow, runDurableWorkflow, sendSignal, resumeWorkflow } from "sayiir";

const workflow = flow<number>("approval")
  .then("submit", (id) => ({ requestId: id }))
  .waitForSignal("approval", "manager_approval", { timeout: "48h" })
  .then("process", (signal) => `Approved: ${signal}`)
  .build();

// First run — parks at the signal
const status = runDurableWorkflow(workflow, "req-1", 42, backend);
// status.status === "awaiting_signal"

// Later, when the approval arrives:
sendSignal("req-1", "manager_approval", { approved: true }, backend);
const final = resumeWorkflow(workflow, "req-1", backend);
```

### Zod validation

```typescript
import { z } from "zod";
import { task, flow, runWorkflow } from "sayiir";

const OrderSchema = z.object({
  id: z.string(),
  amount: z.number().positive(),
});

const processOrder = task("process-order", (order) => {
  return { status: "charged", amount: order.amount };
}, {
  input: OrderSchema,
});

const workflow = flow("checkout").then(processOrder).build();
const result = await runWorkflow(workflow, { id: "abc", amount: 99.99 });
// Zod validates input before the task runs
```

### Task metadata

```typescript
const processPayment = task("process-payment", (order) => {
  // ...
}, {
  timeout: "60s",
  retries: 3,
  tags: ["payments", "critical"],
  description: "Charges the customer's payment method",
});
```

## API Reference

### Task Definition

- **`task(id, fn, opts?)`** — Create a named task. Optional: `timeout`, `retries`, `retry`, `tags`, `description`, `input`/`output` (Zod schemas).

### Flow Builder

- **`flow<TInput>(name)`** — Create a new type-safe flow builder.
- **`.then(fn)`** / **`.then(id, fn, opts?)`** — Append a task step. Accepts `task()` functions, plain functions, or lambdas.
- **`.fork(branches)`** — Start parallel branches. Takes an array of `branch()` definitions.
- **`.join(id, fn)`** — Merge branches with a combining function.
- **`.delay(id, duration)`** — Durable delay (`"30s"`, `"5m"`, `"1h"`, or milliseconds).
- **`.waitForSignal(id, signalName, opts?)`** — Wait for an external signal.
- **`.build()`** — Compile and return a `Workflow<TIn, TOut>`.

### Execution

- **`await runWorkflow(workflow, input)`** — Execute in-memory (async). Returns `Promise<TOut>`.
- **`runWorkflowSync(workflow, input)`** — Execute in-memory (sync-only tasks). Returns `TOut`.
- **`runDurableWorkflow(workflow, instanceId, input, backend)`** — Execute with checkpointing. Returns `WorkflowStatus<TOut>`.
- **`resumeWorkflow(workflow, instanceId, backend)`** — Resume from last checkpoint.
- **`cancelWorkflow(instanceId, backend, opts?)`** — Cancel a running workflow.
- **`pauseWorkflow(instanceId, backend, opts?)`** — Pause a running workflow.
- **`unpauseWorkflow(instanceId, backend)`** — Unpause a paused workflow.
- **`sendSignal(instanceId, signalName, payload, backend)`** — Send an external signal.

### WorkflowStatus\<TOut\>

Discriminated union — use `status.status` with TypeScript narrowing:

```typescript
if (status.status === "completed") {
  console.log(status.output); // TOut
} else if (status.status === "failed") {
  console.log(status.error);  // string
}
```

Variants: `completed`, `in_progress`, `failed`, `cancelled`, `paused`, `waiting`, `awaiting_signal`.

### Backends

- **`new InMemoryBackend()`** — In-memory storage for development and testing.
- **`PostgresBackend.connect(url)`** — PostgreSQL persistence. Auto-runs migrations.

## Architecture

```
Your TypeScript code       Sayiir (Rust)              Storage
┌──────────────┐    ┌─────────────────────┐    ┌──────────────┐
│  task()       │───>│  Orchestration      │───>│  Checkpoint  │
│  functions   │    │  Checkpointing      │    │  after each  │
│              │<───│  Crash recovery     │<───│  task        │
└──────────────┘    │  Fork/join          │    └──────────────┘
                    │  Serialization      │
                    └─────────────────────┘
```

TypeScript provides task implementations. Rust handles everything else: building the execution graph, running tasks in order, checkpointing results, recovering from crashes, and managing parallel branches.

## Requirements

- Node.js 18+
- Optional: `zod` for input/output validation

## License

MIT

## Links

- [Documentation](https://docs.sayiir.dev/getting-started/nodejs/)
- [GitHub](https://github.com/sayiir/sayiir)
- [Discord](https://discord.gg/MWSzsHeg)
- [Roadmap](https://github.com/sayiir/sayiir/blob/main/ROADMAP.md)
