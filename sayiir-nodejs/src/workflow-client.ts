/**
 * Client for submitting and controlling workflow instances.
 *
 * Unlike `runDurableWorkflow()`, the client does **not** execute tasks —
 * it only creates initial snapshots and stores lifecycle signals. A
 * {@link Worker} picks up and executes the work.
 *
 * @example
 * ```ts
 * const backend = PostgresBackend.connect(url);
 * const client = new WorkflowClient(backend);
 * const status = client.submit(orderFlow, "order-123", { items: [...] });
 * ```
 */

import type { Workflow } from "./flow.js";
import type { NapiWorkflowClient } from "./native.js";
import type { NativeWorkflowStatus, WorkflowStatus } from "./types.js";
import {
  type Backend,
  type ConflictPolicy,
  InMemoryBackend,
  PostgresBackend,
} from "./executor.js";
import { getNative } from "./native.js";

/** Options for creating a WorkflowClient. */
export interface WorkflowClientOptions {
  conflictPolicy?: ConflictPolicy;
}

/** Client for submitting and controlling workflow instances. */
export class WorkflowClient {
  /** @internal */
  private readonly _native: NapiWorkflowClient;

  constructor(backend: Backend, opts?: WorkflowClientOptions) {
    this._native = createNapiClient(backend, opts?.conflictPolicy);
  }

  /**
   * Submit a workflow for execution (does not run tasks).
   *
   * Creates an initial snapshot so a Worker can pick it up.
   */
  submit<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    instanceId: string,
    input: TIn,
  ): WorkflowStatus<TOut> {
    const raw = this._native.submit(workflow._inner, instanceId, input);
    return parseWorkflowStatus<TOut>(raw);
  }

  /** Request cancellation of a workflow instance. */
  cancel(
    instanceId: string,
    opts?: { reason?: string; cancelledBy?: string },
  ): void {
    this._native.cancel(instanceId, opts?.reason, opts?.cancelledBy);
  }

  /** Request pausing of a workflow instance. */
  pause(
    instanceId: string,
    opts?: { reason?: string; pausedBy?: string },
  ): void {
    this._native.pause(instanceId, opts?.reason, opts?.pausedBy);
  }

  /** Unpause a paused workflow instance. */
  unpause(instanceId: string): void {
    this._native.unpause(instanceId);
  }

  /** Send an external signal to a workflow instance. */
  sendSignal(instanceId: string, signalName: string, payload: unknown): void {
    this._native.sendSignal(instanceId, signalName, JSON.stringify(payload));
  }

  /**
   * Get a single task result from a workflow instance.
   *
   * Returns the JSON-encoded task output, or `null` if the task was never
   * executed. For completed or failed workflows, the result is recovered
   * from the backend's history or cache.
   */
  getTaskResult(instanceId: string, taskId: string): string | null {
    return this._native.getTaskResult(instanceId, taskId);
  }

  /** Get the current status of a workflow instance. */
  status<TOut = unknown>(instanceId: string): WorkflowStatus<TOut> {
    const raw = this._native.status(instanceId);
    return parseWorkflowStatus<TOut>(raw);
  }
}

function createNapiClient(backend: Backend, conflictPolicy?: ConflictPolicy): NapiWorkflowClient {
  const native = getNative();
  if (backend instanceof InMemoryBackend) {
    return native.NapiWorkflowClient.withInMemory(backend._inner, conflictPolicy);
  }
  if (backend instanceof PostgresBackend) {
    return native.NapiWorkflowClient.withPostgres(backend._inner, conflictPolicy);
  }
  throw new Error("backend must be InMemoryBackend or PostgresBackend");
}

function parseWorkflowStatus<TOut>(
  raw: NativeWorkflowStatus,
): WorkflowStatus<TOut> {
  switch (raw.status) {
    case "completed":
      return {
        status: "completed",
        output: (raw.outputJson != null
          ? JSON.parse(raw.outputJson)
          : undefined) as TOut,
      };
    case "in_progress":
      return { status: "in_progress" };
    case "failed":
      return { status: "failed", error: raw.error ?? "unknown error" };
    case "cancelled":
      return {
        status: "cancelled",
        reason: raw.reason,
        cancelledBy: raw.cancelledBy,
      };
    case "paused":
      return {
        status: "paused",
        reason: raw.reason,
        pausedBy: raw.pausedBy,
      };
    case "waiting":
      return {
        status: "waiting",
        wakeAt: raw.wakeAt ?? "",
        delayId: raw.delayId ?? "",
      };
    case "awaiting_signal":
      return {
        status: "awaiting_signal",
        signalId: raw.signalId ?? "",
        signalName: raw.signalName ?? "",
        wakeAt: raw.wakeAt,
      };
    default:
      throw new Error(`unknown workflow status: ${raw.status}`);
  }
}
