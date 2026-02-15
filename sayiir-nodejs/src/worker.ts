/**
 * Distributed worker for processing workflows across multiple processes.
 *
 * The Worker class polls a backend for available tasks, claims them,
 * and executes them using the registered task functions.
 *
 * **Note:** The distributed worker requires a PostgreSQL backend for
 * cross-process coordination. InMemoryBackend can be used for testing.
 *
 * @example
 * ```ts
 * const backend = PostgresBackend.connect(process.env.DATABASE_URL!);
 * const worker = new Worker("worker-1", backend, [orderFlow], {
 *   pollInterval: "5s",
 *   maxConcurrency: 4,
 * });
 * const handle = await worker.start();
 * process.on("SIGTERM", () => handle.shutdown());
 * ```
 */

import type { Duration } from "./types.js";
import type { Workflow } from "./flow.js";
import type { Backend } from "./executor.js";

/** Worker configuration options. */
export interface WorkerOptions {
  pollInterval?: Duration;
  claimTtl?: Duration;
  batchSize?: number;
  maxConcurrency?: number;
}

/** Handle for controlling a running worker. */
export class WorkerHandle {
  /** Request a graceful shutdown. */
  async shutdown(): Promise<void> {
    // Will be implemented when the native worker bridge is ready
    throw new Error("Worker not yet implemented — use runDurableWorkflow for now");
  }

  /** Cancel a workflow via the worker's backend. */
  async cancelWorkflow(
    instanceId: string,
    opts?: { reason?: string; cancelledBy?: string },
  ): Promise<void> {
    throw new Error("Worker not yet implemented — use cancelWorkflow() directly");
  }

  /** Pause a workflow via the worker's backend. */
  async pauseWorkflow(
    instanceId: string,
    opts?: { reason?: string; pausedBy?: string },
  ): Promise<void> {
    throw new Error("Worker not yet implemented — use pauseWorkflow() directly");
  }

  /** Unpause a workflow via the worker's backend. */
  async unpauseWorkflow(instanceId: string): Promise<void> {
    throw new Error("Worker not yet implemented — use unpauseWorkflow() directly");
  }

  /** Send a signal to a workflow via the worker's backend. */
  async sendSignal(
    instanceId: string,
    signalName: string,
    payload: unknown,
  ): Promise<void> {
    throw new Error("Worker not yet implemented — use sendSignal() directly");
  }
}

/** Distributed workflow worker. */
export class Worker {
  readonly workerId: string;
  readonly backend: Backend;
  readonly workflows: Workflow<any, any>[];
  readonly options: WorkerOptions;

  constructor(
    workerId: string,
    backend: Backend,
    workflows: Workflow<any, any>[],
    opts?: WorkerOptions,
  ) {
    this.workerId = workerId;
    this.backend = backend;
    this.workflows = workflows;
    this.options = opts ?? {};
  }

  /**
   * Start the worker and return a handle for lifecycle control.
   *
   * **Not yet implemented.** The distributed worker requires the native
   * PooledWorker bridge which is planned for a future release.
   * For now, use `runDurableWorkflow()` / `resumeWorkflow()` directly.
   */
  async start(): Promise<WorkerHandle> {
    throw new Error(
      "Worker.start() not yet implemented. " +
        "Use runDurableWorkflow() and resumeWorkflow() for single-process durable execution.",
    );
  }
}
