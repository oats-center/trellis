//! Generated Rust client/provider interoperability for explicit Operation destinations.

use futures_util::StreamExt;
use runtime_trellis::apps::runtime_trellis_caller::{Client, Request};
use runtime_trellis::participants::runtime_trellis_operation_provider::{Participant, Provider};
use runtime_trellis::types::{Update, UpdateDetail, Value};
use serde_json::json;
use std::io::Write as _;
use tokio::io::{AsyncBufReadExt, BufReader};
use trellis_rs::generated::Client as GeneratedClient;

fn emit(value: serde_json::Value) {
    println!("{value}");
    std::io::stdout().flush().expect("stdout");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();
    if std::env::args().any(|argument| argument == "--caller") {
        let client = Client::from_generated(
            GeneratedClient::connect_user(
                trellis_runtime_acceptance::user_options::<Request>(10_000).await?,
            )
            .await?,
        );
        emit(json!({"ready":"caller"}));
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Some(line) = lines.next_line().await? {
            let command: serde_json::Value = serde_json::from_str(&line)?;
            let deployment = command["deploymentId"].as_str().ok_or("deploymentId")?;
            let invocation = command["invocationId"].as_str().ok_or("invocationId")?;
            let input = Value {
                value: command["value"].as_str().ok_or("value")?.to_owned(),
                extra: Default::default(),
            };
            let api = client.runtime_trellis_runtime_v1();
            let invoker = api.work();
            let operation = if command["cancelled"].as_bool() == Some(true) {
                invoker
                    .start_cancelled_on_deployment_with_invocation_id(
                        deployment, invocation, &input,
                    )
                    .await?
            } else {
                invoker
                    .start_on_deployment_with_invocation_id(deployment, invocation, &input)
                    .await?
            };
            emit(json!({"ref":operation.reference(), "snapshot":operation.wait().await?}));
        }
        return Ok(());
    }
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
        .register_work(|_, input, operation| async move {
            if operation.cancellation().is_cancelled() {
                return Ok(());
            }
            if input.value == "self-ref" {
                operation
                    .complete(Value {
                        value: serde_json::to_string(operation.operation_ref())?,
                        extra: Default::default(),
                    })
                    .await?;
                return Ok(());
            }
            if input.value.starts_with("handoff-") {
                if input.value == "handoff-rust" {
                    operation
                        .progress(Update {
                            value: "retained-checkpoint".into(),
                            nested: UpdateDetail {
                                count: runtime_trellis::Int64(i64::MAX),
                                payload: vec![0, 127, 255].into(),
                                extra: Default::default(),
                            },
                            extra: Default::default(),
                        })
                        .await?;
                }
                emit(json!({"entered":input.value}));
                let mut signals = operation.signals().await?;
                if let Some(signal) = signals.next().await {
                    let signal = signal?;
                    let value = signal
                        .input
                        .as_ref()
                        .and_then(|input| input["value"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    operation.acknowledge_signal(signal.signal_sequence).await?;
                    if value == "ack-before" {
                        emit(json!({"acknowledged": signal.signal_sequence}));
                        operation.cancellation().cancelled().await;
                        return Ok(());
                    }
                    operation
                        .complete(Value {
                            value: format!("rust:{value}"),
                            extra: Default::default(),
                        })
                        .await?;
                    return Ok(());
                }
                return Ok(());
            }
            operation
                .complete(Value {
                    value: format!("rust:{}", input.value),
                    extra: Default::default(),
                })
                .await?;
            Ok(())
        });
    emit(json!({"ready":"provider"}));
    let stop = async {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("stop"));
    };
    tokio::select! {
        result = service.run() => result?,
        () = stop => {},
    }
    Ok(())
}
