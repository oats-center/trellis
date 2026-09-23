//! Collecting meter reader used by focused telemetry behavior tests.
//!
//! The pinned SDK ships an in-memory metric exporter; this module wires it to
//! a manual reader so tests observe real exported points from the unchanged
//! production instruments.

use std::sync::{Arc, Mutex};

use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};

/// One collected numeric point: instrument name, attribute pairs, value.
#[derive(Clone, Debug)]
pub(crate) struct CollectedPoint {
    /// Instrument name as exported.
    pub(crate) name: String,
    /// Bounded attribute pairs as exported.
    pub(crate) attributes: Vec<(String, String)>,
    /// Numeric value for counters/gauges; histogram sum for histograms.
    pub(crate) value: f64,
    /// Cumulative count for histograms; 1 otherwise.
    pub(crate) count: u64,
}

/// Shared handle to the exporter used by one captured provider.
#[derive(Clone, Default)]
pub(crate) struct MetricCapture {
    exporter: Arc<Mutex<Option<InMemoryMetricExporter>>>,
}
impl MetricCapture {
    /// Collects every point currently exported by the provider.
    ///
    /// Cumulative temporality keeps each phase's samples visible without
    /// fighting the SDK's own aggregation bookkeeping.
    pub(crate) fn points(&self) -> Vec<CollectedPoint> {
        let mut guard = self.exporter.lock().expect("capture lock");
        let Some(exporter) = guard.as_mut() else {
            return Vec::new();
        };
        let Ok(metrics) = exporter.get_finished_metrics() else {
            return Vec::new();
        };
        exporter.reset();
        let mut points = Vec::new();
        for resource_metrics in metrics {
            collect_resource_metrics(&resource_metrics, &mut points);
        }
        points
    }

    /// Returns every collected point for one instrument name.
    pub(crate) fn points_for(&self, name: &str) -> Vec<CollectedPoint> {
        self.points()
            .into_iter()
            .filter(|point| point.name == name)
            .collect()
    }

    /// Flushes the process provider so a test observes its own samples.
    pub(crate) fn flush(&self) {
        if let Some(provider) = PROCESS_PROVIDER.get() {
            let _ = provider.force_flush();
        }
    }
}

/// Builds a meter provider whose points land in the returned capture.
///
/// The provider is returned so a test can install it as the process meter
/// provider before exercising real instrumentation.
pub(crate) fn captured_meter_provider() -> (SdkMeterProvider, MetricCapture) {
    let exporter = opentelemetry_sdk::metrics::InMemoryMetricExporterBuilder::new()
        .with_temporality(opentelemetry_sdk::metrics::Temporality::Cumulative)
        .build();
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    let capture = MetricCapture {
        exporter: Arc::new(Mutex::new(Some(exporter))),
    };
    (provider, capture)
}

/// Serializes tests that install or observe process meter state.
///
/// Async-aware so a provider test can hold it across awaits.
pub(crate) async fn meter_test_lock() -> tokio::sync::OwnedMutexGuard<()> {
    static LOCK: std::sync::OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
        std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

/// Process-wide capture provider installed by [`process_capture`].
static PROCESS_PROVIDER: std::sync::OnceLock<SdkMeterProvider> = std::sync::OnceLock::new();

/// Installs one process-wide capture provider and returns its capture.
///
/// Instrument handles cache the meter that created them, so tests must share
/// the single process provider exactly as production does instead of swapping
/// the global provider per test.
pub(crate) fn process_capture() -> MetricCapture {
    static CAPTURE: std::sync::OnceLock<MetricCapture> = std::sync::OnceLock::new();
    CAPTURE
        .get_or_init(|| {
            let (provider, capture) = captured_meter_provider();
            opentelemetry::global::set_meter_provider(provider.clone());
            crate::telemetry::instruments::note_meter_provider_changed();
            let _ = PROCESS_PROVIDER.set(provider);
            capture
        })
        .clone()
}
fn string_attributes(
    attributes: impl Iterator<Item = opentelemetry::KeyValue>,
) -> Vec<(String, String)> {
    attributes
        .map(|kv| (kv.key.to_string(), kv.value.to_string()))
        .collect()
}

fn collect_resource_metrics(resource_metrics: &ResourceMetrics, points: &mut Vec<CollectedPoint>) {
    for scope in resource_metrics.scope_metrics() {
        for metric in scope.metrics() {
            match metric.data() {
                AggregatedMetrics::F64(data) => record_f64(metric.name(), data, points),
                AggregatedMetrics::U64(data) => record_u64(metric.name(), data, points),
                AggregatedMetrics::I64(data) => record_i64(metric.name(), data, points),
            }
        }
    }
}

fn record_f64(name: &str, data: &MetricData<f64>, points: &mut Vec<CollectedPoint>) {
    match data {
        MetricData::Gauge(gauge) => {
            for point in gauge.data_points() {
                points.push(CollectedPoint {
                    name: name.to_owned(),
                    attributes: string_attributes(point.attributes().cloned()),
                    value: point.value(),
                    count: 1,
                });
            }
        }
        MetricData::Sum(sum) => {
            for point in sum.data_points() {
                points.push(CollectedPoint {
                    name: name.to_owned(),
                    attributes: string_attributes(point.attributes().cloned()),
                    value: point.value(),
                    count: 1,
                });
            }
        }
        MetricData::Histogram(histogram) => {
            for point in histogram.data_points() {
                points.push(CollectedPoint {
                    name: name.to_owned(),
                    attributes: string_attributes(point.attributes().cloned()),
                    value: point.sum(),
                    count: point.count(),
                });
            }
        }
        MetricData::ExponentialHistogram(_) => {}
    }
}

macro_rules! record_integer_points {
    ($name:expr, $data:expr, $points:expr) => {{
        match $data {
            MetricData::Gauge(gauge) => {
                for point in gauge.data_points() {
                    $points.push(CollectedPoint {
                        name: $name.to_owned(),
                        attributes: string_attributes(point.attributes().cloned()),
                        value: point.value() as f64,
                        count: 1,
                    });
                }
            }
            MetricData::Sum(sum) => {
                for point in sum.data_points() {
                    $points.push(CollectedPoint {
                        name: $name.to_owned(),
                        attributes: string_attributes(point.attributes().cloned()),
                        value: point.value() as f64,
                        count: 1,
                    });
                }
            }
            MetricData::Histogram(histogram) => {
                for point in histogram.data_points() {
                    $points.push(CollectedPoint {
                        name: $name.to_owned(),
                        attributes: string_attributes(point.attributes().cloned()),
                        value: point.sum() as f64,
                        count: point.count(),
                    });
                }
            }
            MetricData::ExponentialHistogram(_) => {}
        }
    }};
}

fn record_u64(name: &str, data: &MetricData<u64>, points: &mut Vec<CollectedPoint>) {
    record_integer_points!(name, data, points);
}

fn record_i64(name: &str, data: &MetricData<i64>, points: &mut Vec<CollectedPoint>) {
    record_integer_points!(name, data, points);
}
