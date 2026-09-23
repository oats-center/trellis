//! Trellis-owned OTLP HTTP/protobuf provider construction.
//!
//! Only compiled with the non-default `telemetry-otlp` feature. Provider
//! construction, flush, and shutdown never change a business result.

use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{
    BatchConfigBuilder, BatchSpanProcessor, Sampler, SdkTracerProvider, SpanLimits,
};
use opentelemetry_sdk::Resource;

use super::{TelemetryIdentity, INSTRUMENTATION_SCOPE};

/// Default OpenTelemetry export timeout when the environment does not set one.
const DEFAULT_EXPORT_TIMEOUT: Duration = Duration::from_secs(3);
/// Metrics export interval from the observability order.
const METRICS_INTERVAL: Duration = Duration::from_secs(15);
/// Trace batch delay from the observability order.
const TRACE_BATCH_DELAY: Duration = Duration::from_millis(5_000);
/// Trace queue capacity from the observability order.
const TRACE_QUEUE_SIZE: usize = 2_048;
/// Trace batch maximum from the observability order.
const TRACE_BATCH_MAX: usize = 256;
/// Bounded shutdown budget for one process telemetry owner.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
/// Default parent-based trace ratio when the environment does not set one.
const DEFAULT_TRACE_RATIO: f64 = 0.10;

/// Providers constructed and installed by Trellis.
pub struct OwnedProviders {
    /// Whether managed metrics were installed.
    pub(crate) metrics_enabled: bool,
    /// Whether a managed tracer or an exported trace signal was installed.
    pub(crate) traces_enabled: bool,
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    operation: InFlightOperation,
}

/// Returns an empty operation slot.
fn operation_slot() -> InFlightOperation {
    std::sync::Arc::new(std::sync::Mutex::new(OperationSlotState::default()))
}

impl OwnedProviders {
    /// Optional tracer for attaching the `tracing_opentelemetry` layer.
    pub(crate) fn tracer(&self) -> Option<opentelemetry_sdk::trace::Tracer> {
        self.tracer_provider
            .as_ref()
            .map(|provider| provider.tracer(INSTRUMENTATION_SCOPE))
    }
}

/// Resolves one non-empty environment variable.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Whether Trellis-owned initialization is disabled by the environment.
fn sdk_disabled() -> bool {
    env_value("OTEL_SDK_DISABLED").is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// Whether the named exporter variable suppresses its signal.
fn exporter_disabled(get: impl Fn(&str) -> Option<String>, name: &str) -> bool {
    non_empty(get(name)).is_some_and(|value| value.eq_ignore_ascii_case("none"))
}

/// Whether the signal-specific or shared endpoint requests the signal.
fn endpoint_configured(get: impl Fn(&str) -> Option<String>, signal: &str) -> bool {
    non_empty(get(&format!("OTEL_EXPORTER_OTLP_{signal}_ENDPOINT"))).is_some()
        || non_empty(get("OTEL_EXPORTER_OTLP_ENDPOINT")).is_some()
}

/// Resolved signal selection for one process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SignalSelection {
    traces: bool,
    metrics: bool,
}

impl SignalSelection {
    /// Resolves which signals explicit, non-empty endpoint variables request.
    fn resolve(get: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            traces: !exporter_disabled(&get, "OTEL_TRACES_EXPORTER")
                && endpoint_configured(&get, "TRACES"),
            metrics: !exporter_disabled(&get, "OTEL_METRICS_EXPORTER")
                && endpoint_configured(&get, "METRICS"),
        }
    }
}

/// Trims one environment value to a non-empty string.
fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Applies the explicit 3-second default only when the environment is silent.
fn export_timeout() -> Option<Duration> {
    let configured = env_value("OTEL_EXPORTER_OTLP_TIMEOUT").is_some()
        || env_value("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT").is_some()
        || env_value("OTEL_EXPORTER_OTLP_METRICS_TIMEOUT").is_some();
    if configured {
        None
    } else {
        Some(DEFAULT_EXPORT_TIMEOUT)
    }
}

