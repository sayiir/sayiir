# Node.js / TypeScript Bindings Design

**Date:** 2026-02-15
**Status:** Approved
**Branch:** `feature/nodejs-bindings`

---

## Goals

- Full feature parity with the Python SDK
- Type-safe generic builder that tracks input/output types through the chain
- Distributed durable workers exposed to Node.js
- Single npm package: `sayiir`

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Binding technology | NAPI-RS | Mirrors PyO3 approach, first-class Tokio→Promise bridging, battle-tested (SWC, Prisma) |
| API style | Mirror Python + generic type chain | Consistency across SDKs, but with full TypeScript type safety (like tRPC/Hono) |
| Task definition | `task()` wrapper function | No decorators — not idiomatic in TS ecosystem |
| Scope | Full Python parity | Delays, signals, pause/resume/cancel, Postgres, distributed workers |
| Package structure | Single `sayiir` package | Matches `pip install sayiir`, native addon bundles everything |
| Duration parsing | `ms` library | Tiny, idiomatic for Node.js developers |
| Validation | Zod (peer dep) | TS equivalent of Pydantic — runtime validation + compile-time inference |

---

## Architecture

Same two-layer pattern as Python:

```
sayiir (npm)
├── TypeScript layer     — Flow builder, task(), types, ergonomic API
└── Native addon (.node) — NAPI-RS, Rust orchestration engine
    ├── sayiir-core
    ├── sayiir-runtime
    ├── sayiir-persistence
    └── sayiir-postgres
```

Rust drives all orchestration. JS provides task implementations. JSON bridges the boundary.

### Crate: `sayiir-node/`

```
sayiir-node/
├── Cargo.toml
├── build.rs
├── src/
│   ├── lib.rs              # #[napi] exports
│   ├── backend.rs          # InMemoryBackend, PostgresBackend wrappers
│   ├── codec.rs            # JSON encode/decode (JS values ↔ Bytes)
│   ├── engine.rs           # runWorkflow, task executor callback
│   ├── durable_engine.rs   # runDurableWorkflow, resume, cancel, pause, unpause, sendSignal
│   ├── flow.rs             # NapiFlowBuilder → WorkflowContinuation tree
│   └── task.rs             # NapiTaskMetadata
```

### Package: `sayiir-nodejs/`

```
sayiir-nodejs/
├── package.json
├── tsconfig.json
├── src/
│   ├── index.ts            # public exports
│   ├── flow.ts             # Flow<TInput, TOutput> generic builder
│   ├── task.ts             # task() wrapper function
│   ├── executor.ts         # runWorkflow, runDurableWorkflow, resume, cancel, pause, etc.
│   └── types.ts            # WorkflowStatus, RetryPolicy, TaskMetadata, Duration, etc.
└── __test__/               # vitest tests
```

---

## Type-Safe Flow Builder

The builder uses generic accumulation to track types through the chain.

```ts
class Flow<TInput, TLast = TInput> {
  then<TOut>(
    id: string,
    fn: TaskFn<TLast, TOut> | ((input: TLast) => TOut | Promise<TOut>),
    opts?: StepOptions
  ): Flow<TInput, Awaited<TOut>>

  // Overload: pass a TaskFn directly (id inferred)
  then<TOut>(fn: TaskFn<TLast, TOut>): Flow<TInput, Awaited<TOut>>

  fork<TBranches extends readonly BranchDef<TLast, any>[]>(
    branches: [...TBranches]
  ): ForkBuilder<TInput, TLast, TBranches>

  delay(id: string, duration: Duration): Flow<TInput, TLast>

  waitForSignal<TSignal = unknown>(
    id: string,
    signalName: string,
    opts?: { timeout?: Duration }
  ): Flow<TInput, TSignal>

  build(): Workflow<TInput, TLast>
}

class ForkBuilder<TInput, TLast, TBranches> {
  join<TOut>(
    id: string,
    fn: (branches: InferBranchOutputs<TBranches>) => TOut | Promise<TOut>
  ): Flow<TInput, Awaited<TOut>>
}
```

Factory function infers input type:

```ts
const wf = flow<number>("welcome")
  .then("fetch", (id) => getUser(id))       // id: number → User
  .then("greet", (user) => `Hi ${user.name}`) // user: User → string
  .build();                                    // Workflow<number, string>
```

---

## Task Function

