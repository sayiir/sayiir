import { describe, it, expect } from "vitest";
import { task, flow, runWorkflow, runWorkflowSync } from "../src/index.js";

describe("flow builder (type-level only)", () => {
  it("builds a single-task workflow", () => {
    const double = task("double", (x: number) => x * 2);

    const wf = flow<number>("test").then(double).build();

    expect(wf.workflowId).toBe("test");
    expect(wf.definitionHash).toBeDefined();
    expect(typeof wf.definitionHash).toBe("string");
    expect(wf._taskRegistry["double"]).toBeDefined();
  });

  it("builds a chained workflow", () => {
    const double = task("double", (x: number) => x * 2);
    const toString = task("to-string", (x: number) => `value: ${x}`);

    const wf = flow<number>("chain").then(double).then(toString).build();

    expect(wf._taskRegistry["double"]).toBeDefined();
    expect(wf._taskRegistry["to-string"]).toBeDefined();
  });

  it("supports inline lambdas with auto-generated ids", () => {
    const wf = flow<number>("lambdas")
      .then("step1", (x: number) => x + 1)
      .then("step2", (x: number) => x * 2)
      .build();

    expect(wf._taskRegistry["step1"]).toBeDefined();
    expect(wf._taskRegistry["step2"]).toBeDefined();
  });

  it("supports lambdas with auto-incrementing ids", () => {
    const wf = flow<number>("auto-ids")
      .then((x) => x + 1)
      .then((x) => x * 2)
      .build();

    expect(wf._taskRegistry["lambda_0"]).toBeDefined();
    expect(wf._taskRegistry["lambda_1"]).toBeDefined();
  });

  it("rejects empty workflows", () => {
    expect(() => flow<number>("empty").build()).toThrow(
      "Workflow must have at least one task",
    );
  });
});

describe("task()", () => {
  it("creates a task with metadata", () => {
    const myTask = task("my-task", (x: number) => x, {
      timeout: "30s",
      retries: 3,
      description: "test task",
      tags: ["test"],
    });

    expect(myTask._taskId).toBe("my-task");
    expect(myTask._metadata.displayName).toBe("my-task");
    expect(myTask._metadata.description).toBe("test task");
    expect(myTask._metadata.timeoutSecs).toBe(30);
    expect(myTask._metadata.retries?.maxRetries).toBe(3);
    expect(myTask._metadata.tags).toEqual(["test"]);
  });

  it("wraps with retry policy", () => {
    const myTask = task("retry-task", (x: number) => x, {
      retry: {
        maxAttempts: 5,
        initialDelay: "2s",
        backoffMultiplier: 3,
      },
    });

    expect(myTask._metadata.retries?.maxRetries).toBe(5);
    expect(myTask._metadata.retries?.initialDelaySecs).toBe(2);
    expect(myTask._metadata.retries?.backoffMultiplier).toBe(3);
  });
});

describe("duration parsing", () => {
  it("parses millisecond strings", async () => {
    const { parseDuration } = await import("../src/duration.js");

    expect(parseDuration(1000)).toBe(1000);
    expect(parseDuration("1s")).toBe(1000);
    expect(parseDuration("5m")).toBe(300000);
    expect(parseDuration("1h")).toBe(3600000);
  });

  it("throws on invalid durations", async () => {
    const { parseDuration } = await import("../src/duration.js");

    expect(() => parseDuration("invalid")).toThrow('Invalid duration: "invalid"');
  });
});

// Tests below require the native addon to be built.
// They are skipped when the addon is not available.
const hasNative = (() => {
  try {
    require("../native/sayiir-node.node");
    return true;
  } catch {
    return false;
  }
})();

describe.skipIf(!hasNative)("runWorkflowSync (native)", () => {
  it("executes a single task", () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("test").then(double).build();
    const result = runWorkflowSync(wf, 21);
    expect(result).toBe(42);
  });

  it("executes chained tasks", () => {
    const double = task("double", (x: number) => x * 2);
    const addOne = task("add-one", (x: number) => x + 1);
    const wf = flow<number>("chain").then(double).then(addOne).build();
    const result = runWorkflowSync(wf, 10);
    expect(result).toBe(21);
  });

  it("executes inline lambdas", () => {
    const wf = flow<number>("lambdas")
      .then("add-one", (x: number) => x + 1)
      .then("double", (x: number) => x * 2)
      .build();
    const result = runWorkflowSync(wf, 5);
    expect(result).toBe(12);
  });

  it("rejects async tasks with helpful error", () => {
    const asyncDouble = task("async-double", async (x: number) => x * 2);
    const wf = flow<number>("async-test").then(asyncDouble).build();
    expect(() => runWorkflowSync(wf, 21)).toThrow("returned a Promise");
  });

  it("propagates task errors", () => {
    const failing = task("fail", (_x: number) => {
      throw new Error("task failed!");
    });
    const wf = flow<number>("fail-test").then(failing).build();
    expect(() => runWorkflowSync(wf, 1)).toThrow("task failed!");
  });
});

describe.skipIf(!hasNative)("runWorkflow — async stepper (native)", () => {
  it("executes sync tasks", async () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("test").then(double).build();
    const result = await runWorkflow(wf, 21);
    expect(result).toBe(42);
  });

  it("executes chained sync tasks", async () => {
    const double = task("double", (x: number) => x * 2);
    const addOne = task("add-one", (x: number) => x + 1);
    const wf = flow<number>("chain").then(double).then(addOne).build();
    const result = await runWorkflow(wf, 10);
    expect(result).toBe(21);
  });

  it("executes async tasks (microtask-only)", async () => {
    const asyncDouble = task("async-double", async (x: number) => x * 2);
    const wf = flow<number>("async-micro").then(asyncDouble).build();
    const result = await runWorkflow(wf, 21);
    expect(result).toBe(42);
  });

  it("executes truly async tasks (setTimeout)", async () => {
    const delayed = task(
      "delayed-double",
      (x: number) =>
        new Promise<number>((resolve) =>
          setTimeout(() => resolve(x * 2), 50),
        ),
    );
    const wf = flow<number>("async-io").then(delayed).build();
    const result = await runWorkflow(wf, 21);
    expect(result).toBe(42);
  });

  it("chains sync and async tasks", async () => {
    const asyncFetch = task(
      "async-fetch",
      (id: number) =>
        new Promise<{ id: number; name: string }>((resolve) =>
          setTimeout(() => resolve({ id, name: "Alice" }), 30),
        ),
    );
    const format = task(
      "format",
      (user: { id: number; name: string }) =>
        `Hello ${user.name} (#${user.id})`,
    );
    const wf = flow<number>("mixed").then(asyncFetch).then(format).build();
    const result = await runWorkflow(wf, 42);
    expect(result).toBe("Hello Alice (#42)");
  });

  it("propagates async task rejections", async () => {
    const failing = task(
      "async-fail",
      (_x: number) =>
        new Promise<number>((_resolve, reject) =>
          setTimeout(() => reject(new Error("async boom")), 10),
        ),
    );
    const wf = flow<number>("async-fail-test").then(failing).build();
    await expect(runWorkflow(wf, 1)).rejects.toThrow("async boom");
  });

  it("propagates sync task errors", async () => {
    const failing = task("fail", (_x: number) => {
      throw new Error("task failed!");
    });
    const wf = flow<number>("fail-test").then(failing).build();
    await expect(runWorkflow(wf, 1)).rejects.toThrow("task failed!");
  });
});
