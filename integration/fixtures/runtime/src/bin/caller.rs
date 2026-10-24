//! Generated Rust caller leg: Feed `Watch` and Operation `Work` through the
//! generated client against the live provider.
use futures_util::StreamExt;
use runtime_trellis::apis::runtime_trellis_runtime_v1::{lives, operations, rpc};
use runtime_trellis::apps::runtime_trellis_caller::{Client, Request};
use runtime_trellis::types::Value;
use std::io::Write as _;
use tracing_subscriber::prelude::*;
use trellis_rs::client::OperationState;
use trellis_rs::telemetry::{init_from_env, TelemetryIdentity, TelemetryRole};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = init_from_env(TelemetryIdentity::new(
        "runtime-rust-caller",
        TelemetryRole::Service,
        env!("CARGO_PKG_VERSION"),
    ));
    tracing_subscriber::registry()
        .with(
            telemetry
                .tracer()
                .map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer)),
        )
        .init();
    let client = Client::from_generated(
        trellis_rs::generated::Client::connect_user(
            trellis_runtime_acceptance::user_options::<Request>(5_000).await?,
        )
        .await?,
    );
    let runtime = client.runtime_trellis_runtime_v1();
    if std::env::var("TRELLIS_CALLER_MODE").as_deref() == Ok("cancel-unresponsive-upload") {
        // Upload from a source that stalls after its first bytes, then cancel
        // on request while the test keeps the provider frozen.
        let upload = runtime
            .upload()
            .start(&operations::UploadInput {
                value: "cancel-unresponsive".to_owned(),
                extra: Default::default(),
            })
            .await?;
        let (mut source, mut reader) = tokio::io::duplex(64 * 1024);
        let cancellation = trellis_rs::client::TransferCancellation::new();
        let transfer = upload.upload_from_with_cancel(&mut reader, &cancellation);
        tokio::pin!(transfer);
        let feed = async {
            use tokio::io::AsyncWriteExt as _;
            source.write_all(&[7; 256 * 1024]).await?;
            println!("uploading");
            std::io::stdout().flush()?;
            use tokio::io::AsyncBufReadExt as _;
            let mut line = String::new();
            tokio::io::BufReader::new(tokio::io::stdin())
                .read_line(&mut line)
                .await?;
            Ok::<_, std::io::Error>(())
        };
        tokio::select! {
            result = &mut transfer => {
                return Err(format!("upload ended before cancellation: {result:?}").into());
            }
            fed = feed => fed?,
        }
        cancellation.cancel();
        let started = std::time::Instant::now();
        let result = transfer.await;
        println!(
            "{}",
            serde_json::json!({
                "cancelled": format!("{:?}", result.err()),
                "ms": started.elapsed().as_millis(),
            })
        );
        std::io::stdout().flush()?;
        return Ok(());
    }
    let echo = runtime
        .echo(&rpc::EchoInput {
            value: "from TypeScript".to_owned(),
            extra: Default::default(),
        })
        .await?;
    assert_eq!(echo.value, "Rust received from TypeScript");

    // Generated Feed call: the provider streams rust-feed-* frames on Watch.
    let mut frames = runtime
        .watch(&lives::WatchInput {
            extra: Default::default(),
        })
        .await?;
    let frame = frames
        .next()
        .await
        .ok_or("Watch feed ended before the first frame")??;
    assert!(
        frame.value.starts_with("rust-feed-"),
        "unexpected Watch frame {}",
        frame.value
    );
    drop(frames);

    // Generated Operation call: start, continue, and await the terminal result.
    let handle = runtime
        .work()
        .start(&operations::WorkInput {
            value: "routing-check".to_owned(),
            extra: Default::default(),
        })
        .await?;
    handle
        .signal::<operations::WorkContinueSignal>(&Value {
            value: "continue".to_owned(),
            extra: Default::default(),
        })
        .await?;
    let snapshot = handle.wait().await?;
    assert_eq!(snapshot.state, OperationState::Completed);
    assert_eq!(
        snapshot.output.as_ref().map(|output| output.value.as_str()),
        Some("completed")
    );

    let bytes: Vec<u8> = (0..131_073).map(|index| (index % 251) as u8).collect();
    let upload = runtime
        .upload()
        .start(&operations::UploadInput {
            value: "rust-upload".to_owned(),
            extra: Default::default(),
        })
        .await?;
    let info = upload.upload(&bytes).await?;
    assert_eq!(info.size, bytes.len() as u64);
    upload
        .signal::<operations::UploadContinueSignal>(&Value {
            value: "continue".to_owned(),
            extra: Default::default(),
        })
        .await?;
    let snapshot = upload.wait().await?;
    assert_eq!(snapshot.state, OperationState::Completed);
    assert_eq!(
        snapshot.output.as_ref().map(|output| output.value.as_str()),
        Some("rust-upload:131073:false")
    );
    println!("rust caller complete");
    std::io::stdout().flush()?;
    telemetry.shutdown().await;
    Ok(())
}
