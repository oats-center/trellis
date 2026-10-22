//! Open built-in Health.Watch through native OAuth Device Authorization.
//!
//! Set TRELLIS_URL to the selected OAuth issuer origin, and TRELLIS_CA_CERTIFICATE
//! to the installation CA when it uses Trellis-generated TLS. The registered native CLI
//! app and consenting user must allow the Health `observe` capability. Custody is
//! temporary for this example; each run asks for fresh authorization.
use futures_util::StreamExt;
use std::io::Write as _;
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::generated::AppRequestDescriptor;
use trellis_runtime_apis::apis::trellis_health_v1;
use trellis_runtime_apis::apps::trellis_cli::Request;
use trellis_runtime_apis::types::HealthWatchRequest;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let roots: Vec<std::path::PathBuf> = std::env::var_os("TRELLIS_CA_CERTIFICATE")
        .map(Into::into)
        .into_iter()
        .collect();
    let server = discover_native_oauth(&url, &roots).await?;
    let custody = tempfile::tempdir()?;
    let oauth = NativeOAuthClient::open(
        custody.path().join("oauth"),
        NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: Request::ID.into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust,
    )
    .await?;
    let login = oauth
        .start_device(&serde_json::json!([{
            "type": "trellis_capabilities",
            "required": Request::REQUIRED_CAPABILITIES,
            "optional": Request::OPTIONAL_CAPABILITIES,
        }]))
        .await?;
    println!(
        "Open {} and enter code {}. Approve Health observation to continue.",
        login.verification_uri(),
        login.user_code()
    );
    std::io::stdout().flush()?;
    let session = login.complete().await?;
    let client = trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
        server.nats_servers,
        8_000,
        session,
    ))
    .await?;
    let health = trellis_health_v1::Client::from_generated(client);
    let mut frames = health
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
    println!("{}", serde_json::to_string(&frame)?);
    println!("health watch complete");
    std::io::stdout().flush()?;
    Ok(())
}
