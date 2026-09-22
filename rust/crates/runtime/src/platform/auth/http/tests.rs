use axum::response::IntoResponse;

use super::{
    canonical_origin, first_admin_token_hash, oauth_cookie_header, oauth_cookie_name,
    oidc_portal_policy_digest, project_service_resource_bindings, require_oauth_browser_binding,
    session_public_key_to_user_nkey, validate_redirect, NatsBootstrapIssuer, EMBEDDED_WEB_ASSETS,
};
use crate::platform::auth::AuthorizationStateError;
use crate::platform::auth::{
    ephemeral::{AuthOAuthKind, AuthOAuthState, AuthOAuthStatus},
    LoginPortalRecord, LoginSettingsRecord, ResourceBindingEvidence, ResourceBindingState,
    ResourceProviderIdentity,
};
use axum::http::header::COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use nats_jwt_rs::user::User;
use nats_jwt_rs::Claims;
use nkeys::KeyPair;
use sha2::{Digest as _, Sha256};
use std::sync::Arc;
use trellis_protocol::ParticipantResourceKind;

const DIGEST: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[test]
fn session_public_key_derives_the_same_nats_user_key() {
    let key = KeyPair::new_user();
    let (_, raw) = nkeys::from_public_key(&key.public_key()).unwrap();
    assert_eq!(
        session_public_key_to_user_nkey(&URL_SAFE_NO_PAD.encode(raw)).unwrap(),
        key.public_key()
    );
}

#[test]
fn browser_security_boundaries_are_exact() {
    let allowed = vec!["https://app.example".to_owned()];
    assert!(validate_redirect("https://app.example/complete", &allowed).is_ok());
    assert!(validate_redirect("https://evil.example/complete", &allowed).is_err());
    assert!(validate_redirect("https://app.example/complete#secret", &allowed).is_err());
    assert_eq!(
        canonical_origin("https://app.example:443/path").unwrap(),
        "https://app.example"
    );

    let token = URL_SAFE_NO_PAD.encode([7_u8; 32]);
    let digest = first_admin_token_hash(&token).unwrap();
    assert_eq!(digest.len(), 43);
    assert!(!digest.contains(&token));
}

#[tokio::test]
async fn http_errors_never_expose_internal_causes() {
    let secret = "postgres://admin:secret@internal/auth";
    let response =
        super::HttpError::from(AuthorizationStateError::Storage(secret.to_owned())).into_response();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let encoded = String::from_utf8(body.to_vec()).unwrap();
    assert!(!encoded.contains(secret));
    assert!(encoded.contains("internal_error"));
}

#[tokio::test]
async fn issuance_errors_have_stable_machine_codes() {
    for (error, expected) in [
        (AuthorizationStateError::SessionMissing, "session_not_found"),
        (AuthorizationStateError::SessionExpired, "session_expired"),
        (AuthorizationStateError::SessionRevoked, "session_revoked"),
        (
            AuthorizationStateError::AuthorityPending,
            "approval_required",
        ),
        (
            AuthorizationStateError::MaterializationStale,
            "authorization_pending",
        ),
    ] {
        let response = super::map_issuance_error(error).into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({ "error": { "code": expected } })
        );
    }
}

#[test]
fn stale_authority_issuance_is_retryable() {
    assert_eq!(
        super::map_issuance_error(AuthorizationStateError::AuthorityStale).status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
    );
    assert_eq!(
        super::map_issuance_error(AuthorizationStateError::MaterializationStale).status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
    );
}

#[test]
fn oauth_cookie_binds_one_browser_and_uses_callback_security_policy() {
    let state_id = "oauth-state";
    let secret = "browser-secret";
    let state = AuthOAuthState {
        format: "trellis.auth-oauth-state.v1".to_owned(),
        state_id: state_id.to_owned(),
        provider_id: "provider".to_owned(),
        kind: AuthOAuthKind::Browser,
        flow_id: "flow".to_owned(),
        status: AuthOAuthStatus::Pending,
        pkce_verifier: "verifier".to_owned(),
        nonce: "nonce".to_owned(),
        redirect_uri: "https://auth.example/auth/callback/provider".to_owned(),
        browser_binding_digest: URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes())),
        portal_binding_digest: Some(DIGEST.to_owned()),
        browser_flow_id: None,
        portal_id: Some("builtin".to_owned()),
        portal_policy_digest: Some(super::digest_parts(&["policy"])),
        claim_owner: None,
        result_digest: None,
        authenticated_principal_id: None,
        authenticated_provider_subject: None,
        authenticated_email: None,
        authenticated_roles: Vec::new(),
        created_at: 1,
        expires_at: 2,
        version: 1,
    };
    let mut headers = HeaderMap::new();
    assert!(require_oauth_browser_binding(state_id, &state, &headers).is_err());
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&format!("{}=wrong", oauth_cookie_name(state_id))).unwrap(),
    );
    assert!(require_oauth_browser_binding(state_id, &state, &headers).is_err());
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&format!("{}={secret}", oauth_cookie_name(state_id))).unwrap(),
    );
    require_oauth_browser_binding(state_id, &state, &headers).unwrap();
    assert!(require_oauth_browser_binding("another-state", &state, &headers).is_err());

    let cookie = oauth_cookie_header("binding", secret, "provider", true, 900)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("Path=/auth/callback/provider"));
    assert!(cookie.contains("Max-Age=900"));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Secure"));
}

