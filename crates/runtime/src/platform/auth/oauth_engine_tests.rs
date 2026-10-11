use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::super::oauth::{OAuthEngine, OAuthStorage, TrellisTokenResponse};
use super::*;
use oauth_as::{
    authorization::AuthorizationRequest,
    jwt::{EcdsaP256Key, JwsSigner},
    server::TokenRequest,
};

#[tokio::test]
async fn oauth_preflight_uses_validated_callbacks_and_origin_rotation_cannot_evade_peer_limits() {
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    f.store.settings.oauth_origin = origin.clone();
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(Arc::new(f.store), &[17; 32]).unwrap()).unwrap(),
    );
    let listener = super::super::oauth::OAuthListener::load(
        socket,
        &origin,
        &super::super::oauth::OAuthTransport::LoopbackDevelopment,
    )
    .await
    .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let stopped = stop.clone();
    let host = tokio::spawn(async move {
        listener
            .run(super::super::oauth::router(engine, vec![]), stopped)
            .await
            .unwrap();
    });
    let http = reqwest::Client::new();
    for (callback, allowed) in [
        ("http://127.0.0.1:9123", true),
        ("https://unapproved.example", false),
    ] {
        let response = http
            .request(reqwest::Method::OPTIONS, format!("{origin}/token"))
            .header("Origin", callback)
            .header("Access-Control-Request-Method", "POST")
            .header("Access-Control-Request-Headers", "content-type,dpop")
            .send()
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Origin")
                .map(|value| value.to_str().unwrap()),
            allowed.then_some(callback)
        );
        assert!(!response
            .headers()
            .contains_key("Access-Control-Allow-Credentials"));
    }
    let mut throttled = false;
    for attempt in 0..121 {
        let response = http
            .get(format!("{origin}/.well-known/oauth-authorization-server"))
            .header("Origin", format!("https://rotated-{attempt}.example"))
            .header("X-Forwarded-For", format!("203.0.113.{}", attempt % 255))
            .send()
            .await
            .unwrap();
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            assert!(response.headers().contains_key("Retry-After"));
            throttled = true;
            break;
        }
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }
    assert!(
        throttled,
        "varying untrusted origin and proxy headers bypassed the actual peer budget"
    );
    let response = http
        .get(format!("{origin}/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    stop.cancel();
    host.await.unwrap();
}

#[tokio::test]
async fn chromium_portal_device_approval_produces_usable_bound_admission() {
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    f.store.settings.oauth_origin = origin.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
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
    let authority = Arc::new(f.store);
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
    );
    let listener = super::super::oauth::OAuthListener::load(
        socket,
        &origin,
        &super::super::oauth::OAuthTransport::LoopbackDevelopment,
    )
    .await
    .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let stopped = stop.clone();
    let hosted = engine.clone();
    let host = tokio::spawn(async move {
        listener
            .run(super::super::oauth::router(hosted, vec![]), stopped)
            .await
            .unwrap();
    });
    let key = EcdsaP256Key::generate("browser-device-client");
    let ephemeral = nkeys::KeyPair::new_user();
    let runtime = URL_SAFE_NO_PAD.encode([39; 16]);
    let details =
        serde_json::json!([{"type":"trellis_capabilities","required":[f.read],"optional":[]}])
            .to_string();
    let http = reqwest::Client::new();
    let proof = dpop(
        &key,
        &runtime,
        &ephemeral,
        &format!("{origin}/device_authorization"),
    )
    .await;
    let response = http
        .post(format!("{origin}/device_authorization"))
        .header("DPoP", proof)
        .form(&[
            ("client_id", "native-client"),
            ("authorization_details", details.as_str()),
            ("trellis_runtime_id", runtime.as_str()),
            ("trellis_session_nkey", ephemeral.public_key().as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let device: oauth_as::device::DeviceAuthorizationResponse = response.json().await.unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let browser = tokio::process::Command::new("node")
        .args(["--test", "ts/browser/oauth_portal.browser_test.ts"])
        .env(
            "TRELLIS_TEST_OAUTH_VERIFICATION_URI",
            &device.verification_uri,
        )
        .env("TRELLIS_TEST_OAUTH_USER_CODE", &device.user_code)
        .current_dir(root)
        .kill_on_drop(true)
        .output();
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), browser)
        .await
        .expect("Chromium portal flow deadline")
        .expect("Node and producer-installed Playwright Chromium are required");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let token_uri = format!("{origin}/token");
    let response = http
        .post(&token_uri)
        .header("DPoP", dpop(&key, &runtime, &ephemeral, &token_uri).await)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", "native-client"),
            ("device_code", device.device_code.as_str()),
            ("trellis_runtime_id", runtime.as_str()),
            ("trellis_session_nkey", ephemeral.public_key().as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let tokens: TrellisTokenResponse = response.json().await.unwrap();
    let (_, public) = nkeys::from_public_key(&ephemeral.public_key()).unwrap();
    let inbox = trellis_protocol::admission_inbox_prefix(&URL_SAFE_NO_PAD.encode(public)).unwrap();
    let admitted = authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            &inbox,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
    assert_eq!(admitted.authority.unsigned.principal_id, f.admin);
    stop.cancel();
    host.await.unwrap();
}

pub(super) async fn dpop(
    key: &EcdsaP256Key,
    runtime: &str,
    ephemeral: &nkeys::KeyPair,
    token_uri: &str,
) -> String {
    let (_, bytes) = nkeys::from_public_key(&ephemeral.public_key()).unwrap();
    let public = serde_json::json!({"crv":"Ed25519","kty":"OKP","x":URL_SAFE_NO_PAD.encode(bytes)});
    let input = format!("{}.{}", URL_SAFE_NO_PAD.encode(serde_json::json!({"typ":"dpop+jwt","alg":"ES256","jwk":key.public_jwk()}).to_string()), URL_SAFE_NO_PAD.encode(serde_json::json!({"htm":"POST","htu":token_uri,"iat":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),"jti":id(),"trellis_runtime_id":runtime,"trellis_nats_jkt":URL_SAFE_NO_PAD.encode(Sha256::digest(super::super::sqlite::json(&public).unwrap().as_bytes()))}).to_string()));
    let signature = key.sign(input.as_bytes()).await.unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_bytes()))
}

