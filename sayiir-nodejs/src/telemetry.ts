import { getNative } from "./native.js";

/**
 * Initialize the tracing subscriber.
 *
 * Sets up a `tracing-subscriber` registry with:
 * - `fmt` layer for human-readable console output
 * - `tracing-opentelemetry` layer for OTLP export (only if `OTEL_EXPORTER_OTLP_ENDPOINT` is set)
 * - `EnvFilter` controlled by `RUST_LOG` (default: `info`)
 *
 * This function is idempotent — calling it multiple times is safe.
 */
export function initTracing(): void {
  getNative().initTracing();
}

/**
 * Flush and shut down the OpenTelemetry tracer provider.
 *
 * Call this before process exit to ensure all pending spans are exported.
 */
export function shutdownTracing(): void {
  getNative().shutdownTracing();
}
