/**
 * Sayiir — Durable workflow engine.
 *
 * Ambient module declaration for Monaco editor intellisense in the playground.
 *
 * Generated from sayiir-nodejs source — keep in sync when public types change.
 */

declare module "sayiir" {
  // ── Duration ──

  /** Milliseconds (number) or human-readable string ("30s", "5m", "1h"). */
  type Duration = string | number;

  // ── Retry ──

  interface RetryPolicy {
    maxAttempts: number;
    initialDelay: Duration;
    backoffMultiplier?: number;
    maxDelay?: Duration;
  }

  // ── Task options ──

  interface TaskOptions<TIn = any, TOut = unknown> {
    timeout?: Duration;
    retries?: number;
    retry?: RetryPolicy;
    description?: string;
    tags?: string[];
    priority?: number;
    input?: ZodLike<TIn>;
    output?: ZodLike<TOut>;
  }

  interface StepOptions {
    timeout?: Duration;
    retries?: number;
    retry?: RetryPolicy;
  }

  interface ZodLike<T = any> {
    parse(data: unknown): T;
    _output: T;
  }

  // ── Task ──

  interface TaskFn<TIn = any, TOut = any> {
    (input: TIn): TOut | Promise<TOut>;
    readonly _taskId: string;
  }

  /**
   * Define a named task.
   *
   * ```ts
   * const getUser = task("get-user", async (id) => db.getUser(id), {
   *   timeout: "30s",
   *   retries: 3,
   * });
   * ```
   */
  function task<TIn, TOut>(
    id: string,
    fn: (input: TIn) => TOut | Promise<TOut>,
    opts?: TaskOptions<TIn, TOut>,
  ): TaskFn<TIn, TOut>;

  interface TaskExecutionContext {
    workflowId: string;
    instanceId: string;
    taskId: string;
    metadata: {
      displayName?: string;
      description?: string;
      timeoutSecs?: number;
      tags?: string[];
      version?: string;
    };
    workflowMetadata?: Record<string, unknown> | null;
  }

  /** Get the current task execution context (null outside a task). */
  function getTaskContext(): TaskExecutionContext | null;

  // ── Workflow status ──

  type WorkflowStatus<TOut = unknown> =
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

  // ── Errors ──

  class WorkflowError extends Error {}
  class TaskError extends WorkflowError {}
  class BackendError extends WorkflowError {}

  // ── Loop ──

  type LoopResult<T> = { _loop: "again"; value: T } | { _loop: "done"; value: T };
  const LoopResult: {
    again<T>(value: T): LoopResult<T>;
    done<T>(value: T): LoopResult<T>;
  };

  type MaxIterationsPolicy = "fail" | "exit_with_last";

  interface LoopOptions {
    /** Maximum number of iterations (default: 10). */
    maxIterations?: number;
    /** What to do when max iterations is reached (default: "fail"). */
    onMax?: MaxIterationsPolicy;
  }

  // ── Branch ──

  interface BranchDef<TIn = any, TOut = any> {
    readonly name: string;
  }

  interface BranchEnvelope<T> {
    branch: string;
    result: T;
  }

  type InferBranchOutputs<T extends readonly BranchDef<any, any>[]> = {
    [K in keyof T]: T[K] extends BranchDef<any, infer O> ? O : never;
  };

  /**
   * Create a branch for `.fork()`.
   *
   * ```ts
   * flow("process")
   *   .fork([branch("email", sendEmail), branch("ship", shipOrder)])
   *   .join("merge", ([email, ship]) => ({ email, ship }))
   *   .build();
   * ```
   */
  function branch<TIn, TOut>(
    name: string,
    fn: TaskFn<TIn, TOut> | ((input: TIn) => TOut | Promise<TOut>),
  ): BranchDef<TIn, Awaited<TOut>>;

  // ── Flow builder ──

  interface FlowOptions {
    metadata?: Record<string, unknown>;
  }