#[test]
fn oauth_portal_policy_digest_tracks_policy_not_wording() {
    let portal = LoginPortalRecord {
        portal_id: "builtin".to_owned(),
        display_name: "Trellis".to_owned(),
        entry_url: None,
        builtin: true,
        disabled: false,
        removed: false,
        local_registration_enabled: false,
        provider_ids: vec!["oidc".to_owned()],
        created_at: 1,
        updated_at: 1,
        version: 1,
    };
    let settings = LoginSettingsRecord {
        portal_id: "builtin".to_owned(),
        default_provider_id: Some("oidc".to_owned()),
        local_login_enabled: true,
        federated_registration_enabled: false,
        provider_selection_enabled: false,
        updated_at: 1,
        version: 1,
    };
    let digest = oidc_portal_policy_digest(&portal, &settings).unwrap();
    let mut wording = portal.clone();
    wording.display_name = "Renamed portal".to_owned();
    assert_eq!(
        oidc_portal_policy_digest(&wording, &settings).unwrap(),
        digest
    );
    let mut registration = settings.clone();
    registration.federated_registration_enabled = true;
    registration.version += 1;
    assert_ne!(
        oidc_portal_policy_digest(&portal, &registration).unwrap(),
        digest
    );
    let mut provider = portal.clone();
    provider.provider_ids.clear();
    provider.version += 1;
    assert_ne!(
        oidc_portal_policy_digest(&provider, &settings).unwrap(),
        digest
    );
}

#[test]
fn embedded_web_app_contains_fallback_and_assets() {
    let shell = EMBEDDED_WEB_ASSETS
        .iter()
        .find_map(|(path, bytes)| (*path == "200.html").then_some(*bytes))
        .expect("embedded web fallback");
    let shell = std::str::from_utf8(shell).expect("web fallback UTF-8");
    assert!(!shell.contains("<script>"));
    assert!(shell.contains("/assets/web/bootstrap.js"));
    assert!(shell.contains("/assets/web/trellis-logo.svg"));
    assert!(EMBEDDED_WEB_ASSETS
        .iter()
        .any(|(path, bytes)| path.starts_with("assets/login/") && !bytes.is_empty()));
    assert!(EMBEDDED_WEB_ASSETS
        .iter()
        .any(|(path, bytes)| path.starts_with("assets/") && !bytes.is_empty()));
}

