/**
 * Core type definitions for the Sayiir Cloudflare Workers engine.
 *
 * Pure types are re-exported from sayiir-flow-js.
 * WASM-specific types are defined here.
 */

// Re-export all pure types from sayiir-flow-js
export type {
  Duration,
  LoopOptions,
  RetryPolicy,
  TaskCallback,
  TaskOptions,
  StepOptions,
  WorkflowStatus,
  ZodLike,
  MaxIterationsPolicy,
} from "sayiir-flow-js";

export { LoopResult, WorkflowError, TaskError, BackendError } from "sayiir-flow-js";

/** All possible workflow status values returned by the WASM layer. */
export type WasmWorkflowStatusKind =
  | "completed"
  | "in_progress"
  | "failed"
  | "cancelled"
  | "paused"
  | "waiting"
  | "awaiting_signal";

/** Internal type for WASM workflow status (matches WasmWorkflowStatus getters). */
export interface WasmWorkflowStatusRaw {
  status: WasmWorkflowStatusKind;
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
