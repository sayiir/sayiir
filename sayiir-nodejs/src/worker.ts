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
 * });
 * const handle = worker.start();
 * process.on("SIGTERM", () => handle.shutdown());
 * ```
 */

import type { Duration, TaskCallback } from "./types.js";
import type { Workflow } from "./flow.js";
import {
  type Backend,
  InMemoryBackend,
  PostgresBackend,
} from "./executor.js";
import type { NapiWorker, NapiWorkerHandle } from "./native.js";
import { getNative } from "./native.js";
import { parseDuration } from "./duration.js";

/** Worker configuration options. */
export interface WorkerOptions {
  pollInterval?: Duration;
  claimTtl?: Duration;
}

/** Handle for controlling a running worker. */
export class WorkerHandle {
  /** @internal */
  private readonly _native: NapiWorkerHandle;

  /** @internal */
  constructor(native: NapiWorkerHandle) {
    this._native = native;
  }

  /** Request a graceful shutdown. */
  shutdown(): void {
    this._native.shutdown();
  }

  /** Cancel a workflow via the worker's backend. */
  cancelWorkflow(
    instanceId: string,
    opts?: { reason?: string; cancelledBy?: string },
  ): void {
    this._native.cancelWorkflow(instanceId, opts?.reason, opts?.cancelledBy);
  }

  /** Pause a workflow via the worker's backend. */
  pauseWorkflow(
    instanceId: string,
    opts?: { reason?: string; pausedBy?: string },
  ): void {
    this._native.pauseWorkflow(instanceId, opts?.reason, opts?.pausedBy);
  }

  /** Unpause a workflow via the worker's backend. */
  unpauseWorkflow(instanceId: string): void {
    this._native.unpauseWorkflow(instanceId);
  }

  /** Send a signal to a workflow via the worker's backend. */
  sendSignal(instanceId: string, signalName: string, payload: unknown): void {
    this._native.sendSignal(instanceId, signalName, JSON.stringify(payload));
  }
}

/** Distributed workflow worker. */
export class Worker {
  readonly workerId: string;
  readonly backend: Backend;
  readonly workflows: readonly Workflow<any, any>[];
  readonly options: WorkerOptions;

  constructor(
    workerId: string,
    backend: Backend,
    workflows: readonly Workflow<any, any>[],
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
   * Spawns a background thread that polls for available tasks, claims them,
   * and dispatches them to the registered task functions.
   */
  start(): WorkerHandle {
    const napiWorker = this.createNapiWorker();

    // Build a combined task registry from all workflows
    const registry: Record<string, TaskCallback> = {};
    for (const wf of this.workflows) {
      Object.assign(registry, wf._taskRegistry);
    }

    // The native worker calls this dispatcher from a background thread.
    // It receives a JSON payload `{ taskId, input }` and must return
    // a JSON-serialized output string.
    const dispatcher = async (payload: string): Promise<string> => {
      const { taskId, input } = JSON.parse(payload) as {
        taskId: string;
        input: unknown;
      };
      const fn = registry[taskId];
      if (!fn) {
        throw new Error(`Task '${taskId}' not found in any workflow registry`);
      }
      const result = await fn(input);
      return JSON.stringify(result);
    };

    const napiHandle = napiWorker.start(
      this.workflows.map((wf) => wf._inner),
      dispatcher,
    );

    return new WorkerHandle(napiHandle);
  }

  /** @internal Create the native worker with the appropriate backend. */
  private createNapiWorker(): NapiWorker {
    const native = getNative();
    const pollMs = this.options.pollInterval
      ? parseDuration(this.options.pollInterval)
      : undefined;
    const claimMs = this.options.claimTtl
      ? parseDuration(this.options.claimTtl)
      : undefined;

    if (this.backend instanceof InMemoryBackend) {
      return native.NapiWorker.withInMemory(
        this.workerId,
        this.backend._inner,
        pollMs,
        claimMs,
      );
    }

    if (this.backend instanceof PostgresBackend) {
      return native.NapiWorker.withPostgres(
        this.workerId,
        this.backend._inner,
        pollMs,
        claimMs,
      );
    }

    throw new Error("Unsupported backend type");
  }
}
