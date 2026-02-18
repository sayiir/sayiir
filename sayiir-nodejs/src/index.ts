/**
 * Sayiir — Durable workflow engine for Node.js.
 *
 * All orchestration runs in Rust. JavaScript provides task implementations.
 */

// Types
export type {
  Duration,
  RetryPolicy,
  TaskCallback,
  TaskOptions,
  StepOptions,
  WorkflowStatus,
  ZodLike,
} from "./types.js";

export { WorkflowError, TaskError, BackendError } from "./types.js";

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

// Executor
export {
  runWorkflow,
  runWorkflowSync,
  runDurableWorkflow,
  resumeWorkflow,
  cancelWorkflow,
  pauseWorkflow,
  unpauseWorkflow,
  sendSignal,
  InMemoryBackend,
  PostgresBackend,
} from "./executor.js";
export type { Backend, DurableRunOptions } from "./executor.js";

// Duration utility
export { parseDuration } from "./duration.js";

// Worker
export { Worker, WorkerHandle } from "./worker.js";
export type { WorkerOptions } from "./worker.js";
