use opentelemetry_sdk::{
    error::OTelSdkResult,
    metrics::{
        data::{AggregatedMetrics, MetricData, ResourceMetrics},
        exporter::PushMetricExporter,
        SdkMeterProvider, Temporality,
    },
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[derive(Debug)]
struct Exporter(PathBuf);
impl PushMetricExporter for Exporter {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        let mut rows = Vec::new();
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                match metric.data() {
                    AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                        for point in histogram.data_points() {
                            let attributes: serde_json::Map<String, serde_json::Value> = point
                                .attributes()
                                .map(|kv| (kv.key.to_string(), json!(kv.value.to_string())))
                                .collect();
                            rows.push(json!({ "name": metric.name(), "attributes": attributes, "count": point.count(), "sum": point.sum(), "max": point.max(), "bounds": point.bounds().collect::<Vec<_>>(), "buckets": point.bucket_counts().collect::<Vec<_>>() }));
                        }
                    }
                    // Cumulative counters, e.g. Transfer payload/wire/control bytes.
                    AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                        for point in sum.data_points() {
                            let attributes: serde_json::Map<String, serde_json::Value> = point
                                .attributes()
                                .map(|kv| (kv.key.to_string(), json!(kv.value.to_string())))
                                .collect();
                            rows.push(json!({ "name": metric.name(), "attributes": attributes, "value": point.value() }));
                        }
                    }
                    _ => {}
                }
            }
        }
        let pending = self.0.with_extension("pending");
        std::fs::write(
            &pending,
            serde_json::to_vec_pretty(&rows).map_err(|error| {
                opentelemetry_sdk::error::OTelSdkError::InternalFailure(error.to_string())
            })?,
        )
        .and_then(|()| std::fs::rename(pending, &self.0))
        .map_err(|error| opentelemetry_sdk::error::OTelSdkError::InternalFailure(error.to_string()))
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        Temporality::Cumulative
    }
}

pub(super) fn install(output: &std::path::Path, role: &str) -> SdkMeterProvider {
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(Exporter(output.join(format!("metrics-{role}.json"))))
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    provider
}
