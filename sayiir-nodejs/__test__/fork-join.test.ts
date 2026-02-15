import { describe, it, expect } from "vitest";
import { task, flow, branch, runWorkflow } from "../src/index.js";

describe("fork/join", () => {
  it("executes parallel branches and joins results", () => {
    const double = task("double", (x: number) => x * 2);
    const addTen = task("add-ten", (x: number) => x + 10);

    const wf = flow<number>("fork-test")
      .fork([
        branch("doubled", double),
        branch("plus-ten", addTen),
      ])
      .join("merge", ([doubled, plusTen]) => ({
        doubled,
        plusTen,
      }))
      .build();

    const result = runWorkflow(wf, 5);
    expect(result).toEqual({ doubled: 10, plusTen: 15 });
  });

  it("supports inline branch functions", () => {
    const wf = flow<number>("inline-fork")
      .fork([
        branch("square", (x: number) => x * x),
        branch("negate", (x: number) => -x),
      ])
      .join("combine", ([sq, neg]) => `${sq},${neg}`)
      .build();

    const result = runWorkflow(wf, 3);
    expect(result).toBe("9,-3");
  });

  it("chains tasks before and after fork", () => {
    const addOne = task("add-one", (x: number) => x + 1);

    const wf = flow<number>("chain-fork")
      .then(addOne) // 5 + 1 = 6
      .fork([
        branch("double", (x: number) => x * 2),   // 6 * 2 = 12
        branch("triple", (x: number) => x * 3),   // 6 * 3 = 18
      ])
      .join("sum", ([d, t]) => d + t)   // 12 + 18 = 30
      .then("final", (x: number) => x.toString())
      .build();

    const result = runWorkflow(wf, 5);
    expect(result).toBe("30");
  });
});
