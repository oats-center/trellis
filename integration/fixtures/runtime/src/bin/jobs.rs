use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use trellis_rs::jobs::{JobCancellationReason, JobProcessError, JobState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = Participant::connect(
        trellis_runtime_acceptance::service_options(
            &std::env::var("TRELLIS_URL")?,
            &std::env::var("TRELLIS_IDENTITY_SEED")?,
        )
        .await?,
    )
    .await?;

    if std::env::var_os("TRELLIS_REPLAY_WORK").is_some() {
        use std::io::Write;
        let failed = Arc::new(AtomicUsize::new(0));
        let dead = Arc::new(AtomicUsize::new(0));
        let mut provider = Provider::new(&mut service);
        provider
            .register_replay_work(move |job| {
                let failed = Arc::clone(&failed);
                let dead = Arc::clone(&dead);
                async move {
                    if job.cancellation_token().reason()
                        == Some(JobCancellationReason::RetryExhausted)
                    {
                        println!("cleanup {}", job.payload().value);
                        std::io::stdout().flush().expect("stdout");
                        return Ok(job.payload().clone());
                    }
                    let count = if job.payload().value == "retry" {
                        failed.fetch_add(1, Ordering::SeqCst) + 1
                    } else {
                        dead.fetch_add(1, Ordering::SeqCst) + 1
                    };
                    println!("execute {} {count}", job.payload().value);
                    std::io::stdout().flush().expect("stdout");
                    if job.payload().value == "retry" && count == 1 {
                        Err(JobProcessError::Failed("first run failed"))
                    } else if job.payload().value == "replay" && count <= 2 {
                        Err(JobProcessError::Retryable("first run exhausted"))
                    } else {
                        Ok(job.payload().clone())
                    }
                }
            })
            .await?;
        for value in ["retry", "replay"] {
            let job = provider
                .submit_replay_work(Value {
                    value: value.into(),
                    extra: Default::default(),
                })
                .await?;
            println!("submitted {value} {}", job.identity().id);
            std::io::stdout().flush()?;
        }
        service.run().await?;
        return Ok(());
    }

    if let Ok(role) = std::env::var("TRELLIS_RESTART_WORK") {
        // A `flaky` job fails once and then succeeds. The `first` worker never
        // finishes a `crash` job, so the test can kill it mid-handler; any
        // other worker completes it.
        use std::io::Write;
        let flaky = Arc::new(AtomicUsize::new(0));
        Provider::new(&mut service)
            .register_restart_work(move |job| {
                let role = role.clone();
                let flaky = Arc::clone(&flaky);
                async move {
                    let value = job.payload().value.clone();
                    println!("execute {value} {role} {}", job.tries());
                    std::io::stdout().flush().expect("stdout");
                    if value == "flaky" && flaky.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(JobProcessError::Retryable("transient failure"));
                    }
                    if value == "crash" && role == "first" {
                        std::future::pending::<()>().await;
                    }
                    Ok(job.payload().clone())
                }
            })
            .await?;
        println!("ready");
        std::io::stdout().flush()?;
        service.run().await?;
        return Ok(());
    }

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
