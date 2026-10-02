//! Rust transport-generation growth acceptance leg.
//!
//! Connects a generated `TransportGrowthSubject` service, invokes its
//! always-granted `Advance` capability, then waits on stdin so the TypeScript
//! case harness can approve the optional `Extend` capability through real
//! deployment consent and ask for `Extend` on the same logical connection. The
//! harness drives the optional capability and the ordinary-renewal window; the
//! process only prints a bounded line protocol.
//!
//! The `CLOSE_AND_WAIT` command tears the logical connection down through its
//! ordinary owners (aborting the serving runtime, any owned observation, and any
//! held RPC, then dropping the generated API handles) while keeping the process
//! itself alive for a minimal `PING`/`EXIT` loop. A case uses it to prove that
//! dropping the real logical client releases a provisional transport resource
//! and that the still-open process performs no recovery work afterwards.

use futures_util::{stream, StreamExt as _};
use runtime_trellis::apis::runtime_trellis_transport_growth_v1::Client as SubjectGrowthClient;
use runtime_trellis::participants::runtime_trellis_transport_growth_subject::{
    Participant as SubjectParticipant, Provider as SubjectProvider,
};
use runtime_trellis::types::{Empty, LiveProbeFrame};
use runtime_trellis::Int64;
use std::io::Write as _;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use trellis_rs::service::ServiceConnectOptions;
use trellis_rs::telemetry::{self, TelemetryGuard, TelemetryIdentity, TelemetryRole};

/// Emits one flushable protocol line the harness can act on immediately.
fn emit(line: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("{line}");
    std::io::stdout().flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = telemetry::init_from_env(TelemetryIdentity::new(
        "transport-growth-subject",
        TelemetryRole::Service,
        env!("CARGO_PKG_VERSION"),
    ));
    let result = run(&telemetry).await;
    telemetry.shutdown().await;
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            emit(&format!("TRANSPORT_GROWTH_ERROR {error}"))?;
            Err(error)
        }
    }
}

/// Stop one owned task, whether or not it is still running, and join it so its
/// owned transport handles are released before the caller continues.
async fn stop_task(task: Option<JoinHandle<()>>) {
    if let Some(task) = task {
        if !task.is_finished() {
            task.abort();
        }
        let _ = task.await;
    }
}