#[tokio::test]
async fn device_code_login_confirmation_and_poll_keep_the_approved_key_and_capability_selection() {
    let mut f = fixture();
    let registration = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Native client".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![],
        eligible_privileges: vec![trellis_protocol::PlatformPrivilege::PrincipalsManage],
        metadata: serde_json::json!({}),
        expected_revision: 1,
    };
    f.store
        .register_client(&mutation(&f.admin, &registration), &registration, NOW)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    f.store.settings.oauth_origin = origin.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for capability in [&f.read, &f.write] {
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::CapabilityGrant {
                principal_id: f.admin.clone(),
                capability_id: capability.clone(),
                expected_revision: 0,
                expires_at: None,
            },
            now,
        );
    }
    let authority = Arc::new(f.store);
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
    );
    let router = super::super::oauth::router(engine.clone(), vec!["nats://127.0.0.1:4222".into()]);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let host = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let key = EcdsaP256Key::generate("device-client");
    let ephemeral = nkeys::KeyPair::new_user();
    let runtime = URL_SAFE_NO_PAD.encode([31; 16]);
    let details=serde_json::json!([{"type":"trellis_capabilities","required":[f.read],"optional":[f.write]},{"type":"trellis_platform_privileges","required":["principals.manage"],"optional":[]}]).to_string();
    let start_proof = dpop(
        &key,
        &runtime,
        &ephemeral,
        &format!("{origin}/device_authorization"),
    )
    .await;
    let response = http
        .post(format!("{origin}/device_authorization"))
        .header("DPoP", &start_proof)
        .form(&[
            ("client_id", "native-client"),
            ("authorization_details", details.as_str()),
            ("trellis_runtime_id", runtime.as_str()),
            ("trellis_session_nkey", ephemeral.public_key().as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let device: oauth_as::device::DeviceAuthorizationResponse = response.json().await.unwrap();
    let replay = http
        .post(format!("{origin}/device_authorization"))
        .header("DPoP", &start_proof)
        .form(&[
            ("client_id", "native-client"),
            ("authorization_details", details.as_str()),
            ("trellis_runtime_id", runtime.as_str()),
            ("trellis_session_nkey", ephemeral.public_key().as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        replay.json::<serde_json::Value>().await.unwrap()["error"],
        "invalid_dpop_proof"
    );
    let stolen_key = EcdsaP256Key::generate("wrong-device-key");
    let token_uri = format!("{origin}/token");
    let wrong_proof = dpop(&stolen_key, &runtime, &ephemeral, &token_uri).await;
    let poll = [
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
        ),
        ("client_id", "native-client".into()),
        ("device_code", device.device_code.clone()),
        ("trellis_runtime_id", runtime.clone()),
        ("trellis_session_nkey", ephemeral.public_key()),
    ];
    let rejected = http
        .post(&token_uri)
        .header("DPoP", wrong_proof)
        .form(&poll)
        .send()
        .await
        .unwrap();
    assert_eq!(
        rejected.json::<serde_json::Value>().await.unwrap()["error"],
        "invalid_grant"
    );
    let entry = http.get(format!("{origin}/device")).send().await.unwrap();
    assert_eq!(entry.status(), reqwest::StatusCode::OK);
    let mut cookies: Vec<String> = entry
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
        .collect();
    let mut csrf = cookies
        .iter()
        .find_map(|value| value.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap()
        .to_owned();
    let entry = http
        .post(format!("{origin}/portal/device"))
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("user_code", device.user_code.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(entry.status(), reqwest::StatusCode::SEE_OTHER);
    let location = entry.headers()[reqwest::header::LOCATION].to_str().unwrap();
    let login_url = url::Url::parse(&format!("{origin}{location}")).unwrap();
    assert_eq!(login_url.path(), "/login");
    let transaction = login_url
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
    cookies = login
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
        .collect();
    csrf = cookies
        .iter()
        .find_map(|value| value.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap()
        .to_owned();
    let consent = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header("Cookie", cookies.join("; "))
        .send()
        .await
        .unwrap();
    assert_eq!(consent.status(), reqwest::StatusCode::OK);
    let consent: serde_json::Value = consent.json().await.unwrap();
    assert_eq!(consent["deviceUserCode"], device.user_code);
    let missing_confirmation = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("transaction", transaction.as_str()),
            ("decision", "approve"),
            ("approved", f.read.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        missing_confirmation.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    // The invalid UI request did not consume the engine's device grant. A fresh
    // continuation still permits the actual user to confirm and approve it.
    let entry = http
        .post(format!("{origin}/portal/device"))
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("user_code", device.user_code.as_str()),
        ])
        .send()
        .await
        .unwrap();
    let location = entry.headers()[reqwest::header::LOCATION].to_str().unwrap();
    let consent_url = url::Url::parse(&format!("{origin}{location}")).unwrap();
    assert_eq!(consent_url.path(), "/consent");
    let transaction = consent_url
        .query_pairs()
        .find(|(name, _)| name == "transaction")
        .unwrap()
        .1
        .into_owned();
    let approved = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("transaction", transaction.as_str()),
            ("decision", "approve"),
            ("approved", f.read.as_str()),
            ("confirm_device", device.user_code.as_str()),
            ("privilege", "principals.manage"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), reqwest::StatusCode::SEE_OTHER);
    let proof = dpop(&key, &runtime, &ephemeral, &token_uri).await;
    let tokens = http
        .post(&token_uri)
        .header("DPoP", proof)
        .form(&poll)
        .send()
        .await
        .unwrap();
    assert_eq!(tokens.status(), reqwest::StatusCode::OK);
    let tokens: TrellisTokenResponse = tokens.json().await.unwrap();
    let granted = serde_json::to_value(&tokens.oauth).unwrap();
    assert_eq!(
        granted["authorization_details"][1]["required"],
        serde_json::json!(["principals.manage"])
    );
    let admitted = authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            "_INBOX.device",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
    assert_eq!(admitted.authority.unsigned.principal_id, f.admin);
    assert_eq!(
        admitted.authority.unsigned.platform_privileges,
        vec![trellis_protocol::PlatformPrivilege::PrincipalsManage]
    );
    assert_eq!(
        admitted
            .authority
            .unsigned
            .capabilities
            .iter()
            .map(|capability| capability.capability_id.as_str())
            .collect::<Vec<_>>(),
        vec![f.read.as_str()]
    );
    let replay = http
        .post(&token_uri)
        .header("DPoP", dpop(&key, &runtime, &ephemeral, &token_uri).await)
        .form(&poll)
        .send()
        .await
        .unwrap();
    assert_eq!(
        replay.json::<serde_json::Value>().await.unwrap()["error"],
        "invalid_grant"
    );
    stop.send(()).unwrap();
    host.await.unwrap();
}

#[tokio::test]
async fn oauth_code_refresh_and_runtime_admission_survive_restart_without_regranting_declines() {
    let mut f = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let token_uri = format!("{origin}/token");
    f.store.settings.oauth_origin = origin.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let key = EcdsaP256Key::generate("native-key");
    let login = f
        .store
        .record_verified_login(
            &VerifiedLogin {
                principal_id: f.principal.clone(),
                provider_id: None,
                upstream_issuer: None,
                upstream_subject: None,
                verified_claims: None,
                expires_at: now + 3600,
            },
            now,
        )
        .unwrap();
    let mut approval = grant_request(&f.principal, &login, &f.read, &f.write, now);
    approval.public_jwk = serde_json::to_value(key.public_jwk()).unwrap();
    approval.durable_dpop_jkt = key.public_jwk().thumbprint();
    approval.binding_value = approval.durable_dpop_jkt.clone();
    approval.approved = vec![f.read.clone()];
    approval.declined = vec![f.write.clone()];
    let verifier = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let request = AuthorizationRequest::from_pairs(vec![
        ("response_type", "code".to_owned()), ("client_id","native-client".to_owned()),
        ("redirect_uri","http://127.0.0.1:9123/callback".to_owned()),
        ("code_challenge",challenge), ("code_challenge_method","S256".to_owned()),
        ("dpop_jkt",approval.durable_dpop_jkt.clone()),
        ("authorization_details",serde_json::json!([{"type":"trellis_capabilities","required":[f.read],"optional":[f.write]}]).to_string()),
    ]);
    let authority = Arc::new(f.store);
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[13; 32]).unwrap()).unwrap(),
    );
    let router = super::super::oauth::router(engine.clone(), vec!["nats://127.0.0.1:4222".into()]);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let host = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let code = engine
        .approve(&request, &mutation(&f.principal, &approval), &approval)
        .await
        .unwrap();
    let ephemeral = nkeys::KeyPair::new_user();
    let runtime = URL_SAFE_NO_PAD.encode([11; 16]);
    let proof = dpop(&key, &runtime, &ephemeral, &token_uri).await;
    let http = reqwest::Client::new();
    let portal_preflight = http
        .request(reqwest::Method::OPTIONS, format!("{origin}/portal/login"))
        .header("origin", "https://app.example")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .unwrap();
    assert!(!portal_preflight
        .headers()
        .contains_key("access-control-allow-origin"));
    let wrong_host = http
        .get(format!("{origin}/.well-known/oauth-authorization-server"))
        .header("Host", "ordinary-app.example")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_host.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        wrong_host.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"invalid_request"})
    );
    let traversal = http
        .get(format!(
            "{origin}/portal-assets/..%2F..%2Fsrc%2Fplatform%2Fauth%2Fmod.rs"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(traversal.status(), reqwest::StatusCode::NOT_FOUND);
    let cross_origin = http
        .post(format!("{origin}/portal/session"))
        .header("Origin", "https://untrusted.example")
        .send()
        .await
        .unwrap();
    assert_eq!(
        cross_origin.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"invalid_request"})
    );
    let guest = http
        .post(format!("{origin}/portal/session"))
        .header("Origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(guest.status(), reqwest::StatusCode::OK);
    let cookies = guest
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|header| {
            header
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap()
        })
        .collect::<BTreeMap<_, _>>();
    let guest_cookie = format!("__Host-trellis-portal={}", cookies["__Host-trellis-portal"]);
    let csrf = cookies["__Host-trellis-portal-csrf"].to_owned();
    let login = http
        .post(format!("{origin}/portal/login"))
        .header("Origin", &origin)
        .header("Cookie", &guest_cookie)
        .form(&[
            ("username", "admin"),
            ("password", "a strong bootstrap password"),
            ("csrf", &csrf),
            ("request_id", &id()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let authenticated_cookie = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|header| header.to_str().unwrap().split(';').next().unwrap())
        .find(|cookie| cookie.starts_with("__Host-trellis-portal="))
        .unwrap()
        .to_owned();
    let retained_guest = http
        .post(format!("{origin}/portal/logout"))
        .header("Origin", &origin)
        .header("Cookie", &guest_cookie)
        .form(&[("csrf", &csrf), ("request_id", &id())])
        .send()
        .await
        .unwrap();
    assert_eq!(
        retained_guest.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"access_denied"})
    );
    let logout = http
        .post(format!("{origin}/portal/logout"))
        .header("Origin", &origin)
        .header("Cookie", &authenticated_cookie)
        .form(&[("csrf", &csrf), ("request_id", &id())])
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), reqwest::StatusCode::OK);
    let retained_login = http
        .post(format!("{origin}/portal/logout"))
        .header("Origin", &origin)
        .header("Cookie", &authenticated_cookie)
        .form(&[("csrf", &csrf), ("request_id", &id())])
        .send()
        .await
        .unwrap();
    assert_eq!(
        retained_login.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"access_denied"})
    );
    let form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("client_id", "native-client".into()),
        ("code", code.code),
        ("redirect_uri", "http://127.0.0.1:9123/callback".into()),
        ("code_verifier", verifier.into()),
        ("trellis_runtime_id", runtime.clone()),
        ("trellis_session_nkey", ephemeral.public_key()),
    ];
    let mut ambiguous = form.clone();
    ambiguous.push((
        "trellis_session_nkey",
        nkeys::KeyPair::new_user().public_key(),
    ));
    let rejected = http
        .post(&token_uri)
        .header("DPoP", &proof)
        .form(&ambiguous)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        rejected.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"invalid_request"})
    );
    let response = http
        .post(&token_uri)
        .header("DPoP", &proof)
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let tokens: TrellisTokenResponse = response.json().await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let initial = authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            "_INBOX.oauthFirst",
            now,
        )
        .unwrap();
    assert_eq!(
        initial
            .authority
            .unsigned
            .capabilities
            .iter()
            .map(|cap| cap.capability_id.as_str())
            .collect::<Vec<_>>(),
        [f.read.as_str()]
    );
    assert!(authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &nkeys::KeyPair::new_user().public_key(),
            "_INBOX.wrong",
            now
        )
        .is_err());
    assert!(authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            "_INBOX.expired",
            now + 300
        )
        .is_err());
    let refresh = tokens.oauth.refresh_token.unwrap();
    stop.send(()).unwrap();
    host.await.unwrap();
    drop(engine);
    drop(authority);
    let mut reopened = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    reopened.settings.oauth_origin = origin;
    let authority = Arc::new(reopened);
    let engine =
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[13; 32]).unwrap()).unwrap();
    let next_ephemeral = nkeys::KeyPair::new_user();
    let next_runtime = URL_SAFE_NO_PAD.encode([12; 16]);
    let refresh_request = TokenRequest::RefreshToken {
        client_id: oauth_as::client::ClientId::new("native-client"),
        client_secret: None,
        refresh_token: refresh,
        scope: None,
    };
    let proof = dpop(&key, &next_runtime, &next_ephemeral, &token_uri).await;
    let next = engine
        .token(
            refresh_request.clone(),
            &proof,
            &next_runtime,
            &next_ephemeral.public_key(),
        )
        .await
        .unwrap();
    let resumed = authority
        .admit_oauth(
            next.trellis_admission.as_bytes(),
            &next_ephemeral.public_key(),
            "_INBOX.oauthRestart",
            now,
        )
        .unwrap();
    assert_ne!(
        initial.authority.unsigned.authorization_session_id,
        resumed.authority.unsigned.authorization_session_id
    );
    assert_ne!(
        initial.authority.unsigned.session_public_key,
        resumed.authority.unsigned.session_public_key
    );
    assert_eq!(
        initial.authority.unsigned.binding,
        resumed.authority.unsigned.binding
    );
    assert_eq!(
        initial.authority.unsigned.capabilities,
        resumed.authority.unsigned.capabilities
    );
    // A proof replay is refused, without spending the replacement refresh token.
    let replacement = TokenRequest::RefreshToken {
        client_id: oauth_as::client::ClientId::new("native-client"),
        client_secret: None,
        refresh_token: next.oauth.refresh_token.clone().unwrap(),
        scope: None,
    };
    assert!(engine
        .token(
            replacement.clone(),
            &proof,
            &next_runtime,
            &next_ephemeral.public_key()
        )
        .await
        .is_err());
    let proof = dpop(&key, &next_runtime, &next_ephemeral, &token_uri).await;
    assert!(engine
        .token(
            replacement,
            &proof,
            &next_runtime,
            &next_ephemeral.public_key()
        )
        .await
        .is_ok());
    // Expiry cleanup removes an expired access credential but retains spent
    // refresh evidence while its family can still mint replacement authority.
    use oauth_as::store::Storage as _;
    let original = engine
        .server
        .store()
        .get_token(&tokens.oauth.access_token)
        .await
        .unwrap()
        .unwrap();
    let expired_at = original
        .expires_at
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    engine.server.store().cleanup_expired(expired_at).unwrap();
    assert!(engine
        .server
        .store()
        .get_token(&tokens.oauth.access_token)
        .await
        .unwrap()
        .is_none());
    // Genuine reuse after cleanup still durably closes the family.
    let proof = dpop(&key, &next_runtime, &next_ephemeral, &token_uri).await;
    assert!(engine
        .token(
            refresh_request,
            &proof,
            &next_runtime,
            &next_ephemeral.public_key()
        )
        .await
        .is_err());
    assert!(authority
        .admit_oauth(
            next.trellis_admission.as_bytes(),
            &next_ephemeral.public_key(),
            "_INBOX.revoked",
            now
        )
        .is_err());
    assert!(authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            "_INBOX.revokedFirst",
            now
        )
        .is_err());
    while authority.enforce_next_page(now).unwrap().is_some() {}
    assert!(authority
        .issue_authority(
            &AdmissionIdentity {
                credential: Credential::OAuthGrant(
                    resumed.authority.unsigned.oauth_grant_id.unwrap()
                ),
                runtime_id: next_runtime,
                session_public_key: resumed.authority.unsigned.session_public_key,
                inbox_prefix: "_INBOX.noRevival".into()
            },
            None,
            now
        )
        .is_err());
    // Reopen again: no process-local revocation state can account for this denial.
    drop(engine);
    drop(authority);
    let reopened = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    assert!(reopened
        .admit_oauth(
            next.trellis_admission.as_bytes(),
            &next_ephemeral.public_key(),
            "_INBOX.reopened",
            now
        )
        .is_err());
}

