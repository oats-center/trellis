//! A real upload handler that retains committed bytes across replica loss.

use futures_util::StreamExt;
use runtime_trellis::participants::runtime_trellis_operation_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use serde_json::json;
use std::io::Write as _;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use trellis_rs::service::ServerError;

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
    let mut provider = Provider::new(&mut service);
    provider
        .runtime_trellis_runtime_v1()
        .register_upload(|_, _, operation| async move {
            let mut upload = operation
                .open_staged_upload()
                .await?
                .ok_or_else(|| ServerError::Nats("committed upload missing".into()))?;
            let mut bytes = Vec::new();
            upload
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;
            println!(
                "{}",
                json!({"uploaded":bytes, "info":operation.upload().await?})
            );
            std::io::stdout().flush().unwrap();
            let mut signals = operation.signals().await?;
            while let Some(signal) = signals.next().await {
                let signal = signal?;
                operation.acknowledge_signal(signal.signal_sequence).await?;
                let value = signal
                    .input
                    .as_ref()
                    .and_then(|input| input["value"].as_str())
                    .unwrap_or_default();
                if value == "during-upload" {
                    println!("{}", json!({"uploadSignal":signal.signal_sequence}));
                    std::io::stdout().flush().unwrap();
                    continue;
                }
                if value == "ack-before" {
                    println!("{}", json!({"acknowledged":signal.signal_sequence}));
                    std::io::stdout().flush().unwrap();
                    operation.cancellation().cancelled().await;
                } else {
                    operation
                        .complete(Value {
                            value: format!("rust:{value}"),
                            extra: Default::default(),
                        })
                        .await?;
                }
                break;
            }
            Ok(())
        });
    println!("{}", json!({"ready":"provider"}));
    std::io::stdout().flush()?;
    let stop = async {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("stop"));
    };
    tokio::select! { result = service.run() => result?, () = stop => {} }
    Ok(())
}
