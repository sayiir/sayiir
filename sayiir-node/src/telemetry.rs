//! OpenTelemetry tracing subscriber setup for Node.js bindings.

#![allow(dead_code)] // Functions exposed via #[napi] for N-API runtime

use napi_derive::napi;

/// Initialize the tracing subscriber.
///
/// Sets up a `tracing-subscriber` registry with:
/// - `fmt` layer for human-readable console output
/// - `tracing-opentelemetry` layer for OTLP export (only if `OTEL_EXPORTER_OTLP_ENDPOINT` is set)
/// - `EnvFilter` controlled by `RUST_LOG` (default: `info`)
///
/// This function is idempotent — calling it multiple times is safe.
#[napi]
pub fn init_tracing() {
    sayiir_runtime::trace_context::init_tracing("sayiir-node");
}

/// Flush and shut down the OpenTelemetry tracer provider.
///
/// Call this before process exit to ensure all pending spans are exported.
#[napi]
pub fn shutdown_tracing() {
    sayiir_runtime::trace_context::shutdown_tracing();
}
