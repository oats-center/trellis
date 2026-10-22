//! Rust device plus companion acceptance leg.
//!
//! Runs the real generated device SDK against a live runtime: performs device
//! activation, connects the native device, connects the server-assigned
//! companion through the ordinary user login path, and invokes the companion's
//! required RPC once.
//!
//! After the initial `Required` call the process stays connected and waits for
//! commands on stdin so the TypeScript case harness can observe an actual
//! persisted authorization renewal before asking the same live connection for
//! another `Required`. The harness then requests a graceful exit. Only a bounded
//! line protocol is written to stdout.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use runtime_trellis::apps::runtime_trellis_agent_device_companion::Request as CompanionRequest;
use runtime_trellis::participants::runtime_trellis_agent_device::{
    Client as DeviceClient, Participant as DeviceParticipant,
};
use runtime_trellis::types::Empty;
use std::io::Write as _;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::auth::{
    check_device_activation, derive_device_identity, discover_native_oauth,
    wait_for_device_activation, DeviceActivationOptions, DeviceActivationStatus, NativeOAuthClient,
};
use trellis_rs::client::{DeviceConnectOptions, UserConnectOptions};
use trellis_rs::generated::{AppRequestDescriptor, Client};
use trellis_runtime_apis::{apis::trellis_auth_v1::rpc, AuthDeviceReview};

/// Emits one flushable protocol line the harness can act on immediately.
fn emit(line: String) -> Result<(), Box<dyn std::error::Error>> {
    println!("{line}");
    std::io::stdout().flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match run().await {
        Ok(()) => Ok(()),
        Err(error) => {
            emit(format!("DEVICE_COMPANION_ERROR {error}"))?;
            Err(error)
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let root_secret = URL_SAFE_NO_PAD.decode(std::env::var("DEVICE_ROOT_SECRET")?)?;
    let provisioning_secret = std::env::var("TRELLIS_PROVISIONING_SECRET").ok();
    let timeout_ms = std::env::var("TRELLIS_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10_000);

    let identity = derive_device_identity(&root_secret)?;
    let mut server =
        discover_native_oauth(&url, &trellis_runtime_acceptance::trust_roots()).await?;
    let options = DeviceConnectOptions::<DeviceParticipant>::new(
        &url,
        &identity.identity_seed_base64url,
        server.nats_servers.clone(),
        server.trust.clone(),
    )
    .with_timeout_ms(timeout_ms);
    let activation = DeviceActivationOptions::new(options, &identity.activation_key_base64url);
    let activation = match provisioning_secret.as_deref() {
        Some(secret) => activation.with_provisioning_secret(secret),
        None => activation,
    };
    let activation = match std::env::var("DEVICE_ACTIVATION_NONCE") {
        Ok(nonce) => activation.with_nonce(nonce),
        Err(_) => activation,
    };

    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let (scope, session) = if let Ok(scope) = std::env::var("COMPANION_CREDENTIAL_SCOPE") {
        let DeviceActivationStatus::Ready(session) = check_device_activation(&activation).await?
        else {
            return Err("resumed device unexpectedly requires enrollment".into());
        };
        (serde_json::from_str(&scope)?, session)
    } else {
        let pending = match check_device_activation(&activation).await? {
            DeviceActivationStatus::Ready(_) => {
                return Err("fresh device did not require enrollment".into())
            }
            DeviceActivationStatus::Pending(pending) => {
                emit(format!(
                    "DEVICE_COMPANION_ACTIVATION {}",
                    serde_json::json!({"reviewId":pending.review_id,"activationUrl":pending.activation_url,"confirmationCode":pending.confirmation_code,"nonce":pending.nonce})
                ))?;
                pending
            }
        };
        let review: AuthDeviceReview = serde_json::from_str(
            &commands
                .next_line()
                .await?
                .ok_or("missing approved device review")?,
        )?;
        if review.review_id.to_string() != pending.review_id
            || review.companion_request_id.as_deref() != Some(CompanionRequest::ID)
        {
            return Err("approved review does not identify this native companion".into());
        }
        let client_id = review
            .companion_client_id
            .as_ref()
            .ok_or("missing companion client")?;
        let (oauth, authorized_server, scope) =
            trellis_runtime_acceptance::authorize_user::<CompanionRequest>(client_id.as_ref())
                .await?;
        server = authorized_server;
        let attaching = Client::connect_user(UserConnectOptions::new(
            server.nats_servers.clone(),
            timeout_ms,
            oauth,
        ))
        .await?;
        attaching
            .call::<rpc::DeviceCompanionsAttach>(&rpc::DeviceCompanionsAttachInput {
                review_id: review.review_id,
                confirmation_code: pending.confirmation_code.clone().into(),
                expected_revision: review.revision,
                idempotency_key: ulid::Ulid::new().to_string().into(),
                extra: Default::default(),
            })
            .await?;
        drop(attaching);
        emit(format!(
            "DEVICE_COMPANION_SCOPE {}",
            serde_json::to_string(&scope)?
        ))?;
        let session =
            wait_for_device_activation(&activation, &pending, Duration::from_secs(60)).await?;
        (scope, session)
    };
    let oauth = NativeOAuthClient::open(
        std::path::PathBuf::from(std::env::var("XDG_CONFIG_HOME")?).join("native-oauth"),
        scope,
        server.trust.clone(),
    )
    .await?
    .resume()
    .await?;
    let device = DeviceClient::connect(activation.into_connect_options(session)?.with_companion(
        UserConnectOptions::new(server.nats_servers, timeout_ms, oauth),
    ))
    .await?;
    emit("DEVICE_COMPANION_CONNECTED".to_owned())?;

    let companion = device.companion();
    let api = companion.runtime_trellis_device_child_v1();
    api.required(&Empty {
        extra: Default::default(),
    })
    .await?;
    // The harness now observes persisted renewal on this still-open connection.
    emit("DEVICE_COMPANION_READY".to_owned())?;

    while let Some(line) = commands.next_line().await? {
        match line.trim() {
            "REQUIRED" => {
                api.required(&Empty {
                    extra: Default::default(),
                })
                .await?;
                emit("DEVICE_COMPANION_RENEWED_OK".to_owned())?;
            }
            "EXIT" => {
                emit("DEVICE_COMPANION_DONE".to_owned())?;
                break;
            }
            "" => {}
            other => {
                return Err(format!("unknown device companion command '{other}'").into());
            }
        }
    }
    Ok(())
}
