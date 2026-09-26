use futures_util::StreamExt;
use runtime_trellis::participants::runtime_trellis_operation_provider::types::KeyedValue;
use runtime_trellis::participants::runtime_trellis_operation_provider::{Participant, Provider};
use runtime_trellis::types::{Update, UpdateDetail, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::Notify;
use tracing_subscriber::prelude::*;
use trellis_rs::jobs::JobProcessError;
use trellis_rs::service::ServiceConnectOptions;
use trellis_rs::telemetry::{init_from_env, TelemetryIdentity, TelemetryRole};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = init_from_env(TelemetryIdentity::new(
        "runtime-rust-provider",
        TelemetryRole::Service,
        env!("CARGO_PKG_VERSION"),
    ));
    tracing_subscriber::registry()
        .with(
            telemetry
                .tracer()
                .map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer)),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(tracing_subscriber::EnvFilter::from_default_env()),
        )
        .init();
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut service = Participant::connect(ServiceConnectOptions::new(&url, &identity)).await?;
    let mut provider = Provider::new(&mut service);
    provider
        .runtime_trellis_runtime_v1()
        .register_echo(|_, input| async move {
            assert_eq!(input.value, "from TypeScript");
            Ok(Value {
                value: format!("Rust received {}", input.value),
                extra: Default::default(),
            })
        });
    let release_first = Arc::new(Notify::new());
    let first_runs = Arc::new(AtomicUsize::new(0));
    let second_runs = Arc::new(AtomicUsize::new(0));
    let retry_runs = Arc::new(AtomicUsize::new(0));
    provider
        .register_keyed_work_with_concurrency(2, {
            let release_first = Arc::clone(&release_first);
            let first_runs = Arc::clone(&first_runs);
            let second_runs = Arc::clone(&second_runs);
            let retry_runs = Arc::clone(&retry_runs);
            move |job| {
                let release_first = Arc::clone(&release_first);
                let first_runs = Arc::clone(&first_runs);
                let second_runs = Arc::clone(&second_runs);
                let retry_runs = Arc::clone(&retry_runs);
                async move {
                    match job.payload().value.as_str() {
                        "first" => {
                            first_runs.fetch_add(1, Ordering::SeqCst);
                            release_first.notified().await;
                        }
                        "second" => {
                            second_runs.fetch_add(1, Ordering::SeqCst);
                        }
                        "retry" if retry_runs.fetch_add(1, Ordering::SeqCst) == 0 => {
                            return Err(JobProcessError::retryable("expected retry".to_owned()));
                        }
                        "retry" => {}
                        value => {
                            return Err(JobProcessError::failed(format!("unexpected {value}")))
                        }
                    }
                    Ok(job.payload().clone())
                }
            }
        })
        .await?;
    let first = provider
        .submit_keyed_work(KeyedValue {
            key: "shared".to_owned(),
            value: "first".to_owned(),
            extra: Default::default(),
        })
        .await?;
    loop {
        if first.get().await?.state == trellis_rs::jobs::JobState::Active {
            break;
        }
        tokio::task::yield_now().await;
    }
    let second = provider
        .submit_keyed_work(KeyedValue {
            key: "shared".to_owned(),
            value: "second".to_owned(),
            extra: Default::default(),
        })
        .await?;
    assert!(tokio::time::timeout(Duration::from_secs(26), second.wait())
        .await
        .is_err());
    let blocked = second.get().await?;
    assert_eq!(blocked.state, trellis_rs::jobs::JobState::Pending);
    assert_eq!(blocked.tries, 0);
    assert_eq!(second_runs.load(Ordering::SeqCst), 0);
    release_first.notify_one();
    first.wait().await?;
    second.wait().await?;
    assert_eq!(first_runs.load(Ordering::SeqCst), 1);
    assert_eq!(second_runs.load(Ordering::SeqCst), 1);

    let retry = provider
        .submit_keyed_work(KeyedValue {
            key: "retry".to_owned(),
            value: "retry".to_owned(),
            extra: Default::default(),
        })
        .await?;
    retry.wait().await?;
    assert_eq!(retry_runs.load(Ordering::SeqCst), 2);
    if std::env::var_os("TRELLIS_FEED_EMPTY").is_some() {
        provider
            .runtime_trellis_runtime_v1()
            .register_watch(|_, _| futures_util::stream::empty());
    } else {
        provider
            .runtime_trellis_runtime_v1()
            .register_watch(|_, _| {
                futures_util::stream::unfold(0_u64, |frame| async move {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    Some((
                        Ok(Value {
                            value: format!("rust-feed-{frame}"),
                            extra: Default::default(),
                        }),
                        frame + 1,
                    ))
                })
            });
    }
    provider
        .runtime_trellis_runtime_v1()
        .register_work(|context, input, op| async move {
            if input.value == "failure" {
                return Err(trellis_rs::service::ServerError::Nats(
                    "domain failure".to_owned(),
                ));
            }
            if !context.resuming {
                op.progress(Update {
                    value: "persisted".to_owned(),
                    nested: UpdateDetail {
                        count: runtime_trellis::Int64(9_007_199_254_740_993),
                        payload: vec![1, 2, 3].into(),
                        extra: Default::default(),
                    },
                    extra: Default::default(),
                })
                .await?;
            } else if context
                .operation_progress
                .as_ref()
                .and_then(|value| value.get("value"))
                .and_then(|value| value.as_str())
                != Some("persisted")
            {
                return Err(trellis_rs::service::ServerError::Nats(
                    "missing resumed progress".to_owned(),
                ));
            }
            if context.resuming {
                op.complete(Value {
                    value: "resumed".to_owned(),
                    extra: Default::default(),
                })
                .await?;
                return Ok(());
            }
            let mut signals = op.signals().await?;
            let signal = signals.next().await.transpose()?.ok_or_else(|| {
                trellis_rs::service::ServerError::Nats("signal stream ended".to_owned())
            })?;
            if input.value == "reconnect-live" {
                op.emit_update(Update {
                    value: "transient".to_owned(),
                    nested: UpdateDetail {
                        count: runtime_trellis::Int64(9_007_199_254_740_993),
                        payload: vec![4, 5, 6].into(),
                        extra: Default::default(),
                    },
                    extra: Default::default(),
                })
                .await?;
                op.acknowledge_signal(signal.signal_sequence).await?;
                let _ = signals.next().await.transpose()?;
                return Ok(());
            }
            op.acknowledge_signal(signal.signal_sequence).await?;
            op.complete(Value {
                value: if context.resuming {
                    "resumed"
                } else {
                    "completed"
                }
                .to_owned(),
                extra: Default::default(),
            })
            .await?;
            Ok(())
        });
    provider
        .runtime_trellis_runtime_v1()
        .register_upload(|context, input, op| async move {
            if context.resuming {
                let upload = op.upload().await?.ok_or_else(|| {
                    trellis_rs::service::ServerError::Nats("committed upload missing".to_owned())
                })?;
                op.complete(Value {
                    value: format!("{}:{}:{}", input.value, upload.size, context.resuming),
                    extra: Default::default(),
                })
                .await?;
                return Ok(());
            }
            let mut signals = op.signals().await?;
            let signal = signals.next().await.transpose()?.ok_or_else(|| {
                trellis_rs::service::ServerError::Nats("signal stream ended".to_owned())
            })?;
            op.acknowledge_signal(signal.signal_sequence).await?;
            let upload = op.upload().await?.ok_or_else(|| {
                trellis_rs::service::ServerError::Nats("committed upload missing".to_owned())
            })?;
            op.complete(Value {
                value: format!("{}:{}:{}", input.value, upload.size, context.resuming),
                extra: Default::default(),
            })
            .await?;
            Ok(())
        });
    service.run().await?;
    telemetry.shutdown().await;
    Ok(())
}
