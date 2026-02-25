//! OpenTelemetry tracing subscriber setup for Python bindings.

use pyo3::prelude::*;

/// Initialize the tracing subscriber.
///
/// Sets up a `tracing-subscriber` registry with:
/// - `fmt` layer for human-readable console output
/// - `tracing-opentelemetry` layer for OTLP export (only if `OTEL_EXPORTER_OTLP_ENDPOINT` is set)
/// - `EnvFilter` controlled by `RUST_LOG` (default: `info`)
///
/// This function is idempotent — calling it multiple times is safe.
#[pyfunction]
pub fn init_tracing() {
    sayiir_runtime::trace_context::init_tracing("sayiir-py");
}

/// Flush and shut down the OpenTelemetry tracer provider.
///
/// Call this before process exit to ensure all pending spans are exported.
#[pyfunction]
pub fn shutdown_tracing() {
    sayiir_runtime::trace_context::shutdown_tracing();
}
