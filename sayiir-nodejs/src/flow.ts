/**
 * Type-safe flow builder for constructing workflows.
 *
 * The builder tracks input/output types through the chain using generics,
 * providing full type inference without manual annotations.
 *
 * ```ts
 * const wf = flow<number>("welcome")
 *   .then("fetch", (id) => getUser(id))       // id: number -> User
 *   .then("greet", (user) => `Hi ${user.name}`) // user: User -> string
 *   .build();                                    // Workflow<number, string>
 * ```
 */

import type { Duration, StepOptions, TaskCallback } from "./types.js";
import type { TaskFn } from "./task.js";
import { parseDuration } from "./duration.js";
import type {
  NapiBranchTask,
  NapiFlowBuilder,
  NapiTaskMetadata,
  NapiWorkflow,
} from "./native.js";
import { getNative } from "./native.js";

/**
 * Suffix convention for branch key-function task IDs.
 * Must match `sayiir_core::workflow::key_fn_id`.
 */
const KEY_FN_SUFFIX = "::key_fn";

/** A compiled workflow ready for execution. */
export class Workflow<TIn, TOut> {
  /** @internal */
  readonly _inner: NapiWorkflow;
  /** @internal */
  readonly _taskRegistry: Record<string, TaskCallback>;

  constructor(inner: NapiWorkflow, taskRegistry: Record<string, TaskCallback>) {
    this._inner = inner;
    this._taskRegistry = taskRegistry;
  }

  get workflowId(): string {
    return this._inner.workflowId;
  }

  get definitionHash(): string {
    return this._inner.definitionHash;
  }

  /** Workflow-level metadata, or `undefined` if none was provided. */
  get metadata(): Record<string, unknown> | undefined {
    const json = this._inner.metadataJson;
    return json != null ? (JSON.parse(json) as Record<string, unknown>) : undefined;
  }
}

/** A branch definition for fork/join. */
export interface BranchDef<TIn, TOut> {
  readonly name: string;
  readonly steps: readonly BranchStep[];
  /** @internal — phantom type marker */
  readonly _in?: TIn;
  readonly _out?: TOut;
}

interface BranchStep {
  taskId: string;
  fn: TaskCallback;
  metadata?: NapiTaskMetadata;
}

/** Infer the output types from a tuple of branch definitions. */
export type InferBranchOutputs<T extends readonly BranchDef<any, any>[]> = {
  [K in keyof T]: T[K] extends BranchDef<any, infer O> ? O : never;
};

/**
 * Create a branch for use with `.fork()`.
 *
 * ```ts
 * flow<Order>("process")
 *   .then(chargePayment)
 *   .fork([
 *     branch("email", sendConfirmation),
 *     branch("ship", shipOrder),
 *   ])
 *   .join("finalize", ([email, ship]) => ({ email, ship }))
 *   .build();
 * ```
 */
export function branch<TIn, TOut>(
  name: string,
  fn: TaskFn<TIn, TOut> | ((input: TIn) => TOut | Promise<TOut>),
): BranchDef<TIn, Awaited<TOut>> {
  const taskId = isTaskFn(fn) ? fn._taskId : name;
  const metadata = isTaskFn(fn) ? fn._metadata : undefined;

  return {
    name,
    steps: [{ taskId, fn: fn as TaskCallback, metadata }],
  } as BranchDef<TIn, Awaited<TOut>>;
}

/** Options for creating a flow. */
export interface FlowOptions {
  /** Workflow-level metadata (opaque to the engine). */
  metadata?: Record<string, unknown>;
}

/** Type-safe workflow builder. */
export class Flow<TInput, TLast = TInput> {
  /** @internal */
  readonly _builder: NapiFlowBuilder;
  /** @internal */
  readonly _taskRegistry: Record<string, TaskCallback> = {};
  /** @internal */
  private _lambdaCounter = 0;
  /** @internal */
  private _branchCounter = 0;

  constructor(name: string, opts?: FlowOptions) {
    this._builder = new (getNative().NapiFlowBuilder)(name);
    if (opts?.metadata != null) {
      this._builder.setMetadataJson(JSON.stringify(opts.metadata));
    }
  }

