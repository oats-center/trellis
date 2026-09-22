//! Optional OpenTelemetry trace and metric export for trellis-server.
//!
//! Export is opt-in through standard OTLP endpoint environment variables. The
//! existing fmt-based stdout/stderr tracing output is always installed, and
//! no OpenTelemetry log exporter or trace-context propagation is configured.

use std::io::IsTerminal as _;

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{MetricExporter, SpanExporter};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::EnvFilter;

/// Layer stack directly above the registry: the environment filter.
type FilteredRegistry = tracing_subscriber::layer::Layered<EnvFilter, tracing_subscriber::Registry>;

/// Resolved export configuration for one process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SignalConfig {
    traces: bool,
    metrics: bool,
}

impl SignalConfig {
    /// Resolve which signals explicit, non-empty endpoint variables request.
    fn resolve(get: impl Fn(&str) -> Option<String>) -> Self {
        let configured = |name: &str| get(name).is_some_and(|value| !value.trim().is_empty());
        let shared = configured("OTEL_EXPORTER_OTLP_ENDPOINT");
        Self {
            traces: shared || configured("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            metrics: shared || configured("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT"),
        }
    }
}

/// Owns the optional OTLP providers for the process lifetime.
pub(crate) struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl TelemetryGuard {
    /// Flush and shut down both providers, warning instead of failing.
    pub(crate) fn shutdown(&self) {
        if let Some(provider) = &self.tracer_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("trellis-server: OpenTelemetry trace shutdown failed: {error}");
            }
        }
        if let Some(provider) = &self.meter_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("trellis-server: OpenTelemetry metric shutdown failed: {error}");
            }
        }
    }
}

/// Install the fmt tracing subscriber plus optional OTLP trace/metric export.
///
/// # Errors
///
/// Returns an error only when the fmt tracing subscriber cannot be installed.
pub(crate) fn init(verbose: bool, check: bool) -> miette::Result<TelemetryGuard> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(if verbose { "debug" } else { "info" }));
    let attached = if check {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    };
    let writer = if check {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr)
    } else {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stdout)
    };

    let signals = if check {
        SignalConfig {
            traces: false,
            metrics: false,
        }
    } else {
        SignalConfig::resolve(|name| std::env::var(name).ok())
    };
    let resource = Resource::builder()
        .with_service_name("trellis-server")
        .build();
    let mut tracer_provider = None;
    let mut otel_layer: Option<Box<dyn tracing_subscriber::Layer<FilteredRegistry> + Send + Sync>> =
        None;
    if signals.traces {
        match SpanExporter::builder().with_http().build() {
            Ok(exporter) => {
                let provider = SdkTracerProvider::builder()
                    .with_resource(resource.clone())
                    .with_batch_exporter(exporter)
                    .build();
                global::set_tracer_provider(provider.clone());
                let tracer = provider.tracer("trellis-server");
                otel_layer = Some(Box::new(tracing_opentelemetry::layer().with_tracer(tracer)));
                tracer_provider = Some(provider);
            }
            Err(error) => {
                eprintln!("trellis-server: OpenTelemetry trace export disabled: {error}");
            }
        }
    }
    let mut meter_provider = None;
    if signals.metrics {
        match MetricExporter::builder().with_http().build() {
            Ok(exporter) => {
                let provider = SdkMeterProvider::builder()
                    .with_resource(resource)
                    .with_periodic_exporter(exporter)
                    .build();
                global::set_meter_provider(provider.clone());
                meter_provider = Some(provider);
            }
            Err(error) => {
                eprintln!("trellis-server: OpenTelemetry metric export disabled: {error}");
            }
        }
    }

    if attached {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(verbose)
                    .with_ansi(true)
                    .with_writer(writer),
            )
            .try_init()
            .map_err(|error| miette::miette!(error.to_string()))?;
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .json()
                    .with_writer(writer),
            )
            .try_init()
            .map_err(|error| miette::miette!(error.to_string()))?;
    }
    Ok(TelemetryGuard {
        tracer_provider,
        meter_provider,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::SignalConfig;

    fn resolve(variables: &[(&str, &str)]) -> SignalConfig {
        let values: HashMap<&str, &str> = variables.iter().copied().collect();
        SignalConfig::resolve(|name| values.get(name).map(|value| (*value).to_owned()))
    }

    #[test]
    fn explicit_endpoint_variables_enable_each_signal_independently() {
        assert_eq!(
            resolve(&[]),
            SignalConfig {
                traces: false,
                metrics: false,
            }
        );
        assert_eq!(
            resolve(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318")]),
            SignalConfig {
                traces: true,
                metrics: true,
            }
        );
        assert_eq!(
            resolve(&[(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://collector:4318"
            )]),
            SignalConfig {
                traces: true,
                metrics: false,
            }
        );
        assert_eq!(
            resolve(&[(
                "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
                "http://collector:4318"
            )]),
            SignalConfig {
                traces: false,
                metrics: true,
            }
        );
        assert_eq!(
            resolve(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "   ")]),
            SignalConfig {
                traces: false,
                metrics: false,
            }
        );
    }
}