/// Resolves the trace sampler from the environment with a 0.10 default.
fn sampler_from_env() -> Sampler {
    let ratio = || {
        env_value("OTEL_TRACES_SAMPLER_ARG")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| (0.0..=1.0).contains(value))
            .unwrap_or(DEFAULT_TRACE_RATIO)
    };
    match env_value("OTEL_TRACES_SAMPLER").as_deref() {
        None => parent_based(DEFAULT_TRACE_RATIO),
        Some("always_on") => Sampler::AlwaysOn,
        Some("always_off") => Sampler::AlwaysOff,
        Some("traceidratio") => Sampler::TraceIdRatioBased(ratio()),
        Some("parentbased_traceidratio") => parent_based(ratio()),
        Some(other) => {
            warn_once(
                "sampler",
                &format!(
                    "unknown OTEL_TRACES_SAMPLER '{other}'; using parentbased_traceidratio {DEFAULT_TRACE_RATIO}"
                ),
            );
            parent_based(DEFAULT_TRACE_RATIO)
        }
    }
}

/// Parent-based wrapper over a trace-id ratio sampler.
fn parent_based(ratio: f64) -> Sampler {
    Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
}

/// Emits one bounded warning for a repeated environment problem.
fn warn_once(category: &str, message: &str) {
    use std::sync::OnceLock;
    static SAMPLER: OnceLock<()> = OnceLock::new();
    static OTHER: OnceLock<()> = OnceLock::new();
    let cell = match category {
        "sampler" => &SAMPLER,
        _ => &OTHER,
    };
    if cell.set(()).is_ok() {
        eprintln!("trellis telemetry: {message}");
    }
}

/// Builds the process resource including identity defaults and env overrides.
fn resource(identity: &TelemetryIdentity) -> Resource {
    let mut attributes = vec![
        KeyValue::new("service.name", identity.service_name.clone()),
        KeyValue::new("service.version", identity.service_version.clone()),
        KeyValue::new("service.namespace", "trellis"),
        KeyValue::new(
            "deployment.environment.name",
            env_value("TRELLIS_ENVIRONMENT").unwrap_or_else(|| "development".to_owned()),
        ),
        KeyValue::new(
            "trellis.cluster",
            env_value("TRELLIS_CLUSTER").unwrap_or_else(|| "local".to_owned()),
        ),
        KeyValue::new("trellis.role", identity.role.as_str()),
    ];
    if let Some(name) = env_value("OTEL_SERVICE_NAME") {
        set_resource_attribute(&mut attributes, "service.name", name);
    }
    if let Some(extra) = env_value("OTEL_RESOURCE_ATTRIBUTES") {
        for member in extra.split(',') {
            if let Some((key, value)) = member.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                if !key.is_empty() && !value.is_empty() {
                    set_resource_attribute(&mut attributes, key, value.to_owned());
                }
            }
        }
    }
    // Process identity honors a configured value, otherwise it is generated
    // exactly once per process and is never a credential.
    if env_value("OTEL_RESOURCE_ATTRIBUTES")
        .is_none_or(|extra| !extra.contains("service.instance.id="))
    {
        set_resource_attribute(
            &mut attributes,
            "service.instance.id",
            ulid::Ulid::new().to_string(),
        );
    }
    Resource::builder().with_attributes(attributes).build()
}

/// Replaces one resource attribute in place.
fn set_resource_attribute(attributes: &mut Vec<KeyValue>, key: &str, value: String) {
    if let Some(existing) = attributes
        .iter_mut()
        .find(|attribute| attribute.key.as_str() == key)
    {
        existing.value = value.into();
    } else {
        attributes.push(KeyValue::new(key.to_owned(), value));
    }
}

/// Resolves one bounded positive environment override in milliseconds.
fn positive_env_ms(name: &str) -> Option<u64> {
    env_value(name)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
}