  /**
   * Add a sequential task step.
   *
   * Accepts either a `TaskFn` (created by `task()`) or an inline function with an id.
   */
  then<TOut>(fn: TaskFn<TLast, TOut>): Flow<TInput, Awaited<TOut>>;
  then<TOut>(
    id: string,
    fn: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
    opts?: StepOptions,
  ): Flow<TInput, Awaited<TOut>>;
  then<TOut>(
    idOrFn:
      | string
      | TaskFn<TLast, TOut>
      | ((input: TLast) => TOut | Promise<TOut>),
    fnOrOpts?:
      | ((input: TLast) => TOut | Promise<TOut>)
      | TaskFn<TLast, TOut>
      | StepOptions,
    maybeOpts?: StepOptions,
  ): Flow<TInput, Awaited<TOut>> {
    let taskId: string;
    let taskFn: TaskCallback;
    let metadata: NapiTaskMetadata | undefined;

    if (typeof idOrFn === "string") {
      // .then("id", fn, opts?)
      taskId = idOrFn;
      taskFn = fnOrOpts as TaskCallback;
      const opts = maybeOpts;
      if (isTaskFn(taskFn)) {
        metadata = (taskFn as TaskFn<TLast, TOut>)._metadata;
        // Keep the wrapped TaskFn (preserves Zod validation); only override the id.
      }
      if (opts) {
        metadata = { ...metadata, ...buildStepMetadata(taskId, opts) };
      }
    } else if (isTaskFn(idOrFn)) {
      // .then(taskFn)
      taskId = idOrFn._taskId;
      taskFn = idOrFn;
      metadata = idOrFn._metadata;
    } else {
      // .then(lambda) — auto-generate id
      taskId = `lambda_${this._lambdaCounter++}`;
      taskFn = idOrFn;
    }

    this._taskRegistry[taskId] = taskFn;
    this._builder.then(taskId, metadata);

    return this as unknown as Flow<TInput, Awaited<TOut>>;
  }

  /**
   * Start a fork for parallel execution.
   *
   * ```ts
   * .fork([
   *   branch("email", sendEmail),
   *   branch("sms", sendSms),
   * ])
   * .join("merge", ([email, sms]) => ({ email, sms }))
   * ```
   */
  fork<TBranches extends readonly BranchDef<TLast, any>[]>(
    branches: [...TBranches],
  ): ForkBuilder<TInput, TLast, TBranches> {
    return new ForkBuilder(this, branches);
  }

  /**
   * Add a durable delay. No workers are held during the delay.
   *
   * Duration can be a number (ms) or a string like "30s", "5m", "1h".
   */
  delay(id: string, duration: Duration): Flow<TInput, TLast> {
    const ms = parseDuration(duration);
    this._builder.delay(id, ms / 1000);
    return this;
  }

  /**
   * Wait for an external signal before continuing.
   *
   * The workflow parks and releases the worker until the signal arrives.
   */
  waitForSignal<TSignal = unknown>(
    id: string,
    signalName: string,
    opts?: { timeout?: Duration },
  ): Flow<TInput, TSignal> {
    const timeoutSecs =
      opts?.timeout != null ? parseDuration(opts.timeout) / 1000 : undefined;
    this._builder.waitForSignal(id, signalName, timeoutSecs);
    return this as unknown as Flow<TInput, TSignal>;
  }

  /**
   * Start a conditional branch based on a routing key.
   *
   * The `keys` array declares all valid routing keys up front, providing
   * type-level safety: the key function's return type and `.branch()` key
   * parameter are constrained to the declared set.
   *
   * ```ts
   * flow<Ticket>("classify")
   *   .then("classify", classify)
   *   .route((result) => result.intent, ["billing", "tech"] as const)
   *     .branch("billing", handleBilling)
   *     .branch("tech", handleTech)
   *     .done()
   *   .then("finalize", finalizeStep)
   *   .build();
   * ```
   */
  route<const TKeys extends readonly string[]>(
    keyFn:
      | TaskFn<TLast, TKeys[number]>
      | ((input: TLast) => TKeys[number] | Promise<TKeys[number]>),
    keys: TKeys,
  ): RouteBuilder<TInput, TLast, never, TKeys[number]> {
    const branchId = `branch_${this._branchCounter++}`;
    return new RouteBuilder(this, branchId, keyFn as TaskCallback, [
      ...keys,
    ]);
  }

  /** Build the workflow definition. */
  build(): Workflow<TInput, TLast> {
    const inner = this._builder.build();
    return new Workflow<TInput, TLast>(inner, this._taskRegistry);
  }
}

/** Builder for fork/join parallel branches. */
export class ForkBuilder<
  TInput,
  TLast,
  TBranches extends readonly BranchDef<TLast, any>[],
