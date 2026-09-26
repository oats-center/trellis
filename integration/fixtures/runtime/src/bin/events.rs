use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runtime_trellis::apis::runtime_trellis_events_v1::events::{Alpha, Beta};
use runtime_trellis::apis::runtime_trellis_events_v1::rpc::ObservedOutput;
use runtime_trellis::participants::runtime_trellis_event_service::{Participant, Provider};
use runtime_trellis::types::{Empty, Sample};
use tracing_subscriber::prelude::*;
use trellis_rs::generated::EventDescriptor;
use trellis_rs::service::{
    ServerError, ServiceConnectOptions, ServiceEventListenOptions, ServiceEventListenerMode,
};
use trellis_rs::telemetry::{init_from_env, TelemetryIdentity, TelemetryRole};

#[derive(Default)]
struct DeliveryStats {
    attempts: Vec<i64>,
    active: i64,
    max_active: i64,
    successes: Vec<String>,
    attempts_by_value: HashMap<String, usize>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = init_from_env(TelemetryIdentity::new(
        "events-rust",
        TelemetryRole::Service,
        env!("CARGO_PKG_VERSION"),
    ));
    tracing_subscriber::registry()
        .with(
            telemetry
                .tracer()
                .map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer)),
        )
        .init();
    assert_eq!(
        Beta::publish_subject(&Sample {
            site: "rust".to_owned(),
            value: "payload".to_owned(),
            extra: Default::default(),
        })?,
        "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Beta"
    );
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    if std::env::var("CONSUMER_ONLY").as_deref() == Ok("true") {
        return run_consumer_only(&url, &identity).await;
    }
    let mut service = Participant::connect(ServiceConnectOptions::new(&url, &identity)).await?;
    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let stats = Arc::new(Mutex::new(DeliveryStats::default()));
    let event_options = if std::env::var("EPHEMERAL").as_deref() == Ok("true") {
        ServiceEventListenOptions {
            mode: ServiceEventListenerMode::Ephemeral,
            group: None,
        }
    } else {
        ServiceEventListenOptions::default()
    };
    let alpha = || {
        let seen = Arc::clone(&seen);
        let stats = Arc::clone(&stats);
        service.listen_event::<Alpha, _, _>(
            move |event, _| {
                let seen = Arc::clone(&seen);
                let stats = Arc::clone(&stats);
                async move {
                    let attempt = {
                        let mut stats = stats.lock().unwrap();
                        stats.attempts.push(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as i64,
                        );
                        stats.active += 1;
                        stats.max_active = stats.max_active.max(stats.active);
                        let attempt = stats
                            .attempts_by_value
                            .entry(event.value.clone())
                            .or_default();
                        *attempt += 1;
                        *attempt
                    };
                    if event.value == "slow" {
                        tokio::time::sleep(Duration::from_millis(600)).await;
                    } else if event.value == "crash"
                        && std::env::var("CRASH").as_deref() == Ok("true")
                    {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                    let fails = event.value == "fail" || (event.value == "replay" && attempt <= 3);
                    let mut stats = stats.lock().unwrap();
                    stats.active -= 1;
                    if fails {
                        return Err(ServerError::Nats("fixture handler failure".to_owned()));
                    }
                    stats.successes.push(event.value.clone());
                    seen.lock().unwrap().insert(event.value);
                    Ok(())
                }
            },
            event_options.clone(),
        )
    };
    let beta = || {
        let seen = Arc::clone(&seen);
        service.listen_event::<Beta, _, _>(
            move |event, _| {
                seen.lock().unwrap().insert(event.value);
                async { Ok(()) }
            },
            event_options.clone(),
        )
    };
    let (alpha, _beta) = if std::env::var("REVERSE")?.parse::<bool>()? {
        let beta = beta().await?;
        (alpha().await?, beta)
    } else {
        (alpha().await?, beta().await?)
    };
    let alpha = Arc::new(Mutex::new(Some(alpha)));
    let mut provider = Provider::new(&mut service);
    provider
        .runtime_trellis_events_v1()
        .register_drop_alpha(move |_, _| {
            alpha.lock().unwrap().take();
            async {
                Ok(Empty {
                    extra: Default::default(),
                })
            }
        });
    let stats_response = Arc::clone(&stats);
    provider
        .runtime_trellis_events_v1()
        .register_delivery_stats(move |_, _| {
            let stats = stats_response.lock().unwrap();
            let output = runtime_trellis::types::DeliveryStats {
                attempts: stats.attempts.iter().copied().map(Into::into).collect(),
                active: stats.active.into(),
                max_active: stats.max_active.into(),
                successes: stats.successes.clone(),
                extra: Default::default(),
            };
            async move { Ok(output) }
        });
    provider
        .runtime_trellis_events_v1()
        .register_observed(move |_, _| {
            let values = seen.lock().unwrap().iter().cloned().collect();
            async {
                Ok(ObservedOutput {
                    values,
                    extra: Default::default(),
                })
            }
        });
    service.run().await?;
    telemetry.shutdown().await;
    Ok(())
}

/// Consumer-only participant: a declared durable consumer grants no raw
/// Event Subscribe authority, so explicit ephemeral must fail fast.
async fn run_consumer_only(url: &str, identity: &str) -> Result<(), Box<dyn std::error::Error>> {
    use runtime_trellis::participants::runtime_trellis_event_service_consumer_only::Participant as ConsumerOnly;
    let service = ConsumerOnly::connect(ServiceConnectOptions::new(url, identity)).await?;
    let outcome = service
        .listen_event::<Alpha, _, _>(
            |_event, _| async { Ok(()) },
            ServiceEventListenOptions {
                mode: ServiceEventListenerMode::Ephemeral,
                group: None,
            },
        )
        .await;
    match outcome {
        Ok(_) => println!("EPHEMERAL_ACCEPTED"),
        Err(error) => println!("EPHEMERAL_REJECTED {error}"),
    }
    Ok(())
}
