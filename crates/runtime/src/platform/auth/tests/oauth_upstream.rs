//! Live upstream interoperability with the unmodified production Dex binary.
use super::super::oauth::{OAuthEngine, OAuthListener, OAuthStorage, OAuthTransport};
use super::*;
use axum::response::IntoResponse as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oauth_as::jwt::EcdsaP256Key;
use openidconnect::TokenResponse as _;
use reqwest::cookie::CookieStore;
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Perform real IdP login/consent forms, preserving their hidden fields. The
/// driver never follows the authorization code to a callback before its caller
/// can process it, and never sends the fixture password to another origin.
async fn dex_login(
    http: &reqwest::Client,
    mut next: url::Url,
    issuer: &url::Url,
    callback: &url::Url,
    login: &str,
) -> url::Url {
    for _ in 0..16 {
        assert_eq!(next.origin(), issuer.origin());
        let response = http.get(next.clone()).send().await.unwrap();
        let response = if response.status().is_redirection() {
            response
        } else {
            assert!(response.status().is_success(), "IdP login page failed");
            let body = response.text().await.unwrap();
            let (target, method, fields) = {
                let document = scraper::Html::parse_document(&body);
                let form = document
                    .select(&scraper::Selector::parse("form").unwrap())
                    .next()
                    .expect("IdP must present a login or consent form");
                let target = next
                    .join(form.value().attr("action").unwrap_or(""))
                    .unwrap();
                assert_eq!(target.origin(), issuer.origin());
                let method = form.value().attr("method").unwrap_or("get").to_owned();
                let mut fields = vec![];
                for input in form.select(&scraper::Selector::parse("input[name]").unwrap()) {
                    let name = input.value().attr("name").unwrap();
                    let kind = input.value().attr("type").unwrap_or("text");
                    if matches!(kind, "checkbox" | "radio")
                        && input.value().attr("checked").is_none()
                    {
                        continue;
                    }
                    let value = match name {
                        "login" => login,
                        "password" => "password",
                        _ => input.value().attr("value").unwrap_or(""),
                    };
                    fields.push((name.to_owned(), value.to_owned()));
                }
                if let Some(button) = form
                    .select(
                        &scraper::Selector::parse("button[name=approval][value=approve]").unwrap(),
                    )
                    .next()
                {
                    fields.push((
                        "approval".into(),
                        button.value().attr("value").unwrap().into(),
                    ));
                }
                (target, method, fields)
            };
            if method.eq_ignore_ascii_case("post") {
                http.post(target).form(&fields).send().await.unwrap()
            } else {
                let mut target = target;
                target.query_pairs_mut().extend_pairs(fields);
                http.get(target).send().await.unwrap()
            }
        };
        assert!(
            response.status().is_redirection(),
            "IdP rejected the login or consent form"
        );
        next = next
            .join(response.headers()["location"].to_str().unwrap())
            .unwrap();
        if next.origin() == callback.origin() && next.path() == callback.path() {
            assert!(
                next.query_pairs().any(|(name, _)| name == "code"),
                "IdP did not return an authorization code"
            );
            return next;
        }
    }
    panic!("IdP login exceeded its bounded redirect/form sequence");
}

