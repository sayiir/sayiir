/**
 * Integration tests for distributed Worker against a real PostgreSQL backend.
 *
 * Uses testcontainers to spin up a throwaway Postgres instance automatically.
 * Requires Docker to be running.
 *
 * Usage:
 *   pnpm vitest run --config vitest.integration.config.mts
 */

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import {
  Worker,
  PostgresBackend,
  flow,
  task,
  runDurableWorkflow,
  resumeWorkflow,
} from "../src/index.js";
import type { WorkflowStatus } from "../src/index.js";
import crypto from "node:crypto";
import {
  PostgreSqlContainer,
  type StartedPostgreSqlContainer,
} from "@testcontainers/postgresql";

function uid(prefix = "test"): string {
  return `${prefix}-${crypto.randomUUID().slice(0, 8)}`;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function pollUntilTerminal<T>(
  workflow: Parameters<typeof resumeWorkflow>[0],
  instanceId: string,
  backend: Parameters<typeof resumeWorkflow>[2],
  timeoutMs = 10_000,
): Promise<WorkflowStatus<T>> {
  const deadline = Date.now() + timeoutMs;
  let status: WorkflowStatus<T>;
  while (Date.now() < deadline) {
    status = resumeWorkflow(workflow, instanceId, backend) as WorkflowStatus<T>;
    if (
      status.status === "completed" ||
      status.status === "failed" ||
      status.status === "cancelled"
    ) {
      return status;
    }
    await sleep(100);
  }
  throw new Error(
    `Workflow ${instanceId} did not reach terminal status within ${timeoutMs}ms (last: ${status!.status})`,
  );
}

// Task definitions
const double = task("double", (x: number) => x * 2);
const addOne = task("add_one", (x: number) => x + 1);
const toString = task("to_string", (x: number) => String(x));
const identity = task("identity", <T>(x: T) => x);

describe("Worker integration (Postgres)", () => {
  let container: StartedPostgreSqlContainer;
  let connectionUrl: string;

  beforeAll(async () => {
    container = await new PostgreSqlContainer("postgres:17-alpine").start();
    connectionUrl = container.getConnectionUri();
  }, 60_000);

  afterAll(async () => {
    await container?.stop();
  });

  function makeBackend() {
    return PostgresBackend.connect(connectionUrl);
  }

  it("executes a single task", async () => {
    const backend = makeBackend();
    const wf = flow<number>("single").then(double).build();
    const iid = uid("single");

    const initial = runDurableWorkflow(wf, iid, 21, backend);
    expect(initial.status).toBe("in_progress");

    const worker = new Worker(uid("w"), backend, [wf], { pollInterval: 100 });
    const handle = worker.start();
    try {
      const result = await pollUntilTerminal<number>(wf, iid, backend);
      expect(result.status).toBe("completed");
      if (result.status === "completed") {
        expect(result.output).toBe(42);
      }
    } finally {
      handle.shutdown();
    }
  });

  it("executes chained tasks", async () => {
    const backend = makeBackend();
    const wf = flow<number>("chain")
      .then(double)
      .then(addOne)
      .then(toString)
      .build();
    const iid = uid("chain");

    runDurableWorkflow(wf, iid, 5, backend);

    const worker = new Worker(uid("w"), backend, [wf], { pollInterval: 100 });
    const handle = worker.start();
    try {
      const result = await pollUntilTerminal<string>(wf, iid, backend);
      expect(result.status).toBe("completed");
      if (result.status === "completed") {
        expect(result.output).toBe("11"); // String((5 * 2) + 1)
      }
    } finally {
      handle.shutdown();
    }
  });

  it("handles multiple workflows", async () => {
    const backend = makeBackend();
    const wf = flow<number>("multi").then(double).build();
    const iidA = uid("multi-a");
    const iidB = uid("multi-b");

    runDurableWorkflow(wf, iidA, 10, backend);
    runDurableWorkflow(wf, iidB, 20, backend);

    const worker = new Worker(uid("w"), backend, [wf], { pollInterval: 100 });
    const handle = worker.start();
    try {
      const [resultA, resultB] = await Promise.all([
        pollUntilTerminal<number>(wf, iidA, backend),
        pollUntilTerminal<number>(wf, iidB, backend),
      ]);
      expect(resultA.status).toBe("completed");
      expect(resultB.status).toBe("completed");
      if (resultA.status === "completed") expect(resultA.output).toBe(20);
      if (resultB.status === "completed") expect(resultB.output).toBe(40);
    } finally {
      handle.shutdown();
    }
  });

  it("cancels a workflow via handle", async () => {
    const backend = makeBackend();
    const wf = flow<string>("cancel")
      .then(identity)
      .waitForSignal("sig", "approval")
      .then(identity)
      .build();
    const iid = uid("cancel");

    runDurableWorkflow(wf, iid, "start", backend);

    const worker = new Worker(uid("w"), backend, [wf], { pollInterval: 100 });
    const handle = worker.start();
    try {
      await sleep(500);
      handle.cancelWorkflow(iid, { reason: "test cancel" });

      const result = await pollUntilTerminal(wf, iid, backend);
      expect(result.status).toBe("cancelled");
    } finally {
      handle.shutdown();
    }
  });

  it("pauses and unpauses a workflow via handle", async () => {
    const backend = makeBackend();
    const wf = flow<string>("pause")
      .then(identity)
      .waitForSignal("sig", "gate")
      .then(identity)
      .build();
    const iid = uid("pause");

    runDurableWorkflow(wf, iid, "data", backend);

    const worker = new Worker(uid("w"), backend, [wf], { pollInterval: 100 });
    const handle = worker.start();
    try {
      await sleep(500);
      handle.pauseWorkflow(iid, { reason: "test pause" });

      const paused = resumeWorkflow(wf, iid, backend);
      expect(paused.status).toBe("paused");

      handle.unpauseWorkflow(iid);
      handle.sendSignal(iid, "gate", "go");

      const result = await pollUntilTerminal(wf, iid, backend);
      expect(result.status).toBe("completed");
    } finally {
      handle.shutdown();
    }
  });

  it("sends a signal via handle", async () => {
    const backend = makeBackend();
    const wf = flow<string>("signal")
      .then(identity)
      .waitForSignal("sig", "approval")
      .then(identity)
      .build();
    const iid = uid("signal");

    runDurableWorkflow(wf, iid, "input", backend);

    const worker = new Worker(uid("w"), backend, [wf], { pollInterval: 100 });
    const handle = worker.start();
    try {
      await sleep(500);
      handle.sendSignal(iid, "approval", { approved: true });

      const result = await pollUntilTerminal<{ approved: boolean }>(
        wf,
        iid,
        backend,
      );
      expect(result.status).toBe("completed");
      if (result.status === "completed") {
        expect(result.output).toEqual({ approved: true });
      }
    } finally {
      handle.shutdown();
    }
  });

  it("shuts down gracefully", () => {
    const backend = makeBackend();
    const wf = flow<number>("shutdown").then(double).build();

    const worker = new Worker(uid("w"), backend, [wf], { pollInterval: 100 });
    const handle = worker.start();
    handle.shutdown();
    // Should return without hanging or crashing
  });
});
