//! Rust transport-generation growth acceptance leg.
//!
//! Connects a generated `TransportGrowthSubject` service, invokes its
//! always-granted `Advance` capability, then waits on stdin so the TypeScript
//! case harness can approve the optional `Extend` capability through real
//! deployment consent and ask for `Extend` on the same logical connection. The
//! harness drives the optional capability and the ordinary-renewal window; the
//! process only prints a bounded line protocol.

use futures_util::{stream, StreamExt as _};
use runtime_trellis::participants::runtime_trellis_transport_growth_subject::{
    Participant as SubjectParticipant, Provider as SubjectProvider,
};
use runtime_trellis::types::{Empty, LiveProbeFrame};
use runtime_trellis::Int64;
use std::io::Write as _;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::sync::oneshot;
use trellis_rs::service::ServiceConnectOptions;

/// Emits one flushable protocol line the harness can act on immediately.
fn emit(line: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("{line}");
    std::io::stdout().flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match run().await {
        Ok(()) => Ok(()),
        Err(error) => {
            emit(&format!("TRANSPORT_GROWTH_ERROR {error}"))?;
            Err(error)
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut runtime = SubjectParticipant::connect(ServiceConnectOptions::new(&url, &seed)).await?;
    let (client, api) = {
        let mut provider = SubjectProvider::new(&mut runtime);
        let client = provider.client();
        // Serve the generated `liveprobe Watch` source so a caller can observe
        // this service across a real transport growth: a monotonic index is
        // emitted on the attachment that accepted the observation, and the source
        // lifetime is driven by the receiving generation's owner controls.
        provider
            .runtime_trellis_liveprobe_v1()
            .register_watch(move |context, input| {
                let cancellation = context.cancellation.clone();
                stream::unfold(
                    (Int64(0), cancellation, input.run_id, input.stream_id),
                    |(index, cancellation, run_id, stream_id)| async move {
                        if cancellation.is_cancelled() {
                            return None;
                        }
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        let next = Int64(index.0 + 1);
                        let frame = LiveProbeFrame {
                            run_id: run_id.clone(),
                            stream_id: stream_id.clone(),
                            source_generation: Int64(1),
                            index: next,
                            payload: Vec::new().into(),
                            padding: String::new(),
                            extra: Default::default(),
                        };
                        Some((Ok(frame), (next, cancellation, run_id, stream_id)))
                    },
                )
            });
        let api = client.runtime_trellis_transport_growth_v1();
        (client, api)
    };
    emit("TRANSPORT_GROWTH_CONNECTED")?;

    api.advance(&Empty {
        extra: Default::default(),
    })
    .await?;
    emit("TRANSPORT_GROWTH_ADVANCE_OK")?;

    // Serve the registered routes — including the liveprobe Watch source — on the
    // connection's transport generations while the harness drives commands.
    let _serve = tokio::spawn(async move {
        if let Err(error) = runtime.run().await {
            let _ = emit(&format!("TRANSPORT_GROWTH_SERVE_ERROR {error}"));
        }
    });

    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let mut observe_close: Option<oneshot::Sender<()>> = None;
    while let Some(line) = commands.next_line().await? {
        match line.trim() {
            "OBSERVE" => {
                // Open a Live observation on the current generation and stream
                // its frames, so growth can be proven not to move the session.
                // Each OBSERVE opens a fresh observation; a previous retained
                // handle stays alive in its own detached task.
                let mut stream = api
                    .progress(&Empty {
                        extra: Default::default(),
                    })
                    .await?;
                let (close_tx, mut close_rx) = oneshot::channel();
                observe_close = Some(close_tx);
                let _ = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = &mut close_rx => {
                                match stream.close().await {
                                    Ok(_) => {
                                        let _ = emit("TRANSPORT_GROWTH_OBSERVED_CLOSED");
                                    }
                                    Err(error) => {
                                        let _ = emit(&format!(
                                            "TRANSPORT_GROWTH_OBSERVED_CLOSE_ERROR {error}"
                                        ));
                                    }
                                }
                                // Retain the closed handle: it must not keep
                                // pinning its generation.
                                std::future::pending::<()>().await;
                            }
                            item = stream.next() => {
                                match item {
                                    Some(Ok(event)) => {
                                        let index: i64 = event.value.parse().unwrap_or(-1);
                                        let _ = emit(&format!(
                                            "TRANSPORT_GROWTH_OBSERVED {index}"
                                        ));
                                    }
                                    Some(Err(error)) => {
                                        let _ = emit(&format!(
                                            "TRANSPORT_GROWTH_OBSERVED_ERROR {error}"
                                        ));
                                        // Retain the failed handle too.
                                        std::future::pending::<()>().await;
                                    }
                                    None => {
                                        let _ = emit("TRANSPORT_GROWTH_OBSERVED_ENDED");
                                        // Retain the naturally ended handle: a
                                        // terminal observation must not keep
                                        // pinning its generation.
                                        std::future::pending::<()>().await;
                                    }
                                }
                            }
                        }
                    }
                });
                emit("TRANSPORT_GROWTH_OBSERVING")?;
            }
            "OBSERVE_CLOSE" => {
                if let Some(close) = observe_close.take() {
                    let _ = close.send(());
                    emit("TRANSPORT_GROWTH_OBSERVED_CLOSING")?;
                }
            }
            "ADVANCE" => {
                api.advance(&Empty {
                    extra: Default::default(),
                })
                .await?;
                emit("TRANSPORT_GROWTH_ADVANCE_OK")?;
            }
            "AVAILABLE" => {
                // Wait until the application layer reflects the approved
                // capability; the transport layer then waits for/uses the
                // adopted generation on the call itself.
                let deadline = Instant::now() + Duration::from_secs(60);
                while !client
                    .availability()
                    .action_runtime_trellis_transport_growth_v1_rpc_extend
                {
                    if Instant::now() >= deadline {
                        return Err("grown capability never became available".into());
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                emit("TRANSPORT_GROWTH_EXTEND_AVAILABLE")?;
            }
            "EXTEND" => {
                api.extend(&Empty {
                    extra: Default::default(),
                })
                .await?;
                emit("TRANSPORT_GROWTH_EXTEND_OK")?;
            }
            "CLOSE" => {
                // Return closes the logical connection: every physical generation
                // is released and the server should drop all attachments.
                emit("TRANSPORT_GROWTH_CLOSED")?;
                break;
            }
            "EXIT" => {
                emit("TRANSPORT_GROWTH_DONE")?;
                break;
            }
            "" => {}
            other => return Err(format!("unknown transport growth command '{other}'").into()),
        }
    }
    Ok(())
}