  class Flow<TInput, TLast = TInput> {
    then<TOut>(fn: TaskFn<TLast, TOut>): Flow<TInput, Awaited<TOut>>;
    then<TOut>(
      id: string,
      fn: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
      opts?: StepOptions,
    ): Flow<TInput, Awaited<TOut>>;

    fork<TBranches extends readonly BranchDef<TLast, any>[]>(
      branches: [...TBranches],
    ): ForkBuilder<TInput, TLast, TBranches>;

    route<const TKeys extends readonly string[]>(
      keyFn:
        | TaskFn<TLast, TKeys[number]>
        | ((input: TLast) => TKeys[number] | Promise<TKeys[number]>),
      keys: TKeys,
    ): RouteBuilder<TInput, TLast, never, TKeys[number]>;

    delay(id: string, duration: Duration): Flow<TInput, TLast>;

    waitForSignal<TSignal = unknown>(
      id: string,
      signalName: string,
      opts?: { timeout?: Duration },
    ): Flow<TInput, TSignal>;

    loop<TOut>(
      fn: TaskFn<TLast, LoopResult<TOut>>,
      opts?: LoopOptions,
    ): Flow<TInput, TOut>;
    loop<TOut>(
      id: string,
      fn:
        | ((input: TLast) => LoopResult<TOut> | Promise<LoopResult<TOut>>)
        | TaskFn<TLast, LoopResult<TOut>>,
      opts?: LoopOptions,
    ): Flow<TInput, TOut>;

    thenFlow<TOut>(workflow: Workflow<TLast, TOut>): Flow<TInput, Awaited<TOut>>;

    build(): Workflow<TInput, TLast>;
  }

  class ForkBuilder<TInput, TLast, TBranches extends readonly BranchDef<TLast, any>[]> {
    join<TOut>(
      id: string,
      fn: (branches: InferBranchOutputs<TBranches>) => TOut | Promise<TOut>,
    ): Flow<TInput, Awaited<TOut>>;
  }

  class RouteBuilder<TInput, TLast, TBranchOut = never, TKey extends string = string> {
    branch<TOut>(
      key: TKey,
      fn: TaskFn<TLast, TOut> | ((input: TLast) => TOut | Promise<TOut>),
    ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;
    branch<TOut>(
      key: TKey,
      id: string,
      fn: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
    ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;

    defaultBranch<TOut>(
      fn: TaskFn<TLast, TOut> | ((input: TLast) => TOut | Promise<TOut>),
    ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;
    defaultBranch<TOut>(
      id: string,
      fn: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
    ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;

    done(): Flow<TInput, BranchEnvelope<TBranchOut>>;
  }

  /** The kind of node in a workflow DAG. */
  type NodeKind = "task" | "fork" | "delay" | "await_signal" | "branch" | "loop" | "child_workflow";

  /** Metadata about a single node in the workflow DAG. */
  interface NodeInfo {
    /** Unique node identifier. */
    id: string;
    /** Node kind. */
    kind: NodeKind;
    /** ID of the preceding node, or `undefined` for the root. */
    predecessorId?: string;
    /** Timeout in seconds (task timeout, delay duration, or signal timeout). */
    timeoutSecs?: number;
    /** Retry policy (tasks only). */
    retryPolicy?: RetryPolicy;
    /** Execution priority 1–5 (tasks only). */
    priority?: number;
  }

  class Workflow<TIn, TOut> {
    readonly workflowId: string;
    readonly definitionHash: string;
    readonly metadata?: Record<string, unknown>;

    /**
     * Return all nodes in topological (execution) order.
     *
     * Each `NodeInfo` carries the node's ID, kind, predecessor,
     * and any configured timeout / retry / priority metadata.
     */
    iterNodes(): NodeInfo[];
  }

  /** Create a new flow builder. */
  function flow<TInput>(name: string, opts?: FlowOptions): Flow<TInput>;

  // ── Execution ──

  /** Run a workflow and return the output (async, supports both sync and async tasks). */
  function runWorkflow<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    input: TIn,
    opts?: DurableRunOptions,
  ): Promise<TOut>;

  /** Run a workflow synchronously (all tasks must be sync). */
  function runWorkflowSync<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    input: TIn,
  ): TOut;

  /** Run with checkpointing and durability. Returns WorkflowStatus. */
  function runDurableWorkflow<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    instanceId: string,
    input: TIn,
    backend: Backend,
    conflictPolicy?: ConflictPolicy,
  ): WorkflowStatus<TOut>;