#[test]
fn bootstrap_projects_soft_store_max_object_as_absent_actual_binding() {
    let mut participant = crate::platform::auth::builtins::auth_runtime_participant_binding(0)
        .unwrap()
        .projection;
    participant.resources.insert(
        "cache".to_owned(),
        crate::platform::auth::evidence::ResourceRuntimeProjection {
            kind: ParticipantResourceKind::Kv,
            optional: false,
            title: "Cache".to_owned(),
            description: "Cache".to_owned(),
            representation: None,
            history: Some(2),
            ttl_ms: Some(60_000),
            desired_max_value: Some(4096),
            desired_max_object: None,
            desired_max_total: None,
            deadline_ms: None,
            payload_schema: None,
            result_schema: None,
            update_schema: None,
            retry_attempts: None,
            retry_backoff_ms: Vec::new(),
            job_key_path: None,
            job_key_policy: None,
            consumer_events: Default::default(),
            consumer_replay_all: false,
            consumer_concurrency: None,
        },
    );
    participant.resources.insert(
        "objects".to_owned(),
        crate::platform::auth::evidence::ResourceRuntimeProjection {
            kind: ParticipantResourceKind::Store,
            optional: false,
            title: "Objects".to_owned(),
            description: "Objects".to_owned(),
            representation: None,
            history: None,
            ttl_ms: Some(0),
            desired_max_value: None,
            desired_max_object: Some(4096),
            desired_max_total: Some(8192),
            deadline_ms: None,
            payload_schema: None,
            result_schema: None,
            update_schema: None,
            retry_attempts: None,
            retry_backoff_ms: Vec::new(),
            job_key_path: None,
            job_key_policy: None,
            consumer_events: Default::default(),
            consumer_replay_all: false,
            consumer_concurrency: None,
        },
    );
    let evidence = vec![
        ResourceBindingEvidence {
            resource_kind: "kv".to_owned(),
            local_name: "cache".to_owned(),
            binding_id: "bind_cache".to_owned(),
            owner_participant_id: "example".to_owned(),
            provider_identity: ResourceProviderIdentity::Kv {
                bucket: "KV_EXAMPLE_CACHE".to_owned(),
            },
            actual: Some(crate::platform::auth::resources::ResourceActual::Kv {
                history: 3,
                ttl_ms: 120_000,
                max_value_bytes: None,
            }),
            state: ResourceBindingState::Available,
            materialized_at: 1,
            error: None,
        },
        ResourceBindingEvidence {
            resource_kind: "store".to_owned(),
            local_name: "objects".to_owned(),
            binding_id: "bind_objects".to_owned(),
            owner_participant_id: "example".to_owned(),
            provider_identity: ResourceProviderIdentity::Store {
                bucket: "OBJ_EXAMPLE_OBJECTS".to_owned(),
            },
            actual: Some(crate::platform::auth::resources::ResourceActual::Store {
                ttl_ms: 0,
                max_object_bytes: None,
                max_total_bytes: Some(16_384),
            }),
            state: ResourceBindingState::Available,
            materialized_at: 1,
            error: None,
        },
    ];

    let projected = project_service_resource_bindings(&participant, &evidence, "example")
        .expect("resource binding");
    assert_eq!(projected.kv["cache"].bucket, "KV_EXAMPLE_CACHE");
    assert_eq!(projected.kv["cache"].history, 3);
    assert_eq!(projected.kv["cache"].ttl_ms, 120_000);
    assert_eq!(projected.kv["cache"].max_value_bytes, None);
    assert_eq!(projected.store["objects"].max_object_bytes, None);
    assert_eq!(projected.store["objects"].max_total_bytes, Some(16_384));
}

#[test]
fn bootstrap_jwt_is_session_keyed_and_deny_all() {
    let signing_key = KeyPair::new_account();
    let auth_account = KeyPair::new_account().public_key();
    let session_key = KeyPair::new_user().public_key();
    let issuer = NatsBootstrapIssuer {
        signing_key: Arc::new(signing_key),
        auth_account: auth_account.clone(),
        maximum_lifetime_seconds: 300,
    };
    let jwt = issuer.deny_all_user_jwt(&session_key, 10_000, 100).unwrap();
    assert_eq!(jwt.expires_at, 400);
    let claims = Claims::<User>::decode(&jwt.jwt).unwrap();
    assert_eq!(
        claims.payload().issuer_account.as_deref(),
        Some(auth_account.as_str())
    );
    assert_eq!(serde_json::to_value(&claims).unwrap()["sub"], session_key);
    assert_eq!(claims.exp, Some(400));
    assert!(claims
        .payload()
        .permissions
        .permissions
        .publish
        .allow
        .is_empty());
    assert_eq!(
        claims.payload().permissions.permissions.publish.deny,
        vec![">".to_owned()]
    );
    assert!(claims
        .payload()
        .permissions
        .permissions
        .subscribe
        .allow
        .is_empty());
}

#[test]
fn administrator_grants_include_console_surfaces() {
    let binding = super::super::cli_participant_binding(0).expect("CLI participant binding");
    let grants = super::browser::complete_participant_grants(&binding)
        .unwrap_or_else(|_| panic!("complete administration grant set"));
    let json = serde_json::to_string(&grants).expect("serialize administration grants");
    assert!(json.contains("Capabilities.List"), "{json}");
}

#[tokio::test]
async fn expired_flows_are_marked_expired_with_a_valid_completion_timestamp() {
    use crate::platform::auth::ephemeral::tests::browser_flow;
    use crate::platform::auth::ephemeral::{
        AuthBrowserFlowState, AuthEphemeralRepository, InMemoryAuthEphemeralRepository,
    };

    let repository = InMemoryAuthEphemeralRepository::default();
    let flow = browser_flow();
    repository.create_browser_flow(flow.clone()).await.unwrap();
    let error = super::load_flow(&repository, &flow.flow_id)
        .await
        .expect_err("expired flow is rejected");
    assert_eq!(error.status, axum::http::StatusCode::GONE);
    assert_eq!(error.code, "flow_expired");
    let stored = repository
        .get_browser_flow(&flow.flow_id)
        .await
        .unwrap()
        .expect("flow remains stored");
    assert_eq!(stored.state, AuthBrowserFlowState::Expired);
    assert_eq!(stored.completed_at, Some(flow.expires_at));
}
