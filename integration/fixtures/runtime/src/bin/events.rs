use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runtime_trellis::apis::runtime_trellis_events_v1::events::{Alpha, Beta};
use runtime_trellis::apis::runtime_trellis_events_v1::rpc::ObservedOutput;
use runtime_trellis::participants::runtime_trellis_event_service::{Participant, Provider};
use runtime_trellis::types::{Empty, Sample};
use tracing_subscriber::prelude::*;
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
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .with(
            telemetry
                .tracer()
                .map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer)),
        )
        .init();
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    if std::env::var("PUBLIC_BACKLOG").as_deref() == Ok("true") {
        return run_public_backlog(&url, &identity).await;
    }
    if std::env::var("CONSUMER_ONLY").as_deref() == Ok("true") {
        return run_consumer_only(&url, &identity).await;
    }
    let server =
        trellis_rs::auth::discover_native_oauth(&url, &trellis_runtime_acceptance::trust_roots())
            .await?;
    let mut service = Participant::connect(ServiceConnectOptions::new(
        server.nats_servers,
        server.trust,
        &identity,
    ))
    .await?;
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

/// Publish before a real lease boundary, then pull that publication through the generated consumer.
async fn run_public_backlog(url: &str, identity: &str) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::StreamExt as _;
    use runtime_trellis::apis::runtime_trellis_events_v1::Client as EventsClient;
    use runtime_trellis::participants::runtime_trellis_event_service_both::{Client, Participant};
    use tokio::io::{AsyncBufReadExt as _, BufReader};

    let server =
        trellis_rs::auth::discover_native_oauth(url, &trellis_runtime_acceptance::trust_roots())
            .await?;
    let service = Participant::connect(ServiceConnectOptions::new(
        server.nats_servers,
        server.trust,
        identity,
    ))
    .await?;
    let owner = service.runtime_native_owner();
    let authority = owner.runtime_authorization_owner();
    let published = authority.own_authority()?;
    let publisher = EventsClient::from_generated(service.generated_client());
    let input = Sample {
        site: "retained-public-rust".into(),
        value: "before-expiry".into(),
        extra: Default::default(),
    };
    publisher.publish_alpha(&input).await?;
    assert_eq!(authority.own_authority()?.digest(), published.digest());
    println!(
        "PUBLIC_BACKLOG_READY {}",
        serde_json::json!({"expiresAt": published.authority().expires_at})
    );
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    assert_eq!(commands.next_line().await?.as_deref(), Some("PULL"));
    assert!(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64
            >= published.authority().expires_at
    );
    assert_ne!(authority.own_authority()?.digest(), published.digest());
    let client = Client::from_generated(service.generated_client());
    let mut backlog = client.events()?.subscribe::<Alpha>().await?;
    let received = tokio::time::timeout(Duration::from_secs(10), backlog.next())
        .await?
        .ok_or("backlog ended before publication")??;
    assert_eq!(received.value, input.value);
    assert_eq!(received.site, input.site);
    println!("PUBLIC_BACKLOG_DELIVERED");
    drop(backlog);
    owner.shutdown_native_runtime().await?;
    Ok(())
}

/// Consumer-only participant: a declared durable consumer grants no raw
/// Event Subscribe authority, so explicit ephemeral must fail fast.
async fn run_consumer_only(url: &str, identity: &str) -> Result<(), Box<dyn std::error::Error>> {
    use runtime_trellis::participants::runtime_trellis_event_service_consumer_only::Participant as ConsumerOnly;
    let server =
        trellis_rs::auth::discover_native_oauth(url, &trellis_runtime_acceptance::trust_roots())
            .await?;
    let service = ConsumerOnly::connect(ServiceConnectOptions::new(
        server.nats_servers,
        server.trust,
        identity,
    ))
    .await?;
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
