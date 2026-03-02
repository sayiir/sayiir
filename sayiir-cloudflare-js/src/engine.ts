/**
 * Durable workflow engine for Cloudflare Workers over D1.
 *
 * Wraps the WASM `WasmDurableEngine` and `WasmContinuationStepper` with a
 * typed TypeScript API. All operations are async (D1 is async).
 *
 * Two execution paths:
 *   - `runWorkflow()` — stepper-based, no persistence (prototyping/testing)
 *   - `Engine` class — durable with D1 checkpointing (production)
 */

import type { Workflow } from "sayiir-flow-js";
import type { WorkflowStatus } from "./types.js";
import { WorkflowError } from "./types.js";

import {
  WasmDurableEngine,
  WasmContinuationStepper,
  type WasmWorkflow,
  type WasmWorkflowStatus,
} from "../wasm/sayiir_cloudflare.js";

/** D1 database binding type (from Cloudflare Workers runtime). */
export interface D1Database {
  prepare(query: string): D1PreparedStatement;
  batch<T = unknown>(statements: D1PreparedStatement[]): Promise<D1Result<T>[]>;
  exec(query: string): Promise<D1ExecResult>;
}

interface D1PreparedStatement {
  bind(...values: unknown[]): D1PreparedStatement;
  first<T = unknown>(colName?: string): Promise<T | null>;
  run<T = unknown>(): Promise<D1Result<T>>;
  all<T = unknown>(): Promise<D1Result<T>>;
  raw<T = unknown>(): Promise<T[]>;
}

interface D1Result<T = unknown> {
  results?: T[];
  success: boolean;
  error?: string;
  meta?: Record<string, unknown>;
}

interface D1ExecResult {
  count: number;
  duration: number;
}

/** Options for durable run/resume. */
export interface DurableRunOptions {
  instanceId: string;
}

/** Durable workflow engine backed by Cloudflare D1. */
export class Engine {
  /** @internal */
  private readonly _inner: WasmDurableEngine;
  /** @internal */
  private readonly _db: D1Database;

  private constructor(inner: WasmDurableEngine, db: D1Database) {
    this._inner = inner;
    this._db = db;
  }

  /**
   * Create an engine backed by a D1 database.
   *
   * Call once at startup and reuse across requests.
   */
  static async create(db: D1Database): Promise<Engine> {
    const inner = await WasmDurableEngine.create(db);
    return new Engine(inner, db);
  }

  /** Run a workflow to completion (or until it parks) with checkpointing. */
  async run<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    instanceId: string,
    input: TIn,
  ): Promise<WorkflowStatus<TOut>> {
    const raw = await this._inner.run(
      workflow._inner as unknown as WasmWorkflow,
      instanceId,
      input,
      workflow._taskRegistry,
    );
    return parseWorkflowStatus<TOut>(raw);
  }

  /** Resume a workflow from a saved checkpoint. */
  async resume<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    instanceId: string,
  ): Promise<WorkflowStatus<TOut>> {
    const raw = await this._inner.resume(
      workflow._inner as unknown as WasmWorkflow,
      instanceId,
      workflow._taskRegistry,
    );
    return parseWorkflowStatus<TOut>(raw);
  }

  /** Request cancellation of a running workflow. */
  async cancel(
    instanceId: string,
    opts?: { reason?: string; cancelledBy?: string },
  ): Promise<void> {
    await this._inner.cancel(instanceId, opts?.reason, opts?.cancelledBy);
  }

  /** Request pausing of a running workflow. */
  async pause(
    instanceId: string,
    opts?: { reason?: string; pausedBy?: string },
  ): Promise<void> {
    await this._inner.pause(instanceId, opts?.reason, opts?.pausedBy);
  }

  /** Unpause a paused workflow so it can be resumed. */
  async unpause(instanceId: string): Promise<void> {
    await this._inner.unpause(instanceId);
  }

  /** Send an external signal to a workflow instance. */
  async sendSignal(
    instanceId: string,
    signalName: string,
    payload: unknown,
  ): Promise<void> {
    await this._inner.sendSignal(instanceId, signalName, payload);
  }

  /**
   * Find and resume workflow instances that are ready or stuck.
   *
   * Picks up two categories in a single pass:
   *   - **Ready** — parked at a delay or signal whose wake time has passed.
   *   - **Stale** — not parked, but not updated within `staleAfter` seconds
   *     (recovers from Worker eviction / CPU-limit kills).
   *
   * Returns the statuses of all resumed instances. Use `limit` to stay
   * within Worker CPU budgets — remaining instances are picked up on the
   * next cron tick.
   *
   * @param workflow  The workflow definition to resume with.
   * @param opts.staleAfter  Seconds before a non-parked instance is
   *                         considered stuck (default: 300 — 5 min).
   * @param opts.limit  Maximum instances to resume per call (default: 10).
   *
   * @example
   * ```ts
   * async scheduled(event: ScheduledEvent, env: Env) {
   *   const engine = await Engine.create(env.DB);
   *   await engine.resumeAll(myWorkflow);
   * }
   * ```
   */
  async resumeAll<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    opts?: { staleAfter?: number; limit?: number },
  ): Promise<WorkflowStatus<TOut>[]> {
    const staleAfter = opts?.staleAfter ?? 300;
    const limit = opts?.limit ?? 10;
    const { results } = await this._db.prepare(
      `SELECT instance_id FROM sayiir_workflow_snapshots
       WHERE status = 'in_progress'
         AND (
           (delay_wake_at IS NOT NULL AND delay_wake_at <= datetime('now'))
           OR
           (delay_wake_at IS NULL AND updated_at <= datetime('now', '-' || ? || ' seconds'))
         )
       ORDER BY updated_at ASC
       LIMIT ?`,
    ).bind(staleAfter, limit).all<{ instance_id: string }>();

    const statuses: WorkflowStatus<TOut>[] = [];
    for (const row of results ?? []) {
      statuses.push(await this.resume(workflow, row.instance_id));
    }
    return statuses;
  }
}

/**
 * Run a workflow to completion and return its output (no persistence).
 *
 * Uses the WASM continuation stepper. Supports async tasks.
 * For durable execution with checkpointing, use `Engine` instead.
 *
 * When called with `opts`, uses the durable engine. If the workflow does not
 * complete (e.g. it parks on a delay or signal), a `WorkflowError` is thrown.
 *
 * @example
 * ```ts
 * // Prototype — no persistence
 * const result = await runWorkflow(wf, input);
 *
 * // Production — same function, just add options
 * const engine = await Engine.create(env.DB);
 * const status = await engine.run(wf, "run-1", input);
 * ```
 */
export async function runWorkflow<TIn, TOut>(
  workflow: Workflow<TIn, TOut>,
  input: TIn,
  opts?: DurableRunOptions & { engine: Engine },
): Promise<TOut> {
  if (opts) {
    const status = await opts.engine.run(workflow, opts.instanceId, input);
    if (status.status !== "completed") {
      throw new WorkflowError(
        `Workflow did not complete (status=${status.status}). ` +
          `Use engine.run() to inspect the full status.`,
      );
    }
    return status.output;
  }

  const stepper = new WasmContinuationStepper(workflow._inner as unknown as WasmWorkflow, input);
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

// ---- Internal helpers ----

function parseWorkflowStatus<TOut>(
  raw: WasmWorkflowStatus,
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