/// Applies the order's trace batch defaults unless the environment overrides.
fn batch_config() -> opentelemetry_sdk::trace::BatchConfig {
    let mut builder = BatchConfigBuilder::default();
    if positive_env_ms("OTEL_BSP_MAX_QUEUE_SIZE").is_none() {
        builder = builder.with_max_queue_size(TRACE_QUEUE_SIZE);
    }
    if positive_env_ms("OTEL_BSP_MAX_EXPORT_BATCH_SIZE").is_none() {
        builder = builder.with_max_export_batch_size(TRACE_BATCH_MAX);
    }
    if positive_env_ms("OTEL_BSP_SCHEDULE_DELAY").is_none() {
        builder = builder.with_scheduled_delay(TRACE_BATCH_DELAY);
    }
    builder.build()
}

/// Resolves the metrics export interval, honoring the standard override.
fn metrics_interval() -> Duration {
    positive_env_ms("OTEL_METRIC_EXPORT_INTERVAL")
        .map(Duration::from_millis)
        .unwrap_or(METRICS_INTERVAL)
}

/// Catalog span limits from the observability order.
const SPAN_ATTRIBUTE_LIMIT: u32 = 32;
const SPAN_EVENT_LIMIT: u32 = 16;
const SPAN_LINK_LIMIT: u32 = 4;
const SPAN_EVENT_ATTRIBUTE_LIMIT: u32 = 32;
const SPAN_LINK_ATTRIBUTE_LIMIT: u32 = 32;

/// Applies the catalog's span/event/link limits to the owned provider.
fn span_limits() -> SpanLimits {
    SpanLimits {
        max_events_per_span: SPAN_EVENT_LIMIT,
        max_attributes_per_span: SPAN_ATTRIBUTE_LIMIT,
        max_links_per_span: SPAN_LINK_LIMIT,
        max_attributes_per_event: SPAN_EVENT_ATTRIBUTE_LIMIT,
        max_attributes_per_link: SPAN_LINK_ATTRIBUTE_LIMIT,
    }
}

/// Builds and installs Trellis-owned providers once per process.
pub(crate) fn build_owned(identity: &TelemetryIdentity) -> OwnedProviders {
    if sdk_disabled() {
        return OwnedProviders {
            metrics_enabled: false,
            traces_enabled: false,
            tracer_provider: None,
            meter_provider: None,
            operation: operation_slot(),
        };
    }
    let resource = resource(identity);
    let mut owned = OwnedProviders {
        metrics_enabled: false,
        traces_enabled: false,
        tracer_provider: None,
        meter_provider: None,
        operation: operation_slot(),
    };

    let signals = SignalSelection::resolve(|name| std::env::var(name).ok());

    let traces_requested = signals.traces;
    if traces_requested {
        let mut builder = SpanExporter::builder().with_http();
        if let Some(timeout) = export_timeout() {
            builder = builder.with_timeout(timeout);
        }
        match builder.build() {
            Ok(exporter) => {
                let batch = batch_config();
                let provider = SdkTracerProvider::builder()
                    .with_resource(resource.clone())
                    .with_span_limits(span_limits())
                    .with_sampler(sampler_from_env())
                    .with_span_processor(
                        BatchSpanProcessor::builder(exporter)
                            .with_batch_config(batch)
                            .build(),
                    )
                    .build();
                opentelemetry::global::set_tracer_provider(provider.clone());
                owned.tracer_provider = Some(provider);
                owned.traces_enabled = true;
            }
            Err(error) => {
                eprintln!("trellis telemetry: trace export disabled: {error}");
            }
        }
    }

    let metrics_requested = signals.metrics;
    if metrics_requested {
        let mut builder = MetricExporter::builder().with_http();
        if let Some(timeout) = export_timeout() {
            builder = builder.with_timeout(timeout);
        }
        match builder.build() {
            Ok(exporter) => {
                let reader = PeriodicReader::builder(exporter)
                    .with_interval(metrics_interval())
                    .build();
                let provider = SdkMeterProvider::builder()
                    .with_resource(resource)
                    .with_reader(reader)
                    .build();
                opentelemetry::global::set_meter_provider(provider.clone());
                super::instruments::note_meter_provider_changed();
                owned.meter_provider = Some(provider);
                owned.metrics_enabled = true;
            }
            Err(error) => {
                eprintln!("trellis telemetry: metric export disabled: {error}");
            }
        }
    }

    owned
}

