/**
 * Workflow execution utilities.
 *
 * Two execution paths:
 *   - `runWorkflow()` — async, uses a stepper pattern that yields control to
 *     JS between each task. Supports both sync and async tasks.
 *   - `runDurableWorkflow()` — sync, uses the native durable engine with
 *     checkpointing. Async tasks require the stepper-based approach (planned).
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

/** Conflict policy when an instance_id already exists. */
export type ConflictPolicy = "fail" | "useExisting" | "terminateExisting";

/** Options for running a workflow with durability. */
export interface DurableRunOptions {
  instanceId: string;
  backend: Backend;
  /** What to do when `instanceId` already has a snapshot. Default: `"fail"`. */
  conflictPolicy?: ConflictPolicy;
}

/**
 * Run a workflow to completion and return its output.
 *
 * When called without options, runs entirely in memory with no persistence
 * (fastest path for prototyping).
 *
 * When called **with** `{ instanceId, backend }`, runs with full
 * checkpointing and durability — but still returns the output directly
 * instead of a `WorkflowStatus` object. If the workflow does not complete
 * (e.g. it parks on a delay or signal), a `WorkflowError` is thrown.
 * Use `runDurableWorkflow()` when you need the full status object.
 *
 * @example
 * ```ts
 * // Prototype — no persistence
 * const result = await runWorkflow(wf, input);
 *
 * // Production — same function, just add options
 * const result = await runWorkflow(wf, input, {
 *   instanceId: "run-1",
 *   backend: PostgresBackend.connect(url),
 * });
 * ```
 */
export async function runWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  input: TIn,
  opts?: DurableRunOptions,
): Promise<TOut> {
  if (opts) {
    const status = runDurableWorkflow(workflow, opts.instanceId, input, opts.backend, opts.conflictPolicy);
    if (status.status !== "completed") {
      throw new WorkflowError(
        `Workflow did not complete (status=${status.status}). ` +
        `Use runDurableWorkflow() to inspect the full status.`,
      );
    }
    return status.output;
  }

  const native = getNative();
  const stepper = new native.NapiContinuationStepper(workflow._inner, input);
  let step = stepper.current();

  while (step.kind === "task") {
    const taskFn = workflow._taskRegistry[step.taskId!];
    if (!taskFn) {
      throw new WorkflowError(`Task '${step.taskId}' not found in registry`);
    }
    const taskInput = step.inputJson != null ? JSON.parse(step.inputJson) : undefined;
    const output = await taskFn(taskInput);
    step = stepper.submitResult(output);
  }

  if (step.kind === "done") {
    return (step.outputJson != null ? JSON.parse(step.outputJson) : undefined) as TOut;
  }

  throw new WorkflowError(`Unexpected step kind: ${step.kind}`);
}

/**
 * Run a workflow to completion synchronously (no persistence).
 *
 * All tasks must return plain values, not Promises. For async tasks,
 * use `runWorkflow()` (async) instead.
 */
export function runWorkflowSync<TIn, TOut>(
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
  conflictPolicy?: ConflictPolicy,
): WorkflowStatus<TOut> {
  const engine = createDurableEngine(backend, conflictPolicy);
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

function createDurableEngine(backend: Backend, conflictPolicy?: ConflictPolicy): NapiDurableEngine {
  const native = getNative();
  if (backend instanceof InMemoryBackend) {
    return native.NapiDurableEngine.withInMemory(backend._inner, conflictPolicy);
  }
  if (backend instanceof PostgresBackend) {
    return native.NapiDurableEngine.withPostgres(backend._inner, conflictPolicy);
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
      throw new WorkflowError(`unknown workflow status: ${raw.status}`);
  }
}
