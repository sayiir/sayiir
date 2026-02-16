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
// eslint-disable-next-line @typescript-eslint/no-explicit-any
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

/** All possible workflow status values returned by the native layer. */
export type NativeWorkflowStatusKind =
  | "completed"
  | "in_progress"
  | "failed"
  | "cancelled"
  | "paused"
  | "waiting"
  | "awaiting_signal";

/** Internal type for native addon workflow status. */
export interface NativeWorkflowStatus {
  status: NativeWorkflowStatusKind;
  error?: string;
  reason?: string;
  cancelledBy?: string;
  pausedBy?: string;
  outputJson?: string;
  wakeAt?: string;
  delayId?: string;
  signalId?: string;
  signalName?: string;
}
