//! Exactly-once observation guards for duration and inflight metrics.
//!
//! A guard records one result at its named boundary. Dropping a guard without
//! an explicit finish records the configured cancelled/interrupted outcome, so
//! `?`, panic, or dropped futures cannot silently skip the observation. Guards
//! never change control flow and cannot turn a business failure into a success.

use std::time::{Duration, Instant};

use opentelemetry::KeyValue;

use super::instruments::{
    add_updown, record_family_duration, DurationFamily, ObservationRegistration, UpDownFamily,
};

/// Duration observation for one synchronous or asynchronous unit of work.
pub struct Observation {
    started: Instant,
    family: DurationFamily,
    attributes: Vec<KeyValue>,
    drop_outcome: &'static str,
    inflight: Option<InflightGuard>,
    finished: bool,
}

impl Observation {
    /// Starts one un-counted duration observation.
    pub fn start(
        family: DurationFamily,
        attributes: Vec<KeyValue>,
        drop_outcome: &'static str,
    ) -> Self {
        Self {
            started: Instant::now(),
            family,
            attributes,
            drop_outcome,
            inflight: None,
            finished: false,
        }
    }

    /// Starts one duration observation plus an inflight up/down increment.
    pub fn start_counted(
        family: DurationFamily,
        inflight: UpDownFamily,
        attributes: Vec<KeyValue>,
        drop_outcome: &'static str,
    ) -> Self {
        let guard = InflightGuard::acquire(inflight, attributes.clone());
        Self {
            started: Instant::now(),
            family,
            attributes,
            drop_outcome,
            inflight: Some(guard),
            finished: false,
        }
    }

    /// Monotonic elapsed time since the observation started.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Finishes the observation with one bounded outcome value.
    pub fn finish(self, outcome: &'static str) {
        self.finish_with(outcome, &[]);
    }

    /// Finishes the observation with an outcome and extra attributes.
    pub fn finish_with(mut self, outcome: &'static str, extra: &[KeyValue]) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut attributes = std::mem::take(&mut self.attributes);
        attributes.push(KeyValue::new("trellis.outcome", outcome));
        attributes.extend_from_slice(extra);
        record_family_duration(self.family, self.started.elapsed(), &attributes);
        if let Some(inflight) = self.inflight.take() {
            drop(inflight);
        }
    }

    /// Releases the observation without recording a duration sample.
    ///
    /// Used when a boundary turns out not to own the metric, for example a
    /// long-lived stream response that must not count as a unary RPC.
    pub fn discard(mut self) {
        self.finished = true;
        self.attributes.clear();
        if let Some(inflight) = self.inflight.take() {
            drop(inflight);
        }
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut attributes = std::mem::take(&mut self.attributes);
        attributes.push(KeyValue::new("trellis.outcome", self.drop_outcome));
        record_family_duration(self.family, self.started.elapsed(), &attributes);
        if let Some(inflight) = self.inflight.take() {
            drop(inflight);
        }
    }
}

/// Inflight up/down counter increment released exactly once.
pub struct InflightGuard {
    family: UpDownFamily,
    attributes: Vec<KeyValue>,
    released: bool,
}

impl InflightGuard {
    /// Increments the family and returns the guard that decrements it.
    pub fn acquire(family: UpDownFamily, attributes: Vec<KeyValue>) -> Self {
        add_updown(family, 1, &attributes);
        Self {
            family,
            attributes,
            released: false,
        }
    }

