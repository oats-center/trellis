//! Generated Rust caller leg: Feed `Watch` and Operation `Work` through the
//! generated client against the live provider.
use futures_util::StreamExt;
use runtime_trellis::apis::runtime_trellis_runtime_v1::{lives, operations, rpc};
use runtime_trellis::participants::runtime_trellis_caller::Client;
use runtime_trellis::types::Value;
use std::io::Write as _;
use tracing_subscriber::prelude::*;
use trellis_rs::auth::{load_admin_session, start_agent_login, StartAgentLoginOpts};
use trellis_rs::client::{OperationState, UserConnectOptions, UserSessionCredentials};
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
    let url = std::env::var("TRELLIS_URL")?;
    let login = start_agent_login(&StartAgentLoginOpts {
        trellis_url: &url,
        participant_id: "runtime-trellis.Caller",
        allow_insecure_origin: false,
    })
    .await?;
    println!("rust login {}", login.login_url());
    std::io::stdout().flush()?;
    let outcome = login.complete_without_persistence(&url).await?;
    trellis_rs::auth::save_admin_session(&outcome.state)?;
    let session = load_admin_session()?;
    let client = Client::connect(UserConnectOptions::new(
        &url,
        5_000,
        UserSessionCredentials {
            login_session_id: &session.login_session_id,
            session_key_seed_base64url: &session.session_seed,
        },
        "runtime-trellis.Caller",
    ))
    .await?;
    let runtime = client.runtime_trellis_runtime_v1();
    let echo = runtime
        .echo(&rpc::EchoInput {
            value: "from TypeScript".to_owned(),
        })
        .await?;
    assert_eq!(echo.value, "Rust received from TypeScript");

    // Generated Feed call: the provider streams rust-feed-* frames on Watch.
    let mut frames = runtime.watch(&lives::WatchInput {}).await?;
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
        })
        .await?;
    handle
        .signal::<operations::WorkContinueSignal>(&Value {
            value: "continue".to_owned(),
        })
        .await?;
    let snapshot = handle.wait().await?;
    assert_eq!(snapshot.state, OperationState::Completed);
    assert_eq!(
        snapshot.output.as_ref().map(|output| output.value.as_str()),
        Some("completed")
    );
    println!("rust caller complete");
    std::io::stdout().flush()?;
    telemetry.shutdown().await;
    Ok(())
}
