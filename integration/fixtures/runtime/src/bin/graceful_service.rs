//! A real Rust Provider process for stop and upgrade tests, the counterpart of
//! `ts/integration/_support/graceful_service.ts`. Every event it sees is one
//! JSON line on standard output.
//!
//! `ROLE=old` holds what it is given: a slow Echo sleeps for the requested
//! milliseconds, a `handoffWork` job runs until cancelled, and a `changes`
//! delivery waits `HOLD_DELIVERY_MS`. It creates one held job at startup.
//! `ROLE=new` finishes all of them at once, slow Echo calls included.

use std::io::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runtime_trellis::apis::runtime_trellis_runtime_v1::events::Changed;
use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use trellis_rs::jobs::JobProcessError;
use trellis_rs::service::ServiceEventListenOptions;

fn emit(event: &str, role: &str, value: Option<&str>) {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let line = serde_json::json!({ "event": event, "role": role, "at": at, "value": value });
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let role: &'static str = std::env::var("ROLE")?.leak();
    let old = role == "old";
    let hold = Duration::from_millis(
        std::env::var("HOLD_DELIVERY_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(60_000),
    );
    let mut service = Participant::connect(
        trellis_runtime_acceptance::service_options(
            &std::env::var("TRELLIS_URL")?,
            &std::env::var("TRELLIS_IDENTITY_SEED")?,
        )
        .await?,
    )
    .await?;
    let _changes = service
        .listen_event::<Changed, _, _>(
            move |event, _| async move {
                emit("delivery-started", role, Some(&event.value));
                if old {
                    tokio::time::sleep(hold).await;
                }
                emit("delivery-finished", role, Some(&event.value));
                Ok(())
            },
            ServiceEventListenOptions::default(),
        )
        .await?;
    let mut provider = Provider::new(&mut service);
    provider
        .register_handoff_work(move |job| async move {
            let value = job.payload().value.clone();
            emit("job-started", role, Some(&value));
            if old {
                job.cancellation_token().cancelled().await;
                emit("job-cancelled", role, Some(&value));
                return Err(JobProcessError::Retryable("stopped"));
            }
            emit("job-finished", role, Some(&value));
            Ok(job.payload().clone())
        })
        .await?;
    let mut api = provider.runtime_trellis_runtime_v1();
    api.register_echo(move |_, input| async move {
        if let Some(ms) = input.value.strip_prefix("slow:").filter(|_| old) {
            emit("slow-started", role, None);
            tokio::time::sleep(Duration::from_millis(ms.parse().unwrap_or(0))).await;
            emit("slow-finished", role, None);
        }
        Ok(Value {
            value: format!("{role}:{}", input.value),
            extra: Default::default(),
        })
    });
    api.register_watch(move |context, _| {
        emit("live-opened", role, None);
        let cancellation = context.cancellation.clone();
        futures_util::stream::unfold(cancellation, move |cancellation| async move {
            tokio::select! {
                () = cancellation.cancelled() => {
                    emit("live-cancelled", role, None);
                    None
                }
                () = tokio::time::sleep(Duration::from_millis(200)) => Some((
                    Ok(Value { value: role.into(), extra: Default::default() }),
                    cancellation,
                )),
            }
        })
    });
    if old {
        Provider::new(&mut service)
            .create_handoff_work(Value {
                value: "held".into(),
                extra: Default::default(),
            })
            .await?;
    }
    emit("waiting", role, None);
    let result = service.run().await;
    match &result {
        Ok(()) => emit("exited", role, None),
        Err(error) => emit("failed", role, Some(&error.to_string())),
    }
    Ok(())
}
