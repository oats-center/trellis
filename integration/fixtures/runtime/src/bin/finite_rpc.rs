//! Public generated finite-RPC caller controlled through ordinary application tasks.

use trellis_rs::__internal::RuntimeOwner as _;
use runtime_trellis::apis::runtime_trellis_finite_v1::{rpc::HoldDeclaredError, Client};
use runtime_trellis::participants::runtime_trellis_finite_caller::Participant;
use runtime_trellis::types::Value;
use std::{io::Write as _, sync::Arc};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::{
    client::{CallError, ServiceConnectWithContractOptions, TrellisClient},
    generated::ParticipantDescriptor,
};

fn emit(line: &str) {
    println!("{line}");
    std::io::stdout().flush().unwrap();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = trellis_rs::auth::discover_native_oauth(
        &std::env::var("TRELLIS_URL")?,
        &trellis_runtime_acceptance::trust_roots(),
    )
    .await?;
    let seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let client = Arc::new(
        TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
            nats_servers: server.nats_servers,
            trust: server.trust,
            participant_id: Participant::ID,
            provisioned_identity_seed_base64url: &seed,
            name: Some("finite-rpc"),
            timeout_ms: 60_000,
        })
        .await?,
    );
    let owner = trellis_rs::__internal::LiveProviderOwner::from_connected_client(client.clone());
    let api = Client::from_generated(owner.runtime_generated_client());
    let mut calls = Vec::new();
    emit("FINITE_READY");
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    while let Some(command) = commands.next_line().await? {
        match command.as_str() {
            "CONTEXT" => {
                let authority = client
                    .authorization_context()?
                    .ok_or("missing authority")?
                    .context;
                emit(&format!(
                    "FINITE_CONTEXT {}",
                    serde_json::json!({
                        "digest": authority.digest()?, "expiresAt": authority.unsigned.expires_at,
                    })
                ));
            }
            "START" => {
                for name in ["success", "declared", "cancelled", "timeout"] {
                    let api = api.clone();
                    calls.push((
                        name,
                        tokio::spawn(async move {
                            let input = Value {
                                value: name.into(),
                                extra: Default::default(),
                            };
                            match name {
                                "success" => match api.hold_success(&input).await {
                                    Ok(value) if value.value == name => emit("FINITE_SUCCESS"),
                                    other => emit(&format!("FINITE_UNEXPECTED {name} {other:?}")),
                                },
                                "declared" => match api.hold_declared(&input).await {
                                    Err(CallError::Declared(error)) => {
                                        let HoldDeclaredError::Rejected(error) = *error;
                                        assert_eq!(error.payload().unwrap().value, name);
                                        emit("FINITE_DECLARED");
                                    }
                                    other => emit(&format!("FINITE_UNEXPECTED {name} {other:?}")),
                                },
                                "cancelled" => emit(&format!(
                                    "FINITE_UNEXPECTED {name} {:?}",
                                    api.hold_cancelled(&input).await
                                )),
                                "timeout" => match api.hold_deadline(&input).await {
                                    Err(CallError::Timeout) => emit("FINITE_TIMEOUT"),
                                    other => emit(&format!("FINITE_UNEXPECTED {name} {other:?}")),
                                },
                                _ => unreachable!(),
                            }
                        }),
                    ));
                }
            }
            "CANCEL" => {
                let index = calls
                    .iter()
                    .position(|(name, _)| *name == "cancelled")
                    .ok_or("missing call")?;
                let (_, call) = calls.remove(index);
                call.abort();
                assert!(call.await.unwrap_err().is_cancelled());
                emit("FINITE_CANCELLED");
            }
            "EXIT" => break,
            other => return Err(format!("unknown command {other}").into()),
        }
    }
    for (_, call) in calls {
        call.await?;
    }
    owner.shutdown_native_runtime().await?;
    Ok(())
}
