//! Issuer key rotations and revocations in quick succession, over the
//! production runtime.
use std::path::PathBuf;

use serde_json::{json, Value};
use trellis_rs::auth::{
    discover_native_oauth, NativeCredentialScope, NativeOAuthClient, NativeOAuthSession,
};
use trellis_rs::client::{
    RuntimeAuthorizationTrust, TrellisClient, TrellisClientError, UserConnectOptions,
};
use trellis_runtime_apis::apis::trellis_auth_v1::rpc::{
    AuthorizationSessionsList, ConnectionsList, IssuersRevoke, IssuersRotate,
};
use trellis_testkit::TrellisTestRuntime;

/// Before chain links were published ahead of what their keys sign, every
/// run failed by the eighth round; 25 stays inside Auth's per-address
/// sign-in budget, which each round's fresh session draws on.
const ROUNDS: usize = 25;

fn action(name: &str) -> trellis_protocol::CatalogActionIdentity {
    trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: name.into(),
        direction: trellis_protocol::PermissionAction::Call,
    }
}

/// One administrator's sign-in, connected again for each round.
struct Admin {
    custody: PathBuf,
    scope: NativeCredentialScope,
    trust: RuntimeAuthorizationTrust,
    nats_servers: Vec<String>,
}

impl Admin {
    async fn sign_in(runtime: &mut TrellisTestRuntime) -> (Self, NativeOAuthSession) {
        let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
            .await
            .unwrap();
        let scope = NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::generate(),
        };
        let custody = runtime.workdir().join("issuer-race-admin");
        let login = NativeOAuthClient::open(custody.clone(), scope.clone(), server.trust.clone())
            .await
            .unwrap()
            .start_device(&json!([
                {"type":"trellis_capabilities","required":[
                    IssuersRotate::CALLER_CAPABILITIES[0],
                    IssuersRevoke::CALLER_CAPABILITIES[0],
                    AuthorizationSessionsList::CALLER_CAPABILITIES[0],
                    ConnectionsList::CALLER_CAPABILITIES[0]
                ],"optional":[]}
            ]))
            .await
            .unwrap();
        runtime
            .approve_device_login("trellis.cli", login.user_code())
            .await
            .unwrap();
        let session = login.complete().await.unwrap();
        let admin = Self {
            custody,
            scope,
            trust: server.trust,
            nats_servers: server.nats_servers,
        };
        (admin, session)
    }

    /// A new session on the same grant, its pin moved on to the current key.
    /// Returns that key's ID with the connected client.
    async fn connect(&self, session: Option<NativeOAuthSession>) -> (String, TrellisClient) {
        let session = match session {
            Some(session) => session,
            None => NativeOAuthClient::open(
                self.custody.clone(),
                self.scope.clone(),
                self.trust.clone(),
            )
            .await
            .unwrap()
            .resume()
            .await
            .unwrap(),
        };
        let current = session.trust.issuer.key_id.clone();
        let client = TrellisClient::connect_user(UserConnectOptions::new(
            self.nats_servers.clone(),
            10_000,
            session,
        ))
        .await
        .unwrap();
        (current, client)
    }
}

async fn call(
    client: &TrellisClient,
    name: &str,
    api: &'static str,
    subject: &'static str,
    input: Value,
) -> Result<Value, TrellisClientError> {
    client
        .request_json_value(api, action(name), subject, &input)
        .await
}

/// Assert that each service session Auth holds as active belongs to a
/// connected participant, and return how many there are. A session whose
/// participant went away renews nothing, so a routine issuer revoke would
/// end it.
async fn assert_active_services_connected(client: &TrellisClient, when: &str) -> usize {
    let sessions = call(
        client,
        "AuthorizationSessions.List",
        AuthorizationSessionsList::API_ID,
        AuthorizationSessionsList::SUBJECT,
        json!({"state": "active", "page": {"limit": 200}}),
    )
    .await
    .unwrap();
    assert!(sessions["page"]["nextCursor"].is_null(), "{when}: one page");
    let services = sessions["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|session| session["principalKind"] == "service")
        .collect::<Vec<_>>();
    for session in &services {
        let connections = call(
            client,
            "Connections.List",
            ConnectionsList::API_ID,
            ConnectionsList::SUBJECT,
            json!({
                "authorizationSessionId": session["authorizationSessionId"],
                "state": "admitted",
            }),
        )
        .await
        .unwrap();
        assert!(
            !connections["items"].as_array().unwrap().is_empty(),
            "{when}: active session {} of {} has no connection",
            session["authorizationSessionId"],
            session["binding"]["participantId"],
        );
    }
    services.len()
}

/// Rotations and revocations of the key the previous rotation retired run
/// together, round after round. Auth keeps answering, a client works as soon
/// as it connects, and every service session Trellis holds stays connected.
///
/// A rotation's key signs from the moment it commits. Were anything it
/// signed to reach a participant before the chain link that introduces it,
/// the participant could not authenticate it: Auth's answers would go
/// unverified until requests time out, and a revocation mirror holding such
/// a cutoff would lose coverage, failing requests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rotating_and_revoking_in_quick_succession_keeps_trellis_answering() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let (admin, session) = Admin::sign_in(&mut runtime).await;
    let mut session = Some(session);
    let mut retired: Option<String> = None;
    for round in 0..ROUNDS {
        // A session holding authority from a key ends when that key is
        // revoked, so each round connects afresh at the key current then.
        let (current, client) = admin.connect(session.take()).await;
        if round == 0 {
            assert!(assert_active_services_connected(&client, "before rotating").await > 0);
        }
        let rotation = call(
            &client,
            "Issuers.Rotate",
            IssuersRotate::API_ID,
            IssuersRotate::SUBJECT,
            json!({
                "expectedCurrentKeyId": current,
                "idempotencyKey": ulid::Ulid::generate().to_string(),
            }),
        );
        let revocation = async {
            match &retired {
                Some(key) => Some(
                    call(
                        &client,
                        "Issuers.Revoke",
                        IssuersRevoke::API_ID,
                        IssuersRevoke::SUBJECT,
                        json!({
                            "keyId": key,
                            "idempotencyKey": ulid::Ulid::generate().to_string(),
                        }),
                    )
                    .await,
                ),
                None => None,
            }
        };
        let (rotated, revoked) = tokio::join!(rotation, revocation);
        let rotated =
            rotated.unwrap_or_else(|error| panic!("round {round}: rotating {current}: {error:?}"));
        let next = rotated["keyId"].as_str().unwrap().to_owned();
        assert_ne!(next, current, "round {round}");
        if let Some(revoked) = revoked {
            let revoked = revoked
                .unwrap_or_else(|error| panic!("round {round}: revoking {retired:?}: {error:?}"));
            assert_eq!(
                revoked["keyId"].as_str(),
                retired.as_deref(),
                "round {round}"
            );
        }
        retired = Some(current);
    }
    let (_, client) = admin.connect(None).await;
    assert!(assert_active_services_connected(&client, "after the rounds").await > 0);
    drop(client);
    runtime.shutdown().await.unwrap();
}
