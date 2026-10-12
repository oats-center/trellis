//! Native SDK custody and refresh against the production OAuth HTTP engine.
use super::super::oauth::{OAuthEngine, OAuthListener, OAuthStorage, OAuthTransport};
use super::*;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use trellis_rs::{
    auth::{NativeCredentialScope, NativeOAuthClient, NativeOAuthError},
    client::RuntimeAuthorizationTrust,
};

#[tokio::test]
async fn native_device_approval_refresh_and_revocation_retain_connection_identity_and_exclusive_custody(
) {
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    f.store.settings.oauth_origin = origin.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let registration = ClientRegistration {
        client_id: "device-only-native".into(),
        kind: "native".into(),
        display_name: "Device-only native client".into(),
        redirect_uris: Vec::new(),
        development: false,
        implied_capabilities: Vec::new(),
        eligible_privileges: Vec::new(),
        metadata: serde_json::json!({
            "grant_types": ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"]
        }),
        expected_revision: 0,
    };
    f.store
        .register_client(&mutation(&f.admin, &registration), &registration, now)
        .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: f.admin.clone(),
            capability_id: f.read.clone(),
            expected_revision: 0,
            expires_at: None,
        },
        now,
    );
    let trust = RuntimeAuthorizationTrust {
        instance_id: f.store.settings.instance_id.clone(),
        account: f.store.settings.account.clone(),
        rotation_sequence: 0,
        issuer: f.store.issuer(),
    };
    let scope = NativeCredentialScope {
        issuer: origin.clone(),
        instance_id: trust.instance_id.clone(),
        account: trust.account.clone(),
        client_id: registration.client_id,
        credential_handle: ulid::Ulid::new(),
    };
    let custody = f.directory.path().join("native-oauth");
    let authority = Arc::new(f.store);
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
    );
    let listener = OAuthListener::load(socket, &origin, &OAuthTransport::LoopbackDevelopment)
        .await
        .unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = tokio::spawn(listener.run(
        super::super::oauth::router(engine, vec!["nats://127.0.0.1:4222".into()]),
        shutdown.clone(),
    ));

    let client = NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
        .await
        .unwrap();
    let flow = client
        .start_device(
            &serde_json::json!([{"type":"trellis_capabilities","required":[f.read],"optional":[]}]),
        )
        .await
        .unwrap();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let cookie_header = |response: &reqwest::Response| {
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let csrf_cookie = |cookies: &[String]| {
        cookies
            .iter()
            .find_map(|cookie| cookie.strip_prefix("__Host-trellis-portal-csrf="))
            .unwrap()
            .to_owned()
    };
    let entry = http.get(flow.verification_uri()).send().await.unwrap();
    assert_eq!(entry.status(), reqwest::StatusCode::OK);
    let mut cookies = cookie_header(&entry);
    let mut csrf = csrf_cookie(&cookies);
    let entry = http
        .post(format!("{origin}/portal/device"))
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[("csrf", csrf.as_str()), ("user_code", flow.user_code())])
        .send()
        .await
        .unwrap();
    assert_eq!(entry.status(), reqwest::StatusCode::SEE_OTHER);
    let login = url::Url::parse(&origin)
        .unwrap()
        .join(entry.headers()["location"].to_str().unwrap())
        .unwrap();
    let transaction = login
        .query_pairs()
        .find(|(name, _)| name == "transaction")
        .unwrap()
        .1
        .into_owned();
    let login = http
        .post(format!("{origin}/portal/login"))
        .header("Accept", "text/html")
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("request_id", transaction.as_str()),
            ("username", "admin"),
            ("password", "a strong bootstrap password"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::SEE_OTHER);
    cookies = cookie_header(&login);
    csrf = csrf_cookie(&cookies);
    let consent = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header("Cookie", cookies.join("; "))
        .send()
        .await
        .unwrap();
    assert_eq!(consent.status(), reqwest::StatusCode::OK);
    let consent: serde_json::Value = consent.json().await.unwrap();
    assert_eq!(consent["deviceUserCode"], flow.user_code());
    let approved = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("transaction", transaction.as_str()),
            ("presentation", consent["presentationId"].as_str().unwrap()),
            ("decision", "approve"),
            ("approved", f.read.as_str()),
            ("confirm_device", flow.user_code()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), reqwest::StatusCode::SEE_OTHER);
    let mut session = tokio::time::timeout(Duration::from_secs(15), flow.complete())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.admission.credential.principal_id, f.admin);
    let runtime = session.runtime_id.clone();
    let nkey = session.auth.session_nkey().unwrap();
    let grant = session.admission.credential.oauth_grant_id.clone();
    let family = session.admission.credential.refresh_family_id.clone();
    for _ in 0..2 {
        session.refresh(trust.clone()).await.unwrap();
        assert_eq!(session.runtime_id, runtime);
        assert_eq!(session.auth.session_nkey().unwrap(), nkey);
        assert_eq!(session.admission.credential.oauth_grant_id, grant);
        assert_eq!(session.admission.credential.refresh_family_id, family);
        let (_, public) = nkeys::from_public_key(&nkey).unwrap();
        let inbox =
            trellis_protocol::admission_inbox_prefix(&URL_SAFE_NO_PAD.encode(public)).unwrap();
        let admitted = authority
            .admit_oauth(
                &serde_json::to_vec(&session.admission).unwrap(),
                &nkey,
                &inbox,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
            )
            .unwrap();
        assert_eq!(admitted.authority.unsigned.principal_id, f.admin);
    }

    // A second ordinary client cannot acquire the credential while its session
    // remains alive, including after a rejected refresh. Its acquisition only
    // completes after drop, and its resume must observe the durable spent marker.
    let (started, waiting) = tokio::sync::oneshot::channel();
    let competing_trust = trust.clone();
    let mut competing = tokio::spawn(async move {
        started.send(()).unwrap();
        NativeOAuthClient::open(custody, scope, competing_trust).await
    });
    waiting.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut competing)
            .await
            .is_err()
    );
    let revocation = EnforcementScope::Grant(grant);
    authority
        .revoke_root(
            &mutation(&f.admin, &revocation),
            &revocation,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
    assert!(matches!(
        session.refresh(trust.clone()).await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    assert!(matches!(
        session.refresh(trust).await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut competing)
            .await
            .is_err()
    );
    drop(session);
    let reopened = tokio::time::timeout(Duration::from_secs(5), competing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        reopened.resume().await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    shutdown.cancel();
    server.await.unwrap().unwrap();
}