type InFlightOperation = std::sync::Arc<std::sync::Mutex<OperationSlotState>>;
type ExportWork = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct OperationSlotState {
    closing: bool,
    active: Option<std::sync::Arc<OperationState>>,
    pending_shutdown: Option<(
        std::sync::Arc<OperationState>,
        tokio::sync::watch::Sender<bool>,
        tokio::time::Instant,
        ExportWork,
    )>,
}

struct OperationState {
    completion: tokio::sync::watch::Receiver<bool>,
}

/// Starts work while holding the owner lock. A pending shutdown runs on the
/// same worker after its flush finishes, only if the shutdown deadline remains.
fn start_operation(
    slot: &InFlightOperation,
    work: ExportWork,
) -> Option<std::sync::Arc<OperationState>> {
    let (sender, receiver) = tokio::sync::watch::channel(false);
    let state = std::sync::Arc::new(OperationState {
        completion: receiver,
    });
    let worker_slot = std::sync::Arc::clone(slot);
    let worker_state = std::sync::Arc::clone(&state);
    let spawned = std::thread::Builder::new()
        .name("trellis-telemetry".to_owned())
        .spawn(move || {
            let mut current = worker_state;
            let mut work = work;
            let mut sender = sender;
            loop {
                work();
                let next = {
                    let mut owner = worker_slot.lock().expect("telemetry operation slot");
                    if owner
                        .active
                        .as_ref()
                        .is_some_and(|active| std::sync::Arc::ptr_eq(active, &current))
                    {
                        let pending = owner.pending_shutdown.take().and_then(
                            |(state, sender, deadline, work)| {
                                (tokio::time::Instant::now() < deadline)
                                    .then_some((state, sender, work))
                            },
                        );
                        owner.active = pending
                            .as_ref()
                            .map(|(state, _, _)| std::sync::Arc::clone(state));
                        pending
                    } else {
                        None
                    }
                };
                sender.send_replace(true);
                match next {
                    Some((state, next_sender, next_work)) => {
                        current = state;
                        work = next_work;
                        sender = next_sender;
                    }
                    None => break,
                }
            }
        });
    match spawned {
        Ok(_) => Some(state),
        Err(error) => {
            eprintln!("trellis telemetry: could not start exporter worker: {error}");
            None
        }
    }
}

async fn wait_operation(state: &OperationState, deadline: tokio::time::Instant, what: &str) {
    let mut completion = state.completion.clone();
    let wait = async {
        while !*completion.borrow_and_update() {
            if completion.changed().await.is_err() {
                break;
            }
        }
    };
    if tokio::time::timeout_at(deadline, wait).await.is_err() {
        eprintln!("trellis telemetry: {what} exceeded the telemetry shutdown budget");
    }
}

async fn lock_operation_until(
    slot: &InFlightOperation,
    deadline: tokio::time::Instant,
) -> Option<std::sync::MutexGuard<'_, OperationSlotState>> {
    loop {
        match slot.try_lock() {
            Ok(owner) => return Some(owner),
            Err(std::sync::TryLockError::Poisoned(_)) => return None,
            Err(std::sync::TryLockError::WouldBlock) if tokio::time::Instant::now() >= deadline => {
                return None;
            }
            Err(std::sync::TryLockError::WouldBlock) => tokio::task::yield_now().await,
        }
    }
}

async fn start_shutdown(
    slot: &InFlightOperation,
    deadline: tokio::time::Instant,
    work: ExportWork,
) -> Option<std::sync::Arc<OperationState>> {
    let mut owner = lock_operation_until(slot, deadline).await?;
    if owner.closing || tokio::time::Instant::now() >= deadline {
        return None;
    }
    owner.closing = true;
    if owner.active.is_some() {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let state = std::sync::Arc::new(OperationState {
            completion: receiver,
        });
        owner.pending_shutdown = Some((std::sync::Arc::clone(&state), sender, deadline, work));
        Some(state)
    } else {
        let state = start_operation(slot, work)?;
        owner.active = Some(std::sync::Arc::clone(&state));
        Some(state)
    }
}

