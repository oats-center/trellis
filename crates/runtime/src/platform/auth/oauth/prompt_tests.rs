//! Where `/authorize` sends someone already signed in to the Portal.

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use oauth_as::jwt::EcdsaP256Key;
use sha2::{Digest as _, Sha256};

use super::super::oauth::{OAuthEngine, OAuthStorage};
use super::super::sqlite::id;
use super::super::tests::*;

#[tokio::test]
async fn a_signed_in_person_continues_silently_unless_the_app_asks_to_choose_or_sign_in_again() {
    let mut f = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_issuer = origin.clone();
    let authority = f.store;
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[19; 32]).unwrap()).unwrap(),
    );
    let router = super::router(engine, vec![]);
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
    let now = crate::clock::unix_now();
    let session = authority.create_portal_session(now).unwrap();
    let signed_in = URL_SAFE_NO_PAD.encode([7u8; 32]);
    authority
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
    let key = EcdsaP256Key::generate("prompt-flow");
    let authorize = |prompt: Option<&str>| {
        let mut url = url::Url::parse(&format!("{origin}/authorize")).unwrap();
        url.query_pairs_mut().extend_pairs([
            ("client_id", "native-client".to_owned()),
            ("redirect_uri", "http://127.0.0.1:9123/callback".into()),
            ("response_type", "code".into()),
            (
                "code_challenge",
                URL_SAFE_NO_PAD.encode(Sha256::digest(
                    b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                )),
            ),
            ("code_challenge_method", "S256".into()),
            ("state", id()),
            ("dpop_jkt", key.public_jwk().thumbprint()),
            (
                "trellis_dpop_jwk",
                serde_json::to_string(&key.public_jwk()).unwrap(),
            ),
            (
                "authorization_details",
                serde_json::json!([
                    {"type":"trellis_capabilities","required":[f.read],"optional":[]}
                ])
                .to_string(),
            ),
        ]);
        if let Some(prompt) = prompt {
            url.query_pairs_mut().append_pair("prompt", prompt);
        }
        http.get(url.as_str()).header("Cookie", &cookies).send()
    };
    let destination = |response: &reqwest::Response| {
        let location = response.headers()["location"].to_str().unwrap();
        let location = url::Url::parse(&format!("{origin}{location}")).unwrap();
        let account = location
            .query_pairs()
            .find(|(key, _)| key == "account")
            .map(|(_, value)| value.into_owned());
        (location.path().to_owned(), account)
    };

    // Without a prompt the signed-in person goes straight on.
    let plain = authorize(None).await.unwrap();
    assert_eq!(destination(&plain), ("/consent".into(), None));
    assert!(plain.headers().get("set-cookie").is_none());

    // `select_account` asks whether to continue as them.
    let choose = authorize(Some("select_account")).await.unwrap();
    assert_eq!(
        destination(&choose),
        ("/consent".into(), Some("confirm".into()))
    );

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
    assert_ne!(replaced, signed_in);
    let _ = stop.send(());
    host.await.unwrap();
}
