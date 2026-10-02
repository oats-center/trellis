use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use trellis_rs::jobs::{JobCancellationReason, JobProcessError, JobState};
use trellis_rs::service::ServiceConnectOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = Participant::connect(ServiceConnectOptions::new(
        &std::env::var("TRELLIS_URL")?,
        &std::env::var("TRELLIS_IDENTITY_SEED")?,
    ))
    .await?;

    if std::env::var_os("TRELLIS_PRE_STARTED_WORK").is_some() {
        use std::io::Write;
        let stop = Arc::new(tokio::sync::Notify::new());
        let stopping = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut command = String::new();
            std::io::stdin()
                .read_line(&mut command)
                .expect("stop command");
            stopping.notify_one();
        });
        Provider::new(&mut service)
            .register_pre_started_work(|job| async move {
                println!("handler {}", job.payload().value);
                std::io::stdout().flush().expect("stdout");
                Ok::<_, JobProcessError<String>>(job.payload().clone())
            })
            .await?;
        println!("ready");
        std::io::stdout().flush()?;
        tokio::select! {
            result = service.run() => result?,
            () = stop.notified() => {},
        }
        return Ok(());
    }
    let ordinary = Arc::new(AtomicUsize::new(0));
    let cleanup = Arc::new(AtomicUsize::new(0));
    let mut provider = Provider::new(&mut service);
    provider
        .register_work({
            let ordinary = Arc::clone(&ordinary);
            let cleanup = Arc::clone(&cleanup);
            move |job| {
                let ordinary = Arc::clone(&ordinary);
                let cleanup = Arc::clone(&cleanup);
                async move {
                    match job.cancellation_token().reason() {
                        Some(
                            JobCancellationReason::RetryExhausted
                            | JobCancellationReason::DeadlineExceeded,
                        ) => {
                            if cleanup.fetch_add(1, Ordering::SeqCst) < 2 {
                                return Err(JobProcessError::Retryable("cleanup interrupted"));
                            }
                            Ok(job.payload().clone())
                        }
                        None => {
                            ordinary.fetch_add(1, Ordering::SeqCst);
                            Err(JobProcessError::Retryable("ordinary failure"))
                        }
                        other => panic!("unexpected cancellation: {other:?}"),
                    }
                }
            }
        })
        .await?;
    let job = provider
        .submit_work(Value {
            value: "native reconciliation".into(),
            extra: Default::default(),
        })
        .await?;
    // The service's ordinary run loop and the generated job reference both use
    // the same production transport and worker lifetime.
    let terminal = tokio::select! {
        result = service.run() => {
            result?;
            return Err("service stopped before job settlement".into());
        }
        result = tokio::time::timeout(std::time::Duration::from_secs(30), job.wait()) => result??,
    };
    assert!(matches!(terminal.state, JobState::Dead | JobState::Expired));
    assert_eq!(ordinary.load(Ordering::SeqCst), 2);
    assert_eq!(cleanup.load(Ordering::SeqCst), 3);
    println!("native ordinary=2 cleanup=3 settled={:?}", terminal.state);
    Ok(())
}