> {
  private readonly flow: Flow<TInput, TLast>;
  private readonly branches: TBranches;

  constructor(flow: Flow<TInput, TLast>, branches: TBranches) {
    this.flow = flow;
    this.branches = branches;
  }

  /**
   * Join branches with a combining function.
   *
   * The join function receives a tuple of branch outputs.
   */
  join<TOut>(
    id: string,
    fn: (
      branches: InferBranchOutputs<TBranches>,
    ) => TOut | Promise<TOut>,
  ): Flow<TInput, Awaited<TOut>> {
    // Register all branch tasks
    const napiBranches: NapiBranchTask[][] = this.branches.map((b) => {
      return b.steps.map((step) => {
        this.flow._taskRegistry[step.taskId] = step.fn;
        return { taskId: step.taskId, metadata: step.metadata };
      });
    });

    // The join function receives a Record (from Rust codec) keyed by the
    // first task ID of each branch. We wrap to convert to a tuple.
    const branchNames = this.branches.map((b) => b.steps[0].taskId);
    const joinWrapper = (branchResults: Record<string, unknown>) => {
      const tuple = branchNames.map((name) => branchResults[name]);
      return fn(tuple as InferBranchOutputs<TBranches>);
    };

    this.flow._taskRegistry[id] = joinWrapper;
    this.flow._builder.addFork(napiBranches, id);

    return this.flow as unknown as Flow<TInput, Awaited<TOut>>;
  }
}

/** Discriminated envelope wrapping a branch result. */
export interface BranchEnvelope<T> {
  branch: string;
  result: T;
}

/** Builder for conditional branching (route). */
export class RouteBuilder<
  TInput,
  TLast,
  TBranchOut = never,
  TKey extends string = string,
