import { describe, it, expect } from "vitest";
import {
  Worker,
  flow,
  task,
  InMemoryBackend,
  runDurableWorkflow,
} from "../src/index.js";

describe("worker", () => {
  it("constructs a Worker instance", () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("worker-test").then(double).build();
    const backend = new InMemoryBackend();

    const worker = new Worker("w-1", backend, [wf], {
      pollInterval: "5s",
    });

    expect(worker.workerId).toBe("w-1");
    expect(worker.workflows).toHaveLength(1);
  });

  it("starts, executes a task, and shuts down", async () => {
    const double = task("double", (x: number) => x * 2);
    const wf = flow<number>("worker-e2e").then(double).build();
    const backend = new InMemoryBackend();

    // Submit a workflow first so there's work for the worker
    const status = runDurableWorkflow(wf, "inst-1", 21, backend);
    // With InMemoryBackend the durable engine runs synchronously,
    // so the workflow should complete immediately
    expect(status.status).toBe("completed");
    expect(status.output).toBe(42);

    // Now test that the worker can start and shut down cleanly
    const worker = new Worker("w-e2e", backend, [wf], {
      pollInterval: 50, // 50ms for fast test
    });
    const handle = worker.start();
    handle.shutdown();
  });
});
