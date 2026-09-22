//! Stable, low-cardinality OpenTelemetry instrumentation for runtime internals.
//!
//! Metric names, dimensions, phases, and outcomes are fixed here so call sites
//! cannot invent high-cardinality telemetry. Identifiers such as principal IDs,
//! participant IDs, deployment IDs, digests, subjects, and URLs are never
//! metric attributes.

use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{global, KeyValue};

const METER_NAME: &str = "@qlever-llc/trellis";

/// Explicit histogram boundaries in seconds, shared by every duration metric.
const DURATION_BOUNDARIES: [f64; 13] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Duration metric families emitted by the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DurationMetric {
    /// Authorization HTTP flow boundaries.
    AuthFlow,
    /// NATS auth callout boundaries.
    AuthCallout,
    /// Contract compilation and compatibility analysis.
    ContractAnalysis,
}

impl DurationMetric {
    fn name(self) -> &'static str {
        match self {
            Self::AuthFlow => "trellis.auth.flow.duration",
            Self::AuthCallout => "trellis.auth.callout.duration",
            Self::ContractAnalysis => "trellis.contract.analysis.duration",
        }
    }

    fn histogram(self) -> &'static Histogram<f64> {
        static AUTH_FLOW: OnceLock<Histogram<f64>> = OnceLock::new();
        static AUTH_CALLOUT: OnceLock<Histogram<f64>> = OnceLock::new();
        static CONTRACT_ANALYSIS: OnceLock<Histogram<f64>> = OnceLock::new();
        match self {
            Self::AuthFlow => AUTH_FLOW.get_or_init(|| duration_histogram(self.name())),
            Self::AuthCallout => AUTH_CALLOUT.get_or_init(|| duration_histogram(self.name())),
            Self::ContractAnalysis => {
                CONTRACT_ANALYSIS.get_or_init(|| duration_histogram(self.name()))
            }
        }
    }
}

/// Outcome dimension for a duration sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    /// The measured operation succeeded.
    Ok,
    /// The measured operation returned an error.
    Error,
    /// The measured operation was denied by authorization.
    Denied,
    /// The measured operation was rejected by rate limiting.
    RateLimited,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Denied => "denied",
            Self::RateLimited => "rate_limited",
        }
    }
}

/// Cache family for request counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheKind {
    /// Compiled installed-evidence graphs.
    Evidence,
    /// Selected-surface compatibility reports.
    Compatibility,
}

impl CacheKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Compatibility => "compatibility",
        }
    }
}

/// Final lookup role for a cache request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheResult {
    /// A resident ready value was returned.
    Hit,
    /// An in-flight owner's completion was awaited.
    Wait,
    /// This caller became the computation owner.
    Miss,
}

impl CacheResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Wait => "wait",
        }
    }
}

fn duration_histogram(name: &'static str) -> Histogram<f64> {
    global::meter(METER_NAME)
        .f64_histogram(name)
        .with_unit("s")
        .with_boundaries(DURATION_BOUNDARIES.to_vec())
        .build()
}

/// Record one duration sample with fixed low-cardinality dimensions.
pub(crate) fn record_duration(
    metric: DurationMetric,
    elapsed: Duration,
    surface: &'static str,
    operation: &'static str,
    phase: &'static str,
    outcome: Outcome,
) {
    metric.histogram().record(
        elapsed.as_secs_f64(),
        &[
            KeyValue::new("trellis.surface", surface),
            KeyValue::new("trellis.operation", operation),
            KeyValue::new("trellis.phase", phase),
            KeyValue::new("trellis.outcome", outcome.as_str()),
        ],
    );
}

/// Record exactly one cache request sample for a completed lookup decision.
pub(crate) fn record_cache_request(kind: CacheKind, result: CacheResult) {
    static CACHE_REQUESTS: OnceLock<Counter<u64>> = OnceLock::new();
    let counter = CACHE_REQUESTS.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("trellis.contract.cache.requests")
            .with_unit("{request}")
            .build()
    });
    counter.add(
        1,
        &[
            KeyValue::new("trellis.cache.kind", kind.as_str()),
            KeyValue::new("trellis.cache.result", result.as_str()),
        ],
    );
}
