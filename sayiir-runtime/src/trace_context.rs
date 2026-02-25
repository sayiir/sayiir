//! W3C trace context propagation and OpenTelemetry subscriber setup.
//!
//! Requires the `otel` feature. Provides:
//! - Trace context extraction/injection for cross-worker propagation
//! - `init_tracing` / `shutdown_tracing` for configuring the tracing subscriber
//!   with optional OTLP export (used by Python and Node.js bindings)

use std::collections::HashMap;
use std::sync::{Mutex, Once};

use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Extract the W3C `traceparent` value from the current tracing span.
///
/// Returns `None` if there is no active `OTel` context (e.g. `OTel` subscriber
/// is not installed, or the span has no trace ID).
#[must_use]
pub fn current_trace_parent() -> Option<String> {
    let context = tracing::Span::current().context();
    let propagator = TraceContextPropagator::new();
    let mut carrier = HashMap::new();
    propagator.inject_context(&context, &mut carrier);
    carrier.remove("traceparent")
}

/// Build an OpenTelemetry [`opentelemetry::Context`] from a W3C `traceparent` value.
///
///
/// Used to restore parent context when a worker picks up a task that was
/// started under a different trace.
#[must_use]
pub fn context_from_trace_parent(trace_parent: &str) -> opentelemetry::Context {
    let propagator = TraceContextPropagator::new();
    let mut carrier = HashMap::new();
    carrier.insert("traceparent".to_string(), trace_parent.to_string());
    propagator.extract(&carrier)
}

// ── Subscriber setup ────────────────────────────────────────────────────

static INIT: Once = Once::new();
static PROVIDER: Mutex<Option<opentelemetry_sdk::trace::SdkTracerProvider>> = Mutex::new(None);

/// Initialize the tracing subscriber.
///
/// Sets up a `tracing-subscriber` registry with:
/// - `fmt` layer for human-readable console output
/// - `tracing-opentelemetry` layer for OTLP export (only if `OTEL_EXPORTER_OTLP_ENDPOINT` is set)
/// - `EnvFilter` controlled by `RUST_LOG` (default: `info`)
///
/// The `default_service_name` is used when `OTEL_SERVICE_NAME` is not set
/// (e.g. `"sayiir-py"` or `"sayiir-node"`).
///
/// This function is idempotent — calling it multiple times is safe.
pub fn init_tracing(default_service_name: &str) {
    let default_name = default_service_name.to_string();
    INIT.call_once(move || {
        use opentelemetry::trace::TracerProvider;
        use opentelemetry_otlp::WithExportConfig;
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let fmt_layer = tracing_subscriber::fmt::layer().with_target(true);

        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();

        if let Some(endpoint) = endpoint {
            let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or(default_name);

            let exporter = match opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&endpoint)
                .build()
            {
                Ok(e) => e,
                Err(err) => {
                    #[allow(clippy::print_stderr)]
                    {
                        eprintln!("sayiir: failed to create OTLP exporter: {err}");
                    }
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(fmt_layer)
                        .init();
                    return;
                }
            };

            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(
                    opentelemetry_sdk::Resource::builder()
                        .with_service_name(service_name)
                        .build(),
                )
                .build();

            let tracer = provider.tracer("sayiir");
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(otel_layer)
                .init();

            if let Ok(mut guard) = PROVIDER.lock() {
                *guard = Some(provider);
            }
        } else {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .init();
        }
    });
}

/// Flush and shut down the OpenTelemetry tracer provider.
///
/// Call this before process exit to ensure all pending spans are exported.
pub fn shutdown_tracing() {
    if let Ok(mut guard) = PROVIDER.lock()
        && let Some(provider) = guard.take()
    {
        let _ = provider.shutdown();
    }
}