```ts
function task<TIn, TOut>(
  id: string,
  fn: (input: TIn) => TOut | Promise<TOut>,
  opts?: TaskOptions
): TaskFn<TIn, TOut>

interface TaskOptions<TIn = any> {
  timeout?: Duration
  retries?: number
  retry?: RetryPolicy
  description?: string
  tags?: string[]
  input?: ZodType<TIn>          // Zod schema for input validation
  output?: ZodType              // Zod schema for output validation
}

interface RetryPolicy {
  maxAttempts: number
  initialDelay: Duration
  backoffMultiplier?: number   // default 2.0
  maxDelay?: Duration
}

// Duration: human-readable strings parsed by `ms` lib, or numeric ms
type Duration = string | number
```

Usage:

```ts
const fetchUser = task("fetch-user", async (id: number) => {
  return await db.getUser(id);
}, { timeout: "30s", retries: 3 });

// In flow — types inferred from TaskFn
flow<number>("welcome")
  .then(fetchUser)                              // id extracted, types flow
  .then("greet", (user) => `Hi ${user.name}`)   // inline step
  .build();
```

---

## Zod Integration

Zod is the TypeScript equivalent of Python's Pydantic. Sayiir supports Zod schemas for **runtime validation** at task boundaries while **inferring types** at compile time — no duplication between schemas and types.

### On `task()`

```ts
import { z } from "zod";

const OrderSchema = z.object({
  id: z.string(),
  items: z.array(z.object({ sku: z.string(), qty: z.number().positive() })),
  total: z.number(),
});

const ChargeSchema = z.object({
  chargeId: z.string(),
  receiptEmail: z.string().email(),
});

// Input/output types inferred from schemas — no manual type annotations needed
const chargePayment = task("charge-payment", async (order) => {
  //                                                 ^ z.infer<typeof OrderSchema>
  const charge = await stripe.charges.create({ amount: order.total });
  return { chargeId: charge.id, receiptEmail: order.email };
  //       ^ validated against ChargeSchema at runtime before passing to next step
}, {
  input: OrderSchema,
  output: ChargeSchema,
  timeout: "30s",
  retries: 3,
});
```

When `input` schema is provided, the task's input type is `z.infer<typeof schema>`. When `output` schema is provided, the return value is validated at runtime before being passed to the next step — catching serialization issues and data corruption at the boundary rather than deep in the next task.

### On `flow()` — workflow input validation

```ts
const wf = flow("process-order", { input: OrderSchema })
  //   TInput inferred as z.infer<typeof OrderSchema>
  .then(chargePayment)    // type-checked: OrderSchema output matches chargePayment input
  .build();

// Compile error if input doesn't match schema
await runWorkflow(wf, { wrong: "shape" });  // TS error
// Runtime error if data fails validation
await runWorkflow(wf, { id: "", items: [], total: -1 });  // Zod error
```

### On `waitForSignal()` — signal payload validation

```ts
const ApprovalSchema = z.enum(["approved", "rejected"]);

flow<Order>("order")
  .then(chargePayment)
  .waitForSignal("fraud-check", "fraud_review", {
    timeout: "24h",
    schema: ApprovalSchema,   // payload validated + typed as "approved" | "rejected"
  })
  .then("decide", (decision) => { /* decision: "approved" | "rejected" */ })
  .build();
```

### Implementation

Zod is a **peer dependency** — not bundled. Users who don't use schemas pay no cost. The validation layer is pure TypeScript (in `sayiir-nodejs/`), not Rust. Validation happens before/after the JSON serialization boundary:

1. **Input validation**: before serializing to JSON bytes for Rust
2. **Output validation**: after deserializing JSON bytes from Rust, before passing to next step

This matches how Python Pydantic integration works — validation wraps the task function in the language layer, transparent to the Rust engine.

---

## Execution API

```ts
// Simple execution — returns output directly
async function runWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  input: TIn
): Promise<TOut>

// Durable execution — returns status (may not complete immediately)
async function runDurableWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  instanceId: string,
  input: TIn,
  backend: Backend
): Promise<WorkflowStatus<TOut>>

async function resumeWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  instanceId: string,
  backend: Backend
): Promise<WorkflowStatus<TOut>>

async function cancelWorkflow(
  instanceId: string, backend: Backend,
  opts?: { reason?: string; cancelledBy?: string }
): Promise<void>

async function pauseWorkflow(
  instanceId: string, backend: Backend,
  opts?: { reason?: string; pausedBy?: string }
): Promise<void>

async function unpauseWorkflow(
  instanceId: string, backend: Backend
): Promise<void>

async function sendSignal(
  instanceId: string, signalName: string, payload: unknown, backend: Backend
): Promise<void>
```

---

## Backends

```ts
class InMemoryBackend {
  constructor()
}

class PostgresBackend {
  static async connect(url: string): Promise<PostgresBackend>
}

type Backend = InMemoryBackend | PostgresBackend
```

---

## Distributed Workers

