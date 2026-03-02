/**
 * Type-safe flow builder for constructing workflows.
 *
 * The pure builder classes are re-exported from sayiir-flow-js.
 * The `flow()` factory function injects the WASM builder backend.
 */

import { createFlowFactory, type FlowOptions, type Flow } from "sayiir-flow-js";

import { WasmFlowBuilder } from "../wasm/sayiir_cloudflare.js";

// Re-export all pure builder types and classes
export {
  branch,
  Flow,
  ForkBuilder,
  RouteBuilder,
  Workflow,
} from "sayiir-flow-js";

export type {
  BranchDef,
  BranchEnvelope,
  FlowOptions,
  InferBranchOutputs,
} from "sayiir-flow-js";

/** Create a new flow with the WASM builder backend. */
export const flow: <TInput>(name: string, opts?: FlowOptions) => Flow<TInput> =
  createFlowFactory((name) => new WasmFlowBuilder(name));
