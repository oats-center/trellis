//! Rust operation/worker growth acceptance provider leg.
//!
//! Serves the `transport_growth` api as a generated Rust service: the always
//! granted `Hold` operation parks inside its handler until the caller sends a
//! `Continue` signal, so the case harness can grow the service's optional
//! `extend` capability (and therefore its physical transport) while one
//! execution is in flight. Every execution counts itself exactly once, so the
//! harness can prove an accepted execution is neither restarted nor duplicated
//! across transport cutover and that a start after cutover is accepted.
//!
//! The service also hosts the `held` job worker: an initial job is submitted
//! before the connection accepts traffic and parks inside its handler. A real
//! transport cutover happens while that job is held; releasing it must complete
//! exactly once. Calling the grown `Extend` operation afterwards submits a fresh
//! job, which the generation-following worker intake accepts on the new
//! generation.

use futures_util::StreamExt as _;
use runtime_trellis::Int64;
use runtime_trellis::apis::runtime_trellis_transport_growth_v1::events::Tick;
use runtime_trellis::participants::runtime_trellis_transport_growth_operation_provider::{
    Participant as ProviderParticipant, Provider as OperationProvider, resources::Held,
};
use runtime_trellis::types::{Empty, Update, UpdateDetail, Value};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::generated::Codec;
use trellis_rs::jobs::JobProcessError;
use trellis_rs::service::{ServiceConnectOptions, ServiceEventListenOptions};

/// Number of `Hold` handler invocations observed for the process lifetime.
static EXECUTIONS: AtomicU64 = AtomicU64::new(0);

/// Number of `Park` RPC handler invocations observed for the process lifetime.
static PARKS: AtomicU64 = AtomicU64::new(0);

/// Number of `held` job handler invocations observed for the process lifetime.
static JOB_EXECUTIONS: AtomicU64 = AtomicU64::new(0);

/// Number of durable `Tick` deliveries observed for the process lifetime.
static TICK_DELIVERIES: AtomicU64 = AtomicU64::new(0);

/// Emits one flushable protocol line the harness can act on immediately.
fn emit(line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
    let _ = std::io::stdout().flush();
}

