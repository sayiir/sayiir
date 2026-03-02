/**
 * Sayiir — Durable workflow engine for Cloudflare Workers.
 *
 * All orchestration runs in WASM. JavaScript provides task implementations.
 */

// Types
export type {
  Duration,
  LoopOptions,
  RetryPolicy,
  TaskCallback,
  TaskOptions,
  StepOptions,
  WorkflowStatus,
  ZodLike,
} from "./types.js";

export { LoopResult, WorkflowError, TaskError, BackendError } from "./types.js";

// Task
export { task } from "./task.js";
export type { TaskFn } from "./task.js";

// Flow
export {
  flow,
  branch,
  Flow,
  ForkBuilder,
  RouteBuilder,
  Workflow,
} from "./flow.js";
export type { BranchDef, BranchEnvelope, FlowOptions, InferBranchOutputs } from "./flow.js";

// Engine
export { Engine, runWorkflow } from "./engine.js";
export type { D1Database, DurableRunOptions } from "./engine.js";

// Duration utility
export { parseDuration } from "./duration.js";
