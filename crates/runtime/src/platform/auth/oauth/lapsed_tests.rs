//! What a browser gets from the Portal when its flow can't continue.

use std::{sync::Arc, time::UNIX_EPOCH};

use super::super::oauth::{OAuthEngine, OAuthStorage};
use super::super::sqlite::id;
use super::super::tests::*;
use super::super::*;

#[tokio::test]
async fn browsers_get_portal_pages_for_unusable_flows_and_one_answer_for_every_unusable_device_code(
) {
    let mut f = fixture();
    let registration = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Native client".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![],
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
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_issuer = origin.clone();
    let authority = f.store;
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
    );
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
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let now = crate::clock::unix_now();
    let session = authority.create_portal_session(now).unwrap();
    let cookies = format!(
        "__Host-trellis-portal={}; __Host-trellis-portal-csrf={}",
        session.cookie, session.csrf
    );
    let at = |seconds: i64| UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64);
    for (device_code, user_code, state, expires) in [
        (
            "expired-device-code",
            "EXPD-WXYZ",
            oauth_as::device::DeviceGrantState::Pending,
            now - 60,
        ),
        (
            "used-device-code",
            "USED-WXYZ",
            oauth_as::device::DeviceGrantState::Approved {
                subject: f.admin.clone(),
            },
            now + 600,
        ),
    ] {
        oauth_as::store::Storage::put_device_grant(
            engine.server.store(),
            oauth_as::device::DeviceGrant {
                device_code: device_code.into(),
                user_code: user_code.into(),
                client_id: oauth_as::client::ClientId::new("native-client"),
                scope: Default::default(),
                state,
                created_at: at(now - 900),
                expires_at: at(expires),
                interval: std::time::Duration::from_secs(5),
                last_poll_at: None,
            },
        )
        .await
        .unwrap();
    }
    let enter = |user_code: &'static str, accept: &'static str| {
        http.post(format!("{origin}/portal/device"))
            .header("Origin", &origin)
            .header("Cookie", &cookies)
            .header("Accept", accept)
            .form(&[("csrf", session.csrf.as_str()), ("user_code", user_code)])
            .send()
    };
    // Unknown, expired and used codes get one identical page, so the page
    // can't tell a valid code from an invalid one; API callers keep the JSON.
    let mut pages = Vec::new();
    let mut answers = Vec::new();
    for code in ["NONE-WXYZ", "EXPD-WXYZ", "USED-WXYZ"] {
        let page = enter(code, "text/html,application/xhtml+xml")
            .await
            .unwrap();
        let kind = page.headers()[reqwest::header::CONTENT_TYPE].clone();
        pages.push((page.status(), kind, page.text().await.unwrap()));
        let answer = enter(code, "application/json").await.unwrap();
        answers.push((
            answer.status(),
            answer.json::<serde_json::Value>().await.unwrap(),
        ));
    }
    assert!(pages[0].1.to_str().unwrap().starts_with("text/html"));
    assert!(
        pages[0].2.contains("This code isn't valid"),
        "{}",
        pages[0].2
    );
    assert!(pages.iter().all(|page| *page == pages[0]), "{pages:?}");
    assert_eq!(answers[0].1["error"], "invalid_request");
    assert!(answers.iter().all(|answer| *answer == answers[0]));

    // An authorization this browser can no longer continue starts again with
    // its original request; a used one, whose record is gone, is explained.
    let lapsed = id();
    engine
        .server
        .store()
        .store_authorization(
            &lapsed,
            &super::portal::PortalAuthorization {
                presentation: None,
                session_id: session.session_id.clone(),
                request: super::portal::ConsentRequest::Authorization(vec![
                    ("client_id".into(), "native-client".into()),
                    ("state".into(), "kept & sent".into()),
                ]),
                public_jwk: serde_json::json!({}),
                expires_at: now - 1,
            },
            "native-client",
        )
        .unwrap();
    for page in ["login", "consent"] {
        let restart = http
            .get(format!("{origin}/{page}?transaction={lapsed}"))
            .header("Cookie", &cookies)
            .header("Accept", "text/html")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            restart.contains(
                r#"href="/authorize?client_id=native-client&amp;state=kept+%26+sent">Start again"#
            ),
            "{restart}"
        );
    }
    let used = http
        .get(format!("{origin}/consent?transaction={}", id()))
        .header("Cookie", &cookies)
        .send()
        .await
        .unwrap();
    assert!(used.status().is_client_error());
    let used = used.text().await.unwrap();
    assert!(used.contains("has expired or was already used"), "{used}");
    assert!(!used.contains("Start again"));
    let api = http
        .get(format!("{origin}/portal/authorization/{lapsed}"))
        .header("Cookie", &cookies)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(api["error"], "access_denied");
    let _ = stop.send(());
    host.await.unwrap();
}
