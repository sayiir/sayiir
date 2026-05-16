/**
 * Backend interfaces for the Sayiir flow builder.
 *
 * These abstract over the concrete builder implementation (NAPI-RS, WASM, etc.)
 * so the pure-TypeScript DSL can be used without native dependencies.
 */

export interface RetryPolicyConfig {
  maxRetries: number;
  initialDelaySecs: number;
  backoffMultiplier: number;
  maxDelaySecs?: number;
}

export interface TaskMetadata {
  displayName?: string;
  description?: string;
  timeoutSecs?: number;
  retries?: RetryPolicyConfig;
  tags?: string[];
  version?: string;
  priority?: number;
}

export interface BranchTask {
  taskId: string;
  metadata?: TaskMetadata;
}

export interface BranchEntry {
  key: string;
  tasks: BranchTask[];
}

/** The kind of node in a workflow DAG. */
export type NodeKind = "task" | "fork" | "delay" | "await_signal" | "branch" | "loop" | "child_workflow";

/** Metadata about a single node in the workflow DAG. */
export interface NodeInfo {
  /** Unique node identifier. */
  id: string;
  /** Node kind. */
  kind: NodeKind;
  /** ID of the preceding node, or `undefined` for the root. */
  predecessorId?: string;
  /** Timeout in seconds (task timeout, delay duration, or signal timeout). */
  timeoutSecs?: number;
  /** Retry policy (tasks only). */
  retryPolicy?: RetryPolicyConfig;
  /** Execution priority 1–5 (tasks only). */
  priority?: number;
}

export interface CompiledWorkflow {
  workflowId: string;
  definitionHash: string;
  metadataJson?: string;
  /** Return all nodes in topological (execution) order. */
  iterNodes(): NodeInfo[];
}

export interface FlowBuilderBackend {
  nextLambdaId(): string;
  then(taskId: string, metadata?: TaskMetadata): void;
  addFork(
    branches: BranchTask[][],
    joinId: string,
    joinMetadata?: TaskMetadata,
  ): void;
  addBranch(
    branches: BranchEntry[],
    defaultBranch?: BranchTask[],
  ): string;
  waitForSignal(
    signalId: string,
    signalName: string,
    timeoutSecs?: number,
  ): void;
  delay(delayId: string, seconds: number): void;
  addLoop(
    bodyTaskId: string,
    bodyMetadata?: TaskMetadata,
    maxIterations?: number,
    onMax?: string,
  ): string;
  addChildWorkflow(childId: string, childBuilder: FlowBuilderBackend): void;
  setMetadataJson(json: string): void;
  build(): CompiledWorkflow;
}