/// Flushes owned providers within the bounded process budget.
pub(crate) async fn flush_bounded(providers: &OwnedProviders) {
    let deadline = tokio::time::Instant::now() + SHUTDOWN_BUDGET;
    let tracer = providers.tracer_provider.clone();
    let meter = providers.meter_provider.clone();
    if tracer.is_none() && meter.is_none() {
        return;
    }
    let state = {
        let Some(mut owner) = lock_operation_until(&providers.operation, deadline).await else {
            return;
        };
        if owner.closing || tokio::time::Instant::now() >= deadline {
            return;
        }
        if let Some(active) = &owner.active {
            std::sync::Arc::clone(active)
        } else if let Some(state) = start_operation(
            &providers.operation,
            Box::new(move || {
                if let Some(provider) = tracer {
                    let _ = provider.force_flush();
                }
                if let Some(provider) = meter {
                    let _ = provider.force_flush();
                }
            }),
        ) {
            owner.active = Some(std::sync::Arc::clone(&state));
            state
        } else {
            return;
        }
    };
    wait_operation(&state, deadline, "flush").await;
}

/// Flushes and shuts down owned providers within the bounded process budget.
pub(crate) async fn shutdown_bounded(providers: &OwnedProviders) {
    let deadline = tokio::time::Instant::now() + SHUTDOWN_BUDGET;
    let tracer = providers.tracer_provider.clone();
    let meter = providers.meter_provider.clone();
    if tracer.is_none() && meter.is_none() {
        return;
    }
    // Owned shutdown happens once for the shared owner, even when guard clones
    // are dropped through several process stop paths. Host-owned providers are
    // never constructed here and are never shut down by Trellis.
    let work: ExportWork = Box::new(move || {
        if let Some(provider) = tracer {
            let _ = provider.shutdown();
        }
        if let Some(provider) = meter {
            let _ = provider.shutdown();
        }
    });
    let Some(state) = start_shutdown(&providers.operation, deadline, work).await else {
        return;
    };
    wait_operation(&state, deadline, "shutdown").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_overrides_replace_defaults() {
        let mut attributes = vec![KeyValue::new("service.name", "trellis-server")];
        set_resource_attribute(&mut attributes, "service.name", "orders-api".to_owned());
        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes[0].value, "orders-api".into());
        set_resource_attribute(&mut attributes, "trellis.role", "server".to_owned());
        assert_eq!(attributes.len(), 2);
    }

    #[tokio::test]
    async fn repeated_flushes_coalesce_into_one_operation() {
        let slot = operation_slot();
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first = std::sync::Arc::clone(&runs);
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let state = {
            let mut owner = slot.lock().unwrap();
            let state = start_operation(
                &slot,
                Box::new(move || {
                    first.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = blocked.recv();
                }),
            )
            .expect("worker");
            owner.active = Some(std::sync::Arc::clone(&state));
            state
        };
        let deadline = tokio::time::Instant::now() + Duration::from_millis(10);
        wait_operation(&state, deadline, "flush").await;
        assert!(
            slot.lock().unwrap().active.is_some(),
            "timeout retains owner"
        );
        let waiting = tokio::spawn({
            let state = std::sync::Arc::clone(&state);
            async move {
                wait_operation(
                    &state,
                    tokio::time::Instant::now() + Duration::from_secs(1),
                    "flush",
                )
                .await;
            }
        });
        waiting.abort();
        assert!(
            slot.lock().unwrap().active.is_some(),
            "cancellation retains owner"
        );
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        release.send(()).expect("release");
        wait_operation(
            &state,
            tokio::time::Instant::now() + Duration::from_secs(1),
            "flush",
        )
        .await;
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "concurrent flushes must share one operation"
        );
        assert!(
            slot.lock().unwrap().active.is_none(),
            "worker releases owner"
        );
    }

    #[tokio::test]
    async fn pending_shutdown_runs_after_flush_even_when_waiter_is_cancelled() {
        let slot = operation_slot();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let (started, observed) = std::sync::mpsc::channel::<()>();
        {
            let mut owner = slot.lock().unwrap();
            let flush = start_operation(
                &slot,
                Box::new(move || {
                    let _ = started.send(());
                    let _ = blocked.recv();
                }),
            )
            .expect("worker");
            owner.active = Some(flush);
        }
        observed.recv().expect("flush started");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&calls);
        let shutdown = start_shutdown(
            &slot,
            tokio::time::Instant::now() + Duration::from_secs(1),
            Box::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        )
        .await
        .expect("shutdown queued");
        assert!(start_shutdown(
            &slot,
            tokio::time::Instant::now() + Duration::from_secs(1),
            Box::new(|| panic!("second shutdown"))
        )
        .await
        .is_none());
        let waiter = tokio::spawn({
            let shutdown = std::sync::Arc::clone(&shutdown);
            async move {
                wait_operation(
                    &shutdown,
                    tokio::time::Instant::now() + Duration::from_secs(1),
                    "shutdown",
                )
                .await
            }
        });
        waiter.abort();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(slot.lock().unwrap().closing);
        assert!(slot.lock().unwrap().pending_shutdown.is_some());
        release.send(()).expect("release flush");
        wait_operation(
            &shutdown,
            tokio::time::Instant::now() + Duration::from_secs(1),
            "shutdown",
        )
        .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(slot.lock().unwrap().active.is_none());
        assert!(slot.lock().unwrap().pending_shutdown.is_none());
    }

    #[tokio::test]
    async fn expired_shutdown_is_not_started_after_stuck_flush_finishes() {
        let slot = operation_slot();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let flush = {
            let mut owner = slot.lock().unwrap();
            let flush = start_operation(
                &slot,
                Box::new(move || {
                    let _ = blocked.recv();
                }),
            )
            .expect("worker");
            owner.active = Some(std::sync::Arc::clone(&flush));
            flush
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&calls);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(20);
        let shutdown = start_shutdown(
            &slot,
            deadline,
            Box::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        )
        .await
        .expect("shutdown queued");
        wait_operation(&shutdown, deadline, "shutdown").await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        release.send(()).expect("release flush");
        wait_operation(
            &flush,
            tokio::time::Instant::now() + Duration::from_secs(1),
            "flush",
        )
        .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        {
            let owner = slot.lock().unwrap();
            assert!(owner.active.is_none());
            assert!(owner.pending_shutdown.is_none());
            assert!(
                owner.closing,
                "once-only shutdown cannot retry after expiration"
            );
        }
        assert!(start_shutdown(
            &slot,
            tokio::time::Instant::now() + Duration::from_secs(1),
            Box::new(|| panic!("late shutdown retry"))
        )
        .await
        .is_none());
    }

    #[test]
    fn sampler_defaults_are_parent_based() {
        assert!(matches!(
            parent_based(DEFAULT_TRACE_RATIO),
            Sampler::ParentBased(_)
        ));
    }

    #[test]
    fn explicit_endpoint_variables_enable_each_signal_independently() {
        let resolve = |variables: &[(&str, &str)]| {
            let values: std::collections::HashMap<&str, &str> = variables.iter().copied().collect();
            SignalSelection::resolve(|name| values.get(name).map(|value| (*value).to_owned()))
        };
        assert_eq!(
            resolve(&[]),
            SignalSelection {
                traces: false,
                metrics: false,
            }
        );
        assert_eq!(
            resolve(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318")]),
            SignalSelection {
                traces: true,
                metrics: true,
            }
        );
        assert_eq!(
            resolve(&[(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://collector:4318"
            )]),
            SignalSelection {
                traces: true,
                metrics: false,
            }
        );
        assert_eq!(
            resolve(&[(
                "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
                "http://collector:4318"
            )]),
            SignalSelection {
                traces: false,
                metrics: true,
            }
        );
        assert_eq!(
            resolve(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "   ")]),
            SignalSelection {
                traces: false,
                metrics: false,
            }
        );
        assert_eq!(
            resolve(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
                ("OTEL_TRACES_EXPORTER", "none"),
            ]),
            SignalSelection {
                traces: false,
                metrics: true,
            }
        );
    }
}
