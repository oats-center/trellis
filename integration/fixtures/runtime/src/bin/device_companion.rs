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
use runtime_trellis::participants::runtime_trellis_device::{
    Client as DeviceClient, Participant as DeviceParticipant,
};
use runtime_trellis::types::Empty;
use std::io::Write as _;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::auth::{
    check_device_activation, derive_device_identity, derive_device_user_companion,
    wait_for_device_activation, DeviceActivationOptions, DeviceActivationStatus,
};
use trellis_rs::client::DeviceConnectOptions;
use trellis_rs::generated::ParticipantDescriptor;

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
    let companion_id = DeviceParticipant::COMPANION
        .expect("the generated device declares a companion")
        .id;
    let companion_seed =
        derive_device_user_companion(&root_secret, &url, companion_id)?.installation_seed_base64url;

    let options =
        DeviceConnectOptions::<DeviceParticipant>::new(&url, &identity.identity_seed_base64url)
            .with_timeout_ms(timeout_ms)
            .with_companion_installation_seed(&companion_seed);
    let activation = DeviceActivationOptions::new(options, &identity.activation_key_base64url);
    let activation = match provisioning_secret.as_deref() {
        Some(secret) => activation.with_provisioning_secret(secret),
        None => activation,
    };

    let session = match check_device_activation(&activation).await? {
        DeviceActivationStatus::Ready(session) => session,
        DeviceActivationStatus::Pending(pending) => {
            emit(format!(
                "DEVICE_COMPANION_ACTIVATION {} {}",
                pending.activation_url, pending.confirmation_code
            ))?;
            wait_for_device_activation(&activation, &pending, Duration::from_secs(60)).await?
        }
    };

    let device = DeviceClient::connect(activation.into_connect_options(session)?).await?;
    emit("DEVICE_COMPANION_CONNECTED".to_owned())?;

    let companion = device.companion();
    let api = companion.runtime_trellis_device_child_v1();
    api.required(&Empty {
        extra: Default::default(),
    })
    .await?;
    // The harness now observes persisted renewal on this still-open connection.
    emit("DEVICE_COMPANION_READY".to_owned())?;

    let mut commands = BufReader::new(tokio::io::stdin()).lines();
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
