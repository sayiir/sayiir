/**
 * Type declarations for the native addon (sayiir-node).
 *
 * These types mirror the #[napi] exports from the Rust crate.
 * The actual native module is loaded at runtime.
 */

import type { NodeKind, NativeWorkflowStatus, TaskCallback } from "./types.js";

export interface NapiRetryPolicy {
  maxRetries: number;
  initialDelaySecs: number;
  backoffMultiplier: number;
  maxDelaySecs?: number;
}

export interface NapiTaskMetadata {
  displayName?: string;
  description?: string;
  timeoutSecs?: number;
  retries?: NapiRetryPolicy;
  tags?: string[];
  version?: string;
}

export interface NapiTaskExecutionContext {
  workflowId: string;
  instanceId: string;
  taskId: string;
  metadata: NapiTaskMetadata;
  workflowMetadata?: Record<string, unknown> | null;
}

export interface NapiBranchTask {
  taskId: string;
  metadata?: NapiTaskMetadata;
}

export interface NapiBranchEntry {
  key: string;
  tasks: NapiBranchTask[];
}

export interface NapiFlowBuilder {
  nextLambdaId(): string;
  then(taskId: string, metadata?: NapiTaskMetadata): void;
  addFork(
    branches: NapiBranchTask[][],
    joinId: string,
    joinMetadata?: NapiTaskMetadata,
  ): void;
  addBranch(
    branches: NapiBranchEntry[],
    defaultBranch?: NapiBranchTask[],
  ): string;
  waitForSignal(
    signalId: string,
    signalName: string,
    timeoutSecs?: number,
  ): void;
  delay(delayId: string, seconds: number): void;
  addLoop(
    bodyTaskId: string,
    bodyMetadata?: NapiTaskMetadata,
    maxIterations?: number,
    onMax?: string,
  ): string;
  addChildWorkflow(childId: string, childBuilder: NapiFlowBuilder): void;
  setMetadataJson(json: string): void;
  build(): NapiWorkflow;
}

export interface NapiNodeInfo {
  id: string;
  kind: NodeKind;
  predecessorId?: string;
  timeoutSecs?: number;
  retryPolicy?: {
    maxRetries: number;
    initialDelaySecs: number;
    backoffMultiplier: number;
    maxDelaySecs?: number;
  };
  priority?: number;
}

export interface NapiWorkflow {
  workflowId: string;
  definitionHash: string;
  metadataJson?: string;
  iterNodes(): NapiNodeInfo[];
}

export interface NapiWorkflowEngine {
  run(
    workflow: NapiWorkflow,
    input: unknown,
    taskRegistry: Record<string, TaskCallback>,
  ): unknown;
}

export interface NapiStepResult {
  kind: "task" | "done";
  taskId?: string;
  inputJson?: string;
  outputJson?: string;
}

export interface NapiContinuationStepper {
  current(): NapiStepResult;
  submitResult(output: unknown): NapiStepResult;
}

export interface NapiInMemoryBackend {}

export interface NapiPostgresBackend {}

export interface NapiDurableEngine {
  run(
    workflow: NapiWorkflow,
    instanceId: string,
    input: unknown,
    taskRegistry: Record<string, TaskCallback>,
  ): NativeWorkflowStatus;
  resume(
    workflow: NapiWorkflow,
    instanceId: string,
    taskRegistry: Record<string, TaskCallback>,
  ): NativeWorkflowStatus;
  cancel(
    instanceId: string,
    reason?: string,
    cancelledBy?: string,
  ): void;
  pause(
    instanceId: string,
    reason?: string,
    pausedBy?: string,
  ): void;
  sendSignal(
    instanceId: string,
    signalName: string,
    payload: unknown,
  ): void;
  unpause(instanceId: string): void;
}

export interface NapiWorker {
  start(
    workflows: NapiWorkflow[],
    taskExecutor: (payload: string) => Promise<string>,
  ): NapiWorkerHandle;
}

export interface NapiWorkerHandle {
  shutdown(): void;
}

export interface NapiWorkflowClient {
  submit(
    workflow: NapiWorkflow,
    instanceId: string,
    input: unknown,
  ): NativeWorkflowStatus;
  cancel(
    instanceId: string,
    reason?: string,
    cancelledBy?: string,
  ): void;
  pause(
    instanceId: string,
    reason?: string,
    pausedBy?: string,
  ): void;
  unpause(instanceId: string): void;
  sendSignal(
    instanceId: string,
    signalName: string,
    payloadJson: string,
  ): void;
  status(instanceId: string): NativeWorkflowStatus;
  getTaskResult(instanceId: string, taskId: string): string | null;
}

export interface NativeAddon {
  initTracing(): void;
  shutdownTracing(): void;
  getTaskContext(): NapiTaskExecutionContext | null;
  NapiFlowBuilder: new (name: string) => NapiFlowBuilder;
  NapiWorkflowEngine: new () => NapiWorkflowEngine;
  NapiContinuationStepper: new (
    workflow: NapiWorkflow,
    input: unknown,
  ) => NapiContinuationStepper;
  NapiInMemoryBackend: new () => NapiInMemoryBackend;
  NapiPostgresBackend: { connect(url: string): NapiPostgresBackend };
  NapiDurableEngine: {
    withInMemory(backend: NapiInMemoryBackend, conflictPolicy?: string): NapiDurableEngine;
    withPostgres(backend: NapiPostgresBackend, conflictPolicy?: string): NapiDurableEngine;
  };
  NapiWorker: {
    withInMemory(
      workerId: string,
      backend: NapiInMemoryBackend,
      pollIntervalMs?: number,
      claimTtlMs?: number,
      tags?: string[],
    ): NapiWorker;
    withPostgres(
      workerId: string,
      backend: NapiPostgresBackend,
      pollIntervalMs?: number,
      claimTtlMs?: number,
      tags?: string[],
    ): NapiWorker;
  };
  NapiWorkflowClient: {
    withInMemory(backend: NapiInMemoryBackend, conflictPolicy?: string): NapiWorkflowClient;
    withPostgres(backend: NapiPostgresBackend, conflictPolicy?: string): NapiWorkflowClient;
  };
}

// Platform package names keyed by `${process.platform}-${process.arch}`.
const PLATFORM_PACKAGES: Record<string, string> = {
  "linux-x64": "@sayiir/node-linux-x64-gnu",
  "linux-arm64": "@sayiir/node-linux-arm64-gnu",
  "darwin-x64": "@sayiir/node-darwin-x64",
  "darwin-arm64": "@sayiir/node-darwin-arm64",
  "win32-x64": "@sayiir/node-win32-x64-msvc",
};

// The native addon is loaded lazily to support environments where
// it may not be available (e.g., type-checking only).
let _native: NativeAddon | undefined;

export function getNative(): NativeAddon {
  if (!_native) {
    _native = loadNative();
  }
  return _native;
}

function loadNative(): NativeAddon {
  // 1. Try the platform-specific npm package (installed via optionalDependencies).
  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORM_PACKAGES[key];
  if (pkg) {
    try {
      return require(pkg) as NativeAddon;
    } catch {
      // Platform package not installed — fall through.
    }
  }

  // 2. Fall back to local native/ directory (development builds).
  try {
    return require("../native/sayiir-node.node") as NativeAddon;
  } catch {
    // Both paths failed.
  }

  throw new Error(
    `Sayiir: no native binary found for ${key}. ` +
      `Ensure the package is installed correctly (pnpm add sayiir).`,
  );
}
