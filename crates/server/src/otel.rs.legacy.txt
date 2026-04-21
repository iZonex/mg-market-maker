//! Optional OpenTelemetry OTLP tracer, gated by the `otel` cargo
//! feature. When the feature is off (default build), this module
//! compiles to an empty `init` that returns `None` — the main
//! subscriber stack stays exactly as before.
//!
//! When the feature is on, we build a tonic-backed OTLP tracer
//! provider and return a `tracing_opentelemetry::OpenTelemetryLayer`
//! that the caller attaches to the root subscriber. Activation
//! requires `OTEL_EXPORTER_OTLP_ENDPOINT` at runtime so a build
//! with the feature still produces a zero-network binary unless
//! an operator opts in.
//!
//! Example (collector at localhost:4317):
//!
//! ```bash
//! cargo build --release --features otel
//! OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
//!   OTEL_SERVICE_NAME=mm-server cargo run -p mm-server
//! ```

#[cfg(feature = "otel")]
mod enabled {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{trace as sdktrace, Resource};
    use tracing::info;

    /// Shutdown guard — drop it at program exit so the BSP flushes
    /// any queued spans to the collector.
    pub struct OtelGuard {
        provider: sdktrace::TracerProvider,
    }

    impl Drop for OtelGuard {
        fn drop(&mut self) {
            let _ = self.provider.shutdown();
        }
    }

    /// Build the OTLP pipeline. The returned layer is typed on
    /// `Registry` so callers must attach it directly on top of
    /// `tracing_subscriber::registry()` before other layers — that
    /// is the only S for which `OpenTelemetryLayer` implements
    /// `Layer<S>` with a concrete tracer.
    pub fn init(
        service_name: &str,
    ) -> Option<(
        tracing_opentelemetry::OpenTelemetryLayer<
            tracing_subscriber::Registry,
            sdktrace::Tracer,
        >,
        OtelGuard,
    )> {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;
        if endpoint.is_empty() {
            return None;
        }

        let exporter = opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint(&endpoint)
            .build_span_exporter()
            .ok()?;

        let resource = Resource::new(vec![KeyValue::new(
            "service.name",
            service_name.to_string(),
        )]);

        let provider = sdktrace::TracerProvider::builder()
            .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
            .with_config(sdktrace::Config::default().with_resource(resource))
            .build();

        let tracer = provider.tracer("mm-server");
        let layer = tracing_opentelemetry::layer().with_tracer(tracer);

        info!(%endpoint, "OpenTelemetry OTLP exporter enabled");

        Some((layer, OtelGuard { provider }))
    }
}

#[cfg(not(feature = "otel"))]
mod disabled {
    /// Zero-sized placeholder so `Option<OtelGuard>` typechecks
    /// in both feature configurations. Never constructed in the
    /// default build — `init_logging` assigns `None` directly.
    pub struct OtelGuard;
}

#[cfg(feature = "otel")]
pub use enabled::{init, OtelGuard};

#[cfg(not(feature = "otel"))]
pub use disabled::OtelGuard;
