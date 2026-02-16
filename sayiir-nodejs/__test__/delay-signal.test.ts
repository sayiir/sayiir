import { describe, it, expect } from "vitest";
import {
  task,
  flow,
  runDurableWorkflow,
  resumeWorkflow,
  sendSignal,
  InMemoryBackend,
} from "../src/index.js";

describe("delay", () => {
  it("builds a workflow with a delay step", () => {
    const wf = flow<number>("delay-test")
      .then("step1", (x: number) => x + 1)
      .delay("wait", "5s")
      .then("step2", (x: number) => x * 2)
      .build();

    expect(wf.workflowId).toBe("delay-test");
  });

  it("durable workflow returns waiting status on delay", () => {
    const wf = flow<number>("delay-durable")
      .then("step1", (x: number) => x + 1)
      .delay("wait", "1h")
      .then("step2", (x: number) => x * 2)
      .build();
    const backend = new InMemoryBackend();

    const status = runDurableWorkflow(wf, "delay-1", 5, backend);

    // Should hit the delay and return waiting with structured fields
    expect(status.status).toBe("waiting");
    if (status.status === "waiting") {
      expect(status.delayId).toBe("wait");
      expect(status.wakeAt).toMatch(/^\d{4}-\d{2}-\d{2}T/);
    }
  });
});

describe("signals", () => {
  it("builds a workflow with a signal step", () => {
    const wf = flow<number>("signal-test")
      .then("step1", (x: number) => x + 1)
      .waitForSignal<string>("approval", "user_approval")
      .then("step2", (signal: string) => `got: ${signal}`)
      .build();

    expect(wf.workflowId).toBe("signal-test");
  });

  it("durable workflow waits for signal then resumes", () => {
    const wf = flow<number>("signal-durable")
      .then("step1", (x: number) => x + 1)
      .waitForSignal("approval", "user_approval")
      .then("step2", (input: unknown) => `result: ${input}`)
      .build();
    const backend = new InMemoryBackend();

    // First run — should park at signal with structured fields
    const status1 = runDurableWorkflow(wf, "sig-1", 5, backend);
    expect(status1.status).toBe("awaiting_signal");
    if (status1.status === "awaiting_signal") {
      expect(status1.signalId).toBe("approval");
      expect(status1.signalName).toBe("user_approval");
    }

    // Send the signal
    sendSignal("sig-1", "user_approval", "go", backend);

    // Resume — the signal payload ("go") becomes the next input.
    const status2 = resumeWorkflow(wf, "sig-1", backend);
    expect(status2.status).toBe("completed");
    if (status2.status === "completed") {
      expect(status2.output).toBe("result: go");
    }
  });

  it("builds a workflow with signal timeout", () => {
    const wf = flow<number>("signal-timeout")
      .then("step1", (x: number) => x + 1)
      .waitForSignal("approval", "user_approval", { timeout: "24h" })
      .then("step2", (signal: unknown) => signal)
      .build();

    expect(wf.workflowId).toBe("signal-timeout");
  });
});