#[tokio::test]
async fn upstream_oidc_verifies_nonce_and_maps_real_signed_group_claims_into_trellis_authority() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let dex = std::env::var_os("TRELLIS_TEST_DEX_BIN")
        .expect("TRELLIS_TEST_DEX_BIN must point to the producer-built production Dex binary");
    let certificate = super::oauth_transport::certificate();
    // Held until Dex is listening; see ReservedPort.
    let idp_reservation = trellis_local_nats::ReservedPort::reserve().unwrap();
    let idp_address = std::net::SocketAddr::from(([127, 0, 0, 1], idp_reservation.port()));
    let public_idp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer =
        url::Url::parse(&format!("https://{}/dex", public_idp.local_addr().unwrap())).unwrap();
    let portal_socket = tokio::net::TcpListener::bind("127.0.0.2:0").await.unwrap();
    let origin = format!("https://{}", portal_socket.local_addr().unwrap());
    let callback = url::Url::parse(&format!("{origin}/portal/oidc/callback")).unwrap();
    let config = certificate.directory.path().join("dex.yaml");
    let database = certificate.directory.path().join("dex.sqlite");
    let user_id = id();
    let newcomer_id = id();
    let secret = "upstream-integration-client-secret";
    std::fs::write(
        &config,
        format!(
            r#"issuer: {issuer}
storage:
  type: sqlite3
  config:
    file: {}
web:
  https: {idp_address}
  tlsCert: {}
  tlsKey: {}
logger:
  level: error
staticClients:
- id: trellis-upstream
  name: Trellis integration fixture
  secret: {secret}
  redirectURIs: [{callback}]
enablePasswordDB: true
staticPasswords:
- email: admin@example.com
  username: admin
  userID: {user_id}
  hash: '$2a$10$2b2cU8CPhOTaGrs1HRQuAueS7JTT5ZHsHSzYiFPm1leZck7Mc8T4W'
  groups: [team-a]
- email: newcomer@example.com
  username: newcomer
  userID: {newcomer_id}
  hash: '$2a$10$2b2cU8CPhOTaGrs1HRQuAueS7JTT5ZHsHSzYiFPm1leZck7Mc8T4W'
"#,
            database.display(),
            certificate.certificate.display(),
            certificate.private_key.display()
        ),
    )
    .unwrap();
    let log = std::fs::File::create(certificate.directory.path().join("dex.log")).unwrap();
    let mut idp = tokio::process::Command::new(dex)
        .args(["serve"])
        .arg(config)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .cookie_provider(jar.clone())
        .add_root_certificate(reqwest::Certificate::from_pem(certificate.pem.as_bytes()).unwrap())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    // Real Dex still owns discovery, signing, authentication and UserInfo bearer
    // validation. The HTTPS fixture alters only optional UserInfo presentation
    // fields, including an adversarial sub, never an ID token or verified context.
    let userinfo_mode = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let proxy_http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(reqwest::Certificate::from_pem(certificate.pem.as_bytes()).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mode = userinfo_mode.clone();
    let proxy = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let http = proxy_http.clone();
        let mode = mode.clone();
        async move {
            let (parts, body) = request.into_parts();
            let userinfo = parts.uri.path().ends_with("/userinfo");
            let target = format!(
                "https://{idp_address}{}",
                parts.uri.path_and_query().unwrap()
            );
            let Ok(body) = axum::body::to_bytes(body, 65536).await else {
                return axum::http::StatusCode::BAD_REQUEST.into_response();
            };
            let Ok(response) = http
                .request(parts.method, target)
                .headers(parts.headers)
                .body(body)
                .send()
                .await
            else {
                return axum::http::StatusCode::BAD_GATEWAY.into_response();
            };
            let status = response.status();
            let mut headers = response.headers().clone();
            headers.remove(axum::http::header::CONTENT_LENGTH);
            headers.remove(axum::http::header::TRANSFER_ENCODING);
            let Ok(body) = response.bytes().await else {
                return axum::http::StatusCode::BAD_GATEWAY.into_response();
            };
            if userinfo && status.is_success() {
                let mut profile: serde_json::Value = serde_json::from_slice(&body).unwrap();
                profile["given_name"] =
                    serde_json::json!(if mode.load(std::sync::atomic::Ordering::SeqCst) == 1 {
                        "UserInfo given"
                    } else {
                        "Wrong principal"
                    });
                profile["name"] = serde_json::json!("Must not replace the ID-token name");
                if mode.load(std::sync::atomic::Ordering::SeqCst) == 2 {
                    profile["sub"] = serde_json::json!("another-authenticated-subject");
                }
                return (status, headers, serde_json::to_vec(&profile).unwrap()).into_response();
            }
            (status, headers, body).into_response()
        }
    });
    let proxy_handle = axum_server::Handle::new();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        &certificate.certificate,
        &certificate.private_key,
    )
    .await
    .unwrap();
    let proxy_server = tokio::spawn(
        axum_server::from_tcp_rustls(public_idp.into_std().unwrap(), tls)
            .unwrap()
            .handle(proxy_handle.clone())
            .serve(proxy.into_make_service()),
    );
    let ready = async {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            if let Some(status) = idp.try_wait().unwrap() {
                panic!(
                    "production Dex exited before discovery became available ({status}): {}",
                    std::fs::read_to_string(certificate.directory.path().join("dex.log")).unwrap()
                );
            }
            if let Ok(response) = http
                .get(issuer.join("dex/.well-known/openid-configuration").unwrap())
                .send()
                .await
            {
                if response.status().is_success() {
                    break;
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(15), ready)
        .await
        .expect("Dex discovery readiness deadline");

    // Resolve the pre-linked subject through a genuine, independently initiated
    // OIDC exchange. Do not manufacture Dex's subject encoding or JWT claims.
    let upstream_http = openidconnect::reqwest::Client::builder()
        .no_proxy()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .add_root_certificate(
            openidconnect::reqwest::Certificate::from_pem(certificate.pem.as_bytes()).unwrap(),
        )
        .build()
        .unwrap();
    let metadata = openidconnect::core::CoreProviderMetadata::discover_async(
        openidconnect::IssuerUrl::new(issuer.to_string()).unwrap(),
        &upstream_http,
    )
    .await
    .unwrap();
    let upstream = openidconnect::core::CoreClient::from_provider_metadata(
        metadata,
        openidconnect::ClientId::new("trellis-upstream".into()),
        Some(openidconnect::ClientSecret::new(secret.into())),
    )
    .set_redirect_uri(openidconnect::RedirectUrl::new(callback.to_string()).unwrap());
    let (challenge, verifier) = openidconnect::PkceCodeChallenge::new_random_sha256();
    let (authorization, _, nonce) = upstream
        .authorize_url(
            openidconnect::core::CoreAuthenticationFlow::AuthorizationCode,
            openidconnect::CsrfToken::new_random,
            openidconnect::Nonce::new_random,
        )
        .set_pkce_challenge(challenge)
        .url();
    let result = dex_login(
        &http,
        authorization,
        &issuer,
        &callback,
        "admin@example.com",
    )
    .await;
    let code = result
        .query_pairs()
        .find(|(name, _)| name == "code")
        .unwrap()
        .1
        .into_owned();
    let response = upstream
        .exchange_code(openidconnect::AuthorizationCode::new(code))
        .unwrap()
        .set_pkce_verifier(verifier)
        .request_async(&upstream_http)
        .await
        .unwrap();
    let subject = response
        .id_token()
        .unwrap()
        .claims(&upstream.id_token_verifier(), &nonce)
        .unwrap()
        .subject()
        .as_str()
        .to_owned();

    let mut f = fixture();
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_issuer = origin.clone();
    Arc::get_mut(&mut f.store)
        .unwrap()
        .settings
        .upstream_tls_roots_pem = vec![certificate.pem.clone()];
    let authority = f.store;
    let storage = OAuthStorage::new(authority.clone(), &[19; 32]).unwrap();
    let provider = id();
    let registration = super::super::lifecycle::OidcProviderRegistration {
        provider_id: provider.clone(),
        issuer: issuer.to_string(),
        display_name: "Company identity".into(),
        client_id: "trellis-upstream".into(),
        sealed_client_secret: Some(
            storage
                .seal_upstream_client_secret(&provider, secret)
                .unwrap(),
        ),
        config: serde_json::json!({"scopes":["profile","email","groups"]}),
        claim_freshness_seconds: 300,
        expected_revision: 0,
    };
    authority
        .transaction(|connection| {
            authority.register_oidc_provider_in_transaction(
                connection,
                &mutation(&f.admin, &registration),
                &registration,
                NOW,
            )
        })
        .unwrap();
    let account = trellis_runtime_apis::__types::trellis::AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "alice".to_string().into(),
        display_name: "Alice".to_string().into(),
        password: Some("initial alice password".to_string().into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    f.principal = super::super::rpc_users::create_user(
        &authority,
        &mutation(&f.admin, &account),
        &account,
        NOW,
    )
    .unwrap()
    .user
    .principal_id
    .to_string();
    // An existing profile with the OP's email must never become the login target.
    let shadow = trellis_runtime_apis::__types::trellis::AuthUserCreateRequest {
        username: "email-shadow".to_string().into(),
        display_name: "Different existing person".to_string().into(),
        email: Some("admin@example.com".into()),
        password: None,
        idempotency_key: id().into(),
        ..account.clone()
    };
    let shadow = super::super::rpc_users::create_user(
        &authority,
        &mutation(&f.admin, &shadow),
        &shadow,
        NOW,
    )
    .unwrap()
    .user
    .principal_id;
    let group_role = id();
    let role = PolicyMutation::RolePut {
        role_id: group_role.clone(),
        title: "Upstream writers".into(),
        description: "Write authority derived from the upstream team-a group".into(),
        capabilities: vec![f.write.clone()],
        expected_revision: 0,
    };
    crate::platform::auth::tests::apply_policy(&authority, &mutation(&f.admin, &role), &role, NOW)
        .unwrap();
    let reader_role = id();
    let reader = PolicyMutation::RolePut {
        role_id: reader_role.clone(),
        title: "Upstream readers".into(),
        description: String::new(),
        capabilities: vec![f.read.clone()],
        expected_revision: 0,
    };
    crate::platform::auth::tests::apply_policy(
        &authority,
        &mutation(&f.admin, &reader),
        &reader,
        NOW,
    )
    .unwrap();
    let mapping_id = id();
    let mapping = PolicyMutation::OidcMappingPut {
        mapping_id: mapping_id.clone(),
        provider_id: provider.clone(),
        claim_name: "/groups".into(),
        claim_value: "team-a".into(),
        role_ids: vec![group_role, reader_role],
        expected_revision: 0,
    };
    crate::platform::auth::tests::apply_policy(
        &authority,
        &mutation(&f.admin, &mapping),
        &mapping,
        NOW,
    )
    .unwrap();
    let engine = Arc::new(OAuthEngine::new(storage).unwrap());
    let listener = OAuthListener::load(
        portal_socket,
        &origin,
        &OAuthTransport::Tls {
            certificate_chain: certificate.certificate.clone(),
            private_key: certificate.private_key.clone(),
        },
    )
    .await
    .unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = tokio::spawn(listener.run(
        super::super::oauth::router(engine.clone(), vec!["tls://127.0.0.1:4222".into()]),
        shutdown.clone(),
    ));
    // Exercise the actual account-link forms and the same verified OIDC callback,
    // not a manufactured ID token or a trusted-context injection.
    let portal = url::Url::parse(&origin).unwrap();
    let current = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let browser_request = trellis_runtime_apis::__types::trellis::AuthIdentityLinkCreateRequest {
        provider_id: provider.parse().unwrap(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let browser_link = super::super::oauth::account::create_link(
        engine.server.store(),
        &mutation(&f.principal, &browser_request),
        &browser_request,
        current,
    )
    .unwrap();
    let admin_request = trellis_runtime_apis::__types::trellis::AuthIdentityLinkCreateRequest {
        provider_id: provider.parse().unwrap(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let admin_link = super::super::oauth::account::create_link(
        engine.server.store(),
        &mutation(&f.admin, &admin_request),
        &admin_request,
        current,
    )
    .unwrap();
    let browser = tokio::process::Command::new("node")
        .args(["--test", "ts/browser/oauth_account_link.browser_driver.ts"])
        .env(
            "TRELLIS_TEST_ACCOUNT_LINK_URI",
            &browser_link.authorization_url,
        )
        .env("TRELLIS_TEST_OIDC_ISSUER", issuer.as_str())
        .env("TRELLIS_TEST_ADMIN_LINK_URI", &admin_link.authorization_url)
        .env("TRELLIS_TEST_WRONG_PROVIDER", id())
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .kill_on_drop(true)
        .output();
    let browser = tokio::time::timeout(Duration::from_secs(45), browser)
        .await
        .expect("account-link Chromium deadline")
        .expect("account-link Chromium runner");
    assert!(
        browser.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&browser.stdout),
        String::from_utf8_lossy(&browser.stderr)
    );
    let linked: String = authority
        .transaction(|connection| {
            Ok(connection.query_row(
                "SELECT principal_id \
                FROM auth_provider_identities \
                WHERE provider=?1 AND provider_subject=?2",
                rusqlite::params![provider, subject],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(linked, f.principal);
    for (actor, username, password, success) in [
        (&f.principal, "alice", "initial alice password", true),
        (&f.admin, "admin", "a strong bootstrap password", false),
    ] {
        let request = trellis_runtime_apis::__types::trellis::AuthIdentityLinkCreateRequest {
            provider_id: provider.parse().unwrap(),
            idempotency_key: id().into(),
            extra: Default::default(),
        };
        let created = super::super::oauth::account::create_link(
            engine.server.store(),
            &mutation(actor, &request),
            &request,
            current,
        )
        .unwrap();
        let page = http.get(&created.authorization_url).send().await.unwrap();
        assert!(page.status().is_success());
        let cookies = jar.cookies(&portal).unwrap();
        let csrf = cookies
            .to_str()
            .unwrap()
            .split("; ")
            .find_map(|value| value.strip_prefix("__Host-trellis-portal-csrf="))
            .unwrap()
            .to_owned();
        let token = url::Url::parse(&created.authorization_url)
            .unwrap()
            .query_pairs()
            .find(|(name, _)| name == "token")
            .unwrap()
            .1
            .into_owned();
        {
            let login = http
                .post(format!("{origin}/portal/account/login"))
                .header("Origin", &origin)
                .form(&[
                    ("token", token.as_str()),
                    ("csrf", csrf.as_str()),
                    ("username", username),
                    ("password", password),
                ])
                .send()
                .await
                .unwrap();
            assert_eq!(login.status(), reqwest::StatusCode::SEE_OTHER);
            let wrong = trellis_runtime_apis::__types::trellis::AuthIdentityLinkCreateRequest {
                provider_id: provider.parse().unwrap(),
                idempotency_key: id().into(),
                extra: Default::default(),
            };
            let wrong = super::super::oauth::account::create_link(
                engine.server.store(),
                &mutation(if success { &f.admin } else { &f.principal }, &wrong),
                &wrong,
                current,
            )
            .unwrap();
            assert_eq!(
                http.get(wrong.authorization_url)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                reqwest::StatusCode::BAD_REQUEST
            );
        }
        let denied = http
            .post(format!("{origin}/portal/account/link"))
            .header("Origin", "https://attacker.example")
            .form(&[("token", token.as_str()), ("csrf", csrf.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::BAD_REQUEST);
        let begin = http
            .post(format!("{origin}/portal/account/link"))
            .header("Origin", &origin)
            .form(&[("token", token.as_str()), ("csrf", csrf.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(begin.status(), reqwest::StatusCode::OK);
        let continuation = {
            let document = scraper::Html::parse_document(&begin.text().await.unwrap());
            let selector = scraper::Selector::parse("a[href]").unwrap();
            let link = document.select(&selector).next().unwrap();
            portal.join(link.value().attr("href").unwrap()).unwrap()
        };
        let begin = http.get(continuation.clone()).send().await.unwrap();
        assert_eq!(begin.status(), reqwest::StatusCode::SEE_OTHER);
        assert_eq!(
            http.get(continuation).send().await.unwrap().status(),
            reqwest::StatusCode::BAD_REQUEST
        );
        let authorization = url::Url::parse(begin.headers()["location"].to_str().unwrap()).unwrap();
        assert!(authorization
            .query_pairs()
            .any(|(name, value)| name == "prompt" && value == "login"));
        let callback_url = dex_login(
            &http,
            authorization,
            &issuer,
            &callback,
            "admin@example.com",
        )
        .await;
        let completed = http.get(callback_url.clone()).send().await.unwrap();
        if success {
            assert!(completed.status().is_success());
            assert_eq!(
                http.get(&created.authorization_url)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                reqwest::StatusCode::BAD_REQUEST
            );
            assert_eq!(
                http.get(callback_url).send().await.unwrap().status(),
                reqwest::StatusCode::BAD_REQUEST
            );
            let linked: String = authority
                .transaction(|connection| {
                    Ok(connection.query_row(
                        "SELECT principal_id \
                        FROM auth_provider_identities \
                        WHERE provider=?1 AND provider_subject=?2",
                        rusqlite::params![provider, subject],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(linked, f.principal);
            let profile = authority
                .transaction(|connection| {
                    super::super::rpc_users::read_user(&authority, connection, &linked, true)
                })
                .unwrap();
            assert_eq!(profile.given_name.as_deref(), Some("UserInfo given"));
            assert_eq!(profile.display_name, "Alice");
        } else {
            assert_eq!(completed.status(), reqwest::StatusCode::BAD_REQUEST);
            let unchanged: String = authority
                .transaction(|connection| {
                    Ok(connection.query_row(
                        "SELECT principal_id \
                        FROM auth_provider_identities \
                        WHERE provider=?1 AND provider_subject=?2",
                        rusqlite::params![provider, subject],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(unchanged, f.principal);
            // Reset uses a real admitted administrative session, the native HTTP
            // form, and real Argon2 credentials, not a fake verified context.
            let client = super::super::lifecycle::ClientRegistration {
                client_id: "native-client".into(),
                kind: "native".into(),
                display_name: "Native client".into(),
                redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
                development: false,
                implied_capabilities: vec![],
                metadata: serde_json::json!({}),
                expected_revision: 1,
            };
            authority
                .transaction(|connection| {
                    authority.register_client_in_transaction(
                        connection,
                        &mutation(&f.admin, &client),
                        &client,
                        current,
                    )
                })
                .unwrap();
            let cookies = jar.cookies(&portal).unwrap();
            let cookie = cookies
                .to_str()
                .unwrap()
                .split("; ")
                .find_map(|value| value.strip_prefix("__Host-trellis-portal="))
                .unwrap();
            let login = authority.portal_login(cookie, &csrf, current).unwrap();
            let mut approval = grant_request(&f.admin, &login.login_session_id, "", "", current);
            let manage = crate::platform::auth::Administration::ManageAccounts.capability_id();
            approval.selection.required = vec![manage.into()];
            approval.selection.optional.clear();
            approval.approved = vec![manage.into()];
            approval.administration_confirmed = true;
            approval.remember = false;
            let grant = authority
                .approve_grant(&mutation(&f.admin, &approval), &approval, current)
                .unwrap();
            let admitted = crate::platform::auth::tests::issue_authority(
                &authority,
                &AdmissionIdentity {
                    refresh_family_id: Some(super::oauth_family(
                        &authority,
                        &grant,
                        current,
                        current + 86400,
                    )),
                    credential: Credential::OAuthGrant(grant),
                    runtime_id: URL_SAFE_NO_PAD.encode([22; 16]),
                    session_public_key: URL_SAFE_NO_PAD.encode(
                        ed25519_dalek::SigningKey::from_bytes(&[52; 32])
                            .verifying_key()
                            .as_bytes(),
                    ),
                    inbox_prefix: "_INBOX.resetHttp".into(),
                },
                None,
                current,
            )
            .unwrap();
            let reset_request =
                trellis_runtime_apis::__types::trellis::AuthPasswordResetCreateRequest {
                    user_id: f.principal.parse().unwrap(),
                    idempotency_key: id().into(),
                    extra: Default::default(),
                };
            let reset = super::super::oauth::account::create_reset(
                engine.server.store(),
                &mutation(&f.admin, &reset_request),
                &reset_request,
                &admitted.authority.unsigned.authorization_session_id,
                current,
            )
            .unwrap();
            let before = authority.create_portal_session(current).unwrap();
            authority
                .portal_local_login(
                    &before.cookie,
                    &before.csrf,
                    &"r".repeat(43),
                    ("alice", "initial alice password"),
                    &id(),
                    current,
                )
                .unwrap();
            assert!(http
                .get(format!("{origin}/account/reset?token={}", reset.token))
                .send()
                .await
                .unwrap()
                .status()
                .is_success());
            let reset_form = [
                ("token", reset.token.as_str()),
                ("csrf", csrf.as_str()),
                ("password", "updated alice password"),
            ];
            assert_eq!(
                http.post(format!("{origin}/portal/account/reset"))
                    .header("Origin", "https://attacker.example")
                    .form(&reset_form)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                reqwest::StatusCode::BAD_REQUEST
            );
            assert_eq!(
                http.post(format!("{origin}/portal/account/reset"))
                    .header("Origin", &origin)
                    .form(&[
                        ("token", reset.token.as_str()),
                        ("csrf", "wrong csrf"),
                        ("password", "updated alice password")
                    ])
                    .send()
                    .await
                    .unwrap()
                    .status(),
                reqwest::StatusCode::BAD_REQUEST
            );
            assert!(http
                .post(format!("{origin}/portal/account/reset"))
                .header("Origin", &origin)
                .form(&reset_form)
                .send()
                .await
                .unwrap()
                .status()
                .is_success());
            assert_eq!(
                http.post(format!("{origin}/portal/account/reset"))
                    .header("Origin", &origin)
                    .form(&reset_form)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                reqwest::StatusCode::BAD_REQUEST
            );
            assert!(authority
                .portal_login(&"r".repeat(43), &before.csrf, current + 1)
                .is_err());
            let after = authority.create_portal_session(current).unwrap();
            assert!(authority
                .portal_local_login(
                    &after.cookie,
                    &after.csrf,
                    &"s".repeat(43),
                    ("alice", "updated alice password"),
                    &id(),
                    current
                )
                .is_ok());
        }
        let cookies = jar.cookies(&portal).unwrap();
        let csrf = cookies
            .to_str()
            .unwrap()
            .split("; ")
            .find_map(|value| value.strip_prefix("__Host-trellis-portal-csrf="))
            .unwrap();
        assert!(http
            .post(format!("{origin}/portal/logout"))
            .header("Origin", &origin)
            .form(&[("csrf", csrf), ("request_id", id().as_str())])
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
    }
    let key = EcdsaP256Key::generate("upstream-native-client");
    let verifier = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
    let mut authorization = url::Url::parse(&format!("{origin}/authorize")).unwrap();
    authorization.query_pairs_mut().extend_pairs([
        ("client_id", "native-client".into()),
        ("redirect_uri", "http://127.0.0.1:9123/callback".into()),
        ("response_type", "code".into()),
        ("state", id()),
        (
            "code_challenge",
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        ),
        ("code_challenge_method", "S256".into()),
        ("dpop_jkt", key.public_jwk().thumbprint()),
        (
            "trellis_dpop_jwk",
            serde_json::to_string(&key.public_jwk()).unwrap(),
        ),
        (
            "authorization_details",
            serde_json::json!([{"type":"trellis_capabilities","required":[f.write],"optional":[]}])
                .to_string(),
        ),
    ]);
    let browser = tokio::process::Command::new("node")
        .args(["--test", "ts/browser/oauth_upstream.browser_driver.ts"])
        .env(
            "TRELLIS_TEST_OAUTH_AUTHORIZATION_URI",
            authorization.as_str(),
        )
        .env("TRELLIS_TEST_OIDC_ISSUER", issuer.as_str())
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .kill_on_drop(true)
        .output();
    let result = tokio::time::timeout(Duration::from_secs(45), browser)
        .await
        .expect("Chromium upstream flow deadline")
        .expect("Node and producer-installed Playwright Chromium are required");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let initial = http.get(authorization).send().await.unwrap();
    assert_eq!(initial.status(), reqwest::StatusCode::SEE_OTHER);
    let login_url = url::Url::parse(&origin)
        .unwrap()
        .join(initial.headers()["location"].to_str().unwrap())
        .unwrap();
    let transaction = login_url
        .query_pairs()
        .find(|(name, _)| name == "transaction")
        .unwrap()
        .1
        .into_owned();
    let begin = format!("{origin}/portal/oidc/{provider}/begin?transaction={transaction}");
    let first = http.get(&begin).send().await.unwrap();
    let mut wrong_nonce = url::Url::parse(first.headers()["location"].to_str().unwrap()).unwrap();
    let pairs = wrong_nonce
        .query_pairs()
        .map(|(name, value)| {
            let value = if name == "nonce" {
                "wrong-bound-nonce".into()
            } else {
                value.into_owned()
            };
            (name.into_owned(), value)
        })
        .collect::<Vec<_>>();
    wrong_nonce.set_query(None);
    wrong_nonce.query_pairs_mut().extend_pairs(pairs);
    let rejected = dex_login(&http, wrong_nonce, &issuer, &callback, "admin@example.com").await;
    let rejected = http.get(rejected).send().await.unwrap();
    assert_eq!(
        rejected.json::<serde_json::Value>().await.unwrap()["error"],
        "access_denied"
    );
    let retry = http.get(&begin).send().await.unwrap();
    let upstream_authorize =
        url::Url::parse(retry.headers()["location"].to_str().unwrap()).unwrap();
    userinfo_mode.store(2, std::sync::atomic::Ordering::SeqCst);
    let verified = dex_login(
        &http,
        upstream_authorize,
        &issuer,
        &callback,
        "admin@example.com",
    )
    .await;
    let logged_in = http.get(verified).send().await.unwrap();
    assert_eq!(logged_in.status(), reqwest::StatusCode::SEE_OTHER);
    let profile = authority
        .transaction(|connection| {
            super::super::rpc_users::read_user(&authority, connection, &f.principal, true)
        })
        .unwrap();
    assert_eq!(profile.principal_id.to_string(), f.principal);
    assert_eq!(profile.email.as_deref(), Some("admin@example.com"));
    assert!(profile.email_verified);
    let separate = authority
        .transaction(|connection| {
            super::super::rpc_users::read_user(&authority, connection, &shadow.to_string(), true)
        })
        .unwrap();
    assert_ne!(separate.principal_id.to_string(), f.principal);
    assert_eq!(separate.email.as_deref(), Some("admin@example.com"));
    assert!(!separate.email_verified);
    assert_eq!(profile.given_name.as_deref(), Some("UserInfo given"));
    assert_eq!(profile.display_name, "Alice");
    let consent = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(consent["capabilities"][0]["capabilityId"], f.write);
    assert_eq!(consent["capabilities"][0]["available"], true);

    // Company sign-in started from the device approval page, which has no OAuth
    // transaction, returns there signed in: the review is then refused only for
    // its code, not for a missing login.
    let review = id();
    let review_jar = Arc::new(reqwest::cookie::Jar::default());
    let review_http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .cookie_provider(review_jar.clone())
        .add_root_certificate(reqwest::Certificate::from_pem(certificate.pem.as_bytes()).unwrap())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let inspect = |csrf: String| {
        review_http
            .post(format!("{origin}/portal/device-review/inspect"))
            .header("Origin", &origin)
            .form(&[
                ("csrf", csrf),
                ("review_id", review.clone()),
                ("code", "ABCDEFGH".to_owned()),
            ])
            .send()
    };
    let page = review_http
        .get(format!("{origin}/device-enrollment?reviewId={review}"))
        .send()
        .await
        .unwrap();
    assert!(page.status().is_success());
    let review_csrf = review_jar
        .cookies(&url::Url::parse(&origin).unwrap())
        .unwrap()
        .to_str()
        .unwrap()
        .split("; ")
        .find_map(|value| value.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap()
        .to_owned();
    assert_eq!(
        inspect(review_csrf.clone()).await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let started = review_http
        .get(format!(
            "{origin}/portal/oidc/{provider}/begin?review={review}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::SEE_OTHER);
    let upstream_review = url::Url::parse(started.headers()["location"].to_str().unwrap()).unwrap();
    let returned = dex_login(
        &review_http,
        upstream_review,
        &issuer,
        &callback,
        "admin@example.com",
    )
    .await;
    let finished = review_http.get(returned).send().await.unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        finished.headers()["location"].to_str().unwrap(),
        format!("/device-enrollment?reviewId={review}")
    );
    assert_eq!(
        inspect(review_csrf).await.unwrap().status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let portal = url::Url::parse(&origin).unwrap();
    let cookies = jar.cookies(&portal).unwrap();
    let csrf = cookies
        .to_str()
        .unwrap()
        .split("; ")
        .find_map(|value| value.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap();
    let approved = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", &origin)
        .form(&[
            ("transaction", transaction.as_str()),
            ("presentation", consent["presentationId"].as_str().unwrap()),
            ("csrf", csrf),
            ("decision", "approve"),
            ("approved", f.write.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), reqwest::StatusCode::SEE_OTHER);
    let code = url::Url::parse(approved.headers()["location"].to_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == "code")
        .unwrap()
        .1
        .into_owned();
    let ephemeral = nkeys::KeyPair::new_user();
    let runtime = URL_SAFE_NO_PAD.encode([18; 16]);
    let proof = super::super::oauth::engine_tests::dpop(
        &key,
        &runtime,
        &ephemeral,
        &format!("{origin}/token"),
    )
    .await;
    let token_request = http.post(format!("{origin}/token")).form(&[
        ("grant_type", "authorization_code"),
        ("client_id", "native-client"),
        ("code", code.as_str()),
        ("redirect_uri", "http://127.0.0.1:9123/callback"),
        ("code_verifier", verifier),
        ("trellis_runtime_id", runtime.as_str()),
        ("trellis_session_nkey", ephemeral.public_key().as_str()),
    ]);
    let challenge = token_request
        .try_clone()
        .unwrap()
        .header("DPoP", proof)
        .send()
        .await
        .unwrap();
    assert_eq!(challenge.status(), reqwest::StatusCode::BAD_REQUEST);
    let nonce = challenge.headers()["DPoP-Nonce"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        challenge.json::<serde_json::Value>().await.unwrap()["error"],
        "use_dpop_nonce"
    );
    let proof = super::super::oauth::engine_tests::dpop_with_nonce(
        &key,
        &runtime,
        &ephemeral,
        &format!("{origin}/token"),
        Some(&nonce),
    )
    .await;
    let token = token_request.header("DPoP", proof).send().await.unwrap();
    assert_eq!(token.status(), reqwest::StatusCode::OK);
    let tokens = token
        .json::<super::super::oauth::TrellisTokenResponse>()
        .await
        .unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let admitted = crate::platform::auth::tests::admit_credential(
        &authority,
        tokens.trellis_admission.as_bytes(),
        &ephemeral.public_key(),
        now,
    )
    .unwrap();
    assert_eq!(admitted.authority.unsigned.principal_id, f.principal);
    assert_eq!(
        admitted
            .authority
            .unsigned
            .capabilities
            .iter()
            .map(|capability| capability.capability_id.as_str())
            .collect::<Vec<_>>(),
        vec![f.write.as_str()]
    );
    // Both mapped roles contribute only through claims verified by the real IdP flow.
    let login: String = authority
        .transaction(|connection| {
            Ok(connection.query_row(
                "SELECT login_session_id \
                FROM auth_login_sessions \
                WHERE principal_id=?1 \
                AND provider_id=?2 ORDER BY observation_order DESC \
                LIMIT 1",
                rusqlite::params![f.principal, provider],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let access = authority
        .transaction(|connection| {
            crate::platform::auth::policy::entitlements(connection, &f.principal, Some(&login), now)
        })
        .unwrap();
    assert!(access.contains_key(&f.read));
    assert!(access.contains_key(&f.write));
    let delete = trellis_runtime_apis::__types::trellis::AuthOIDCRoleMappingDeleteRequest {
        mapping_id: mapping_id.parse().unwrap(),
        expected_revision: trellis_runtime_apis::__types::trellis::AuthRevision(1.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let delete_identity = mutation(&f.admin, &delete);
    let deleted = crate::platform::auth::rpc_users::delete_mapping(
        &authority,
        &delete_identity,
        &delete,
        now,
    )
    .unwrap();
    assert!(authority
        .transaction(|connection| crate::platform::auth::policy::entitlements(
            connection,
            &f.principal,
            Some(&login),
            now
        ))
        .unwrap()
        .is_empty());
    assert_eq!(
        crate::platform::auth::rpc_users::delete_mapping(
            &authority,
            &delete_identity,
            &delete,
            now + 1
        )
        .unwrap(),
        deleted
    );
    // A first company sign-in from someone nobody linked creates their account
    // only once the app they are signing in to allows it.
    // Each attempt is a fresh browser, so none reuses an earlier Portal login.
    let company_sign_in = || async {
        let newcomer_http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .cookie_provider(Arc::new(reqwest::cookie::Jar::default()))
            .add_root_certificate(
                reqwest::Certificate::from_pem(certificate.pem.as_bytes()).unwrap(),
            )
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let key = EcdsaP256Key::generate("newcomer");
        let mut authorize = url::Url::parse(&format!("{origin}/authorize")).unwrap();
        authorize.query_pairs_mut().extend_pairs([
            ("client_id", "native-client".to_owned()),
            ("redirect_uri", "http://127.0.0.1:9123/callback".into()),
            ("response_type", "code".into()),
            ("state", id()),
            (
                "code_challenge",
                URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
            ("code_challenge_method", "S256".into()),
            ("dpop_jkt", key.public_jwk().thumbprint()),
            (
                "trellis_dpop_jwk",
                serde_json::to_string(&key.public_jwk()).unwrap(),
            ),
            (
                "authorization_details",
                serde_json::json!([{"type":"trellis_capabilities","required":[f.write],"optional":[]}])
                    .to_string(),
            ),
        ]);
        let initial = newcomer_http.get(authorize).send().await.unwrap();
        let transaction = url::Url::parse(&origin)
            .unwrap()
            .join(initial.headers()["location"].to_str().unwrap())
            .unwrap()
            .query_pairs()
            .find(|(name, _)| name == "transaction")
            .unwrap()
            .1
            .into_owned();
        let begin = newcomer_http
            .get(format!(
                "{origin}/portal/oidc/{provider}/begin?transaction={transaction}"
            ))
            .send()
            .await
            .unwrap();
        let upstream = url::Url::parse(begin.headers()["location"].to_str().unwrap()).unwrap();
        let returned = dex_login(
            &newcomer_http,
            upstream,
            &issuer,
            &callback,
            "newcomer@example.com",
        )
        .await;
        (
            transaction,
            newcomer_http.get(returned).send().await.unwrap(),
        )
    };
    let newcomer = || {
        authority
            .read(|connection| {
                use rusqlite::OptionalExtension as _;
                Ok(connection
                    .query_row(
                        "SELECT principal_id FROM auth_provider_identities \
                        WHERE provider=?1 AND principal_id!=?2",
                        rusqlite::params![provider, f.principal],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?)
            })
            .unwrap()
    };
    let (_, refused) = company_sign_in().await;
    assert_eq!(
        refused.json::<serde_json::Value>().await.unwrap()["error"],
        "access_denied"
    );
    assert_eq!(newcomer(), None);
    let revision = authority
        .transaction(|connection| {
            super::super::rpc_credentials::read_client(connection, "native-client")
        })
        .unwrap()
        .revision;
    let allow = trellis_runtime_apis::__types::trellis::AuthOAuthClientSignUpUpdateRequest {
        client_id: "native-client".to_string().into(),
        sign_up: trellis_runtime_apis::__types::trellis::AuthOAuthClientSignUp {
            password: false,
            providers: true,
            extra: Default::default(),
        },
        expected_revision: revision,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    super::super::rpc_credentials::update_client_sign_up(
        &authority,
        &mutation(&f.admin, &allow),
        &allow,
        now,
    )
    .unwrap();
    let (transaction, signed_up) = company_sign_in().await;
    assert_eq!(
        signed_up.headers()["location"].to_str().unwrap(),
        format!("/consent?transaction={transaction}")
    );
    let created = newcomer().expect("the company sign-in created a linked account");
    let profile = authority
        .transaction(|connection| {
            super::super::rpc_users::read_user(&authority, connection, &created, true)
        })
        .unwrap();
    assert_eq!(profile.email.as_deref(), Some("newcomer@example.com"));
    // Signing in again finds the same account rather than creating another.
    let (_, again) = company_sign_in().await;
    assert!(again.status().is_redirection());
    assert_eq!(newcomer(), Some(created));
    // With company sign-in turned off for the installation, even a linked
    // identity no longer signs in, and the Portal offers no provider.
    let off = trellis_runtime_apis::__types::trellis::AuthSignInSettingsUpdateRequest {
        local_users: true,
        local_sign_up: false,
        providers: false,
        expected_revision: trellis_runtime_apis::__types::trellis::AuthRevision(1u64.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    super::super::sign_in::update(&authority, &mutation(&f.admin, &off), &off, now).unwrap();
    let (_, refused) = company_sign_in().await;
    assert_eq!(
        refused.json::<serde_json::Value>().await.unwrap()["error"],
        "access_denied"
    );
    let offered: serde_json::Value = http
        .get(format!("{origin}/portal/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(offered["providers"], serde_json::json!([]));
    shutdown.cancel();
    server.await.unwrap().unwrap();
    proxy_handle.graceful_shutdown(Some(Duration::from_secs(3)));
    proxy_server.await.unwrap().unwrap();
    idp.kill().await.unwrap();
    idp.wait().await.unwrap();
}
