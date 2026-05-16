/**
 * sayiir-flow-js — Pure TypeScript workflow builder DSL for Sayiir.
 *
 * This package contains only the builder logic with no native dependencies.
 * It is used by binding packages (sayiir-nodejs, sayiir-cloudflare, etc.)
 * that provide the concrete FlowBuilderBackend implementation.
 */

// Interfaces (backend abstraction)
export type {
  FlowBuilderBackend,
  CompiledWorkflow,
  NodeKind,
  NodeInfo,
  TaskMetadata,
  RetryPolicyConfig,
  BranchTask,
  BranchEntry,
} from "./interfaces.js";

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
  MaxIterationsPolicy,
} from "./types.js";

export { LoopResult, WorkflowError, TaskError, BackendError } from "./types.js";

// Task
export { task } from "./task.js";
export type { TaskFn } from "./task.js";

// Flow
export {
  branch,
  Flow,
  ForkBuilder,
  RouteBuilder,
  Workflow,
  createFlowFactory,
} from "./flow.js";
export type {
  BranchDef,
  BranchEnvelope,
  FlowOptions,
  InferBranchOutputs,
  BuilderFactory,
} from "./flow.js";

// Duration utility
export { parseDuration } from "./duration.js";
