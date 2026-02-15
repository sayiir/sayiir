/**
 * Workflow execution utilities.
 *
 * Thin wrappers around the native addon, mirroring the Python executor.
 */

import type { Workflow } from "./flow.js";
import type { NativeWorkflowStatus, WorkflowStatus } from "./types.js";
import { WorkflowError } from "./types.js";
import type {
  NapiDurableEngine,
  NapiInMemoryBackend,
  NapiPostgresBackend,
} from "./native.js";
import { getNative } from "./native.js";

/** Backend type union. */
export type Backend = InMemoryBackend | PostgresBackend;

/** In-memory persistence backend for testing and development. */
export class InMemoryBackend {
  /** @internal */
  readonly _inner: NapiInMemoryBackend;

  constructor() {
    this._inner = new (getNative().NapiInMemoryBackend)();
  }
}

/** PostgreSQL persistence backend for durable production workflows. */
export class PostgresBackend {
  /** @internal */
  readonly _inner: NapiPostgresBackend;

  private constructor(inner: NapiPostgresBackend) {
    this._inner = inner;
  }

  static connect(url: string): PostgresBackend {
    const inner = getNative().NapiPostgresBackend.connect(url);
    return new PostgresBackend(inner);
  }
}

/**
 * Run a workflow to completion (no persistence).
 *
 * Returns the workflow output directly.
 */
export function runWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  input: TIn,
): TOut {
  const engine = new (getNative().NapiWorkflowEngine)();
  return engine.run(
    workflow._inner,
    input,
    workflow._taskRegistry,
  ) as TOut;
}

/**
 * Run a workflow with checkpointing and durability.
 *
 * Returns a WorkflowStatus indicating the outcome.
 */
export function runDurableWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  instanceId: string,
  input: TIn,
  backend: Backend,
): WorkflowStatus<TOut> {
  const engine = createDurableEngine(backend);
  const raw = engine.run(
    workflow._inner,
    instanceId,
    input,
    workflow._taskRegistry,
  );
  return parseWorkflowStatus<TOut>(raw);
}

/** Resume a workflow from a saved checkpoint. */
export function resumeWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  instanceId: string,
  backend: Backend,
): WorkflowStatus<TOut> {
  const engine = createDurableEngine(backend);
  const raw = engine.resume(
    workflow._inner,
    instanceId,
    workflow._taskRegistry,
  );
  return parseWorkflowStatus<TOut>(raw);
}

/** Request cancellation of a running workflow. */
export function cancelWorkflow(
  instanceId: string,
  backend: Backend,
  opts?: { reason?: string; cancelledBy?: string },
): void {
  const engine = createDurableEngine(backend);
  engine.cancel(instanceId, opts?.reason, opts?.cancelledBy);
}

/** Request pausing of a running workflow. */
export function pauseWorkflow(
  instanceId: string,
  backend: Backend,
  opts?: { reason?: string; pausedBy?: string },
): void {
  const engine = createDurableEngine(backend);
  engine.pause(instanceId, opts?.reason, opts?.pausedBy);
}

/** Unpause a paused workflow so it can be resumed. */
export function unpauseWorkflow(
  instanceId: string,
  backend: Backend,
): void {
  const engine = createDurableEngine(backend);
  engine.unpause(instanceId);
}

/** Send an external signal to a workflow instance. */
export function sendSignal(
  instanceId: string,
  signalName: string,
  payload: unknown,
  backend: Backend,
): void {
  const engine = createDurableEngine(backend);
  engine.sendSignal(instanceId, signalName, payload);
}

// ---- Internal helpers ----

function createDurableEngine(backend: Backend): NapiDurableEngine {
  const native = getNative();
  if (backend instanceof InMemoryBackend) {
    return native.NapiDurableEngine.withInMemory(backend._inner);
  }
  if (backend instanceof PostgresBackend) {
    return native.NapiDurableEngine.withPostgres(backend._inner);
  }
  throw new WorkflowError("backend must be InMemoryBackend or PostgresBackend");
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
        pausedBy: raw.cancelledBy,
      };
    default:
      return { status: raw.status as "in_progress" };
  }
}
