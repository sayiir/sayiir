//! OpenTelemetry tracing subscriber setup for Python bindings.

use std::sync::{Mutex, Once};

use opentelemetry_otlp::WithExportConfig;
use pyo3::prelude::*;

static INIT: Once = Once::new();
static PROVIDER: Mutex<Option<opentelemetry_sdk::trace::SdkTracerProvider>> = Mutex::new(None);

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
    INIT.call_once(|| {
        use opentelemetry::trace::TracerProvider;
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let fmt_layer = tracing_subscriber::fmt::layer().with_target(true);

        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();

        if let Some(endpoint) = endpoint {
            let service_name =
                std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "sayiir-py".to_string());

            let exporter = match opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&endpoint)
                .build()
            {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("sayiir: failed to create OTLP exporter: {err}");
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
#[pyfunction]
pub fn shutdown_tracing() {
    if let Ok(mut guard) = PROVIDER.lock()
        && let Some(provider) = guard.take()
    {
        let _ = provider.shutdown();
    }
}
