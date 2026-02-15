import { describe, it, expect } from "vitest";
import { Worker, WorkerHandle, flow, task, InMemoryBackend } from "../src/index.js";

describe("worker (placeholder)", () => {
  it("constructs a Worker instance", () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("worker-test").then(double).build();
    const backend = new InMemoryBackend();

    const worker = new Worker("w-1", backend, [wf], {
      pollInterval: "5s",
      maxConcurrency: 4,
    });

    expect(worker.workerId).toBe("w-1");
    expect(worker.workflows).toHaveLength(1);
    expect(worker.options.maxConcurrency).toBe(4);
  });

  it("start() throws not-yet-implemented", async () => {
    const wf = flow<number>("worker-test2")
      .then("inc", (x: number) => x + 1)
      .build();
    const backend = new InMemoryBackend();
    const worker = new Worker("w-2", backend, [wf]);

    await expect(worker.start()).rejects.toThrow("not yet implemented");
  });

  it("WorkerHandle methods throw not-yet-implemented", async () => {
    const handle = new WorkerHandle();

    await expect(handle.shutdown()).rejects.toThrow("not yet implemented");
    await expect(handle.cancelWorkflow("id")).rejects.toThrow("not yet implemented");
    await expect(handle.pauseWorkflow("id")).rejects.toThrow("not yet implemented");
    await expect(handle.unpauseWorkflow("id")).rejects.toThrow("not yet implemented");
    await expect(handle.sendSignal("id", "sig", {})).rejects.toThrow("not yet implemented");
  });
});