```ts
class Worker {
  constructor(
    workerId: string,
    backend: Backend,
    workflows: Workflow<any, any>[],
    opts?: WorkerOptions
  )

  async start(): Promise<WorkerHandle>
}

interface WorkerOptions {
  pollInterval?: Duration
  claimTtl?: Duration
  batchSize?: number
  maxConcurrency?: number
}

class WorkerHandle {
  async shutdown(): Promise<void>
  async cancelWorkflow(instanceId: string, opts?: { reason?: string; cancelledBy?: string }): Promise<void>
  async pauseWorkflow(instanceId: string, opts?: { reason?: string; pausedBy?: string }): Promise<void>
  async unpauseWorkflow(instanceId: string): Promise<void>
  async sendSignal(instanceId: string, signalName: string, payload: unknown): Promise<void>
}
```

---

## WorkflowStatus

Discriminated union for idiomatic TypeScript narrowing:

```ts
type WorkflowStatus<TOut = unknown> =
  | { status: "completed"; output: TOut }
  | { status: "in_progress" }
  | { status: "failed"; error: string }
  | { status: "cancelled"; reason?: string; cancelledBy?: string }
  | { status: "paused"; reason?: string; pausedBy?: string }
  | { status: "waiting"; wakeAt: Date; delayId: string }
  | { status: "awaiting_signal"; signalId: string; signalName: string; wakeAt?: Date }
```

---

## Rust ↔ JS Bridge

**Task execution callback flow:**

1. Rust hits a task node, calls JS via `ThreadsafeFunction` with `(taskId, inputBuffer)`
2. JS looks up task in `Map<string, Function>` registry
3. Deserialize JSON bytes → JS object
4. Call user function (await if async)
5. Serialize result → JSON bytes, return `Buffer` to Rust

**Key NAPI-RS primitives:**
- `#[napi]` structs for builder, workflow, backend, metadata
- `ThreadsafeFunction` for Rust worker threads → JS main thread callbacks
- `AsyncTask` / `JsPromise` for Tokio futures → JS Promises
- `Buffer` for zero-copy byte passing

**Backend enum dispatch:** Same as Python — `BackendKind` enum with `InMemory`/`Postgres` variants, monomorphized via `with_backend!` macro.

---

## End-to-End Example

```ts
import {
  task, flow, branch, Worker, PostgresBackend,
  runDurableWorkflow, sendSignal
} from "sayiir";
import { z } from "zod";

const OrderSchema = z.object({
  id: z.string(),
  items: z.array(z.object({ sku: z.string(), qty: z.number().positive() })).min(1),
  total: z.number().positive(),
  email: z.string().email(),
});

const ChargeSchema = z.object({
  chargeId: z.string(),
  receiptEmail: z.string().email(),
  metadata: z.object({ orderId: z.string() }),
});

const validateOrder = task("validate-order", async (order) => {
  return { ...order, validated: true as const };
}, { input: OrderSchema, timeout: "5s" });

const chargePayment = task("charge-payment", async (order) => {
  const charge = await stripe.charges.create({ amount: order.total });
  return { chargeId: charge.id, receiptEmail: order.email, metadata: { orderId: order.id } };
}, { input: OrderSchema, output: ChargeSchema, timeout: "30s", retries: 3 });

const sendConfirmation = task("send-confirmation", async (charge) => {
  await mailer.send(charge.receiptEmail, "Order confirmed!");
  return { sent: true };
}, { input: ChargeSchema });

const shipOrder = task("ship-order", async (charge) => {
  return await warehouse.createShipment(charge.metadata.orderId);
}, { input: ChargeSchema });

const orderFlow = flow<Order>("process-order")
  .then(validateOrder)
  .then(chargePayment)
  .waitForSignal("fraud-check", "fraud_review", {
    timeout: "24h",
    schema: z.enum(["approved", "rejected"]),
  })
  .then("check-fraud", (decision) => {
    // decision: "approved" | "rejected" — inferred from Zod schema
    if (decision === "rejected") throw new Error("Fraud detected");
    return decision;
  })
  .fork([
    branch("email", sendConfirmation),
    branch("ship", shipOrder),
  ])
  .join("finalize", ([email, shipment]) => ({
    confirmed: email.sent,
    trackingId: shipment.trackingId,
  }))
  .build();

// Worker process
const backend = await PostgresBackend.connect(process.env.DATABASE_URL!);
const worker = new Worker("worker-1", backend, [orderFlow]);
const handle = await worker.start();
process.on("SIGTERM", () => handle.shutdown());

// API process
const status = await runDurableWorkflow(orderFlow, "order-123", myOrder, backend);
await sendSignal("order-123", "fraud_review", "approved", backend);
```