async fn hold_tick(
    value: String,
    mut release: tokio::sync::watch::Receiver<bool>,
    hold_only: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) {
    let delivery = TICK_DELIVERIES.fetch_add(1, Ordering::SeqCst) + 1;
    emit(&format!("TICK {delivery} {value}"));
    let parks = hold_only
        .lock()
        .map(|only| only.as_deref().is_none_or(|only| only == value))
        .unwrap_or(true);
    if parks {
        while !*release.borrow() {
            if release.changed().await.is_err() {
                break;
            }
        }
    }
    emit(&format!("TICK_DONE {delivery} {value}"));
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match run().await {
        Ok(()) => Ok(()),
        Err(error) => {
            emit(&format!("OPERATION_PROVIDER_ERROR {error}"));
            Err(error)
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut runtime = ProviderParticipant::connect(ServiceConnectOptions::new(&url, &seed)).await?;
    // Held RPC callbacks and the held job park until the harness releases them,
    // so a real transport cutover happens while they are in flight.
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    let concurrency = std::env::var("TRELLIS_JOB_CONCURRENCY")
        .ok()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(1);
    let (job_release_tx, job_release_rx) =
        tokio::sync::watch::channel(std::collections::BTreeSet::<String>::new());
    // The durable `ticks` delivery parks until released, holding the received
    // delivery across a real transport cutover before acking. The harness can
    // arm the hold for one value only, so it can complete a warmup delivery that
    // moves the intake onto the current generation before holding the real one;
    // unarmed, every delivery parks.
    let tick_release = release_rx.clone();
    let tick_hold_only: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let tick_hold_only_hook = tick_hold_only.clone();
    let (raw_ticks_start, mut raw_ticks_started) = tokio::sync::watch::channel(false);
    // Retaining an unpolled public consumer stream must neither reserve a
    // delivery from the shared durable consumer nor retain the original socket.
    let _idle_ticks = if std::env::var("TRELLIS_RAW_CONSUMER").as_deref() == Ok("1") {
        Some(
            OperationProvider::new(&mut runtime)
                .ticks()?
                .messages()
                .await?,
        )
    } else {
        None
    };
    let raw_ticks_task = if std::env::var("TRELLIS_RAW_CONSUMER").as_deref() == Ok("1") {
        let consumer = OperationProvider::new(&mut runtime).ticks()?;
        let release = tick_release.clone();
        let hold_only = tick_hold_only.clone();
        Some(tokio::spawn(async move {
            let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
                let mut messages = consumer.messages().await?;
                if raw_ticks_started
                    .wait_for(|started| *started)
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                while let Some(message) = messages.next().await {
                    let message = message?;
                    let event = Value::decode(std::str::from_utf8(message.payload())?.parse()?)?;
                    hold_tick(event.value.to_string(), release.clone(), hold_only.clone()).await;
                    message.ack().await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                emit(&format!("RAW_TICKS_ERROR {error}"));
            }
        }))
    } else {
        None
    };
    let ticks_handle = if raw_ticks_task.is_some() {
        None
    } else {
        Some(
            runtime
                .listen_event::<Tick, _, _>(
                    move |event, _| {
                        let release = tick_release.clone();
                        let hold_only = tick_hold_only_hook.clone();
                        let value = event.value.to_string();
                        async move {
                            hold_tick(value, release, hold_only).await;
                            Ok(())
                        }
                    },
                    ServiceEventListenOptions::default(),
                )
                .await?,
        )
    };
    {
        // The `held` job worker parks until released, counting each execution
        // exactly once so the harness can prove no restart or duplication.
        let job_release = release_rx.clone();
        runtime
            .register_generated_job_worker_with_concurrency::<Held, _, _, String>(
                move |job| {
                    let mut release = job_release.clone();
                    let mut individual = job_release_rx.clone();
                    let payload = job.payload().value.to_string();
                    async move {
                        let execution = JOB_EXECUTIONS.fetch_add(1, Ordering::SeqCst) + 1;
                        emit(&format!("JOB_STARTED {execution} {payload}"));
                        while !*release.borrow() && !individual.borrow().contains(&payload) {
                            tokio::select! {
                                result = release.changed() => { if result.is_err() { break; } }
                                result = individual.changed() => { if result.is_err() { break; } }
                            }
                        }
                        emit(&format!("JOB_DONE {execution} {payload}"));
                        Ok::<Value, JobProcessError<String>>(Value {
                            value: format!("held-{execution}"),
                            extra: Default::default(),
                        })
                    }
                },
                concurrency,
            )
            .await?;
        let mut provider = OperationProvider::new(&mut runtime);
        let mut api = provider.runtime_trellis_transport_growth_v1();
        api.register_advance(|_context, _input| async move {
            Ok(Empty {
                extra: Default::default(),
            })
        });
        api.register_extend(|_context, _input| async move {
            Ok(Empty {
                extra: Default::default(),
            })
        });
        api.register_nudge(|context, _input| async move {
            // A fresh job submitted after cutover must be accepted on the new
            // generation by the same logical worker host.
            let _ = context
                .handle()
                .generated_submit_job::<Held>(Value {
                    value: "extended".to_owned(),
                    extra: Default::default(),
                })
                .await;
            Ok(Empty {
                extra: Default::default(),
            })
        });
        let park_release = release_rx.clone();
        api.register_park(move |_context, _input| {
            let mut release = park_release.clone();
            async move {
                let park = PARKS.fetch_add(1, Ordering::SeqCst) + 1;
                while !*release.borrow() {
                    if release.changed().await.is_err() {
                        break;
                    }
                }
                Ok(Value {
                    value: format!("parked-{park}"),
                    extra: Default::default(),
                })
            }
        });
        api.register_hold(|_context, _input, op| async move {
            // One counted execution per accepted start. The handler parks until
            // the caller continues it, so the transport may cut over underneath.
            let execution = EXECUTIONS.fetch_add(1, Ordering::SeqCst) + 1;
            op.progress(Update {
                value: format!("running-{execution}"),
                nested: UpdateDetail {
                    count: Int64(0),
                    payload: Vec::new().into(),
                    extra: Default::default(),
                },
                extra: Default::default(),
            })
            .await?;
            let mut signals = op.signals().await?;
            while let Some(signal) = signals.next().await {
                let signal = signal?;
                if signal.signal == "Continue" {
                    op.acknowledge_signal(signal.signal_sequence).await?;
                    break;
                }
            }
            op.complete(Value {
                value: format!("completed-{execution}"),
                extra: Default::default(),
            })
            .await?;
            Ok(())
        });
    }
    let _ticks_handle = ticks_handle;
    let _raw_ticks_task = raw_ticks_task;
    let mut jobs = std::collections::BTreeMap::new();
    {
        // Submit the initial job before the connection serves traffic; it parks
        // in its handler until the harness releases it after cutover.
        let provider = OperationProvider::new(&mut runtime);
        match provider
            .submit_held(Value {
                value: "initial".to_owned(),
                extra: Default::default(),
            })
            .await
        {
            Ok(job) => {
                jobs.insert("initial".to_owned(), job);
                emit("JOB_SUBMITTED initial");
            }
            Err(error) => emit(&format!("JOB_SUBMIT_ERROR {error}")),
        }
    }
    emit("OPERATION_PROVIDER_READY");

    let job_submit = runtime.generated_handle();
    let commands = tokio::spawn(async move {
        let tick_hold_only = tick_hold_only;
        let mut observations = tokio::task::JoinSet::new();
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let command = line.trim();
            if command == "START_TICKS" {
                raw_ticks_start.send_replace(true);
                continue;
            }
            if let Some(value) = command.strip_prefix("JOB_RELEASE ") {
                job_release_tx.send_modify(|released| {
                    released.insert(value.to_owned());
                });
                continue;
            }
            if let Some(value) = command.strip_prefix("JOB_SUBMIT ") {
                match job_submit
                    .generated_submit_job::<Held>(Value {
                        value: value.to_owned(),
                        extra: Default::default(),
                    })
                    .await
                {
                    Ok(job) => {
                        jobs.insert(value.to_owned(), job);
                        emit(&format!("JOB_SUBMITTED {value}"));
                    }
                    Err(error) => emit(&format!("JOB_SUBMIT_ERROR {error}")),
                }
                continue;
            }
            if let Some(value) = command.strip_prefix("JOB_STATUS ") {
                if let Some(job) = jobs.get(value) {
                    match job.get().await {
                        Ok(snapshot) => {
                            emit(&format!("JOB_ID {value} {}", snapshot.id));
                            emit(&format!(
                                "JOB_STATUS {value} {:?} {} {}",
                                snapshot.state,
                                snapshot.tries,
                                snapshot
                                    .result
                                    .map(|result| result.value)
                                    .unwrap_or_default()
                            ));
                        }
                        Err(error) => emit(&format!("JOB_STATUS_ERROR {error}")),
                    }
                }
                continue;
            }
            if let Some((action, value)) = command.split_once(' ') {
                if matches!(action, "JOB_WAIT" | "JOB_CANCEL") {
                    if let Some(job) = jobs.get(value).cloned() {
                        let action = action.to_owned();
                        let value = value.to_owned();
                        observations.spawn(async move {
                            let result = if action == "JOB_WAIT" {
                                job.wait().await
                            } else {
                                job.cancel().await
                            };
                            match result {
                                Ok(snapshot) => emit(&format!(
                                    "{action} {value} {:?} {} {}",
                                    snapshot.state,
                                    snapshot.tries,
                                    snapshot
                                        .result
                                        .map(|result| result.value)
                                        .unwrap_or_default()
                                )),
                                Err(error) => emit(&format!("{action}_ERROR {value} {error}")),
                            }
                        });
                    }
                    continue;
                }
            }
            if let Some(value) = command.strip_prefix("HOLD_ONLY ") {
                if let Ok(mut only) = tick_hold_only.lock() {
                    *only = Some(value.to_string());
                }
                continue;
            }
            match command {
                "REFRESH" => match job_submit
                    .generated_client()
                    .refresh_authorization_context()
                    .await
                {
                    Ok(()) => emit("AUTH_REFRESHED"),
                    Err(error) => emit(&format!("AUTH_REFRESH_ERROR {error}")),
                },
                "AUTH_STATUS" => {
                    emit(&format!(
                        "AUTH_USABLE {}",
                        job_submit
                            .generated_client()
                            .availability()
                            .has_resource(trellis_rs::generated::ResourceKind::Job, "held")
                    ));
                }
                "COUNT" => {
                    emit(&format!(
                        "HOLD_EXECUTIONS {}",
                        EXECUTIONS.load(Ordering::SeqCst)
                    ));
                }
                "PARKS" => {
                    emit(&format!("PARK_CALLS {}", PARKS.load(Ordering::SeqCst)));
                }
                "JOB_COUNT" => {
                    emit(&format!(
                        "JOB_EXECUTIONS {}",
                        JOB_EXECUTIONS.load(Ordering::SeqCst)
                    ));
                }
                "RELEASE" | "JOB_RELEASE" => {
                    let _ = release_tx.send(true);
                }
                "EXIT" => {
                    emit("OPERATION_PROVIDER_DONE");
                    break;
                }
                "" => {}
                _ => {}
            }
        }
        while observations.join_next().await.is_some() {}
    });

    tokio::select! {
        result = runtime.run() => {
            result?;
        }
        _ = commands => {}
    }
    Ok(())
}
