//! Rust operation-execution growth acceptance provider leg.
//!
//! Serves the `transport_growth` api as a generated Rust service: the always
//! granted `Hold` operation parks inside its handler until the caller sends a
//! `Continue` signal, so the case harness can grow the service's optional
//! `extend` capability (and therefore its physical transport) while one
//! execution is in flight. Every execution counts itself exactly once, so the
//! harness can prove an accepted execution is neither restarted nor duplicated
//! across transport cutover and that a start after cutover is accepted.

use futures_util::StreamExt as _;
use runtime_trellis::participants::runtime_trellis_transport_growth_operation_provider::{
    Participant as ProviderParticipant, Provider as OperationProvider,
};
use runtime_trellis::types::{Empty, Update, UpdateDetail, Value};
use runtime_trellis::Int64;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::service::ServiceConnectOptions;

/// Number of `Hold` handler invocations observed for the process lifetime.
static EXECUTIONS: AtomicU64 = AtomicU64::new(0);

/// Number of `Park` RPC handler invocations observed for the process lifetime.
static PARKS: AtomicU64 = AtomicU64::new(0);

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
    // Held RPC callbacks park until the harness releases them, so a real
    // transport cutover happens while several accepted callbacks are in flight.
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    {
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
    emit("OPERATION_PROVIDER_READY");

    let commands = tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match line.trim() {
                "COUNT" => {
                    emit(&format!(
                        "HOLD_EXECUTIONS {}",
                        EXECUTIONS.load(Ordering::SeqCst)
                    ));
                }
                "PARKS" => {
                    emit(&format!("PARK_CALLS {}", PARKS.load(Ordering::SeqCst)));
                }
                "RELEASE" => {
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
