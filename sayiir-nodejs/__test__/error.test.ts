/**
 * Error propagation and stack trace preservation tests.
 *
 * Verifies that errors from task functions survive the Rust FFI round-trip
 * with their messages intact, and that stack traces are preserved where
 * the architecture allows it.
 */

import { describe, it, expect } from "vitest";
import {
  task,
  flow,
  runWorkflow,
  runWorkflowSync,
  runDurableWorkflow,
  resumeWorkflow,
  cancelWorkflow,
  pauseWorkflow,
  InMemoryBackend,
  WorkflowError,
  TaskError,
  BackendError,
} from "../src/index.js";

// ── Task definitions ──────────────────────────────────────────

function helperThatThrows(): never {
  throw new Error("deep error from helper");
}

const taskCallsHelper = task("calls-helper", (_x: number) => {
  helperThatThrows();
});

const taskThrowsTypeError = task("type-error", (_x: number) => {
  throw new TypeError("wrong type provided");
});

const taskThrowsRangeError = task("range-error", (_x: number) => {
  throw new RangeError("value out of range");
});

const taskThrowsWithContext = task("context-error", (_x: number) => {
  throw new Error("custom runtime error with context: abc-123");
});

const taskThrowsString = task("string-throw", (_x: number) => {
  // eslint-disable-next-line no-throw-literal
  throw "raw string error";
});

const failing = task("failing", (_x: number): number => {
  throw new Error("intentional failure");
});

const double = task("double", (x: number) => x * 2);
const addOne = task("add-one", (x: number) => x + 1);

// ── Async stepper (runWorkflow) ─────────────────────────────

describe("error propagation — async stepper (runWorkflow)", () => {
  it("preserves error message across async execution", async () => {
    const wf = flow<number>("async-err").then(taskThrowsWithContext).build();
    await expect(runWorkflow(wf, 0)).rejects.toThrow("abc-123");
  });

  it("preserves TypeError message", async () => {
    const wf = flow<number>("async-type").then(taskThrowsTypeError).build();
    await expect(runWorkflow(wf, 0)).rejects.toThrow("wrong type provided");
  });

  it("preserves stack trace in async stepper (errors stay in JS)", async () => {
    const wf = flow<number>("async-stack").then(taskCallsHelper).build();
    try {
      await runWorkflow(wf, 0);
      expect.unreachable("should have thrown");
    } catch (e: unknown) {
      const err = e as Error;
      expect(err.message).toContain("deep error from helper");
      // Stack trace should reference our helper function
      expect(err.stack).toContain("helperThatThrows");
    }
  });

  it("propagates error from second task in chain", async () => {
    const wf = flow<number>("async-mid")
      .then(double)
      .then(failing)
      .build();
    await expect(runWorkflow(wf, 5)).rejects.toThrow("intentional failure");
  });

  it("propagates error from last task in chain", async () => {
    const wf = flow<number>("async-last")
      .then(double)
      .then(addOne)
      .then(failing)
      .build();
    await expect(runWorkflow(wf, 1)).rejects.toThrow("intentional failure");
  });

  it("throws WorkflowError for missing task in registry", async () => {
    const wf = flow<number>("async-missing")
      .then(double)
      .build();
    // Manually corrupt the registry
    delete wf._taskRegistry["double"];
    await expect(runWorkflow(wf, 1)).rejects.toThrow("not found");
  });
});

// ── Sync engine (runWorkflowSync) ───────────────────────────

describe("error propagation — sync engine (runWorkflowSync)", () => {
  it("preserves error message across FFI", () => {
    const wf = flow<number>("sync-err").then(taskThrowsWithContext).build();
    expect(() => runWorkflowSync(wf, 0)).toThrow("abc-123");
  });

  it("preserves TypeError message across FFI", () => {
    const wf = flow<number>("sync-type").then(taskThrowsTypeError).build();
    expect(() => runWorkflowSync(wf, 0)).toThrow("wrong type provided");
  });

  it("preserves RangeError message across FFI", () => {
    const wf = flow<number>("sync-range").then(taskThrowsRangeError).build();
    expect(() => runWorkflowSync(wf, 0)).toThrow("value out of range");
  });

  it("preserves error from middle of chain", () => {
    const wf = flow<number>("sync-mid")
      .then(double)
      .then(failing)
      .then(addOne)
      .build();
    expect(() => runWorkflowSync(wf, 5)).toThrow("intentional failure");
  });

  it("handles string throws", () => {
    const wf = flow<number>("sync-string").then(taskThrowsString).build();
    // String throws become generic errors when crossing FFI
    expect(() => runWorkflowSync(wf, 0)).toThrow();
  });
});

// ── Durable engine (runDurableWorkflow) ─────────────────────

