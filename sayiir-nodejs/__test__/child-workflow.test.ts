import { describe, it, expect } from "vitest";
import { task, flow, runWorkflow, runWorkflowSync } from "../src/index.js";

const hasNative = (() => {
  try {
    require("../native/sayiir-node.node");
    return true;
  } catch {
    return false;
  }
})();

describe("thenFlow builder (type-level only)", () => {
  it("builds a workflow with thenFlow", () => {
    const child = flow<number>("child")
      .then("double", (x: number) => x * 2)
      .build();

    const parent = flow<number>("parent")
      .then("inc", (x: number) => x + 1)
      .thenFlow(child)
      .build();

    expect(parent.workflowId).toBe("parent");
    expect(parent._taskRegistry["inc"]).toBeDefined();
    expect(parent._taskRegistry["double"]).toBeDefined();
  });

  it("merges child task registry into parent", () => {
    const child = flow<number>("child")
      .then("step_a", (x: number) => x + 10)
      .then("step_b", (x: number) => x * 3)
      .build();

    const parent = flow<number>("parent")
      .then("prep", (x: number) => x + 1)
      .thenFlow(child)
      .then("final", (x: number) => x - 1)
      .build();

    expect(parent._taskRegistry["prep"]).toBeDefined();
    expect(parent._taskRegistry["step_a"]).toBeDefined();
    expect(parent._taskRegistry["step_b"]).toBeDefined();
    expect(parent._taskRegistry["final"]).toBeDefined();
  });
});

describe.skipIf(!hasNative)("thenFlow — sync execution (native)", () => {
  it("basic composition: parent → child → done", () => {
    const child = flow<number>("child")
      .then("double", (x: number) => x * 2)
      .build();

    const parent = flow<number>("parent")
      .then("inc", (x: number) => x + 1)
      .thenFlow(child)
      .build();

    // 5 + 1 = 6, then child: 6 * 2 = 12
    const result = runWorkflowSync(parent, 5);
    expect(result).toBe(12);
  });

  it("output flows through child to next step", () => {
    const child = flow<number>("child")
      .then("add_ten", (x: number) => x + 10)
      .build();

    const parent = flow<number>("parent")
      .then("inc", (x: number) => x + 1)
      .thenFlow(child)
      .then("double", (x: number) => x * 2)
      .build();

    // 5 + 1 = 6, child: 6 + 10 = 16, then 16 * 2 = 32
    const result = runWorkflowSync(parent, 5);
    expect(result).toBe(32);
  });

  it("error in child propagates to parent", () => {
    const child = flow<number>("child")
      .then("fail", (_x: number) => {
        throw new Error("child failed!");
      })
      .build();

    const parent = flow<number>("parent")
      .then("inc", (x: number) => x + 1)
      .thenFlow(child)
      .build();

    expect(() => runWorkflowSync(parent, 5)).toThrow("child failed!");
  });
});

describe.skipIf(!hasNative)("thenFlow — async execution (native)", () => {
  it("basic async composition", async () => {
    const child = flow<number>("child")
      .then("double", async (x: number) => x * 2)
      .build();

    const parent = flow<number>("parent")
      .then("inc", async (x: number) => x + 1)
      .thenFlow(child)
      .build();

    const result = await runWorkflow(parent, 5);
    expect(result).toBe(12);
  });

  it("async output flows through", async () => {
    const child = flow<number>("child")
      .then("add_ten", async (x: number) => x + 10)
      .build();

    const parent = flow<number>("parent")
      .then("inc", async (x: number) => x + 1)
      .thenFlow(child)
      .then("double", async (x: number) => x * 2)
      .build();

    const result = await runWorkflow(parent, 5);
    expect(result).toBe(32);
  });

  it("async error propagation", async () => {
    const child = flow<number>("child")
      .then("fail", async (_x: number) => {
        throw new Error("async child failed!");
      })
      .build();

    const parent = flow<number>("parent")
      .then("inc", async (x: number) => x + 1)
      .thenFlow(child)
      .build();

    await expect(runWorkflow(parent, 5)).rejects.toThrow("async child failed!");
  });
});
