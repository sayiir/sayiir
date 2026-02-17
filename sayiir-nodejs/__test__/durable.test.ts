import { describe, it, expect } from "vitest";
import {
  task,
  flow,
  runDurableWorkflow,
  resumeWorkflow,
  cancelWorkflow,
  pauseWorkflow,
  unpauseWorkflow,
  InMemoryBackend,
} from "../src/index.js";

describe("durable execution", () => {
  it("runs a workflow to completion with checkpointing", () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("durable-test").then(double).build();
    const backend = new InMemoryBackend();

    const status = runDurableWorkflow(wf, "run-1", 21, backend);

    expect(status.status).toBe("completed");
    if (status.status === "completed") {
      expect(status.output).toBe(42);
    }
  });

  it("runs chained tasks durably", () => {
    const double = task("double", (x: number) => x * 2);
    const addOne = task("add-one", (x: number) => x + 1);
    const wf = flow<number>("chain")
      .then(double)
      .then(addOne)
      .build();
    const backend = new InMemoryBackend();

    const status = runDurableWorkflow(wf, "chain-1", 10, backend);

    expect(status.status).toBe("completed");
    if (status.status === "completed") {
      expect(status.output).toBe(21);
    }
  });

  it("can resume an already-completed workflow", () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("resume-test").then(double).build();
    const backend = new InMemoryBackend();

    // First run
    const first = runDurableWorkflow(wf, "resume-1", 21, backend);
    expect(first.status).toBe("completed");

    // Resume should return completed with output
    const resumed = resumeWorkflow(wf, "resume-1", backend);
    expect(resumed.status).toBe("completed");
    if (resumed.status === "completed") {
      expect(resumed.output).toBe(42);
    }
  });

  it("handles task failures in durable mode", () => {
    const failing = task("fail", (_x: number): number => {
      throw new Error("durable task failed!");
    });
    const wf = flow<number>("fail-durable").then(failing).build();
    const backend = new InMemoryBackend();

    const status = runDurableWorkflow(wf, "fail-1", 1, backend);
    expect(status.status).toBe("failed");
    if (status.status === "failed") {
      expect(status.error).toContain("durable task failed!");
    }
  });

  it("rejects cancelling a completed workflow", () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("cancel-test").then(double).build();
    const backend = new InMemoryBackend();

    runDurableWorkflow(wf, "cancel-1", 21, backend);

    expect(() =>
      cancelWorkflow("cancel-1", backend, {
        reason: "testing",
        cancelledBy: "test-suite",
      }),
    ).toThrow("Cannot cancel");
  });

  it("rejects pausing a completed workflow", () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("pause-test").then(double).build();
    const backend = new InMemoryBackend();

    runDurableWorkflow(wf, "pause-1", 21, backend);

    expect(() =>
      pauseWorkflow("pause-1", backend, {
        reason: "maintenance",
        pausedBy: "test-suite",
      }),
    ).toThrow("Cannot pause");
  });
});
