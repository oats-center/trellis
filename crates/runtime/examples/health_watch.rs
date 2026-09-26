//! NX02: generated Console client opens built-in Health.Watch.
use futures_util::StreamExt;
use std::io::Write as _;
use trellis_rs::auth::{load_admin_session, start_agent_login, StartAgentLoginOpts};
use trellis_rs::client::{UserConnectOptions, UserSessionCredentials};
use trellis_runtime_apis::participants::trellis_console::Client;
use trellis_runtime_apis::types::HealthWatchRequest;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let login = start_agent_login(&StartAgentLoginOpts {
        trellis_url: &url,
        participant_id: "trellis.console",
    })
    .await?;
    println!("health login {}", login.login_url());
    std::io::stdout().flush()?;
    let _ = login.complete(&url).await;
    let session = load_admin_session()?;
    let client = Client::connect(UserConnectOptions::new(
        &url,
        8_000,
        UserSessionCredentials {
            login_session_id: &session.login_session_id,
            session_key_seed_base64url: &session.session_seed,
        },
        "trellis.console",
    ))
    .await?;
    let mut frames = client
        .trellis_health_v1()
        .watch(&HealthWatchRequest {
            contract_ids: None,
            deployment_ids: None,
            instance_ids: None,
            participant_kinds: None,
            extra: Default::default(),
        })
        .await?;
    let frame = frames
        .next()
        .await
        .ok_or("Health Watch ended before the first frame")??;
    let _ = frame;
    println!("health watch complete");
    std::io::stdout().flush()?;
    Ok(())
}
