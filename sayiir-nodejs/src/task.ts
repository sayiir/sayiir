/**
 * Task definition for workflow steps.
 *
 * The `task()` function wraps a function with metadata (id, timeout, retries)
 * and optionally Zod schemas for input/output validation. The Flow builder
 * reads these properties when constructing workflows.
 */

import type { Duration, RetryPolicy, TaskOptions, ZodLike } from "./types.js";
import { parseDuration } from "./duration.js";
import type { NapiRetryPolicy, NapiTaskMetadata } from "./native.js";

/** Branded type for a registered task function. */
export interface TaskFn<TIn, TOut> {
  (input: TIn): TOut | Promise<TOut>;
  readonly _taskId: string;
  readonly _metadata: NapiTaskMetadata;
  readonly _inputSchema?: ZodLike<TIn>;
  readonly _outputSchema?: ZodLike<TOut>;
  readonly _rawFn: (input: TIn) => TOut | Promise<TOut>;
}

/**
 * Define a named task with optional configuration.
 *
 * ```ts
 * const fetchUser = task("fetch-user", async (id: number) => {
 *   return await db.getUser(id);
 * }, { timeout: "30s", retries: 3 });
 * ```
 */
export function task<TIn, TOut>(
  id: string,
  fn: (input: TIn) => TOut | Promise<TOut>,
  opts?: TaskOptions<TIn>,
): TaskFn<TIn, TOut> {
  const metadata = buildMetadata(id, opts);

  // Wrap the function with optional Zod validation
  const wrapped = wrapWithValidation(fn, opts?.input, opts?.output);

  // Brand the function as a TaskFn
  const taskFn = wrapped as TaskFn<TIn, TOut>;
  Object.defineProperties(taskFn, {
    _taskId: { value: id, enumerable: false },
    _metadata: { value: metadata, enumerable: false },
    _inputSchema: { value: opts?.input, enumerable: false },
    _outputSchema: { value: opts?.output, enumerable: false },
    _rawFn: { value: fn, enumerable: false },
  });

  return taskFn;
}

/** Build NAPI metadata from task options. */
function buildMetadata(id: string, opts?: TaskOptions): NapiTaskMetadata {
  const metadata: NapiTaskMetadata = {
    displayName: id,
    description: opts?.description,
    tags: opts?.tags,
  };

  if (opts?.timeout != null) {
    metadata.timeoutSecs = parseDuration(opts.timeout) / 1000;
  }

  if (opts?.retry) {
    metadata.retries = buildRetryPolicy(opts.retry);
  } else if (opts?.retries != null) {
    metadata.retries = {
      maxRetries: opts.retries,
      initialDelaySecs: 1.0,
      backoffMultiplier: 2.0,
    };
  }

  return metadata;
}

/** Convert a RetryPolicy to the native format. */
function buildRetryPolicy(policy: RetryPolicy): NapiRetryPolicy {
  return {
    maxRetries: policy.maxAttempts,
    initialDelaySecs: parseDuration(policy.initialDelay) / 1000,
    backoffMultiplier: policy.backoffMultiplier ?? 2.0,
    maxDelaySecs:
      policy.maxDelay != null ? parseDuration(policy.maxDelay) / 1000 : undefined,
  };
}

/** Wrap a function with optional Zod validation. */
function wrapWithValidation<TIn, TOut>(
  fn: (input: TIn) => TOut | Promise<TOut>,
  inputSchema?: ZodLike<TIn>,
  outputSchema?: ZodLike<TOut>,
): (input: TIn) => TOut | Promise<TOut> {
  if (!inputSchema && !outputSchema) {
    return fn;
  }

  return (input: TIn) => {
    const validated = inputSchema ? inputSchema.parse(input) : input;
    const result = fn(validated as TIn);

    if (!outputSchema) return result;

    // Handle both sync and async results
    if (result instanceof Promise) {
      return result.then((v) => outputSchema.parse(v));
    }
    return outputSchema.parse(result);
  };
}
