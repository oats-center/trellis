//! A service asking about another service's update learns nothing more than
//! about an update that doesn't exist.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use runtime_trellis::participants::runtime_trellis_provider::Participant;
use trellis_rs::auth::{
    discover_native_oauth, session_public_key, NativeCredentialScope, NativeOAuthClient,
    NativeOAuthServer,
};
use trellis_rs::client::{ServiceConnectWithContractOptions, TrellisClient, UserConnectOptions};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::apis::trellis_auth_v1::rpc::{
    ServiceClaimsCreate, ServiceUpdatesStatus, ServiceUpdatesSubmit,
};
use trellis_testkit::TrellisTestRuntime;

fn action(name: &str) -> trellis_protocol::CatalogActionIdentity {
    trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: name.into(),
        direction: trellis_protocol::PermissionAction::Call,
    }
}

/// A service claim's identity, connected the way a service that introduces
/// itself connects before an administrator approves it.
async fn claimed(admin: &TrellisClient, server: &NativeOAuthServer, title: &str) -> TrellisClient {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).unwrap();
    let seed = URL_SAFE_NO_PAD.encode(seed);
    admin
        .request_json_value(
            ServiceClaimsCreate::API_ID,
            action("ServiceClaims.Create"),
            ServiceClaimsCreate::SUBJECT,
            &serde_json::json!({
                "title": title,
                "instanceTitle": title,
                "identityPublicKey": session_public_key(&seed).unwrap(),
                "idempotencyKey": ulid::Ulid::generate().to_string(),
            }),
        )
        .await
        .unwrap();
    TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
        nats_servers: server.nats_servers.clone(),
        trust: server.trust.clone(),
        participant_id: Participant::ID,
        provisioned_identity_seed_base64url: &seed,
        name: None,
        timeout_ms: 10_000,
    })
    .await
    .unwrap()
}

/// The raw reply to `ServiceUpdates.Status` for `update`, with the error and
/// request IDs every reply gets on its own left out.
async fn status_reply(client: &TrellisClient, update: &str) -> Result<String, String> {
    match client
        .request_json_value(
            ServiceUpdatesStatus::API_ID,
            action("ServiceUpdates.Status"),
            ServiceUpdatesStatus::SUBJECT,
            &serde_json::json!({ "updateId": update }),
        )
        .await
    {
        Ok(reply) => Ok(reply.to_string()),
        Err(trellis_rs::client::TrellisClientError::RpcError(payload)) => {
            let value = payload.value().unwrap();
            let id = value["id"].as_str().unwrap();
            let request = value["context"]["requestId"].as_str().unwrap();
            Err(payload
                .raw()
                .replace(id, "<id>")
                .replace(request, "<request>"))
        }
        Err(other) => panic!("unexpected failure: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_services_update_is_answered_exactly_like_a_missing_one() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("update-status-admin"),
        NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::generate(),
        },
        server.trust.clone(),
    )
    .await
    .unwrap();
    let login = native
        .start_device(&serde_json::json!([{
            "type": "trellis_capabilities",
            "required": [ServiceClaimsCreate::CALLER_CAPABILITIES[0]],
            "optional": []
        }]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", login.user_code())
        .await
        .unwrap();
    let admin = TrellisClient::connect_user(UserConnectOptions::new(
        server.nats_servers.clone(),
        10_000,
        login.complete().await.unwrap(),
    ))
    .await
    .unwrap();

    // One waiting service submits its build; a claim's update always waits
    // for an administrator, so it stays on record.
    let owner = claimed(&admin, &server, "owner").await;
    let other = claimed(&admin, &server, "other").await;
    let submitted = owner
        .request_json_value(
            ServiceUpdatesSubmit::API_ID,
            action("ServiceUpdates.Submit"),
            ServiceUpdatesSubmit::SUBJECT,
            &serde_json::json!({
                "participantId": Participant::ID,
                "packageEvidence": Participant::package_evidence(),
            }),
        )
        .await
        .unwrap();
    let update = submitted["update"]["updateId"].as_str().unwrap().to_owned();
    assert!(status_reply(&owner, &update).await.is_ok());

    // The other service is told the same about the owner's update as about
    // one that was never submitted, byte for byte but for the ID asked about.
    let hidden = status_reply(&other, &update).await.unwrap_err();
    let unknown = ulid::Ulid::generate().to_string();
    let missing = status_reply(&other, &unknown).await.unwrap_err();
    assert!(missing.contains("not_found"), "{missing}");
    assert_eq!(
        hidden.replace(&update, "<update>"),
        missing.replace(&unknown, "<update>"),
    );
    drop((owner, other, admin));
    runtime.shutdown().await.unwrap();
}
