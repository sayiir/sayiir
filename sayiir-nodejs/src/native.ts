/**
 * Type declarations for the native addon (sayiir-node).
 *
 * These types mirror the #[napi] exports from the Rust crate.
 * The actual native module is loaded at runtime.
 */

import type { NativeWorkflowStatus } from "./types.js";

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
}

export interface NapiBranchTask {
  taskId: string;
  metadata?: NapiTaskMetadata;
}

export interface NapiFlowBuilder {
  then(taskId: string, metadata?: NapiTaskMetadata): void;
  addFork(
    branches: NapiBranchTask[][],
    joinId: string,
    joinMetadata?: NapiTaskMetadata,
  ): void;
  waitForSignal(
    signalId: string,
    signalName: string,
    timeoutSecs?: number,
  ): void;
  delay(delayId: string, seconds: number): void;
  build(): NapiWorkflow;
}

export interface NapiWorkflow {
  workflowId: string;
  definitionHash: string;
}

export interface NapiWorkflowEngine {
  run(
    workflow: NapiWorkflow,
    input: unknown,
    taskRegistry: Record<string, Function>,
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
    taskRegistry: Record<string, Function>,
  ): NativeWorkflowStatus;
  resume(
    workflow: NapiWorkflow,
    instanceId: string,
    taskRegistry: Record<string, Function>,
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

export interface NativeAddon {
  NapiFlowBuilder: new (name: string) => NapiFlowBuilder;
  NapiWorkflowEngine: new () => NapiWorkflowEngine;
  NapiContinuationStepper: new (
    workflow: NapiWorkflow,
    input: unknown,
  ) => NapiContinuationStepper;
  NapiInMemoryBackend: new () => NapiInMemoryBackend;
  NapiPostgresBackend: { connect(url: string): NapiPostgresBackend };
  NapiDurableEngine: {
    withInMemory(backend: NapiInMemoryBackend): NapiDurableEngine;
    withPostgres(backend: NapiPostgresBackend): NapiDurableEngine;
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
