//! Issuer key rotations and revocations in quick succession, and native
//! session renewal under Auth's per-address request limit, over the
//! production runtime.
use std::path::PathBuf;
use std::time::Duration;

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
use trellis_testkit::{TestNatsUser, TrellisTestRuntime};

/// Before chain links were published ahead of what their keys sign, every
/// run failed by the eighth round; 25 stays inside Auth's per-address
/// sign-in budget, which each round's fresh session draws on.
const ROUNDS: usize = 25;

/// A short activation delay, an ordinary configuration setting, keeps these
/// tests quick; production installations default to 60 seconds.
const ACTIVATION_DELAY: Duration = Duration::from_secs(1);

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
    async fn sign_in(runtime: &mut TrellisTestRuntime, name: &str) -> (Self, NativeOAuthSession) {
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
        let custody = runtime.workdir().join(name);
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

/// The issuer key that signed the client's current own authority.
fn own_issuer(client: &TrellisClient) -> String {
    client
        .authorization_context()
        .unwrap()
        .unwrap()
        .context
        .unsigned
        .issuer_key_id
}

/// Wait, bounded, until Auth's issuer notice has renewed the client onto
/// authority `key` signed, which follows the activation of `key`.
async fn wait_for_own_issuer(client: &TrellisClient, key: &str, when: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while own_issuer(client) != key {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{when}: the session never moved onto issuer key {key}"));
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
/// Were anything a new key signed to reach a participant before the chain
/// link that introduces it, the participant could not authenticate it, and
/// Auth's answers would go unverified until requests time out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rotating_and_revoking_in_quick_succession_keeps_trellis_answering() {
    let mut runtime = TrellisTestRuntime::builder()
        .issuer_activation_delay(ACTIVATION_DELAY)
        .start()
        .await
        .unwrap();
    let (admin, session) = Admin::sign_in(&mut runtime, "issuer-race-admin").await;
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
        // The next round rotates and revokes once the new key signs.
        wait_for_own_issuer(&client, &next, &format!("round {round}")).await;
        retired = Some(current);
    }
    let (_, client) = admin.connect(None).await;
    assert!(assert_active_services_connected(&client, "after the rounds").await > 0);
    drop(client);
    runtime.shutdown().await.unwrap();
}

/// Rounds of the test that revokes a key as Auth retires it. Each round signs
/// in again, and with the final sign-in they stay inside Auth's per-address
/// budget of 120 sign-in requests a minute. Before revokes refused a request
/// made under the key, every run failed within the second round.
const RETIREMENT_ROUNDS: usize = 6;

/// Revoke `key`, returning the call's outcome as the operator sees it.
async fn revoke(client: &TrellisClient, key: &str) -> Result<Value, TrellisClientError> {
    call(
        client,
        "Issuers.Revoke",
        IssuersRevoke::API_ID,
        IssuersRevoke::SUBJECT,
        json!({
            "keyId": key,
            "idempotencyKey": ulid::Ulid::generate().to_string(),
        }),
    )
    .await
}

/// Assert that a revoke got Auth's refusal, a definitive answer, and return
/// its text. Any other error, a timeout or a reply the client would not
/// accept, leaves the operator not knowing whether the key is revoked.
fn refusal(outcome: Result<Value, TrellisClientError>, when: &str) -> String {
    match outcome {
        Err(TrellisClientError::RpcError(payload)) => payload.raw().to_owned(),
        other => panic!("{when}: no definitive answer: {other:?}"),
    }
}

/// An operator revokes key K while Trellis retires it: concurrently with the
/// rotation from K, and then again and again across the moment the activation
/// switches signing off K. Every revoke gets a definitive answer. A refusal
/// writes nothing, so no later revoke finds K already revoked, and the one
/// that succeeds is reflected: a revoke after it is refused as already
/// revoked. Auth keeps answering throughout.
///
/// Were the revoke to commit while Auth's reply credentials or the request's
/// own authority still came from K, the reply would not verify once the
/// caller learned of the revocation, and the operator would get an error or
/// a timeout for a revoke that took effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_a_key_as_trellis_retires_it_always_gets_a_definitive_answer() {
    let mut runtime = TrellisTestRuntime::builder()
        .issuer_activation_delay(ACTIVATION_DELAY)
        .start()
        .await
        .unwrap();
    for round in 0..RETIREMENT_ROUNDS {
        // Revoking K retires the session, which holds authority K signed,
        // and the custody stays pinned to K, so each round signs in afresh.
        let (admin, session) =
            Admin::sign_in(&mut runtime, &format!("issuer-retirement-admin-{round}")).await;
        let (key, client) = admin.connect(Some(session)).await;
        let (rotated, revoked) = tokio::join!(
            call(
                &client,
                "Issuers.Rotate",
                IssuersRotate::API_ID,
                IssuersRotate::SUBJECT,
                json!({
                    "expectedCurrentKeyId": key,
                    "idempotencyKey": ulid::Ulid::generate().to_string(),
                }),
            ),
            revoke(&client, &key),
        );
        let rotated =
            rotated.unwrap_or_else(|error| panic!("round {round}: rotating {key}: {error:?}"));
        // K signs on either side of the rotation, so the revoke is refused.
        let refused = refusal(
            revoked,
            &format!("round {round}: revoking during the rotation"),
        );
        assert!(
            refused.contains("current signing key"),
            "round {round}: {refused}"
        );
        let activates_at = time::OffsetDateTime::parse(
            rotated["activatesAt"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let lead =
            activates_at - time::OffsetDateTime::now_utc() - time::Duration::milliseconds(200);
        if lead.is_positive() {
            tokio::time::sleep(lead.unsigned_abs()).await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut attempts = 0usize;
        let revoked = loop {
            attempts += 1;
            let when = format!("round {round}: revoke {attempts} of {key} at its retirement");
            match revoke(&client, &key).await {
                Ok(revoked) => break revoked,
                outcome => {
                    let refused = refusal(outcome, &when);
                    assert!(
                        !refused.contains("already revoked"),
                        "{when}: an earlier refused revoke took effect: {refused}"
                    );
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{when}: the key never became revocable"
            );
        };
        assert_eq!(
            revoked["keyId"].as_str(),
            Some(key.as_str()),
            "round {round}"
        );
        let again = refusal(
            revoke(&client, &key).await,
            &format!("round {round}: revoking {key} again"),
        );
        assert!(again.contains("already revoked"), "round {round}: {again}");
        drop(client);
    }
    let (admin, session) = Admin::sign_in(&mut runtime, "issuer-retirement-admin").await;
    let (_, client) = admin.connect(Some(session)).await;
    assert!(assert_active_services_connected(&client, "after the rounds").await > 0);
    drop(client);
    runtime.shutdown().await.unwrap();
}

/// A rotation announces its key before Auth signs with it: the chain link is
/// in the registry at once, the current key keeps signing, and only after the
/// activation delay does Auth sign with the new key. Requests run throughout,
/// and none of Auth's replies is rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rotation_announces_its_key_before_auth_signs_with_it() {
    let delay = Duration::from_secs(4);
    let mut runtime = TrellisTestRuntime::builder()
        .issuer_activation_delay(delay)
        .start()
        .await
        .unwrap();
    let (admin, session) = Admin::sign_in(&mut runtime, "issuer-race-admin").await;
    let (current, client) = admin.connect(Some(session)).await;
    assert_eq!(own_issuer(&client), current);
    let client = std::sync::Arc::new(client);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let traffic = tokio::spawn({
        let (client, stop) = (client.clone(), stop.clone());
        async move {
            let mut calls = 0usize;
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                call(
                    &client,
                    "AuthorizationSessions.List",
                    AuthorizationSessionsList::API_ID,
                    AuthorizationSessionsList::SUBJECT,
                    json!({"state": "active", "page": {"limit": 1}}),
                )
                .await
                .unwrap_or_else(|error| panic!("call {calls} failed: {error:?}"));
                calls += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            calls
        }
    });
    let rotated = call(
        &client,
        "Issuers.Rotate",
        IssuersRotate::API_ID,
        IssuersRotate::SUBJECT,
        json!({
            "expectedCurrentKeyId": current,
            "idempotencyKey": ulid::Ulid::generate().to_string(),
        }),
    )
    .await
    .unwrap();
    let next = rotated["keyId"].as_str().unwrap().to_owned();
    let activates_at = time::OffsetDateTime::parse(
        rotated["activatesAt"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();

    // The chain link is in the registry well before the new key signs.
    let nats = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::Trellis).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let material = async_nats::jetstream::new(nats)
        .get_key_value("TRELLIS_AUTH_MATERIAL")
        .await
        .unwrap();
    // The installation's first rotation.
    let key = "rotation.1";
    tokio::time::timeout(Duration::from_secs(2), async {
        while material.get(key).await.unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the chain link is published at once");
    // Auth still signs with the current key: a renewal now returns authority
    // it signed, and the key cannot be revoked while it signs.
    let renewed = client.refresh_authorization_context().await.unwrap();
    assert!(time::OffsetDateTime::now_utc() < activates_at);
    assert_eq!(renewed.context.unsigned.issuer_key_id, current);
    let refused = call(
        &client,
        "Issuers.Revoke",
        IssuersRevoke::API_ID,
        IssuersRevoke::SUBJECT,
        json!({
            "keyId": current,
            "idempotencyKey": ulid::Ulid::generate().to_string(),
        }),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{refused:?}").contains(&next),
        "the refusal names the key that takes over: {refused:?}"
    );

    // After the delay Auth switches, and its notice renews the session onto
    // the new key.
    wait_for_own_issuer(&client, &next, "after activation").await;
    assert!(time::OffsetDateTime::now_utc() >= activates_at);
    let renewed = client.refresh_authorization_context().await.unwrap();
    assert_eq!(renewed.context.unsigned.issuer_key_id, next);
    stop.store(true, std::sync::atomic::Ordering::Release);
    assert!(traffic.await.unwrap() > 0);
    drop(client);
    runtime.shutdown().await.unwrap();
}

/// Auth limits OAuth requests per address, and every participant behind one
/// address draws on that budget: other people's sign-ins can spend it. A
/// native session whose renewal meets the limit is refused before Auth reads
/// the refresh token, so the session keeps working and its grant stays
/// usable instead of ending as though the token were spent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renewal_auth_limits_leaves_the_native_session_working() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let (admin, session) = Admin::sign_in(&mut runtime, "budget-admin").await;
    let (_, client) = admin.connect(Some(session)).await;

    // Others behind this address spend the budget.
    let limited = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots()).await {
                Ok(_) => {}
                Err(trellis_rs::auth::NativeOAuthError::Busy) => return,
                Err(error) => panic!("discovery failed otherwise: {error:?}"),
            }
        }
    })
    .await;
    assert!(limited.is_ok(), "Auth never limited the address");

    let refused = client.refresh_authorization_context().await.unwrap_err();
    assert!(
        refused.to_string().contains("limiting"),
        "the renewal names the limit: {refused}"
    );
    call(
        &client,
        "AuthorizationSessions.List",
        AuthorizationSessionsList::API_ID,
        AuthorizationSessionsList::SUBJECT,
        json!({"state": "active", "page": {"limit": 1}}),
    )
    .await
    .expect("the session still works after a limited renewal");
    drop(client);
    runtime.shutdown().await.unwrap();
}