    /// Releases the increment early; dropping has the same effect once.
    pub fn release(mut self) {
        if !self.released {
            self.released = true;
            add_updown(self.family, -1, &self.attributes);
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            add_updown(self.family, -1, &self.attributes);
        }
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[tokio::test]
    async fn process_connection_gauge_survives_registration_churn() {
        let _guard = crate::telemetry::capture::meter_test_lock().await;
        let capture = crate::telemetry::capture::process_capture();
        // The production entry point retains its registration for the process
        // lifetime; a connection appearing and disappearing must still leave a
        // live sample rather than removing the family callback.
        let connection = ConnectionRegistration::start("service");
        connection.usable();
        capture.flush();
        let samples = capture.points_for("trellis.connection.count");
        assert!(
            samples.iter().any(|point| point
                .attributes
                .iter()
                .any(|(key, value)| { key == "trellis.state" && value == "usable" })),
            "a usable connection must export a live count: {samples:?}"
        );
        drop(connection);
        capture.flush();
        let after = capture.points_for("trellis.connection.count");
        assert!(
            after
                .iter()
                .all(|point| point.attributes.iter().all(|(_, value)| value != "usable")),
            "a closed connection must stop contributing usable count: {after:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_observations_finish_once() {
        let observation = Observation::start(
            DurationFamily::RpcClient,
            vec![KeyValue::new("trellis.route", "_unknown")],
            "cancelled",
        );
        drop(observation);
    }

    #[test]
    fn explicit_finish_is_idempotent_under_drop() {
        let observation = Observation::start(
            DurationFamily::Cli,
            vec![KeyValue::new("trellis.command", "whoami")],
            "cancelled",
        );
        observation.finish("ok");
    }

    #[test]
    fn inflight_guard_releases_early_once() {
        let guard = InflightGuard::acquire(
            UpDownFamily::RpcServerInflight,
            vec![KeyValue::new("trellis.route", "_unknown")],
        );
        guard.release();
    }
}

/// One process-local connection registration for observable state gauges.
///
/// The registry holds only a kind and current state per connection; it never
/// holds a credential, session, or connection handle, and it creates no
/// per-connection metric series.
pub struct ConnectionRegistration {
    id: u64,
    kind: &'static str,
}

type ConnectionStates =
    std::sync::Mutex<std::collections::BTreeMap<u64, (&'static str, &'static str)>>;

static CONNECTIONS: std::sync::OnceLock<ConnectionStates> = std::sync::OnceLock::new();
static NEXT_CONNECTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Returns the process-local connection state registry.
fn registry() -> &'static ConnectionStates {
    CONNECTIONS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Process-lifetime aggregate gauge registration.
///
/// The registration must outlive the process registry: dropping it would
/// silently remove the family callback while the registry keeps updating.
static CONNECTION_GAUGE: std::sync::OnceLock<ObservationRegistration> = std::sync::OnceLock::new();

/// Registers the connection count gauge once per process.
fn ensure_connection_gauge() {
    CONNECTION_GAUGE.get_or_init(|| {
        super::instruments::register_observable(
            super::instruments::ObservableFamily::ConnectionCount,
            std::sync::Arc::new(|| {
                let Ok(states) = registry().lock() else {
                    return Vec::new();
                };
                let mut counts: std::collections::BTreeMap<(&'static str, &'static str), u64> =
                    std::collections::BTreeMap::new();
                for (kind, state) in states.values() {
                    *counts.entry((*kind, *state)).or_default() += 1;
                }
                counts
                    .into_iter()
                    .map(|((kind, state), count)| {
                        (
                            count as f64,
                            vec![
                                KeyValue::new("trellis.participant.kind", kind),
                                KeyValue::new("trellis.state", state),
                            ],
                        )
                    })
                    .collect()
            }),
        )
    });
}

impl ConnectionRegistration {
    /// Registers one connection in the `connecting` state.
    pub fn start(kind: &'static str) -> Self {
        ensure_connection_gauge();
        let id = NEXT_CONNECTION_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut states) = registry().lock() {
            states.insert(id, (kind, "connecting"));
        }
        Self { id, kind }
    }

    /// Records a completed successful installation.
    pub fn usable(&self) {
        self.transition("usable", "connected");
    }

    /// Records suspended coverage.
    pub fn suspended(&self, reason: &'static str) {
        self.transition("suspended", reason);
    }

    /// Records one observed lifecycle transition.
    fn transition(&self, state: &'static str, reason: &'static str) {
        if let Ok(mut states) = registry().lock() {
            states.insert(self.id, (self.kind, state));
        }
        super::instruments::add_counter(
            super::instruments::CounterFamily::ConnectionTransitions,
            1,
            &[
                KeyValue::new("trellis.participant.kind", self.kind),
                KeyValue::new("trellis.reason", reason),
            ],
        );
    }
}

impl Drop for ConnectionRegistration {
    fn drop(&mut self) {
        let state = registry()
            .lock()
            .ok()
            .and_then(|mut states| states.remove(&self.id))
            .map(|(_, state)| state);
        if state.is_some_and(|state| state != "terminal") {
            super::instruments::add_counter(
                super::instruments::CounterFamily::ConnectionTransitions,
                1,
                &[
                    KeyValue::new("trellis.participant.kind", self.kind),
                    KeyValue::new("trellis.reason", "closed"),
                ],
            );
        }
    }
}