> {
  private readonly flow: Flow<TInput, TLast>;
  private readonly branchId: string;
  private readonly keyFn: TaskCallback;
  private readonly declaredKeys: string[];
  private readonly branches: Array<{
    key: string;
    steps: BranchStep[];
  }> = [];
  private defaultSteps: BranchStep[] | undefined;

  constructor(
    flow: Flow<TInput, TLast>,
    branchId: string,
    keyFn: TaskCallback,
    declaredKeys: string[],
  ) {
    this.flow = flow;
    this.branchId = branchId;
    this.keyFn = keyFn;
    this.declaredKeys = declaredKeys;
  }

  /**
   * Add a named branch.
   *
   * The key must be one of the keys declared in the `route()` call.
   *
   * ```ts
   * .route((r) => r.intent, ["billing", "tech"] as const)
   *   .branch("billing", handleBilling)
   *   .branch("tech", handleTech)
   *   .done()
   * ```
   */
  branch<TOut>(
    key: TKey,
    fn: TaskFn<TLast, TOut> | ((input: TLast) => TOut | Promise<TOut>),
  ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;
  branch<TOut>(
    key: TKey,
    id: string,
    fn: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
  ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;
  branch<TOut>(
    key: TKey,
    idOrFn:
      | string
      | TaskFn<TLast, TOut>
      | ((input: TLast) => TOut | Promise<TOut>),
    maybeFn?: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
  ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey> {
    const { taskId, taskFn, metadata } = resolveBranchTask(
      key,
      idOrFn,
      maybeFn,
      this.flow,
    );
    this.branches.push({
      key,
      steps: [{ taskId, fn: taskFn, metadata }],
    });
    return this as unknown as RouteBuilder<
      TInput,
      TLast,
      TBranchOut | Awaited<TOut>,
      TKey
    >;
  }

  /** Add a default branch for unmatched keys. */
  defaultBranch<TOut>(
    fn: TaskFn<TLast, TOut> | ((input: TLast) => TOut | Promise<TOut>),
  ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;
  defaultBranch<TOut>(
    id: string,
    fn: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
  ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey>;
  defaultBranch<TOut>(
    idOrFn:
      | string
      | TaskFn<TLast, TOut>
      | ((input: TLast) => TOut | Promise<TOut>),
    maybeFn?: ((input: TLast) => TOut | Promise<TOut>) | TaskFn<TLast, TOut>,
  ): RouteBuilder<TInput, TLast, TBranchOut | Awaited<TOut>, TKey> {
    const { taskId, taskFn, metadata } = resolveBranchTask(
      "_default",
      idOrFn,
      maybeFn,
      this.flow,
    );
    this.defaultSteps = [{ taskId, fn: taskFn, metadata }];
    return this as unknown as RouteBuilder<
      TInput,
      TLast,
      TBranchOut | Awaited<TOut>,
      TKey
    >;
  }

  /**
   * Finish the route and return to the Flow builder.
   *
   * Performs exhaustiveness checks:
   * - Throws if a declared key has no `.branch()` call and no `.defaultBranch()`.
   * - Throws if a `.branch()` key is not in the declared set.
   */
  done(): Flow<TInput, BranchEnvelope<TBranchOut>> {
    const branchedKeys = new Set(this.branches.map((b) => b.key));
    const declaredSet = new Set(this.declaredKeys);

    // Check for orphan branches
    const orphans = [...branchedKeys].filter((k) => !declaredSet.has(k));
    if (orphans.length > 0) {
      throw new Error(
        `Branch node '${this.branchId}': orphan branches for keys: ${orphans.join(", ")}`,
      );
    }

    // Check for missing branches (no default)
    if (!this.defaultSteps) {
      const missing = this.declaredKeys.filter((k) => !branchedKeys.has(k));
      if (missing.length > 0) {
        throw new Error(
          `Branch node '${this.branchId}': missing branches for keys: ${missing.join(", ")}`,
        );
      }
    }

    // Register key function
    const keyFnId = `${this.branchId}${KEY_FN_SUFFIX}`;
    this.flow._taskRegistry[keyFnId] = this.keyFn;

    // Build native branch entries
    const napiBranches = this.branches.map((b) => ({
      key: b.key,
      tasks: b.steps.map((step) => {
        this.flow._taskRegistry[step.taskId] = step.fn;
        return { taskId: step.taskId, metadata: step.metadata };
      }),
    }));

    const napiDefault = this.defaultSteps?.map((step) => {
      this.flow._taskRegistry[step.taskId] = step.fn;
      return { taskId: step.taskId, metadata: step.metadata };
    });

    this.flow._builder.addBranch(this.branchId, napiBranches, napiDefault);

    return this.flow as unknown as Flow<TInput, BranchEnvelope<TBranchOut>>;
  }
}

/** Factory function to create a new flow. */
export function flow<TInput>(name: string, opts?: FlowOptions): Flow<TInput> {
  return new Flow<TInput>(name, opts);
}

// ---- Helpers ----

function isTaskFn(fn: unknown): fn is TaskFn<any, any> {
  return typeof fn === "function" && "_taskId" in fn;
}

/** Resolve task id/fn/metadata from branch-style overloaded args. */
function resolveBranchTask<TIn, TOut>(
  fallbackId: string,
  idOrFn:
    | string
    | TaskFn<TIn, TOut>
    | ((input: TIn) => TOut | Promise<TOut>),
  maybeFn:
    | ((input: TIn) => TOut | Promise<TOut>)
    | TaskFn<TIn, TOut>
    | undefined,
  flow: Flow<any, any>,
): { taskId: string; taskFn: TaskCallback; metadata?: NapiTaskMetadata } {
  if (typeof idOrFn === "string") {
    const fn = maybeFn as TaskCallback;
    const metadata = isTaskFn(fn)
      ? (fn as TaskFn<TIn, TOut>)._metadata
      : undefined;
    return { taskId: idOrFn, taskFn: fn, metadata };
  } else if (isTaskFn(idOrFn)) {
    return {
      taskId: idOrFn._taskId,
      taskFn: idOrFn as TaskCallback,
      metadata: idOrFn._metadata,
    };
  } else {
    const taskId = `${fallbackId}_lambda_${flow["_lambdaCounter"]++}`;
    return { taskId, taskFn: idOrFn as TaskCallback };
  }
}

function buildStepMetadata(
  id: string,
  opts: StepOptions,
): NapiTaskMetadata {
  const metadata: NapiTaskMetadata = { displayName: id };
  if (opts.timeout != null) {
    metadata.timeoutSecs = parseDuration(opts.timeout) / 1000;
  }
  if (opts.retry) {
    metadata.retries = {
      maxRetries: opts.retry.maxAttempts,
      initialDelaySecs: parseDuration(opts.retry.initialDelay) / 1000,
      backoffMultiplier: opts.retry.backoffMultiplier ?? 2.0,
      maxDelaySecs:
        opts.retry.maxDelay != null
          ? parseDuration(opts.retry.maxDelay) / 1000
          : undefined,
    };
  } else if (opts.retries != null) {
    metadata.retries = {
      maxRetries: opts.retries,
      initialDelaySecs: 1.0,
      backoffMultiplier: 2.0,
    };
  }
  return metadata;
}
