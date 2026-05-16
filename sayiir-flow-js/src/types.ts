/**
 * Core type definitions for the Sayiir workflow engine.
 */

/** Duration as milliseconds (number) or human-readable string parsed by `ms` (e.g. "30s", "5m", "1h"). */
export type Duration = string | number;

/** Retry policy for task execution. */
export interface RetryPolicy {
  maxAttempts: number;
  initialDelay: Duration;
  backoffMultiplier?: number;
  maxDelay?: Duration;
}

/** Task options for configuring step behavior. */
export interface TaskOptions<TIn = any, TOut = unknown> {
  timeout?: Duration;
  retries?: number;
  retry?: RetryPolicy;
  description?: string;
  tags?: string[];
  priority?: number;
  input?: ZodLike<TIn>;
  output?: ZodLike<TOut>;
}

/** Step options for inline `.then()` steps. */
export interface StepOptions {
  timeout?: Duration;
  retries?: number;
  retry?: RetryPolicy;
}

/**
 * A task callback: receives input data and returns output (sync or async).
 *
 * Used internally in registries and native bindings wherever a
 * task function reference is stored.
 */
export type TaskCallback = (input: any) => unknown | Promise<unknown>;

/** Minimal Zod-like schema interface (avoids hard dependency). */
export interface ZodLike<T = any> {
  parse(data: unknown): T;
  _output: T;
}

/**
 * Discriminated union for workflow status.
 * Enables TypeScript narrowing: `if (status.status === "completed") { status.output }`
 */
export type WorkflowStatus<TOut = unknown> =
  | { status: "completed"; output: TOut }
  | { status: "in_progress" }
  | { status: "failed"; error: string }
  | { status: "cancelled"; reason?: string; cancelledBy?: string }
  | { status: "paused"; reason?: string; pausedBy?: string }
  | { status: "waiting"; wakeAt: string; delayId: string }
  | {
      status: "awaiting_signal";
      signalId: string;
      signalName: string;
      wakeAt?: string;
    };

/** Error classes */
export class WorkflowError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WorkflowError";
  }
}

export class TaskError extends WorkflowError {
  constructor(message: string) {
    super(message);
    this.name = "TaskError";
  }
}

export class BackendError extends WorkflowError {
  constructor(message: string) {
    super(message);
    this.name = "BackendError";
  }
}

/**
 * Result from a loop body task.
 *
 * Return `LoopResult.again(value)` to continue iterating,
 * or `LoopResult.done(value)` to exit the loop.
 *
 * Serializes as `{ _loop: "again"|"done", value: ... }` matching the
 * Rust `LoopResult<T>` serde format.
 */
export type LoopResult<T> = { _loop: "again"; value: T } | { _loop: "done"; value: T };

/** Factory functions for creating `LoopResult` values. */
export const LoopResult = {
  /** Continue the loop with a new value. */
  again<T>(value: T): LoopResult<T> {
    return { _loop: "again", value };
  },
  /** Exit the loop with a final value. */
  done<T>(value: T): LoopResult<T> {
    return { _loop: "done", value };
  },
} as const;

/** Policy for when max iterations is reached. */
export type MaxIterationsPolicy = "fail" | "exit_with_last";

/** Options for a loop step. */
export interface LoopOptions {
  /** Maximum number of iterations (default: 10). */
  maxIterations?: number;
  /** What to do when max iterations is reached (default: "fail"). */
  onMax?: MaxIterationsPolicy;
}
