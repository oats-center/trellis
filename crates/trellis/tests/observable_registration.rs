//! A metric collection that runs while gauge families register must not
//! deadlock. The SDK runs observable callbacks while holding its pipeline lock,
//! so registration must never build an instrument under the lock those
//! callbacks take. Its own process: families bind once per meter provider.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use opentelemetry_sdk::metrics::{InMemoryMetricExporterBuilder, SdkMeterProvider};
use trellis_rs::telemetry::instruments::{
    note_meter_provider_changed, register_observable, ObservableFamily,
};

#[test]
fn gauge_families_register_while_metrics_are_collected() {
    use ObservableFamily as F;
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(InMemoryMetricExporterBuilder::new().build())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    note_meter_provider_changed();

    // A slow source keeps each collection inside the SDK's callback loop.
    let slow = register_observable(
        F::ConnectionCount,
        Arc::new(|| {
            std::thread::sleep(Duration::from_millis(2));
            vec![(1.0, Vec::new())]
        }),
    );
    let collecting = Arc::new(AtomicBool::new(true));
    let collector = {
        let (provider, collecting) = (provider.clone(), Arc::clone(&collecting));
        std::thread::spawn(move || {
            while collecting.load(Ordering::Relaxed) {
                let _ = provider.force_flush();
            }
        })
    };

    let (done, registered) = mpsc::channel();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for family in [
            F::AuthCoverageCount,
            F::AuthPostCommitPending,
            F::AuthPostCommitOldestAge,
            F::JobsReady,
            F::JobsOldestReadyAge,
            F::JobsDead,
            F::JobsWorkerRegistrations,
            F::JobsWaitingRetry,
            F::ConsumerPending,
            F::ConsumerAckPending,
            F::ConsumerMissing,
            F::ConsumerProgressAge,
            F::DeadLetters,
            F::ProjectionPending,
            F::ProjectionProgressAge,
            F::ResourceBindings,
            F::ComponentReady,
            F::ComponentObservedTime,
            F::SnapshotObservedTime,
        ] {
            held.push(register_observable(
                family,
                Arc::new(|| vec![(1.0, Vec::new())]),
            ));
            std::thread::sleep(Duration::from_millis(3));
        }
        let _ = done.send(held.len());
    });
    let outcome = registered.recv_timeout(Duration::from_secs(20));
    collecting.store(false, Ordering::Relaxed);
    if outcome != Ok(19) {
        // Dropping the registration would wait on the deadlocked lock.
        std::mem::forget(slow);
        panic!("registration deadlocked against a running collection");
    }
    collector.join().unwrap();
}
