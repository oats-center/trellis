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

#[derive(Clone, Copy, Default)]
enum ChainTampering {
    #[default]
    None,
    Missing,
    Forged,
}

#[derive(Default)]
struct ProxyControl {
    chain: ChainTampering,
    refresh_posts: usize,
    successes: Vec<(Vec<u8>, Vec<u8>)>,
    pause_refresh: Option<tokio::sync::oneshot::Sender<tokio::sync::oneshot::Sender<()>>>,
    hide_chain_after_success: bool,
    lose_success_response: bool,
}

struct NativeOAuthProxy {
    backend: String,
    http: reqwest::Client,
    control: std::sync::Mutex<ProxyControl>,
}

// All authentication responses come from the ordinary HTTP engine. Only this
// network intermediary changes frames or holds a real refresh across rotation.
async fn proxy_request(
    axum::extract::State(proxy): axum::extract::State<Arc<NativeOAuthProxy>>,
    request: axum::http::Request<axum::body::Body>,
) -> axum::http::Response<axum::body::Body> {
    use axum::{body::Body, http::header};

    let (mut parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
    let form: BTreeMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
    let refreshing = parts.uri.path() == "/token"
        && form
            .get("grant_type")
            .is_some_and(|value| value == "refresh_token");
    if refreshing {
        let pause = {
            let mut control = proxy.control.lock().unwrap();
            control.refresh_posts += 1;
            control.pause_refresh.take()
        };
        if let Some(arrived) = pause {
            let (forward, held) = tokio::sync::oneshot::channel();
            arrived.send(forward).unwrap();
            tokio::time::timeout(Duration::from_secs(15), held)
                .await
                .expect("held real refresh must be released")
                .unwrap();
        }
    }
    parts.headers.remove(header::TRANSFER_ENCODING);
    parts.headers.remove(header::CONTENT_LENGTH);
    let Ok(response) = proxy
        .http
        .request(parts.method, format!("{}{}", proxy.backend, parts.uri))
        .headers(parts.headers)
        .body(body)
        .send()
        .await
    else {
        return axum::http::Response::builder()
            .status(axum::http::StatusCode::BAD_GATEWAY)
            .body(Body::empty())
            .unwrap();
    };
    let status = response.status();
    let mut headers = response.headers().clone();
    let mut bytes = response.bytes().await.unwrap().to_vec();
    assert!(bytes.len() <= 1024 * 1024);
    let mut control = proxy.control.lock().unwrap();
    if refreshing && status == axum::http::StatusCode::OK {
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        control.successes.push((
            Sha256::digest(form["refresh_token"].as_bytes()).to_vec(),
            Sha256::digest(result["refresh_token"].as_str().unwrap().as_bytes()).to_vec(),
        ));
        if control.hide_chain_after_success {
            control.chain = ChainTampering::Missing;
            control.hide_chain_after_success = false;
        }
        if control.lose_success_response {
            control.lose_success_response = false;
            // The backend consumed the token and produced a real success, but
            // the caller never receives its replacement secret.
            let lost = Body::from_stream(futures_util::stream::once(async {
                Err::<axum::body::Bytes, _>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "lost upstream token response",
                ))
            }));
            return axum::http::Response::builder()
                .status(status)
                .body(lost)
                .unwrap();
        }
    }
    if parts.uri.path() == "/.well-known/trellis/issuer-chain"
        && status == axum::http::StatusCode::OK
    {
        let mut chain: trellis_runtime_apis::types::AuthIssuerChainResponse =
            serde_json::from_slice(&bytes).unwrap();
        match control.chain {
            ChainTampering::None => {}
            ChainTampering::Missing => {
                assert!(!chain.rotations.is_empty());
                chain.rotations.clear();
            }
            ChainTampering::Forged => {
                let blob = &mut chain.rotations[0];
                let mut rotation: trellis_protocol::SignedIssuerRotation =
                    serde_json::from_slice(&blob.0 .0).unwrap();
                let replacement = if rotation.signature.starts_with('A') {
                    "B"
                } else {
                    "A"
                };
                rotation.signature.replace_range(..1, replacement);
                blob.0 .0 = serde_json::to_vec(&rotation).unwrap();
            }
        }
        bytes = serde_json::to_vec(&chain).unwrap();
    }
    headers.remove(header::TRANSFER_ENCODING);
    headers.insert(
        header::CONTENT_LENGTH,
        axum::http::HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
    );
    let mut response = axum::http::Response::new(Body::from(bytes));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

