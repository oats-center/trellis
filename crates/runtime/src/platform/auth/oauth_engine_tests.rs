use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::super::oauth::{OAuthEngine, OAuthStorage, TokenError, TrellisTokenResponse};
use super::super::phase_two_tests::*;
use super::super::sqlite::id;
use super::super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use oauth_as::{
    authorization::AuthorizationRequest,
    jwt::{EcdsaP256Key, JwsSigner},
    server::TokenRequest,
};
use sha2::{Digest as _, Sha256};

#[tokio::test]
async fn managed_client_updates_select_flows_and_redeem_native_and_browser_codes() {
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
    crate::platform::auth::phase_two_tests::apply_policy(
        &f.store,
        &mutation(&f.admin, &grant),
        &grant,
        now,
    )
    .unwrap();
    let guest = f.store.create_portal_session(now).unwrap();
    let login = f
        .store
        .portal_local_login(
            &guest.cookie,
            &guest.csrf,
            &URL_SAFE_NO_PAD.encode([87; 32]),
            ("admin", "a strong bootstrap password"),
            &id(),
            now,
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_origin = origin.clone();
    let authority = f.store;
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
    let http = reqwest::Client::new();
    let callback = "http://127.0.0.1:9123/callback";
    let verifier = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
    let details = serde_json::json!([
        {"type":"trellis_capabilities","required":[f.read],"optional":[f.write]}
    ])
    .to_string();
    let mut revision = 0;
    for (kind, callbacks) in [("native", false), ("native", true), ("browser", true)] {
        let mut input: trellis_runtime_apis::__types::trellis::AuthOAuthClientPutRequest =
            serde_json::from_value(serde_json::json!({
                "clientId":"managed-client",
                "kind":kind,
                "title":"Managed client",
                "description":"Flow transition",
                "redirectUris":if callbacks {vec![callback]}else{vec![]},
                "development":true,
                "impliedConsent": [],
                "expectedRevision":revision.to_string(),
                "idempotencyKey":id()
            }))
            .unwrap();
        if kind == "browser" {
            assert!(matches!(
                super::super::rpc_credentials::put_client(
                    &authority,
                    &mutation(&f.admin, &input),
                    &input,
                    now
                ),
                Err(AuthError::Invalid(_))
            ));
            // Kind fixes credential binding semantics; use a separate browser
            // registration rather than reinterpreting outstanding native grants.
            input.client_id = "managed-browser".to_owned().into();
            input.expected_revision = 0.into();
        }
        let updated = super::super::rpc_credentials::put_client(
            &authority,
            &mutation(&f.admin, &input),
            &input,
            now,
        )
        .unwrap();
        revision = updated.client.revision.0 .0;
        let key = EcdsaP256Key::generate("managed-client");
        let ephemeral = nkeys::KeyPair::new_user();
        let runtime = URL_SAFE_NO_PAD.encode([88; 16]);
        let device_uri = format!("{origin}/device_authorization");
        let device = http
            .post(&device_uri)
            .header("DPoP", dpop(&key, &runtime, &ephemeral, &device_uri).await)
            .form(&[
                ("client_id", input.client_id.as_ref()),
                ("authorization_details", details.as_str()),
                ("trellis_runtime_id", runtime.as_str()),
                ("trellis_session_nkey", ephemeral.public_key().as_str()),
            ])
            .send()
            .await
            .unwrap();
        if kind == "native" {
            assert_eq!(device.status(), reqwest::StatusCode::OK);
            let device: oauth_as::device::DeviceAuthorizationResponse =
                device.json().await.unwrap();
            assert!(!device.device_code.is_empty());
        } else {
            assert_eq!(
                device.json::<serde_json::Value>().await.unwrap()["error"],
                "unauthorized_client"
            );
        }
        if !callbacks {
            continue;
        }
        let mut approval = grant_request(&f.admin, &login.login_session_id, &f.read, &f.write, now);
        approval.client_id = input.client_id.to_string();
        approval.public_jwk = serde_json::to_value(key.public_jwk()).unwrap();
        approval.durable_dpop_jkt = key.public_jwk().thumbprint();
        approval.binding_kind = kind.into();
        approval.binding_value = if kind == "browser" {
            "http://127.0.0.1:9123".into()
        } else {
            approval.durable_dpop_jkt.clone()
        };
        approval.approved = vec![f.read.clone()];
        approval.declined = vec![f.write.clone()];
        let request = AuthorizationRequest::from_pairs(vec![
            ("response_type", "code".into()),
            ("client_id", input.client_id.to_string()),
            ("redirect_uri", callback.into()),
            (
                "code_challenge",
                URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
            ("code_challenge_method", "S256".into()),
            ("dpop_jkt", approval.durable_dpop_jkt.clone()),
            ("authorization_details", details.clone()),
        ]);
        let code = engine
            .approve(&request, &mutation(&f.admin, &approval), &approval)
            .await
            .unwrap();
        let token_uri = format!("{origin}/token");
        let redemption = [
            ("grant_type", "authorization_code".to_owned()),
            ("client_id", input.client_id.to_string()),
            ("code", code.code.as_str().to_owned()),
            ("redirect_uri", callback.into()),
            ("code_verifier", verifier.into()),
            ("trellis_runtime_id", runtime.clone()),
            ("trellis_session_nkey", ephemeral.public_key()),
        ];
        let first = http
            .post(&token_uri)
            .header("DPoP", dpop(&key, &runtime, &ephemeral, &token_uri).await)
            .form(&redemption)
            .send()
            .await
            .unwrap();
        let nonce = first.headers()["dpop-nonce"].to_str().unwrap().to_owned();
        let mut redemption_request = http
            .post(&token_uri)
            .header(
                "DPoP",
                dpop_with_nonce(&key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await,
            )
            .form(&redemption);
        if kind == "browser" {
            redemption_request = redemption_request.header("Origin", "http://127.0.0.1:9123");
        }
        let response = redemption_request.send().await.unwrap();
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, reqwest::StatusCode::OK, "{kind}: {body}");
        let tokens: TrellisTokenResponse = serde_json::from_value(body).unwrap();
        let issued = crate::platform::auth::phase_two_tests::admit_credential(
            &authority,
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
        assert_eq!(issued.authority.unsigned.principal_id.to_string(), f.admin);
    }
    stop.send(()).unwrap();
    host.await.unwrap();
}

#[tokio::test]
async fn cimd_client_cannot_receive_administrative_delegation_from_a_privileged_user() {
    use oauth_as::cimd::{CimdPolicy, ClientIdUrl, ValidatedClientIdDocument};
    let f = fixture();
    let client_id = "https://public.example/client.json";
    // Public metadata cannot assert an Auth-owned companion enrollment binding.
    let metadata = serde_json::json!({
        "client_id":client_id,
        "application_type":"native",
        "client_name":"Public client",
        "redirect_uris": ["http://127.0.0.1:9123/callback"],
        "token_endpoint_auth_method":"none",
        "grant_types": ["authorization_code","refresh_token"],
        "response_types": ["code"],
        "trellis_companion": {"deploymentId":id(),"requestId":"public.Companion"}
    });
    let body = serde_json::to_vec(&metadata).unwrap();
    let mut policy = CimdPolicy::new();
    policy.redirect_uris_same_origin = false;
    let identity = ClientIdUrl::parse(client_id, &policy).unwrap();
    let document = super::super::oauth::ClientDocument {
        document: ValidatedClientIdDocument::validate(&identity, &body, &policy).unwrap(),
        revision: URL_SAFE_NO_PAD.encode(Sha256::digest(&body)),
        metadata,
        kind: "native".into(),
        observed_at: SystemTime::now(),
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let login = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: f.admin.clone(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: now + 3600,
                },
                now,
            )
        })
        .unwrap();
    let mut approval = grant_request(&f.admin, &login, &f.read, &f.write, now);
    approval.client_id = client_id.into();
    approval.selection.required.clear();
    approval.selection.optional.clear();
    approval.approved.clear();
    let storage = OAuthStorage::new(f.store, &[17; 32]).unwrap();
    storage.install_client_document(&document).unwrap();
    // The same installed CIMD client works for ordinary consent, ruling out an
    // unrelated metadata, login or binding failure as the reason for denial.
    let grant = storage
        .authority
        .approve_grant(&mutation(&f.admin, &approval), &approval, now)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.refresh_family_id = Some(crate::platform::auth::phase_two_tests::oauth_family(
        &storage.authority,
        &grant,
        now,
        now + 3600,
    ));
    admission.credential = Credential::OAuthGrant(grant);
    assert!(crate::platform::auth::phase_two_tests::issue_authority(
        &storage.authority,
        &admission,
        None,
        now
    )
    .unwrap()
    .authority
    .unsigned
    .platform_privileges
    .is_empty());
    approval.privileges = vec![trellis_protocol::PlatformPrivilege::PrincipalsManage];
    assert!(matches!(
        storage
            .authority
            .approve_grant(&mutation(&f.admin, &approval), &approval, now),
        Err(AuthError::Denied)
    ));
}

#[tokio::test]
async fn provisional_client_capacity_recovers_without_breaking_an_approved_refresh() {
    use oauth_as::{
        cimd::{CimdPolicy, ClientIdUrl, ValidatedClientIdDocument},
        client::ClientId,
        store::Storage as _,
    };
    let f = fixture();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let key = EcdsaP256Key::generate("cimd-custody");
    let login = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
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
        })
        .unwrap();
    let document = |client_id: &str| {
        let metadata = serde_json::json!({
            "client_id":client_id,
            "application_type":"native",
            "client_name":"Reviewed public client",
            "redirect_uris": ["http://127.0.0.1:9123/callback"],
            "token_endpoint_auth_method":"none",
            "grant_types": ["authorization_code","refresh_token"],
            "response_types": ["code"]
        });
        let body = serde_json::to_vec(&metadata).unwrap();
        let mut policy = CimdPolicy::new();
        policy.redirect_uris_same_origin = false;
        let identity = ClientIdUrl::parse(client_id, &policy).unwrap();
        super::super::oauth::ClientDocument {
            document: ValidatedClientIdDocument::validate(&identity, &body, &policy).unwrap(),
            revision: URL_SAFE_NO_PAD.encode(Sha256::digest(&body)),
            metadata,
            kind: "native".into(),
            observed_at: SystemTime::now(),
        }
    };
    let approved_id = format!("https://{}.example/client.json", id().to_ascii_lowercase());
    let mut approval = grant_request(&f.principal, &login, &f.read, &f.write, now);
    approval.client_id = approved_id.clone();
    approval.expires_at = now + 3600;
    approval.public_jwk = serde_json::to_value(key.public_jwk()).unwrap();
    approval.durable_dpop_jkt = key.public_jwk().thumbprint();
    approval.binding_value = approval.durable_dpop_jkt.clone();
    approval.approved = vec![f.read.clone()];
    approval.declined = vec![f.write.clone()];
    let engine = OAuthEngine::new(OAuthStorage::new(f.store, &[17; 32]).unwrap()).unwrap();
    engine
        .server
        .store()
        .install_client_document(&document(&approved_id))
        .unwrap();
    let verifier = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
    let request = AuthorizationRequest::from_pairs(vec![
        ("response_type", "code".to_owned()),
        ("client_id", approved_id.clone()),
        ("redirect_uri", "http://127.0.0.1:9123/callback".to_owned()),
        (
            "code_challenge",
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        ),
        ("code_challenge_method", "S256".to_owned()),
        ("dpop_jkt", approval.durable_dpop_jkt.clone()),
        (
            "authorization_details",
            serde_json::json!([
                {"type":"trellis_capabilities","required":[f.read],"optional":[f.write]}
            ])
            .to_string(),
        ),
    ]);
    let code = engine
        .approve(&request, &mutation(&f.principal, &approval), &approval)
        .await
        .unwrap();
    let token_uri = format!(
        "{}/token",
        engine.server.store().authority.settings.oauth_origin
    );
    let runtime = URL_SAFE_NO_PAD.encode([21; 16]);
    let ephemeral = nkeys::KeyPair::new_user();
    let proof = dpop(&key, &runtime, &ephemeral, &token_uri).await;
    let redemption = TokenRequest::AuthorizationCode {
        client_id: ClientId::new(&approved_id),
        client_secret: None,
        code: code.code,
        redirect_uri: Some("http://127.0.0.1:9123/callback".into()),
        code_verifier: Some(verifier.into()),
    };
    let nonce = match engine
        .token(
            redemption.clone(),
            &proof,
            &runtime,
            &ephemeral.public_key(),
        )
        .await
    {
        Err(TokenError::UseDpopNonce(nonce)) => nonce,
        _ => panic!("expected a nonce challenge before code redemption"),
    };
    let proof = dpop_with_nonce(&key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await;
    let tokens = engine
        .token(redemption, &proof, &runtime, &ephemeral.public_key())
        .await
        .unwrap();
    let mut abandoned = Vec::new();
    for _ in 0..128 {
        let client_id = format!("https://{}.example/client.json", id().to_ascii_lowercase());
        engine
            .server
            .store()
            .install_client_document(&document(&client_id))
            .unwrap();
        let invalid = AuthorizationRequest::from_pairs(vec![
            ("client_id", client_id.clone()),
            ("response_type", "code".into()),
            (
                "redirect_uri",
                "https://not-reviewed.example/callback".into(),
            ),
        ]);
        assert!(engine
            .server
            .validate_authorization_request(&invalid)
            .await
            .is_err());
        abandoned.push(ClientId::new(client_id));
    }
    let next_id = format!("https://{}.example/client.json", id().to_ascii_lowercase());
    assert!(matches!(
        engine
            .server
            .store()
            .install_client_document(&document(&next_id)),
        Err(AuthError::ResourceLimit)
    ));
    assert!(engine
        .server
        .store()
        .get_client(&ClientId::new(&next_id))
        .await
        .unwrap()
        .is_none());
    let after_intake = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    while engine
        .server
        .store()
        .cleanup_expired(after_intake + 301)
        .unwrap()
        > 0
    {}
    for client in abandoned {
        assert!(engine
            .server
            .store()
            .get_client(&client)
            .await
            .unwrap()
            .is_none());
    }
    assert!(engine
        .server
        .store()
        .get_client(&ClientId::new("native-client"))
        .await
        .unwrap()
        .is_some());
    engine
        .server
        .store()
        .install_client_document(&document(&next_id))
        .unwrap();
    let fresh = AuthorizationRequest::from_pairs(vec![
        ("response_type", "code".to_owned()),
        ("client_id", next_id),
        ("redirect_uri", "http://127.0.0.1:9123/callback".to_owned()),
        (
            "code_challenge",
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        ),
        ("code_challenge_method", "S256".to_owned()),
        ("dpop_jkt", approval.durable_dpop_jkt.clone()),
        (
            "authorization_details",
            serde_json::json!([{"type":"trellis_capabilities","required":[f.read],"optional":[]}])
                .to_string(),
        ),
    ]);
    engine
        .server
        .validate_authorization_request(&fresh)
        .await
        .unwrap();
    let proof = dpop_with_nonce(&key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await;
    let refreshed = engine
        .token(
            TokenRequest::RefreshToken {
                client_id: ClientId::new(&approved_id),
                client_secret: None,
                refresh_token: tokens.oauth.refresh_token.unwrap(),
                scope: None,
            },
            &proof,
            &runtime,
            &ephemeral.public_key(),
        )
        .await
        .unwrap();
    assert!(crate::platform::auth::phase_two_tests::admit_credential(
        &engine.server.store().authority,
        refreshed.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    )
    .is_ok());
}

#[tokio::test]
async fn oauth_preflight_uses_validated_callbacks_and_origin_rotation_cannot_evade_peer_limits() {
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_origin = origin.clone();
    let engine =
        Arc::new(OAuthEngine::new(OAuthStorage::new(f.store, &[17; 32]).unwrap()).unwrap());
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
async fn trusted_proxy_clients_without_origin_have_independent_budgets() {
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_origin = origin.clone();
    let engine =
        Arc::new(OAuthEngine::new(OAuthStorage::new(f.store, &[17; 32]).unwrap()).unwrap());
    let listener = super::super::oauth::OAuthListener::load(
        socket,
        &origin,
        &super::super::oauth::OAuthTransport::LoopbackDevelopment,
    )
    .await
    .unwrap()
    .forwarded_by(&["127.0.0.1/32".parse().unwrap()]);
    let stop = tokio_util::sync::CancellationToken::new();
    let stopped = stop.clone();
    let host = tokio::spawn(async move {
        listener
            .run(super::super::oauth::router(engine, Vec::new()), stopped)
            .await
            .unwrap();
    });
    let http = reqwest::Client::new();
    let get = |forwarded: String| {
        http.get(format!("{origin}/.well-known/oauth-authorization-server"))
            .header("X-Forwarded-For", forwarded)
            .send()
    };
    // Native token clients and navigations send no Origin. Five clients that
    // each spend their whole budget must not exhaust a sixth client's.
    for client in 0..5 {
        for _ in 0..120 {
            let response = get(format!("198.51.100.{client}")).await.unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }
    }
    let fresh = get("198.51.100.5".into()).await.unwrap();
    assert_eq!(fresh.status(), reqwest::StatusCode::OK);
    // The proxy-identified client is still limited, including when another
    // trusted hop appended itself after the client address.
    let exhausted = get("198.51.100.0, 127.0.0.1".into()).await.unwrap();
    assert_eq!(exhausted.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    stop.cancel();
    host.await.unwrap();
}

#[tokio::test]
async fn chromium_portal_device_approval_produces_usable_bound_admission() {
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_origin = origin.clone();
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
    let authority = f.store;
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
        .args(["--test", "ts/browser/oauth_portal.browser_driver.ts"])
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
    let poll = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("client_id", "native-client"),
        ("device_code", device.device_code.as_str()),
        ("trellis_runtime_id", runtime.as_str()),
        ("trellis_session_nkey", &ephemeral.public_key()),
    ];
    let response = http
        .post(&token_uri)
        .header("DPoP", dpop(&key, &runtime, &ephemeral, &token_uri).await)
        .form(&poll)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let nonce = response.headers()["dpop-nonce"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "use_dpop_nonce"
    );
    let response = http
        .post(&token_uri)
        .header(
            "DPoP",
            dpop_with_nonce(&key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await,
        )
        .form(&poll)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let tokens: TrellisTokenResponse = response.json().await.unwrap();
    let admitted = crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
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

pub(in crate::platform::auth) async fn dpop(
    key: &EcdsaP256Key,
    runtime: &str,
    ephemeral: &nkeys::KeyPair,
    token_uri: &str,
) -> String {
    dpop_with_nonce(key, runtime, ephemeral, token_uri, None).await
}

pub(in crate::platform::auth) async fn dpop_with_nonce(
    key: &EcdsaP256Key,
    runtime: &str,
    ephemeral: &nkeys::KeyPair,
    token_uri: &str,
    nonce: Option<&str>,
) -> String {
    let (_, bytes) = nkeys::from_public_key(&ephemeral.public_key()).unwrap();
    let public = serde_json::json!({"crv":"Ed25519","kty":"OKP","x":URL_SAFE_NO_PAD.encode(bytes)});
    let mut claims = serde_json::json!({
        "htm":"POST",
        "htu":token_uri,
        "iat":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        "jti":id(),
        "trellis_runtime_id":runtime,
        "trellis_nats_jkt": URL_SAFE_NO_PAD.encode(Sha256::digest(
                super::super::sqlite::json(&public).unwrap().as_bytes(),
            ))
    });
    if let Some(nonce) = nonce {
        claims["nonce"] = serde_json::json!(nonce);
    }
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(
            serde_json::json!({"typ":"dpop+jwt","alg":"ES256","jwk":key.public_jwk()}).to_string()
        ),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
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
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &registration),
                &registration,
                NOW,
            )
        })
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_origin = origin.clone();
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
    let authority = f.store;
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
    let details = serde_json::json!([
        {"type":"trellis_capabilities","required":[f.read],"optional":[f.write]},
        {
            "type":"trellis_platform_privileges",
            "required": ["principals.manage"],
            "optional": []
        }
    ])
    .to_string();
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
    let nonce = rejected.headers()["dpop-nonce"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        rejected.json::<serde_json::Value>().await.unwrap()["error"],
        "use_dpop_nonce"
    );
    let rejected = http
        .post(&token_uri)
        .header(
            "DPoP",
            dpop_with_nonce(&stolen_key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await,
        )
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
            ("presentation", consent["presentationId"].as_str().unwrap()),
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
    let consent: serde_json::Value = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header("Cookie", cookies.join("; "))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let changed = SOURCE
        .replace("Read documents.", "Read confidential documents.")
        .replace(
            "consent_revision 1; allows { rpc Get; }",
            "consent_revision 2; allows { rpc Get; }",
        );
    accept(&authority, &f.admin, &changed, false, now);
    let stale = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", &origin)
        .header("Cookie", cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("transaction", transaction.as_str()),
            ("presentation", consent["presentationId"].as_str().unwrap()),
            ("decision", "approve"),
            ("approved", f.read.as_str()),
            ("confirm_device", device.user_code.as_str()),
            ("privilege", "principals.manage"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        stale.headers()["location"],
        format!("/consent?transaction={transaction}&error=consent_changed")
    );
    use oauth_as::store::Storage as _;
    assert_eq!(
        engine
            .server
            .store()
            .get_device_grant(&device.device_code)
            .await
            .unwrap()
            .unwrap()
            .state,
        oauth_as::device::DeviceGrantState::Pending
    );
    let consent: serde_json::Value = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header("Cookie", cookies.join("; "))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        consent["capabilities"][0]["description"],
        "Read confidential documents."
    );
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
            ("confirm_device", device.user_code.as_str()),
            ("privilege", "principals.manage"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), reqwest::StatusCode::SEE_OTHER);
    let proof = dpop_with_nonce(&key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await;
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
    let admitted = crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
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
        .header(
            "DPoP",
            dpop_with_nonce(&key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await,
        )
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
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_origin = origin.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let key = EcdsaP256Key::generate("native-key");
    let login = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: f.principal.clone(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: now + 35 * 24 * 60 * 60,
                },
                now,
            )
        })
        .unwrap();
    let mut approval = grant_request(&f.principal, &login, &f.read, &f.write, now);
    approval.expires_at = now + 35 * 24 * 60 * 60;
    approval.public_jwk = serde_json::to_value(key.public_jwk()).unwrap();
    approval.durable_dpop_jkt = key.public_jwk().thumbprint();
    approval.binding_value = approval.durable_dpop_jkt.clone();
    approval.approved = vec![f.read.clone()];
    approval.declined = vec![f.write.clone()];
    let verifier = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let request = AuthorizationRequest::from_pairs(vec![
        ("response_type", "code".to_owned()),
        ("client_id", "native-client".to_owned()),
        ("redirect_uri", "http://127.0.0.1:9123/callback".to_owned()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".to_owned()),
        ("dpop_jkt", approval.durable_dpop_jkt.clone()),
        (
            "authorization_details",
            serde_json::json!([
                {"type":"trellis_capabilities","required":[f.read],"optional":[f.write]}
            ])
            .to_string(),
        ),
    ]);
    let authority = f.store;
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
    let untrusted = http
        .post(&token_uri)
        .header("DPoP", &proof)
        .header("Origin", "https://untrusted.example")
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(untrusted.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(!untrusted.headers().contains_key("dpop-nonce"));
    assert!(!untrusted
        .headers()
        .contains_key("access-control-allow-origin"));
    let response = http
        .post(&token_uri)
        .header("DPoP", &proof)
        .header("Origin", "http://127.0.0.1:9123")
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://127.0.0.1:9123"
    );
    assert_eq!(
        response.headers()["access-control-expose-headers"],
        "dpop-nonce"
    );
    let nonce = response.headers()["dpop-nonce"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"use_dpop_nonce"})
    );
    // Re-sign with the challenge but retain the unconsumed jti: this proves
    // neither the code nor proof identifier was spent by the challenge. Normal
    // clients generate a new jti as well as signing a fresh proof on retry.
    let parts = proof.split('.').collect::<Vec<_>>();
    let mut claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    claims["nonce"] = serde_json::json!(nonce);
    let input = format!(
        "{}.{}",
        parts[0],
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let signature = key.sign(input.as_bytes()).await.unwrap();
    let proof = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_bytes()));
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
    let initial = crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
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
    assert!(crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &nkeys::KeyPair::new_user().public_key(),
        now
    )
    .is_err());
    assert!(crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
        now + 300
    )
    .is_err());
    let refresh = tokens.oauth.refresh_token.unwrap();
    use oauth_as::store::Storage as _;
    let absolute_expiry = engine
        .server
        .store()
        .get_refresh_token(&refresh)
        .await
        .unwrap()
        .unwrap()
        .expires_at
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    stop.send(()).unwrap();
    host.await.unwrap();
    drop(engine);
    drop(authority);
    let mut reopened = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    reopened.settings.oauth_origin = origin.clone();
    let authority = Arc::new(reopened);
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[13; 32]).unwrap()).unwrap(),
    );
    let listener = tokio::net::TcpListener::bind(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    let router = super::super::oauth::router(engine.clone(), vec![]);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let host = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let next_ephemeral = nkeys::KeyPair::new_user();
    let next_runtime = URL_SAFE_NO_PAD.encode([12; 16]);
    let refresh_request = TokenRequest::RefreshToken {
        client_id: oauth_as::client::ClientId::new("native-client"),
        client_secret: None,
        refresh_token: refresh.clone(),
        scope: None,
    };
    let refresh_form = [
        ("grant_type", "refresh_token".to_owned()),
        ("client_id", "native-client".into()),
        ("refresh_token", refresh),
        ("trellis_runtime_id", next_runtime.clone()),
        ("trellis_session_nkey", next_ephemeral.public_key()),
    ];
    // A restart loses only process-local nonces, never the durable refresh.
    let proof = dpop_with_nonce(
        &key,
        &next_runtime,
        &next_ephemeral,
        &token_uri,
        Some(&nonce),
    )
    .await;
    let response = http
        .post(&token_uri)
        .header("DPoP", &proof)
        .form(&refresh_form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let restarted_nonce = response.headers()["dpop-nonce"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(nonce, restarted_nonce);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"error":"use_dpop_nonce"})
    );
    let nonce = restarted_nonce;
    let proof = dpop_with_nonce(
        &key,
        &next_runtime,
        &next_ephemeral,
        &token_uri,
        Some(&nonce),
    )
    .await;
    let response = http
        .post(&token_uri)
        .header("DPoP", &proof)
        .form(&refresh_form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let next: TrellisTokenResponse = response.json().await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let resumed = crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        next.trellis_admission.as_bytes(),
        &next_ephemeral.public_key(),
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
    // Exercise the authority transaction at the real engine family's deadline,
    // while the login and grant remain valid. Rotation must not extend it.
    let rotated = engine
        .server
        .store()
        .get_refresh_token(next.oauth.refresh_token.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rotated
            .expires_at
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        absolute_expiry
    );
    let admission: trellis_protocol::SignedOAuthAdmission =
        serde_json::from_str(&next.trellis_admission).unwrap();
    let identity = AdmissionIdentity {
        refresh_family_id: Some(admission.credential.refresh_family_id.clone()),
        credential: Credential::OAuthGrant(admission.credential.oauth_grant_id),
        runtime_id: next_runtime.clone(),
        session_public_key: resumed.authority.unsigned.session_public_key.clone(),
        inbox_prefix: resumed.authority.unsigned.inbox_prefix.clone(),
    };
    let renewed = crate::platform::auth::phase_two_tests::issue_authority(
        &authority,
        &identity,
        Some(&resumed.authority.unsigned.authorization_session_id),
        absolute_expiry - 1,
    )
    .unwrap();
    assert_eq!(renewed.authority.unsigned.expires_at, absolute_expiry);
    assert!(crate::platform::auth::phase_two_tests::issue_authority(
        &authority,
        &identity,
        Some(&resumed.authority.unsigned.authorization_session_id),
        absolute_expiry
    )
    .is_err());
    assert!(crate::platform::auth::phase_two_tests::issue_authority(
        &authority,
        &identity,
        None,
        absolute_expiry
    )
    .is_err());
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
    let proof = dpop_with_nonce(
        &key,
        &next_runtime,
        &next_ephemeral,
        &token_uri,
        Some(&nonce),
    )
    .await;
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
    let proof = dpop_with_nonce(
        &key,
        &next_runtime,
        &next_ephemeral,
        &token_uri,
        Some(&nonce),
    )
    .await;
    assert!(engine
        .token(
            refresh_request,
            &proof,
            &next_runtime,
            &next_ephemeral.public_key()
        )
        .await
        .is_err());
    assert!(crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        next.trellis_admission.as_bytes(),
        &next_ephemeral.public_key(),
        now
    )
    .is_err());
    assert!(crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
        now
    )
    .is_err());
    while authority.enforce_next_page(now).unwrap().is_some() {}
    assert!(crate::platform::auth::phase_two_tests::issue_authority(
        &authority,
        &AdmissionIdentity {
            refresh_family_id: Some(admission.credential.refresh_family_id.clone()),
            credential: Credential::OAuthGrant(resumed.authority.unsigned.oauth_grant_id.unwrap()),
            runtime_id: next_runtime,
            session_public_key: resumed.authority.unsigned.session_public_key,
            inbox_prefix: "_INBOX.noRevival".into()
        },
        None,
        now
    )
    .is_err());
    // Reopen again: no process-local revocation state can account for this denial.
    stop.send(()).unwrap();
    host.await.unwrap();
    drop(engine);
    drop(authority);
    let reopened = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    assert!(crate::platform::auth::phase_two_tests::admit_credential(
        &reopened,
        next.trellis_admission.as_bytes(),
        &next_ephemeral.public_key(),
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
            ("admin", "a strong bootstrap password"),
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
            ("admin", "wrong password"),
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
            ("ADMIN", "a strong bootstrap password"),
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
            ("admin", "a strong bootstrap password"),
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
    crate::platform::auth::phase_two_tests::apply_policy(
        &f.store,
        &mutation(&f.admin, &grant),
        &grant,
        now,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_origin = origin.clone();
    let authority = f.store;
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
        ("client_id", "native-client".to_owned()),
        ("redirect_uri", "http://127.0.0.1:9123/callback".into()),
        ("response_type", "code".into()),
        (
            "code_challenge",
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        ),
        ("code_challenge_method", "S256".into()),
        ("state", state.clone()),
        ("dpop_jkt", key.public_jwk().thumbprint()),
        (
            "trellis_dpop_jwk",
            serde_json::to_string(&key.public_jwk()).unwrap(),
        ),
        (
            "authorization_details",
            serde_json::json!([
                {"type":"trellis_capabilities","required":[f.read],"optional":[f.write]}
            ])
            .to_string(),
        ),
    ]);
    let response = http.get(authorize.as_str()).send().await.unwrap();
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
    let presentation = view["presentationId"].as_str().unwrap().to_owned();
    let consent = [
        ("csrf", &csrf),
        ("transaction", &transaction),
        ("presentation", &presentation),
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
    let changed = SOURCE
        .replace("Read documents.", "Read confidential documents.")
        .replace(
            "consent_revision 1; allows { rpc Get; }",
            "consent_revision 2; allows { rpc Get; }",
        );
    accept(&authority, &f.admin, &changed, false, now);
    let stale = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", &origin)
        .header("Cookie", &cookie)
        .form(&consent)
        .send()
        .await
        .unwrap();
    assert_eq!(
        stale.headers()["location"],
        format!("/consent?transaction={transaction}&error=consent_changed")
    );
    let view: serde_json::Value = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header(
            "Cookie",
            format!("{cookie}; __Host-trellis-portal-csrf={csrf}"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let presentation = view["presentationId"].as_str().unwrap().to_owned();
    let consent = [
        ("csrf", &csrf),
        ("transaction", &transaction),
        ("presentation", &presentation),
        ("decision", &"approve".to_owned()),
        ("approved", &approved),
        ("remember", &"true".to_owned()),
    ];
    let unrelated = PolicyMutation::RolePut {
        role_id: id(),
        expected_revision: 0,
        title: "Unassigned role".into(),
        description: "This role is not part of the displayed authorization".into(),
        capabilities: vec![f.read.clone()],
    };
    crate::platform::auth::phase_two_tests::apply_policy(
        &authority,
        &mutation(&f.admin, &unrelated),
        &unrelated,
        now,
    )
    .unwrap();
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
    let redemption = [
        ("grant_type", "authorization_code".to_owned()),
        ("client_id", "native-client".into()),
        ("code", parameters["code"].clone()),
        ("redirect_uri", "http://127.0.0.1:9123/callback".into()),
        ("code_verifier", verifier.into()),
        ("trellis_runtime_id", runtime.clone()),
        ("trellis_session_nkey", ephemeral.public_key()),
    ];
    let tokens = http
        .post(&token_uri)
        .header("DPoP", proof)
        .form(&redemption)
        .send()
        .await
        .unwrap();
    let nonce = tokens.headers()["dpop-nonce"].to_str().unwrap().to_owned();
    assert_eq!(
        tokens.json::<serde_json::Value>().await.unwrap()["error"],
        "use_dpop_nonce"
    );
    let tokens = http
        .post(&token_uri)
        .header(
            "DPoP",
            dpop_with_nonce(&key, &runtime, &ephemeral, &token_uri, Some(&nonce)).await,
        )
        .form(&redemption)
        .send()
        .await
        .unwrap();
    assert_eq!(tokens.status(), reqwest::StatusCode::OK);
    let tokens: TrellisTokenResponse = tokens.json().await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let issued = crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
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
    // The declined optional choice must stay declined when the account later
    // acquires it. An explicit one-time approval must not overwrite that memory.
    let grant_write = PolicyMutation::CapabilityGrant {
        principal_id: f.admin.clone(),
        capability_id: f.write.clone(),
        expected_revision: 0,
        expires_at: None,
    };
    crate::platform::auth::phase_two_tests::apply_policy(
        &authority,
        &mutation(&f.admin, &grant_write),
        &grant_write,
        now,
    )
    .unwrap();
    for approve_once in [true, false] {
        let response = http
            .get(authorize.as_str())
            .header(
                "Cookie",
                format!("{cookie}; __Host-trellis-portal-csrf={csrf}"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
        let consent_url = url::Url::parse(&format!(
            "{origin}{}",
            response.headers()["location"].to_str().unwrap()
        ))
        .unwrap();
        let transaction = consent_url
            .query_pairs()
            .find(|(key, _)| key == "transaction")
            .unwrap()
            .1
            .into_owned();
        let view: serde_json::Value = http
            .get(format!("{origin}/portal/authorization/{transaction}"))
            .header(
                "Cookie",
                format!("{cookie}; __Host-trellis-portal-csrf={csrf}"),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(view["capabilities"].is_array(), "consent view: {view}");
        let optional = view["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|capability| capability["capabilityId"] == f.write)
            .unwrap();
        assert_eq!(optional["available"], true);
        assert_eq!(
            optional["selected"], false,
            "remembered decline survives new entitlement and approve-once"
        );
        if approve_once {
            let callback = http
                .post(format!("{origin}/portal/approve"))
                .header("Origin", &origin)
                .header("Cookie", &cookie)
                .form(&[
                    ("csrf", csrf.as_str()),
                    ("transaction", transaction.as_str()),
                    ("presentation", view["presentationId"].as_str().unwrap()),
                    ("decision", "approve"),
                    ("approved", f.read.as_str()),
                    ("approved", f.write.as_str()),
                    ("remember", "false"),
                ])
                .send()
                .await
                .unwrap();
            assert_eq!(callback.status(), reqwest::StatusCode::SEE_OTHER);
            let callback =
                url::Url::parse(callback.headers()["location"].to_str().unwrap()).unwrap();
            assert!(callback.query_pairs().any(|(key, _)| key == "code"));
        }
    }
    let logout = http
        .post(format!("{origin}/portal/logout"))
        .header("Origin", &origin)
        .header("Cookie", cookie)
        .form(&[("csrf", &csrf), ("request_id", &id())])
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), reqwest::StatusCode::OK);
    assert!(crate::platform::auth::phase_two_tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
        now
    )
    .is_err());
    stop.send(()).unwrap();
    host.await.unwrap();
}
