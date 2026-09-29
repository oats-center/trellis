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
use runtime_trellis::apis::runtime_trellis_transport_growth_v1::events::Tick;
use runtime_trellis::participants::runtime_trellis_transport_growth_operation_provider::{
    resources::Held, Participant as ProviderParticipant, Provider as OperationProvider,
};
use runtime_trellis::types::{Empty, Update, UpdateDetail, Value};
use runtime_trellis::Int64;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt as _, BufReader};
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
    // The durable `ticks` delivery parks until released, holding the received
    // delivery across a real transport cutover before acking. The harness can
    // arm the hold for one value only, so it can complete a warmup delivery that
    // moves the intake onto the current generation before holding the real one;
    // unarmed, every delivery parks.
    let tick_release = release_rx.clone();
    let tick_hold_only: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let tick_hold_only_hook = tick_hold_only.clone();
    let ticks_handle = runtime
        .listen_event::<Tick, _, _>(
            move |event, _| {
                let mut release = tick_release.clone();
                let hold_only = tick_hold_only_hook.clone();
                let value = event.value.to_string();
                async move {
                    let delivery = TICK_DELIVERIES.fetch_add(1, Ordering::SeqCst) + 1;
                    emit(&format!("TICK {delivery} {value}"));
                    let parks = hold_only
                        .lock()
                        .map(|only| only.as_deref().map_or(true, |only| only == value))
                        .unwrap_or(true);
                    if parks {
                        while !*release.borrow() {
                            if release.changed().await.is_err() {
                                break;
                            }
                        }
                    }
                    emit(&format!("TICK_DONE {delivery} {value}"));
                    Ok(())
                }
            },
            ServiceEventListenOptions::default(),
        )
        .await?;
    {
        let mut provider = OperationProvider::new(&mut runtime);
        // The `held` job worker parks until released, counting each execution
        // exactly once so the harness can prove no restart or duplication.
        let job_release = release_rx.clone();
        provider
            .register_held(move |job| {
                let mut release = job_release.clone();
                let payload = job.payload().value.to_string();
                async move {
                    let execution = JOB_EXECUTIONS.fetch_add(1, Ordering::SeqCst) + 1;
                    emit(&format!("JOB_STARTED {execution} {payload}"));
                    while !*release.borrow() {
                        if release.changed().await.is_err() {
                            break;
                        }
                    }
                    emit(&format!("JOB_DONE {execution} {payload}"));
                    Ok::<Value, JobProcessError<String>>(Value {
                        value: format!("held-{execution}"),
                        extra: Default::default(),
                    })
                }
            })
            .await?;
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
            Ok(_) => emit("JOB_SUBMITTED initial"),
            Err(error) => emit(&format!("JOB_SUBMIT_ERROR {error}")),
        }
    }
    emit("OPERATION_PROVIDER_READY");

    let commands = tokio::spawn(async move {
        let tick_hold_only = tick_hold_only;
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let command = line.trim();
            if let Some(value) = command.strip_prefix("HOLD_ONLY ") {
                if let Ok(mut only) = tick_hold_only.lock() {
                    *only = Some(value.to_string());
                }
                continue;
            }
            match command {
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
    });

    tokio::select! {
        result = runtime.run() => {
            result?;
        }
        _ = commands => {}
    }
    Ok(())
}