async fn run(telemetry: &TelemetryGuard) -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    if std::env::var("TRELLIS_NATIVE_OWNER_SCOPE").as_deref() == Ok("1") {
        return run_native_owner_scope(&url, &seed).await;
    }
    // The held `Advance` deliberately spans a real transport growth, so the
    // caller's own request budget must outlast automatic adoption. Every other
    // call completes well within it; this is the ordinary public client option,
    // not a runtime or test-only timer.
    let mut runtime = SubjectParticipant::connect(
        ServiceConnectOptions::new(&url, &seed).with_timeout_ms(60_000),
    )
    .await?;
    // The generated API and outer client handles each own a real reference to the
    // logical connection. They are optional so `CLOSE_AND_WAIT` can drop them and
    // the serving runtime, letting the last reference release the connection
    // through ordinary `Drop`.
    let (mut client, mut api) = {
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
        (Some(client), Some(api))
    };
    emit("TRANSPORT_GROWTH_CONNECTED")?;

    /// The connected generated growth client, while the logical connection is
    /// still owned by this process.
    fn growth(api: &Option<SubjectGrowthClient>) -> &SubjectGrowthClient {
        api.as_ref().expect("transport growth client is connected")
    }

    growth(&api)
        .advance(&Empty {
            extra: Default::default(),
        })
        .await?;
    emit("TRANSPORT_GROWTH_ADVANCE_OK")?;

    // Serve the registered routes — including the liveprobe Watch source — on the
    // connection's transport generations while the harness drives commands.
    let mut serve: Option<JoinHandle<()>> = Some(tokio::spawn(async move {
        if let Err(error) = runtime.run().await {
            let _ = emit(&format!("TRANSPORT_GROWTH_SERVE_ERROR {error}"));
        }
    }));

    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let mut observe_close: Option<oneshot::Sender<()>> = None;
    let mut observe_task: Option<JoinHandle<()>> = None;
    // One in-flight `Advance` owned by this process while the harness drives the
    // rest of the protocol. It is joined (or aborted) before exit so a failure is
    // reported rather than detached.
    let mut held_advance: Option<JoinHandle<()>> = None;
    while let Some(line) = commands.next_line().await? {
        match line.trim() {
            "FLUSH_TELEMETRY" => {
                telemetry.force_flush().await;
                emit("TRANSPORT_GROWTH_TELEMETRY_FLUSHED")?;
            }
            "OBSERVE" => {
                // Open a Live observation on the current generation and stream
                // its frames, so growth can be proven not to move the session.
                // Each OBSERVE opens a fresh observation; a previous retained
                // handle stays alive in its own owned task.
                let mut stream = growth(&api)
                    .progress(&Empty {
                        extra: Default::default(),
                    })
                    .await?;
                let (close_tx, mut close_rx) = oneshot::channel();
                observe_close = Some(close_tx);
                observe_task = Some(tokio::spawn(async move {
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
                }));
                emit("TRANSPORT_GROWTH_OBSERVING")?;
            }
            "OBSERVE_CLOSE" => {
                if let Some(close) = observe_close.take() {
                    let _ = close.send(());
                    emit("TRANSPORT_GROWTH_OBSERVED_CLOSING")?;
                }
            }
            "ADVANCE" => {
                growth(&api)
                    .advance(&Empty {
                        extra: Default::default(),
                    })
                    .await?;
                emit("TRANSPORT_GROWTH_ADVANCE_OK")?;
            }
            "START_ADVANCE" => {
                // Issue one real generated `Advance` on an owned task so stdin
                // keeps processing commands while the call is held in flight by
                // the receiving handler. The completion line is the only signal
                // that the accepted call terminalized.
                if held_advance.is_some() {
                    return Err("an Advance is already held".into());
                }
                let api = growth(&api).clone();
                emit("TRANSPORT_GROWTH_ADVANCE_STARTED")?;
                held_advance = Some(tokio::spawn(async move {
                    match api
                        .advance(&Empty {
                            extra: Default::default(),
                        })
                        .await
                    {
                        Ok(_) => {
                            let _ = emit("TRANSPORT_GROWTH_ADVANCE_OK");
                        }
                        Err(error) => {
                            let _ = emit(&format!("TRANSPORT_GROWTH_ADVANCE_ERROR {error}"));
                        }
                    }
                }));
            }
            "AVAILABLE" => {
                // Wait until the application layer reflects the approved
                // capability; the transport layer then waits for/uses the
                // adopted generation on the call itself.
                let client = client.as_ref().expect("client is still connected");
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
                growth(&api)
                    .extend(&Empty {
                        extra: Default::default(),
                    })
                    .await?;
                emit("TRANSPORT_GROWTH_EXTEND_OK")?;
            }
            "TRY_EXTEND" => {
                // Report the real public outcome of an optional call without
                // treating a refusal as a process failure: a narrowed authority
                // must refuse `Extend` while the always-granted surface keeps
                // working.
                match growth(&api)
                    .extend(&Empty {
                        extra: Default::default(),
                    })
                    .await
                {
                    Ok(_) => emit("TRANSPORT_GROWTH_EXTEND_OK")?,
                    Err(error) => emit(&format!("TRANSPORT_GROWTH_EXTEND_REFUSED {error}"))?,
                }
            }
            "CLOSE_AND_WAIT" => {
                // Release the logical connection through its ordinary owners,
                // then keep the process alive so a case can observe the broker
                // after the client is gone. Every owning handle is dropped (or its
                // owner aborted and joined) and the serving runtime is aborted so
                // the last `TrellisClient` reference is released by `Drop`.
                if let Some(close) = observe_close.take() {
                    let _ = close.send(());
                }
                stop_task(observe_task.take()).await;
                stop_task(held_advance.take()).await;
                drop(api.take());
                drop(client.take());
                stop_task(serve.take()).await;
                emit("TRANSPORT_GROWTH_CLOSED_AND_WAITING")?;
            }
            "PING" => {
                emit("TRANSPORT_GROWTH_PONG")?;
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
    // Never leave a held call or observation detached: an abort is awaited so a
    // failed call surfaces through its own error line first, and the serving
    // runtime is stopped so the process exits without a lingering connection.
    stop_task(held_advance.take()).await;
    if let Some(close) = observe_close.take() {
        let _ = close.send(());
    }
    stop_task(observe_task.take()).await;
    drop(api.take());
    drop(client.take());
    stop_task(serve.take()).await;
    Ok(())
}

/// Use the runtime's ordinary native bootstrap and owner, without a generated
/// service facade hiding its client. Keep both real references after scope end.
async fn run_native_owner_scope(url: &str, seed: &str) -> Result<(), Box<dyn std::error::Error>> {
    use trellis_rs::generated::ParticipantDescriptor as _;
    let client = std::sync::Arc::new(
        trellis_rs::client::TrellisClient::connect_service_with_contract(
            trellis_rs::client::ServiceConnectWithContractOptions {
                trellis_url: url,
                participant_id: SubjectParticipant::ID,
                participant_path: SubjectParticipant::PATH,
                package_evidence: SubjectParticipant::package_evidence(),
                name: Some("native-owner-scope"),
                provisioned_identity_seed_base64url: seed,
                timeout_ms: 60_000,
            },
        )
        .await?,
    );
    let owner = trellis_rs::service::LiveProviderOwner::from_connected_client(client.clone());
    emit("TRANSPORT_GROWTH_READY")?;
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = commands.next_line().await? {
        match line.trim() {
            "SHUTDOWN_NATIVE" => {
                owner.shutdown_native_runtime().await?;
                emit("TRANSPORT_GROWTH_CLOSED_AND_WAITING")?;
            }
            "REFRESH_AFTER_SHUTDOWN" => match client.refresh_authorization_context().await {
                Err(error) => emit(&format!("TRANSPORT_GROWTH_REFRESH_REFUSED {error}"))?,
                Ok(_) => return Err("a stopped native owner refreshed authorization".into()),
            },
            "PING" => {
                // The client is still a usable Rust value after transport scope
                // end; no last-reference drop or process exit proves retirement.
                let _ = client.authorization_context()?;
                emit("TRANSPORT_GROWTH_PONG")?;
            }
            "EXIT" => {
                emit("TRANSPORT_GROWTH_DONE")?;
                break;
            }
            other => return Err(format!("unknown native scope command '{other}'").into()),
        }
    }
    Ok(())
}
