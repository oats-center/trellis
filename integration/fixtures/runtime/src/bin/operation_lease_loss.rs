use runtime_trellis::participants::runtime_trellis_operation_provider::{Participant, Provider};
use runtime_trellis::types::{Update, UpdateDetail, Value};
use std::io::{BufRead, Write};
use std::sync::Arc;
use tokio::sync::Notify;
use trellis_rs::service::{OperationCancellationReason, ServerError, ServiceConnectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();
    let cleanup = Arc::new(Notify::new());
    let stop = Arc::new(Notify::new());
    let commands = (Arc::clone(&cleanup), Arc::clone(&stop));
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line.expect("stdin command").as_str() {
                "cleanup" => commands.0.notify_one(),
                "stop" => {
                    commands.1.notify_one();
                    break;
                }
                other => panic!("unknown command: {other}"),
            }
        }
    });
    let mut service = Participant::connect(ServiceConnectOptions::new(
        &std::env::var("TRELLIS_URL")?,
        &std::env::var("TRELLIS_IDENTITY_SEED")?,
    ))
    .await?;
    let mut provider = Provider::new(&mut service);
    let mut api = provider.runtime_trellis_runtime_v1();
    api.register_work(move |_, input, op| {
        let cleanup = Arc::clone(&cleanup);
        async move {
            if input.value == "fresh" {
                op.complete(Value {
                    value: "original".to_owned(),
                    extra: Default::default(),
                })
                .await?;
            } else {
                op.started().await?;
                println!("entered");
                std::io::stdout().flush().expect("stdout");
                if input.value == "pending-control" {
                    // The real five-second CAS ACK timeout spans heartbeat ten.
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                    let error = op
                        .progress(Update {
                            value: "pending".to_owned(),
                            nested: UpdateDetail {
                                count: runtime_trellis::Int64(1),
                                payload: vec![1].into(),
                                extra: Default::default(),
                            },
                            extra: Default::default(),
                        })
                        .await
                        .expect_err("withheld progress acknowledgement must time out");
                    assert!(
                        matches!(error, ServerError::Nats(ref message) if message.contains("timed out")),
                        "{error}"
                    );
                    println!("control failed");
                    std::io::stdout().flush().expect("stdout");
                    cleanup.notified().await;
                    op.complete(Value {
                        value: "original".to_owned(),
                        extra: Default::default(),
                    }).await?;
                    return Ok(());
                }
                assert_eq!(
                    op.cancellation().cancelled().await,
                    OperationCancellationReason::OwnershipLost
                );
                println!("ownership lost");
                std::io::stdout().flush().expect("stdout");
                cleanup.notified().await;
                let error = op
                    .emit_update(Update {
                        value: "stale".to_owned(),
                        nested: UpdateDetail {
                            count: runtime_trellis::Int64(2),
                            payload: vec![2].into(),
                            extra: Default::default(),
                        },
                        extra: Default::default(),
                    })
                    .await
                    .expect_err("interrupted native transport must fail the cleanup read");
                assert!(
                    matches!(error, ServerError::Nats(_)),
                    "{error}"
                );
                println!("cleanup complete");
                std::io::stdout().flush().expect("stdout");
            }
            Ok(())
        }
    });
    tokio::select! {
        result = service.run() => result?,
        () = stop.notified() => {}
    }
    Ok(())
}
