//! NX03: finite empty Watch completes with no application frames.
use futures_util::StreamExt;
use runtime_trellis::apis::runtime_trellis_runtime_v1::lives;
use runtime_trellis::participants::runtime_trellis_caller::Client;
use std::io::Write as _;
use trellis_rs::auth::{load_admin_session, start_agent_login, StartAgentLoginOpts};
use trellis_rs::client::{UserConnectOptions, UserSessionCredentials};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let login = start_agent_login(&StartAgentLoginOpts {
        trellis_url: &url,
        participant_id: "runtime-trellis.Caller",
        allow_insecure_origin: false,
    })
    .await?;
    println!("empty login {}", login.login_url());
    std::io::stdout().flush()?;
    let outcome = login.complete_without_persistence(&url).await?;
    trellis_rs::auth::save_admin_session(&outcome.state)?;
    let session = load_admin_session()?;
    let client = Client::connect(UserConnectOptions::new(
        &url,
        8_000,
        UserSessionCredentials {
            login_session_id: &session.login_session_id,
            session_key_seed_base64url: &session.session_seed,
        },
        "runtime-trellis.Caller",
    ))
    .await?;
    let mut frames = client
        .runtime_trellis_runtime_v1()
        .watch(&lives::WatchInput {})
        .await?;
    if frames.next().await.is_some() {
        return Err("empty Watch emitted an application frame".into());
    }
    println!("empty watch complete");
    std::io::stdout().flush()?;
    Ok(())
}
