//! Public delegation revocation must retire only the selected authorization root.

use trellis_rs::auth::{
    discover_native_oauth, NativeCredentialScope, NativeOAuthClient, NativeOAuthError,
};
use trellis_rs::client::UserConnectOptions;
use trellis_runtime_apis::apis::trellis_auth_v1::Client;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delegation_revocation_is_atomic_idempotent_and_grant_scoped() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let mut sessions = Vec::new();
    for name in ["manager", "target", "sibling"] {
        let native = NativeOAuthClient::open(
            runtime.workdir().join(name),
            NativeCredentialScope {
                issuer: server.issuer.clone(),
                instance_id: server.trust.instance_id.clone(),
                account: server.trust.account.clone(),
                client_id: "trellis.cli".into(),
                credential_handle: ulid::Ulid::new(),
            },
            server.trust.clone(),
        )
        .await
        .unwrap();
        let device = native
            .start_device(&serde_json::json!([
                {
                    "type":"trellis_capabilities",
                    "required": [
                        "trellis.auth.v1:account.read",
                        "trellis.auth.v1:delegations.read",
                        "trellis.auth.v1:delegations.revoke",
                        "trellis.auth.v1:sessions.read"
                                ],
                    "optional": []
                }
            ]))
            .await
            .unwrap();
        runtime
            .approve_device_login("trellis.cli", device.user_code())
            .await
            .unwrap();
        sessions.push(device.complete().await.unwrap());
    }
    let mut sibling = sessions.pop().unwrap();
    let mut target = sessions.pop().unwrap();
    let target_grant = target.admission.credential.oauth_grant_id.clone();
    let target_login = target.admission.credential.login_session_id.clone();
    let admin = Client::from_generated(
        trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            server.nats_servers.clone(),
            5_000,
            sessions.pop().unwrap(),
        ))
        .await
        .unwrap(),
    );
    target.refresh(server.trust.clone()).await.unwrap();
    let delegation = admin
        .delegations_review(
            &serde_json::from_value(serde_json::json!({
                "delegationId": target_grant
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let request = serde_json::from_value(serde_json::json!({
        "delegationId": target_grant, "expectedRevision": delegation.delegation.revision,
        "idempotencyKey": ulid::Ulid::new().to_string()
    }))
    .unwrap();
    let revoked = admin.delegations_revoke(&request).await.unwrap();
    let replayed = admin.delegations_revoke(&request).await.unwrap();
    assert_eq!(
        serde_json::to_value(revoked).unwrap(),
        serde_json::to_value(replayed).unwrap()
    );
    assert!(matches!(
        target.refresh(server.trust.clone()).await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    assert!(
        trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            server.nats_servers.clone(),
            5_000,
            target
        ))
        .await
        .is_err(),
        "revoked grant cannot admit a session with its previously issued credential"
    );
    // Same client/principal binding, independently authorized grant: still usable.
    sibling.refresh(server.trust).await.unwrap();
    let sibling = Client::from_generated(
        trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            server.nats_servers,
            5_000,
            sibling,
        ))
        .await
        .unwrap(),
    );
    sibling
        .users_resolve(&serde_json::from_value(serde_json::json!({})).unwrap())
        .await
        .unwrap();
    let logins = admin
        .sessions_list(&serde_json::from_value(serde_json::json!({})).unwrap())
        .await
        .unwrap();
    let login = logins
        .items
        .iter()
        .find(|login| login.login_session_id.to_string() == target_login)
        .unwrap();
    assert!(
        serde_json::to_value(&login.state).unwrap() == "active",
        "delegation retirement must preserve its Portal login"
    );
    drop(sibling);
    drop(admin);
    runtime.shutdown().await.unwrap();
}