#[test]
fn portal_local_login_rotates_cookie_survives_restart_and_logout_invalidates_retained_cookie() {
    let f = fixture();
    let guest = f.store.create_portal_session(NOW).unwrap();
    let replacement = URL_SAFE_NO_PAD.encode([19; 32]);
    let request_id = id();
    assert!(f
        .store
        .portal_local_login(
            &guest.cookie,
            "wrong csrf",
            &replacement,
            "admin",
            "a strong bootstrap password",
            &request_id,
            NOW
        )
        .is_err());
    assert!(f
        .store
        .portal_local_login(
            &guest.cookie,
            &guest.csrf,
            &replacement,
            "admin",
            "wrong password",
            &request_id,
            NOW
        )
        .is_err());
    let login = f
        .store
        .portal_local_login(
            &guest.cookie,
            &guest.csrf,
            &replacement,
            "ADMIN",
            "a strong bootstrap password",
            &request_id,
            NOW,
        )
        .unwrap();
    assert_eq!(login.principal_id, f.admin);
    assert_eq!(login.session_id, guest.session_id);
    assert!(f
        .store
        .portal_login(&guest.cookie, &guest.csrf, NOW)
        .is_err());
    assert!(f
        .store
        .portal_local_login(
            &guest.cookie,
            &guest.csrf,
            &replacement,
            "admin",
            "a strong bootstrap password",
            &request_id,
            NOW
        )
        .is_err());
    drop(f.store);
    let store = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let restored = store.portal_login(&replacement, &guest.csrf, NOW).unwrap();
    assert_eq!(restored.login_session_id, login.login_session_id);
    assert!(store
        .portal_login(&replacement, &guest.csrf, restored.expires_at)
        .is_err());
    let logout = mutation(&f.admin, &restored.login_session_id);
    store.portal_logout(&logout, &restored, NOW).unwrap();
    store.portal_logout(&logout, &restored, NOW).unwrap();
    assert!(store.portal_login(&replacement, &guest.csrf, NOW).is_err());
}