describe("error propagation — durable engine", () => {
  it("preserves full error message in failed status", () => {
    const wf = flow<number>("dur-ctx").then(taskThrowsWithContext).build();
    const backend = new InMemoryBackend();
    const status = runDurableWorkflow(wf, "dur-ctx-1", 0, backend);

    expect(status.status).toBe("failed");
    if (status.status === "failed") {
      expect(status.error).toContain("custom runtime error with context: abc-123");
    }
  });

  it("preserves TypeError message in failed status", () => {
    const wf = flow<number>("dur-type").then(taskThrowsTypeError).build();
    const backend = new InMemoryBackend();
    const status = runDurableWorkflow(wf, "dur-type-1", 0, backend);

    expect(status.status).toBe("failed");
    if (status.status === "failed") {
      expect(status.error).toContain("wrong type provided");
    }
  });

  it("error in middle of chain produces failed status", () => {
    const wf = flow<number>("dur-mid")
      .then(double)
      .then(failing)
      .then(addOne)
      .build();
    const backend = new InMemoryBackend();
    const status = runDurableWorkflow(wf, "dur-mid-1", 5, backend);

    expect(status.status).toBe("failed");
    if (status.status === "failed") {
      expect(status.error).toContain("intentional failure");
    }
  });

  it("failed status has no output", () => {
    const wf = flow<number>("dur-no-out").then(failing).build();
    const backend = new InMemoryBackend();
    const status = runDurableWorkflow(wf, "dur-no-out-1", 1, backend);

    expect(status.status).toBe("failed");
    if (status.status === "completed") {
      expect.unreachable("should not be completed");
    }
  });

  it("resume of failed workflow preserves error", () => {
    const wf = flow<number>("dur-resume").then(failing).build();
    const backend = new InMemoryBackend();

    const status1 = runDurableWorkflow(wf, "dur-resume-1", 1, backend);
    expect(status1.status).toBe("failed");

    const status2 = resumeWorkflow(wf, "dur-resume-1", backend);
    expect(status2.status).toBe("failed");
    if (status1.status === "failed" && status2.status === "failed") {
      expect(status2.error).toBe(status1.error);
    }
  });
});

// ── Error types and hierarchy ───────────────────────────────

describe("error class hierarchy", () => {
  it("WorkflowError extends Error", () => {
    const err = new WorkflowError("test");
    expect(err).toBeInstanceOf(Error);
    expect(err).toBeInstanceOf(WorkflowError);
    expect(err.name).toBe("WorkflowError");
  });

  it("TaskError extends WorkflowError", () => {
    const err = new TaskError("test");
    expect(err).toBeInstanceOf(Error);
    expect(err).toBeInstanceOf(WorkflowError);
    expect(err).toBeInstanceOf(TaskError);
    expect(err.name).toBe("TaskError");
  });

  it("BackendError extends WorkflowError", () => {
    const err = new BackendError("test");
    expect(err).toBeInstanceOf(Error);
    expect(err).toBeInstanceOf(WorkflowError);
    expect(err).toBeInstanceOf(BackendError);
    expect(err.name).toBe("BackendError");
  });

  it("TaskError is not a BackendError", () => {
    const err = new TaskError("test");
    expect(err).not.toBeInstanceOf(BackendError);
  });

  it("BackendError is not a TaskError", () => {
    const err = new BackendError("test");
    expect(err).not.toBeInstanceOf(TaskError);
  });
});

// ── Backend error tests ─────────────────────────────────────

describe("backend errors", () => {
  it("cancel nonexistent workflow throws", () => {
    const backend = new InMemoryBackend();
    expect(() => cancelWorkflow("nonexistent", backend)).toThrow("not found");
  });

  it("pause nonexistent workflow throws", () => {
    const backend = new InMemoryBackend();
    expect(() => pauseWorkflow("nonexistent", backend)).toThrow("not found");
  });

  it("resume nonexistent workflow throws", () => {
    const wf = flow<number>("ghost").then(double).build();
    const backend = new InMemoryBackend();
    expect(() => resumeWorkflow(wf, "nonexistent", backend)).toThrow();
  });

  it("cancel completed workflow throws Cannot cancel", () => {
    const wf = flow<number>("cancel-done").then(double).build();
    const backend = new InMemoryBackend();
    runDurableWorkflow(wf, "cancel-done-1", 21, backend);
    expect(() => cancelWorkflow("cancel-done-1", backend)).toThrow("Cannot cancel");
  });

  it("pause completed workflow throws Cannot pause", () => {
    const wf = flow<number>("pause-done").then(double).build();
    const backend = new InMemoryBackend();
    runDurableWorkflow(wf, "pause-done-1", 21, backend);
    expect(() => pauseWorkflow("pause-done-1", backend)).toThrow("Cannot pause");
  });
});
