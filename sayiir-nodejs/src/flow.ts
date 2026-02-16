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

/** Type-safe workflow builder. */
export class Flow<TInput, TLast = TInput> {
  /** @internal */
  readonly _builder: NapiFlowBuilder;
  /** @internal */
  readonly _taskRegistry: Record<string, TaskCallback> = {};
  /** @internal */
  private _lambdaCounter = 0;

  constructor(name: string) {
    this._builder = new (getNative().NapiFlowBuilder)(name);
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

/** Factory function to create a new flow. */
export function flow<TInput>(name: string): Flow<TInput> {
  return new Flow<TInput>(name);
}

// ---- Helpers ----

function isTaskFn(fn: unknown): fn is TaskFn<any, any> {
  return typeof fn === "function" && "_taskId" in fn;
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