#[tokio::test]
async fn oauth_authorization_redirect_login_consent_and_redemption_bind_the_same_user_session() {
    let mut f = fixture();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let grant = PolicyMutation::CapabilityGrant {
        principal_id: f.admin.clone(),
        capability_id: f.read.clone(),
        expected_revision: 0,
        expires_at: None,
    };
    f.store
        .apply_policy(&mutation(&f.admin, &grant), &grant, now)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    f.store.settings.oauth_origin = origin.clone();
    let authority = Arc::new(f.store);
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[13; 32]).unwrap()).unwrap(),
    );
    let router = super::super::oauth::router(engine, vec!["nats://127.0.0.1:4222".into()]);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let host = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let key = EcdsaP256Key::generate("native-flow");
    let verifier = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
    let state = id();
    let mut authorize = url::Url::parse(&format!("{origin}/authorize")).unwrap();
    authorize.query_pairs_mut().extend_pairs([
        ("client_id","native-client".to_owned()),("redirect_uri","http://127.0.0.1:9123/callback".into()),("response_type","code".into()),
        ("code_challenge",URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))),("code_challenge_method","S256".into()),("state",state.clone()),
        ("dpop_jkt",key.public_jwk().thumbprint()),("trellis_dpop_jwk",serde_json::to_string(&key.public_jwk()).unwrap()),
        ("authorization_details",serde_json::json!([{"type":"trellis_capabilities","required":[f.read],"optional":[f.write]}]).to_string()),
    ]);
    let response = http.get(authorize).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    let login_url = url::Url::parse(&format!(
        "{origin}{}",
        response.headers()["location"].to_str().unwrap()
    ))
    .unwrap();
    let transaction = login_url
        .query_pairs()
        .find(|(key, _)| key == "transaction")
        .unwrap()
        .1
        .into_owned();
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|header| {
            header
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap()
        })
        .collect::<BTreeMap<_, _>>();
    let cookie = format!("__Host-trellis-portal={}", cookies["__Host-trellis-portal"]);
    let csrf = cookies["__Host-trellis-portal-csrf"].to_owned();
    let login = http
        .post(format!("{origin}/portal/login"))
        .header("Origin", &origin)
        .header("Cookie", cookie)
        .form(&[
            ("csrf", &csrf),
            ("username", &"admin".to_owned()),
            ("password", &"a strong bootstrap password".to_owned()),
            ("request_id", &transaction),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|header| header.to_str().unwrap().split(';').next().unwrap())
        .find(|cookie| cookie.starts_with("__Host-trellis-portal="))
        .unwrap()
        .to_owned();
    let approved = f.read.clone();
    let view = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header(
            "Cookie",
            format!("{cookie}; __Host-trellis-portal-csrf={csrf}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(view.status(), reqwest::StatusCode::OK);
    let view: serde_json::Value = view.json().await.unwrap();
    let capabilities = view["capabilities"].as_array().unwrap();
    assert_eq!(
        capabilities
            .iter()
            .find(|capability| capability["capabilityId"] == f.read)
            .unwrap()["available"],
        true
    );
    assert_eq!(
        capabilities
            .iter()
            .find(|capability| capability["capabilityId"] == f.write)
            .unwrap()["available"],
        false
    );
    let consent = [
        ("csrf", &csrf),
        ("transaction", &transaction),
        ("decision", &"approve".to_owned()),
        ("approved", &approved),
        ("remember", &"true".to_owned()),
    ];
    let denied = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", "https://untrusted.example")
        .header("Cookie", &cookie)
        .form(&consent)
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"invalid_request"})
    );
    let callback = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", &origin)
        .header("Cookie", &cookie)
        .form(&consent)
        .send()
        .await
        .unwrap();
    assert_eq!(callback.status(), reqwest::StatusCode::SEE_OTHER);
    let callback = url::Url::parse(callback.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(
        callback.origin().ascii_serialization(),
        "http://127.0.0.1:9123"
    );
    let parameters = callback
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(parameters["state"], state);
    assert_eq!(parameters["iss"], origin);
    let ephemeral = nkeys::KeyPair::new_user();
    let runtime = URL_SAFE_NO_PAD.encode([22; 16]);
    let token_uri = format!("{origin}/token");
    let proof = dpop(&key, &runtime, &ephemeral, &token_uri).await;
    let tokens = http
        .post(&token_uri)
        .header("DPoP", proof)
        .form(&[
            ("grant_type", "authorization_code".to_owned()),
            ("client_id", "native-client".into()),
            ("code", parameters["code"].clone()),
            ("redirect_uri", "http://127.0.0.1:9123/callback".into()),
            ("code_verifier", verifier.into()),
            ("trellis_runtime_id", runtime),
            ("trellis_session_nkey", ephemeral.public_key()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(tokens.status(), reqwest::StatusCode::OK);
    let tokens: TrellisTokenResponse = tokens.json().await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let issued = authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            "_INBOX.fullFlow",
            now,
        )
        .unwrap();
    assert_eq!(issued.authority.unsigned.principal_id, f.admin);
    assert_eq!(
        issued
            .authority
            .unsigned
            .capabilities
            .iter()
            .map(|capability| &capability.capability_id)
            .collect::<Vec<_>>(),
        vec![&f.read]
    );
    let logout = http
        .post(format!("{origin}/portal/logout"))
        .header("Origin", &origin)
        .header("Cookie", cookie)
        .form(&[("csrf", &csrf), ("request_id", &id())])
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), reqwest::StatusCode::OK);
    assert!(authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            "_INBOX.afterLogout",
            now
        )
        .is_err());
    stop.send(()).unwrap();
    host.await.unwrap();
}
