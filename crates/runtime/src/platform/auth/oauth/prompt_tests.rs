//! Where a sign-in goes for someone already signed in to the Portal.

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use oauth_as::jwt::EcdsaP256Key;
use sha2::{Digest as _, Sha256};

use super::super::oauth::{OAuthEngine, OAuthStorage};
use super::super::sqlite::id;
use super::super::tests::*;
use super::super::*;

/// A router over `f`'s store with `admin` signed in to one Portal session.
struct Signed {
    engine: Arc<OAuthEngine>,
    origin: String,
    http: reqwest::Client,
    session: super::portal::PortalCookie,
    cookies: String,
    stop: tokio::sync::oneshot::Sender<()>,
    host: tokio::task::JoinHandle<()>,
}

async fn signed_in(mut f: Fixture) -> (Signed, Fixture) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_issuer = origin.clone();
    let engine =
        Arc::new(OAuthEngine::new(OAuthStorage::new(f.store.clone(), &[19; 32]).unwrap()).unwrap());
    let router = super::router(engine.clone(), vec![]);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let host = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let now = crate::clock::unix_now();
    let session = f.store.create_portal_session(now).unwrap();
    let signed_in = URL_SAFE_NO_PAD.encode([7u8; 32]);
    f.store
        .portal_local_login(
            &session.cookie,
            &session.csrf,
            &signed_in,
            ("admin", "a strong bootstrap password"),
            &id(),
            now,
        )
        .unwrap();
    let cookies = format!(
        "__Host-trellis-portal={signed_in}; __Host-trellis-portal-csrf={}",
        session.csrf
    );
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    (
        Signed {
            engine,
            origin,
            http,
            session,
            cookies,
            stop,
            host,
        },
        f,
    )
}

/// An authorization request's parameters for `client`, returning to `redirect`.
fn request(
    key: &EcdsaP256Key,
    client: &str,
    redirect: &str,
    optional: &str,
    prompt: Option<&str>,
) -> Vec<(String, String)> {
    let mut pairs = vec![
        ("client_id".to_owned(), client.to_owned()),
        ("redirect_uri".into(), redirect.into()),
        ("response_type".into(), "code".into()),
        (
            "code_challenge".into(),
            URL_SAFE_NO_PAD.encode(Sha256::digest(
                b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
            )),
        ),
        ("code_challenge_method".into(), "S256".into()),
        ("state".into(), id()),
        ("dpop_jkt".into(), key.public_jwk().thumbprint()),
        (
            "trellis_dpop_jwk".into(),
            serde_json::to_string(&key.public_jwk()).unwrap(),
        ),
        (
            "authorization_details".into(),
            serde_json::json!([
                {"type":"trellis_capabilities","required":[],"optional":[optional]}
            ])
            .to_string(),
        ),
    ];
    if let Some(prompt) = prompt {
        pairs.push(("prompt".into(), prompt.into()));
    }
    pairs
}

#[tokio::test]
async fn a_signed_in_person_is_asked_about_the_account_unless_the_app_asks_to_sign_in_again() {
    let (signed, f) = signed_in(fixture()).await;
    let key = EcdsaP256Key::generate("prompt-flow");
    let authorize = |prompt: Option<&str>| {
        let mut url = url::Url::parse(&format!("{}/authorize", signed.origin)).unwrap();
        url.query_pairs_mut().extend_pairs(request(
            &key,
            "native-client",
            "http://127.0.0.1:9123/callback",
            &f.read,
            prompt,
        ));
        signed
            .http
            .get(url.as_str())
            .header("Cookie", &signed.cookies)
            .send()
    };
    let destination = |response: &reqwest::Response| {
        let location = response.headers()["location"].to_str().unwrap();
        let location = url::Url::parse(&format!("{}{location}", signed.origin)).unwrap();
        let account = location
            .query_pairs()
            .find(|(key, _)| key == "account")
            .map(|(_, value)| value.into_owned());
        (location.path().to_owned(), account)
    };

    // The signed-in person continues on their Portal session, and the
    // consent page decides whether to ask which account.
    let plain = authorize(None).await.unwrap();
    assert_eq!(
        destination(&plain),
        ("/consent".into(), Some("confirm".into()))
    );
    assert!(plain.headers().get("set-cookie").is_none());

    // `login` signs in afresh, on a new Portal session for this browser.
    let fresh = authorize(Some("login")).await.unwrap();
    assert_eq!(destination(&fresh), ("/login".into(), None));
    let replaced = fresh
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .split(';')
                .next()?
                .strip_prefix("__Host-trellis-portal=")
        })
        .unwrap()
        .to_owned();
    assert!(!signed.cookies.contains(&replaced));
    let _ = signed.stop.send(());
    signed.host.await.unwrap();
}