#[tokio::test]
async fn native_device_approval_refresh_and_revocation_retain_connection_identity_and_exclusive_custody(
) {
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_address = socket.local_addr().unwrap();
    let backend = format!("http://{backend_address}");
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", public.local_addr().unwrap());
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let proxy = Arc::new(NativeOAuthProxy {
        backend: backend.clone(),
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
        control: Default::default(),
    });
    let proxy_shutdown = tokio_util::sync::CancellationToken::new();
    let stopping = proxy_shutdown.clone();
    let proxy_router = axum::Router::new()
        .fallback(proxy_request)
        .with_state(proxy.clone());
    let proxy_server = tokio::spawn(async move {
        axum::serve(public, proxy_router)
            .with_graceful_shutdown(stopping.cancelled_owned())
            .await
            .unwrap();
    });
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
        implied_capabilities: vec![f.read.clone()],
        eligible_privileges: Vec::new(),
        metadata: serde_json::json!({
            "grant_types": ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"]
        }),
        expected_revision: 0,
    };
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &registration),
                &registration,
                now,
            )
        })
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
        issuer: f.store.issuer().unwrap(),
    };
    let scope = NativeCredentialScope {
        issuer: origin.clone(),
        instance_id: trust.instance_id.clone(),
        account: trust.account.clone(),
        client_id: registration.client_id,
        credential_handle: ulid::Ulid::new(),
    };
    let custody = f.directory.path().join("native-oauth");
    let mut authority = Arc::new(f.store);
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
    );
    let listener = OAuthListener::load(socket, &backend, &OAuthTransport::LoopbackDevelopment)
        .await
        .unwrap();
    let mut shutdown = tokio_util::sync::CancellationToken::new();
    let mut server = tokio::spawn(listener.run(
        super::super::oauth::router(engine, vec!["nats://127.0.0.1:4222".into()]),
        shutdown.clone(),
    ));

    let client = NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
        .await
        .unwrap();
    // Reuse the ordinary device approval flow for initial login and for recovery
    // after an irretrievably lost refresh response; no grant is injected locally.
    let approve = async |client: NativeOAuthClient| {
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
        let implicit = http
            .post(format!("{origin}/portal/approve"))
            .header("Origin", &origin)
            .header("Cookie", cookies.join("; "))
            .form(&[
                ("csrf", csrf.as_str()),
                ("transaction", transaction.as_str()),
                ("presentation", consent["presentationId"].as_str().unwrap()),
                ("decision", "approve"),
                ("confirm_device", flow.user_code()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(
            implicit.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({"error":"access_denied"}),
            "even trusted Device Code clients need explicit requested-capability approval"
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
                ("confirm_device", flow.user_code()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(approved.status(), reqwest::StatusCode::SEE_OTHER);
        tokio::time::timeout(Duration::from_secs(15), flow.complete())
            .await
            .unwrap()
            .unwrap()
    };
    let mut session = approve(client).await;
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
        let admitted = crate::platform::auth::phase_two_tests::admit_credential(
            &authority,
            &serde_json::to_vec(&session.admission).unwrap(),
            &nkey,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
        assert_eq!(admitted.authority.unsigned.principal_id, f.admin);
    }

    let durable_jkt = session.admission.credential.persistent_dpop_jkt.clone();
    // Restart the real HTTP engine and SQLite signer. Cover one cold link,
    // two unseen cold links, then a warm response signed after the owner's pin
    // snapshot: it must authenticate the successor without replacing runtime keys.
    for (successors, cold) in [
        (vec![76_u8], true),
        (vec![77_u8, 78_u8], true),
        (vec![79_u8], false),
    ] {
        let previous_runtime = session.runtime_id.clone();
        let previous_nkey = session.auth.session_nkey().unwrap();
        let owner_trust = session.trust.clone();
        let suspended = if cold {
            drop(session);
            None
        } else {
            Some(session)
        };
        shutdown.cancel();
        server.await.unwrap().unwrap();
        for _ in successors {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            crate::platform::auth::phase_two_tests::rotate_issuer(&authority, &f.admin, now)
                .unwrap();
            let settings = authority.settings.clone();
            authority = Arc::new(
                SqliteAuthorizationStore::from_connection(
                    Connection::open(f.directory.path().join("platform.db")).unwrap(),
                    authority.signer.clone(),
                    settings,
                    authority.custody_directory.clone(),
                )
                .unwrap(),
            );
        }
        // A missing discovery/chain endpoint is recoverable, and cannot spend
        // the old refresh token. Successful resume below proves it stayed usable.
        if cold {
            let unavailable =
                NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
                    .await
                    .unwrap();
            assert!(matches!(
                unavailable.resume().await,
                Err(NativeOAuthError::TrustUnavailable)
            ));
        }
        let socket = tokio::net::TcpListener::bind(backend_address)
            .await
            .unwrap();
        let engine = Arc::new(
            OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
        );
        let listener = OAuthListener::load(socket, &backend, &OAuthTransport::LoopbackDevelopment)
            .await
            .unwrap();
        shutdown = tokio_util::sync::CancellationToken::new();
        server = tokio::spawn(listener.run(
            super::super::oauth::router(engine, vec!["nats://127.0.0.1:4222".into()]),
            shutdown.clone(),
        ));
        session = match suspended {
            Some(mut warm) => {
                warm.refresh(owner_trust).await.unwrap();
                warm
            }
            None => {
                // Real discovery advertises the new key; neither an absent nor
                // cryptographically corrupted signed chain may trigger /token.
                for tampering in [ChainTampering::Missing, ChainTampering::Forged] {
                    let before = {
                        let mut control = proxy.control.lock().unwrap();
                        control.chain = tampering;
                        control.refresh_posts
                    };
                    let rejected =
                        NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
                            .await
                            .unwrap();
                    assert!(matches!(
                        rejected.resume().await,
                        Err(NativeOAuthError::TrustUnavailable)
                    ));
                    assert_eq!(proxy.control.lock().unwrap().refresh_posts, before);
                }
                proxy.control.lock().unwrap().chain = ChainTampering::None;
                NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
                    .await
                    .unwrap()
                    .resume()
                    .await
                    .unwrap()
            }
        };
        assert_eq!(session.trust.issuer, authority.issuer().unwrap());
        assert_eq!(session.runtime_id == previous_runtime, !cold);
        assert_eq!(session.auth.session_nkey().unwrap() == previous_nkey, !cold);
        assert_eq!(
            session.admission.credential.persistent_dpop_jkt,
            durable_jkt
        );
        assert_eq!(session.admission.credential.oauth_grant_id, grant);
        assert_eq!(session.admission.credential.refresh_family_id, family);
        let nkey = session.auth.session_nkey().unwrap();
        let admitted = crate::platform::auth::phase_two_tests::admit_credential(
            &authority,
            &serde_json::to_vec(&session.admission).unwrap(),
            &nkey,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
        assert_eq!(admitted.authority.unsigned.principal_id, f.admin);
    }
    // The SDK has already sent this actual refresh when the intermediary holds
    // it. Rotate the genuine SQLite signer before forwarding that same request.
    let previous_runtime = session.runtime_id.clone();
    let previous_nkey = session.auth.session_nkey().unwrap();
    let previous_admission = session.admission.clone();
    let owner_trust = session.trust.clone();
    let (arrived, held_request) = tokio::sync::oneshot::channel();
    {
        let mut control = proxy.control.lock().unwrap();
        control.pause_refresh = Some(arrived);
        control.hide_chain_after_success = true;
    }
    let refresh = tokio::spawn(async move {
        let outcome = session.refresh(owner_trust).await;
        (session, outcome)
    });
    let forward = tokio::time::timeout(Duration::from_secs(10), held_request)
        .await
        .expect("SDK must send the real refresh before rotation")
        .unwrap();
    shutdown.cancel();
    server.await.unwrap().unwrap();
    crate::platform::auth::phase_two_tests::rotate_issuer(
        &authority,
        &f.admin,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    )
    .unwrap();
    authority = Arc::new(
        SqliteAuthorizationStore::from_connection(
            Connection::open(f.directory.path().join("platform.db")).unwrap(),
            authority.signer.clone(),
            authority.settings.clone(),
            authority.custody_directory.clone(),
        )
        .unwrap(),
    );
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
    );
    let socket = tokio::net::TcpListener::bind(backend_address)
        .await
        .unwrap();
    let listener = OAuthListener::load(socket, &backend, &OAuthTransport::LoopbackDevelopment)
        .await
        .unwrap();
    shutdown = tokio_util::sync::CancellationToken::new();
    server = tokio::spawn(listener.run(
        super::super::oauth::router(engine, vec!["nats://127.0.0.1:4222".into()]),
        shutdown.clone(),
    ));
    forward.send(()).unwrap();
    let (unverified, outcome) = tokio::time::timeout(Duration::from_secs(15), refresh)
        .await
        .expect("real refresh and unavailable chain must finish")
        .unwrap();
    assert!(matches!(outcome, Err(NativeOAuthError::TrustUnavailable)));
    assert_eq!(unverified.runtime_id, previous_runtime);
    assert_eq!(unverified.auth.session_nkey().unwrap(), previous_nkey);
    assert_eq!(unverified.admission, previous_admission);
    assert_ne!(unverified.trust.issuer, authority.issuer().unwrap());
    let (spent, successor) = proxy
        .control
        .lock()
        .unwrap()
        .successes
        .last()
        .unwrap()
        .clone();
    assert_ne!(spent, successor);
    drop(unverified);
    proxy.control.lock().unwrap().chain = ChainTampering::None;
    session = NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
        .await
        .unwrap()
        .resume()
        .await
        .unwrap();
    let next_spent = proxy
        .control
        .lock()
        .unwrap()
        .successes
        .last()
        .unwrap()
        .0
        .clone();
    assert_eq!(
        next_spent, successor,
        "restart must spend only the checkpointed successor"
    );
    assert_ne!(
        next_spent, spent,
        "the submitted predecessor must never be retried"
    );
    assert_eq!(session.admission.credential.oauth_grant_id, grant);
    assert_eq!(session.admission.credential.refresh_family_id, family);
    assert_eq!(
        session.admission.credential.persistent_dpop_jkt,
        durable_jkt
    );
    assert_eq!(session.trust.issuer, authority.issuer().unwrap());
    assert_ne!(session.runtime_id, previous_runtime);
    assert_ne!(session.auth.session_nkey().unwrap(), previous_nkey);
    // Consume a real token at the backend, then reset the response stream before
    // the SDK can learn its replacement. Restoring the transport must not cause
    // either this session or a reopened public client to retry that predecessor.
    let successes_before_loss = {
        let mut control = proxy.control.lock().unwrap();
        control.lose_success_response = true;
        control.successes.len()
    };
    assert!(matches!(
        session.refresh(session.trust.clone()).await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    assert_eq!(
        proxy.control.lock().unwrap().successes.len(),
        successes_before_loss + 1
    );
    let posts_after_loss = proxy.control.lock().unwrap().refresh_posts;
    assert!(matches!(
        session.refresh(session.trust.clone()).await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    assert_eq!(
        proxy.control.lock().unwrap().refresh_posts,
        posts_after_loss
    );
    drop(session);
    let reopened = NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
        .await
        .unwrap();
    assert!(matches!(
        reopened.resume().await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    assert_eq!(
        proxy.control.lock().unwrap().refresh_posts,
        posts_after_loss
    );
    let recovering = NativeOAuthClient::open(custody.clone(), scope.clone(), trust.clone())
        .await
        .unwrap();
    session = approve(recovering).await;
    assert_eq!(session.admission.credential.principal_id, f.admin);
    assert_eq!(
        session.admission.credential.persistent_dpop_jkt,
        durable_jkt
    );
    let grant = session.admission.credential.oauth_grant_id.clone();
    let current_trust = session.trust.clone();

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
    crate::platform::auth::phase_two_tests::revoke_root(
        &authority,
        &mutation(&f.admin, &revocation),
        &revocation,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    )
    .unwrap();
    assert!(matches!(
        session.refresh(current_trust.clone()).await,
        Err(NativeOAuthError::RecoveryRequired)
    ));
    assert!(matches!(
        session.refresh(current_trust).await,
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
    proxy_shutdown.cancel();
    proxy_server.await.unwrap();
}