  /** Resume a workflow from its last checkpoint. */
  function resumeWorkflow<TIn, TOut>(
    workflow: Workflow<TIn, TOut>,
    instanceId: string,
    backend: Backend,
  ): WorkflowStatus<TOut>;

  /** Cancel a running workflow. */
  function cancelWorkflow(
    instanceId: string,
    backend: Backend,
    opts?: { reason?: string; cancelledBy?: string },
  ): void;

  /** Pause a running workflow. */
  function pauseWorkflow(
    instanceId: string,
    backend: Backend,
    opts?: { reason?: string; pausedBy?: string },
  ): void;

  /** Unpause a paused workflow. */
  function unpauseWorkflow(instanceId: string, backend: Backend): void;

  /** Send an external signal to a workflow instance. */
  function sendSignal(
    instanceId: string,
    signalName: string,
    payload: unknown,
    backend: Backend,
  ): void;

  /** Conflict policy when an `instanceId` already exists. */
  type ConflictPolicy = "fail" | "useExisting" | "terminateExisting";

  interface DurableRunOptions {
    instanceId: string;
    backend: Backend;
    /** What to do when `instanceId` already has a snapshot. Default: `"fail"`. */
    conflictPolicy?: ConflictPolicy;
  }

  // ── Backends ──

  type Backend = InMemoryBackend | PostgresBackend;

  /** In-memory backend for testing and development. */
  class InMemoryBackend {
    constructor();
  }

  /** PostgreSQL backend for production. */
  class PostgresBackend {
    static connect(url: string): PostgresBackend;
  }

  // ── Duration utility ──

  function parseDuration(input: Duration): number;

  // ── Telemetry ──

  /** Initialize the tracing subscriber (idempotent). */
  function initTracing(): void;

  /** Flush and shut down the OpenTelemetry tracer provider. */
  function shutdownTracing(): void;

  // ── Worker ──

  interface WorkerOptions {
    pollInterval?: Duration;
    claimTtl?: Duration;
  }

  class Worker {
    readonly workerId: string;
    readonly backend: Backend;
    readonly workflows: readonly Workflow<any, any>[];
    readonly options: WorkerOptions;

    constructor(
      workerId: string,
      backend: Backend,
      workflows: readonly Workflow<any, any>[],
      opts?: WorkerOptions,
    );
    start(): WorkerHandle;
  }

  class WorkerHandle {
    shutdown(): void;
  }

  interface WorkflowClientOptions {
    conflictPolicy?: ConflictPolicy;
  }

  class WorkflowClient {
    constructor(backend: Backend, opts?: WorkflowClientOptions);
    submit<TIn, TOut>(
      workflow: Workflow<TIn, TOut>,
      instanceId: string,
      input: TIn,
    ): WorkflowStatus<TOut>;
    cancel(
      instanceId: string,
      opts?: { reason?: string; cancelledBy?: string },
    ): void;
    pause(
      instanceId: string,
      opts?: { reason?: string; pausedBy?: string },
    ): void;
    unpause(instanceId: string): void;
    sendSignal(instanceId: string, signalName: string, payload: unknown): void;
    status<TOut = unknown>(instanceId: string): WorkflowStatus<TOut>;
    getTaskResult(instanceId: string, taskId: string): string | null;
  }
}
