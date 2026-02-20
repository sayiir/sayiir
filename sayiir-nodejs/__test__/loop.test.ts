import { describe, it, expect } from "vitest";
import { task, flow, runWorkflow, runWorkflowSync, LoopResult } from "../src/index.js";

describe("LoopResult", () => {
  it("creates again result", () => {
    const result = LoopResult.again(42);
    expect(result).toEqual({ _loop: "again", value: 42 });
  });

  it("creates done result", () => {
    const result = LoopResult.done("final");
    expect(result).toEqual({ _loop: "done", value: "final" });
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

describe.skipIf(!hasNative)("loop execution", () => {
  it("exits immediately on Done", () => {
    const doubleAndDone = task("double-and-done", (x: number) => {
      return LoopResult.done(x * 2);
    });

    const wf = flow<number>("immediate-done").loop(doubleAndDone).build();

    const result = runWorkflowSync(wf, 21);
    expect(result).toBe(42);
  });

  it("iterates until Done", () => {
    const countdown = task("countdown", (n: number) => {
      if (n <= 1) return LoopResult.done(0);
      return LoopResult.again(n - 1);
    });

    const wf = flow<number>("countdown").loop(countdown).build();

    const result = runWorkflowSync(wf, 5);
    expect(result).toBe(0);
  });

  it("respects max iterations with fail", () => {
    const wf = flow<number>("infinite-loop")
      .loop(
        "always-again",
        (n: number) => LoopResult.again(n + 1),
        { maxIterations: 3 },
      )
      .build();

    expect(() => runWorkflowSync(wf, 5)).toThrow();
  });

  it("respects max iterations with exit_with_last", () => {
    const wf = flow<number>("infinite-loop-exit")
      .loop(
        "always-again",
        (n: number) => LoopResult.again(n + 1),
        { maxIterations: 3, onMax: "exit_with_last" },
      )
      .build();

    const result = runWorkflowSync(wf, 5);
    // Starting at 5: iteration 1 -> 6, iteration 2 -> 7, iteration 3 -> 8
    expect(result).toBe(8);
  });

  it("works in a chain", () => {
    const setup = task("setup", (x: number) => x + 10);

    const countdown = task("countdown", (n: number) => {
      if (n <= 1) return LoopResult.done(0);
      return LoopResult.again(n - 1);
    });

    const finalize = task("finalize", (x: number) => x * 100);

    const wf = flow<number>("chained-loop")
      .then(setup)
      .loop(countdown, { maxIterations: 20 })
      .then(finalize)
      .build();

    const result = runWorkflowSync(wf, 5);
    // 5 + 10 = 15, countdown to 0, 0 * 100 = 0
    expect(result).toBe(0);
  });
});

describe.skipIf(!hasNative)("loop execution (async stepper)", () => {
  it("exits immediately on Done", async () => {
    const doubleAndDone = task("double-and-done", (x: number) => {
      return LoopResult.done(x * 2);
    });

    const wf = flow<number>("immediate-done-async").loop(doubleAndDone).build();

    const result = await runWorkflow(wf, 21);
    expect(result).toBe(42);
  });

  it("iterates until Done", async () => {
    const countdown = task("countdown", (n: number) => {
      if (n <= 1) return LoopResult.done(0);
      return LoopResult.again(n - 1);
    });

    const wf = flow<number>("countdown-async").loop(countdown).build();

    const result = await runWorkflow(wf, 5);
    expect(result).toBe(0);
  });

  it("respects max iterations with fail", async () => {
    const wf = flow<number>("infinite-loop-async")
      .loop(
        "always-again",
        (n: number) => LoopResult.again(n + 1),
        { maxIterations: 3 },
      )
      .build();

    await expect(runWorkflow(wf, 5)).rejects.toThrow();
  });

  it("respects max iterations with exit_with_last", async () => {
    const wf = flow<number>("infinite-loop-exit-async")
      .loop(
        "always-again",
        (n: number) => LoopResult.again(n + 1),
        { maxIterations: 3, onMax: "exit_with_last" },
      )
      .build();

    const result = await runWorkflow(wf, 5);
    expect(result).toBe(8);
  });

  it("works in a chain", async () => {
    const setup = task("setup", (x: number) => x + 10);

    const countdown = task("countdown", (n: number) => {
      if (n <= 1) return LoopResult.done(0);
      return LoopResult.again(n - 1);
    });

    const finalize = task("finalize", (x: number) => x * 100);

    const wf = flow<number>("chained-loop-async")
      .then(setup)
      .loop(countdown, { maxIterations: 20 })
      .then(finalize)
      .build();

    const result = await runWorkflow(wf, 5);
    expect(result).toBe(0);
  });

  it("works with async tasks", async () => {
    const countdown = task("countdown-async", async (n: number) => {
      await new Promise((resolve) => setTimeout(resolve, 1));
      if (n <= 1) return LoopResult.done(0);
      return LoopResult.again(n - 1);
    });

    const wf = flow<number>("async-loop").loop(countdown).build();

    const result = await runWorkflow(wf, 3);
    expect(result).toBe(0);
  });
});
