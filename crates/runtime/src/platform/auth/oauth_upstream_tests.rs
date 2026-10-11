//! Live upstream interoperability with the unmodified production Dex binary.
use super::super::oauth::{OAuthEngine, OAuthListener, OAuthStorage, OAuthTransport};
use super::*;
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
                        "login" => "admin@example.com",
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
    let certificate = super::oauth_transport_tests::certificate();
    let idp_socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let idp_address = idp_socket.local_addr().unwrap();
    drop(idp_socket);
    let issuer = url::Url::parse(&format!("https://{idp_address}/dex")).unwrap();
    let portal_socket = tokio::net::TcpListener::bind("127.0.0.2:0").await.unwrap();
    let origin = format!("https://{}", portal_socket.local_addr().unwrap());
    let callback = url::Url::parse(&format!("{origin}/portal/oidc/callback")).unwrap();
    let config = certificate.directory.path().join("dex.yaml");
    let database = certificate.directory.path().join("dex.sqlite");
    let user_id = id();
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
    let result = dex_login(&http, authorization, &issuer, &callback).await;
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
    f.store.settings.oauth_origin = origin.clone();
    f.store.settings.upstream_tls_roots_pem = vec![certificate.pem.clone()];
    let authority = Arc::new(f.store);
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
        config: serde_json::json!({"scopes":["groups"]}),
        claim_freshness_seconds: 300,
        expected_revision: 0,
    };
    authority
        .register_oidc_provider(&mutation(&f.admin, &registration), &registration, NOW)
        .unwrap();
    authority
        .link_oidc_identity(
            &mutation(&f.admin, &(&f.principal, &provider, &subject)),
            &f.principal,
            &provider,
            &subject,
            NOW,
        )
        .unwrap();
    let group_role = id();
    let role = PolicyMutation::RolePut {
        role_id: group_role.clone(),
        title: "Upstream writers".into(),
        description: "Write authority derived from the upstream team-a group".into(),
        capabilities: vec![f.write.clone()],
        expected_revision: 0,
    };
    authority
        .apply_policy(&mutation(&f.admin, &role), &role, NOW)
        .unwrap();
    let mapping = PolicyMutation::OidcMappingPut {
        mapping_id: id(),
        provider_id: provider.clone(),
        claim_name: "/groups".into(),
        claim_value: "team-a".into(),
        role_id: group_role,
        expected_revision: 0,
    };
    authority
        .apply_policy(&mutation(&f.admin, &mapping), &mapping, NOW)
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
    let rejected = dex_login(&http, wrong_nonce, &issuer, &callback).await;
    let rejected = http.get(rejected).send().await.unwrap();
    assert_eq!(
        rejected.json::<serde_json::Value>().await.unwrap()["error"],
        "access_denied"
    );
    let retry = http.get(&begin).send().await.unwrap();
    let upstream_authorize =
        url::Url::parse(retry.headers()["location"].to_str().unwrap()).unwrap();
    let verified = dex_login(&http, upstream_authorize, &issuer, &callback).await;
    let logged_in = http.get(verified).send().await.unwrap();
    assert_eq!(logged_in.status(), reqwest::StatusCode::SEE_OTHER);
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
    let proof =
        super::oauth_engine_tests::dpop(&key, &runtime, &ephemeral, &format!("{origin}/token"))
            .await;
    let token = http
        .post(format!("{origin}/token"))
        .header("DPoP", proof)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", "native-client"),
            ("code", code.as_str()),
            ("redirect_uri", "http://127.0.0.1:9123/callback"),
            ("code_verifier", verifier),
            ("trellis_runtime_id", runtime.as_str()),
            ("trellis_session_nkey", ephemeral.public_key().as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(token.status(), reqwest::StatusCode::OK);
    let tokens = token
        .json::<super::super::oauth::TrellisTokenResponse>()
        .await
        .unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let admitted = authority
        .admit_oauth(
            tokens.trellis_admission.as_bytes(),
            &ephemeral.public_key(),
            "_INBOX.upstream",
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
    shutdown.cancel();
    server.await.unwrap().unwrap();
    idp.kill().await.unwrap();
    idp.wait().await.unwrap();
}
