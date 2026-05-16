/**
 * Core type definitions for the Sayiir workflow engine.
 *
 * Pure types are re-exported from sayiir-flow-js.
 * Native-specific types are defined here.
 */

// Re-export all pure types from sayiir-flow-js
export type {
  Duration,
  LoopOptions,
  NodeKind,
  NodeInfo,
  RetryPolicy,
  TaskCallback,
  TaskOptions,
  StepOptions,
  WorkflowStatus,
  ZodLike,
  MaxIterationsPolicy,
} from "sayiir-flow-js";

export { LoopResult, WorkflowError, TaskError, BackendError } from "sayiir-flow-js";

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