#[tokio::test]
async fn only_the_operators_own_apps_sign_a_signed_in_person_in_without_showing_the_account() {
    use oauth_as::cimd::{CimdPolicy, ClientIdUrl, ValidatedClientIdDocument};
    let f = fixture();
    let now = crate::clock::unix_now();
    let grant = PolicyMutation::CapabilityGrant {
        principal_id: f.admin.clone(),
        capability_id: f.read.clone(),
        expected_revision: 0,
        expires_at: None,
    };
    apply_policy(&f.store, &mutation(&f.admin, &grant), &grant, now).unwrap();
    // The operator's app allows the capability without asking.
    let registered = ClientRegistration {
        client_id: "operator-app".into(),
        kind: "browser".into(),
        display_name: "Operator app".into(),
        redirect_uris: vec!["https://operator.example/callback".into()],
        development: false,
        implied_capabilities: vec![f.read.clone()],
        metadata: serde_json::json!({}),
        expected_revision: 0,
    };
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &registered),
                &registered,
                now,
            )
        })
        .unwrap();
    let (signed, f) = signed_in(f).await;
    // A third-party app known only by its client metadata document.
    let third_party = "https://public.example/client.json";
    let metadata = serde_json::json!({
        "client_id":third_party,
        "application_type":"web",
        "client_name":"Public app",
        "redirect_uris": ["https://public.example/callback"],
        "token_endpoint_auth_method":"none",
        "grant_types": ["authorization_code","refresh_token"],
        "response_types": ["code"]
    });
    let body = serde_json::to_vec(&metadata).unwrap();
    let mut policy = CimdPolicy::new();
    policy.redirect_uris_same_origin = false;
    let identity = ClientIdUrl::parse(third_party, &policy).unwrap();
    signed
        .engine
        .server
        .store()
        .install_client_document(&super::ClientDocument {
            document: ValidatedClientIdDocument::validate(&identity, &body, &policy).unwrap(),
            revision: URL_SAFE_NO_PAD.encode(Sha256::digest(&body)),
            metadata,
            kind: "browser".into(),
            observed_at: std::time::SystemTime::now(),
        })
        .unwrap();
    let key = EcdsaP256Key::generate("account-flow");
    let pending = |client: &str, redirect: &str, prompt: Option<&str>| {
        let transaction = id();
        signed
            .engine
            .server
            .store()
            .store_authorization(
                &transaction,
                &super::portal::PortalAuthorization {
                    presentation: None,
                    session_id: signed.session.session_id.clone(),
                    request: super::portal::ConsentRequest::Authorization(request(
                        &key, client, redirect, &f.read, prompt,
                    )),
                    public_jwk: serde_json::to_value(key.public_jwk()).unwrap(),
                    expires_at: now + 600,
                },
                client,
            )
            .unwrap();
        transaction
    };
    let consent = |transaction: String| {
        signed
            .http
            .get(format!(
                "{}/consent?transaction={transaction}&account=confirm",
                signed.origin
            ))
            .header("Cookie", &signed.cookies)
            .send()
    };
    let card = |page: &str| page.contains(r#""confirmAccount":true"#);

    // The third-party app's consent is remembered once the person allows it.
    let first = pending(third_party, "https://public.example/callback", None);
    let view: serde_json::Value = signed
        .http
        .get(format!("{}/portal/authorization/{first}", signed.origin))
        .header("Cookie", &signed.cookies)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let approved = signed
        .http
        .post(format!("{}/portal/approve", signed.origin))
        .header("Origin", &signed.origin)
        .header("Cookie", &signed.cookies)
        .form(&[
            ("csrf", signed.session.csrf.as_str()),
            ("transaction", first.as_str()),
            ("presentation", view["presentationId"].as_str().unwrap()),
            ("decision", "approve"),
            ("approved", f.read.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert!(approved.headers()["location"]
        .to_str()
        .unwrap()
        .starts_with("https://public.example/callback?code="));

    // With nothing left to decide, the third-party app still shows which
    // account it gets; continuing finishes without asking again.
    let returning = pending(third_party, "https://public.example/callback", None);
    let shown = consent(returning.clone()).await.unwrap();
    assert_eq!(shown.status(), reqwest::StatusCode::OK);
    assert!(card(&shown.text().await.unwrap()));
    let continued = signed
        .http
        .get(format!("{}/consent?transaction={returning}", signed.origin))
        .header("Cookie", &signed.cookies)
        .send()
        .await
        .unwrap();
    assert!(continued.headers()["location"]
        .to_str()
        .unwrap()
        .starts_with("https://public.example/callback?code="));

    // The operator's app signs the same person straight in.
    let operator = consent(pending(
        "operator-app",
        "https://operator.example/callback",
        None,
    ))
    .await
    .unwrap();
    assert!(operator.headers()["location"]
        .to_str()
        .unwrap()
        .starts_with("https://operator.example/callback?code="));

    // Unless it asks to choose.
    let choosing = consent(pending(
        "operator-app",
        "https://operator.example/callback",
        Some("select_account"),
    ))
    .await
    .unwrap();
    assert!(card(&choosing.text().await.unwrap()));
    let _ = signed.stop.send(());
    signed.host.await.unwrap();
}
