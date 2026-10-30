use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::task::AbortOnDropHandle;
use trellis_rs::__internal::RuntimeClient as _;
use trellis_rs::auth::{
    discover_accepting_certificate, discover_native_oauth, CertificateAcceptance,
    CertificateAcceptanceError, NativeCredentialScope, NativeOAuthClient,
};
use trellis_rs::client::{TrellisClientError, UserConnectOptions};

async fn start_runtime(
    config: &crate::RuntimeConfig,
    origin: &str,
    roots: &[PathBuf],
) -> (
    crate::shutdown::StopHandle,
    AbortOnDropHandle<Result<(), crate::RuntimeError>>,
) {
    let stop = crate::shutdown::StopHandle::new();
    let mut runtime = AbortOnDropHandle::new(tokio::spawn(crate::run_with_stop(
        crate::RuntimeOptions {
            mode: crate::RuntimeMode::All,
            config: config.clone(),
            reset_admin: false,
            nats_override: None,
        },
        Some(stop.clone()),
    )));
    let mut http = reqwest::Client::builder().no_proxy();
    for root in roots {
        http = http.add_root_certificate(
            reqwest::Certificate::from_pem(&std::fs::read(root).unwrap()).unwrap(),
        );
    }
    let http = http.build().unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            tokio::select! {
                result = &mut runtime => panic!("production runtime exited during startup: {result:?}"),
                response = http.get(format!("{origin}/readyz")).send() => {
                    if response.is_ok_and(|response| response.status().is_success()) {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("production readiness deadline");
    (stop, runtime)
}

/// Starts the installation's NATS broker the way a managed server would.
async fn start_broker(
    root: &std::path::Path,
    config_dir: &std::path::Path,
    [nats, monitor, websocket]: [u16; 3],
) -> trellis_local_nats::LocalNats {
    std::fs::create_dir_all(root.join("runtime")).unwrap();
    let nats_root = root.to_owned();
    let nats_config = config_dir.join("nats");
    tokio::task::spawn_blocking(move || {
        let source = std::env::var_os("TRELLIS_NATS_SERVER_BIN")
            .map(|path| trellis_local_nats::NatsBinarySource::Path(path.into()))
            .unwrap_or(trellis_local_nats::NatsBinarySource::PathLookup);
        trellis_local_nats::LocalNats::builder()
            .binary(source)
            .source(nats_config)
            .state(nats_root.join("state/nats"))
            .pid_file(nats_root.join("runtime/nats-server.pid"))
            .output(trellis_local_nats::NatsOutput::Log {
                path: nats_root.join("logs/nats.log"),
                mirror: false,
            })
            .ports(trellis_local_nats::LocalNatsPorts {
                nats,
                monitor,
                websocket,
            })
            .start()
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_cold_rotation_resumes_custody_and_calls_signed_sessions_me() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let server = std::env::var_os("TRELLIS_TEST_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target/release/trellis-server"));
    // Held until the listeners are up; see ReservedPort.
    let reserved: Vec<_> = (0..5)
        .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
        .collect();
    let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
    let origin = format!("http://127.0.0.1:{}", ports[0]);
    let oauth_origin = format!("http://127.0.0.1:{}", ports[1]);
    let config_dir = root.join("config");
    let generated = tokio::process::Command::new(&server)
        .args(["init", "--out"])
        .arg(&config_dir)
        .arg("--external-nats")
        .args([
            "--console-port",
            &ports[0].to_string(),
            "--nats-port",
            &ports[2].to_string(),
            "--nats-monitor-port",
            &ports[3].to_string(),
            "--nats-ws-port",
            &ports[4].to_string(),
            "--console-origin",
            &origin,
            "--trellis-origin",
            &oauth_origin,
            "--trellis-port",
            &ports[1].to_string(),
            "--tls",
            "loopback-development",
            "--bind-address",
            "127.0.0.1",
        ])
        .output()
        .await
        .expect("provide the producer-built CLI through TRELLIS_TEST_CLI_BIN");
    assert!(
        generated.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let (config, _) = crate::RuntimeConfig::load_from_path_with_defaults(
        config_dir.join("config.toml"),
        crate::RuntimePathDefaults {
            data: root.join("data"),
            state: root.join("state"),
            cache: root.join("cache"),
            runtime: root.join("runtime"),
            logs: root.join("logs"),
        },
    )
    .unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        toml::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    crate::platform::seed_admin_credentials(&config, "admin", "a strong bootstrap password")
        .await
        .unwrap();
    let mut broker = start_broker(root, &config_dir, [ports[2], ports[3], ports[4]]).await;
    let (mut stop, mut runtime) = start_runtime(&config, &origin, &[]).await;
    let discovered = discover_native_oauth(&oauth_origin, &[])
        .await
        .unwrap_or_else(|error| panic!("native discovery at {oauth_origin} failed: {error:?}"));
    let initial_trust = discovered.trust.clone();
    let scope = NativeCredentialScope {
        issuer: discovered.issuer,
        instance_id: initial_trust.instance_id.clone(),
        account: initial_trust.account.clone(),
        client_id: "trellis.cli".into(),
        credential_handle: ulid::Ulid::new(),
    };
    let custody = root.join("native-custody");
    let native = NativeOAuthClient::open(custody.clone(), scope.clone(), initial_trust.clone())
        .await
        .unwrap();
    let device = native
        .start_device(&serde_json::json!([
        {"type":"trellis_capabilities","required":["trellis.auth.v1:account.read"],"optional":[]}
        ]))
        .await
        .unwrap();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let cookies = |response: &reqwest::Response| {
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .map(|header| {
                header
                    .to_str()
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let response = http
        .post(format!("{oauth_origin}/portal/session"))
        .header("Origin", &oauth_origin)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let cookie = cookies(&response);
    let csrf = cookie
        .iter()
        .find_map(|cookie| cookie.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap()
        .to_owned();
    let response = http
        .post(format!("{oauth_origin}/portal/login"))
        .header("Origin", &oauth_origin)
        .header("Cookie", cookie.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("username", "admin"),
            ("password", "a strong bootstrap password"),
            ("request_id", &ulid::Ulid::new().to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let cookie = cookies(&response).join("; ");
    let approve_device = |user_code: String| {
        let http = &http;
        let oauth_origin = &oauth_origin;
        let csrf = &csrf;
        let cookie = &cookie;
        async move {
            let response = http
                .post(format!("{oauth_origin}/portal/device"))
                .header("Origin", oauth_origin.as_str())
                .header("Cookie", cookie.as_str())
                .form(&[("csrf", csrf.as_str()), ("user_code", user_code.as_str())])
                .send()
                .await
                .unwrap();
            assert!(response.status().is_redirection());
            let location = url::Url::parse(oauth_origin)
                .unwrap()
                .join(response.headers()["location"].to_str().unwrap())
                .unwrap();
            let transaction = location
                .query_pairs()
                .find(|(key, _)| key == "transaction")
                .unwrap()
                .1
                .into_owned();
            let response = http
                .get(format!("{oauth_origin}/portal/authorization/{transaction}"))
                .header("Cookie", cookie.as_str())
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            let view: serde_json::Value = response.json().await.unwrap();
            let response = http
                .post(format!("{oauth_origin}/portal/approve"))
                .header("Origin", oauth_origin.as_str())
                .header("Cookie", cookie.as_str())
                .form(&[
                    ("csrf", csrf.as_str()),
                    ("transaction", transaction.as_str()),
                    ("presentation", view["presentationId"].as_str().unwrap()),
                    ("decision", "approve"),
                    ("approved", "trellis.auth.v1:account.read"),
                ])
                .send()
                .await
                .unwrap();
            assert!(response.status().is_redirection());
        }
    };
    approve_device(device.user_code().to_owned()).await;
    let mut session = device.complete().await.unwrap();
    let identity = session.admission.credential.clone();
    let warm_native = NativeOAuthClient::open(
        root.join("warm-custody"),
        scope.clone(),
        initial_trust.clone(),
    )
    .await
    .unwrap();
    let warm_device = warm_native.start_device(&serde_json::json!([
        {"type":"trellis_capabilities","required":["trellis.auth.v1:account.read"],"optional":[]}
    ])).await.unwrap();
    approve_device(warm_device.user_code().to_owned()).await;
    let warm = trellis_rs::client::TrellisClient::connect_user(UserConnectOptions::new(
        discovered.nats_servers.clone(),
        30_000,
        warm_device.complete().await.unwrap(),
    ))
    .await
    .unwrap();
    // Close the actual initial socket. A successful signed call must then use
    // the manager's replacement generation, without replacing this client.
    let baseline = warm.runtime_nats();
    baseline.drain().await.unwrap();
    let warm_input = serde_json::json!({});
    let warm_me = || {
        warm.request_json_value(
            trellis_runtime_apis::apis::trellis_auth_v1::rpc::SessionsMe::API_ID,
            trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Rpc,
                name: "Sessions.Me".into(),
                direction: trellis_protocol::PermissionAction::Call,
            },
            trellis_runtime_apis::apis::trellis_auth_v1::rpc::SessionsMe::SUBJECT,
            &warm_input,
        )
    };
    let warm_initial = tokio::time::timeout(Duration::from_secs(30), async {
        let mut readiness = tokio::time::interval(Duration::from_millis(100));
        loop {
            readiness.tick().await;
            if baseline.flush().await.is_ok() {
                continue;
            }
            match warm_me().await {
                Ok(me) => break me,
                Err(TrellisClientError::AuthorizationUnavailable(_)) => {}
                // The baseline socket was deliberately drained. An in-flight
                // request may lose its subscription before a replacement
                // generation is published; only that exact transient closure
                // is eligible for another readiness probe.
                Err(TrellisClientError::NatsRequest(message))
                    if message.contains("subscribe failed: channel closed") => {}
                Err(error) => panic!("replacement socket signed readiness: {error:?}"),
            }
        }
    })
    .await
    .expect("replacement socket and authentic mirror coverage");
    assert!(baseline.flush().await.is_err());
    let storage = crate::storage::SqliteStore::new(
        crate::SubsystemName::Platform,
        match config.platform_storage_backend().unwrap() {
            crate::StorageBackend::Sqlite(storage) => storage,
        },
    );
    for step in 0..3 {
        assert_eq!(
            session.admission.credential.principal_id,
            identity.principal_id
        );
        assert_eq!(
            session.admission.credential.oauth_grant_id,
            identity.oauth_grant_id
        );
        assert_eq!(
            session.admission.credential.refresh_family_id,
            identity.refresh_family_id
        );
        assert_eq!(
            session.admission.credential.persistent_dpop_jkt,
            identity.persistent_dpop_jkt
        );
        let runtime_id = session.runtime_id.clone();
        let nkey = session.auth.session_nkey().unwrap();
        let pinned = session.trust.clone();
        let connected = trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            discovered.nats_servers.clone(),
            30_000,
            session,
        ))
        .await
        .unwrap();
        let auth =
            trellis_runtime_apis::apis::trellis_auth_v1::Client::from_generated(connected.clone());
        let input = trellis_runtime_apis::types::AuthEmpty {
            extra: Default::default(),
        };
        let me = tokio::select! {
            result = &mut runtime => panic!("production runtime exited before signed Sessions.Me at rotation step \
                {step}: {result:?}"),
            result = auth.sessions_me(&input) => result.unwrap_or_else(|error| panic!("signed Sessions.Me at rotation step {step}: {error:?}")),
        };
        assert_eq!(me.principal.principal_id.to_string(), identity.principal_id);
        let again = auth
            .sessions_me(&trellis_runtime_apis::types::AuthEmpty {
                extra: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(again.authorization_session_id, me.authorization_session_id);
        drop(auth);
        drop(connected);
        stop.stop();
        tokio::time::timeout(Duration::from_secs(30), &mut runtime)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if step == 2 {
            break;
        }
        // Lose the warm client's final physical attachment while Auth is down.
        // Its next renewal must authenticate rotation and admit a fresh socket,
        // not depend on any predecessor connection surviving the restart.
        let lost = warm.runtime_nats();
        // Runtime shutdown may already have retired this attachment.
        let _ = lost.drain().await;
        assert!(lost.flush().await.is_err());
        // Rotate offline through the store, as an operator tool would.
        let (offline_storage, offline_config, account, principal, offline_pin) = (
            storage.clone(),
            config.clone(),
            identity.audience_nats_account.clone(),
            identity.principal_id.clone(),
            pinned.clone(),
        );
        let expected = crate::platform::auth::blocking(move || {
            let store = SqliteAuthorizationStore::open(&offline_storage, &offline_config, &account)
                .unwrap();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let rotation =
                crate::platform::auth::tests::rotate_issuer(&store, &principal, now).unwrap();
            Ok::<_, AuthError>(
                rotation
                    .verify_successor(
                        &offline_pin.issuer,
                        &offline_pin.instance_id,
                        &offline_pin.account,
                        offline_pin.rotation_sequence,
                        now,
                    )
                    .unwrap(),
            )
        })
        .await
        .unwrap();
        (stop, runtime) = start_runtime(&config, &origin, &[]).await;
        warm.refresh_authorization_context().await.unwrap();
        let warm_renewed = tokio::time::timeout(Duration::from_secs(30), async {
            let mut readiness = tokio::time::interval(Duration::from_millis(100));
            loop {
                readiness.tick().await;
                if let Ok(me) = warm_me().await {
                    break me;
                }
            }
        })
        .await
        .expect("issuer rotation recovery without a surviving attachment");
        assert_eq!(warm_renewed["principal"], warm_initial["principal"]);
        session = NativeOAuthClient::open(custody.clone(), scope.clone(), pinned.clone())
            .await
            .unwrap()
            .resume()
            .await
            .unwrap();
        assert_eq!(session.trust.issuer, expected);
        assert_ne!(session.runtime_id, runtime_id);
        assert_ne!(session.auth.session_nkey().unwrap(), nkey);
    }
    tokio::task::spawn_blocking(move || broker.stop().unwrap())
        .await
        .unwrap();
}

/// A development installation serves sign-in on the Trellis port with its
/// generated certificate: a native client accepts it by fingerprint and
/// discovers it there, the Portal signs the administrator in and approves a
/// device login through it, that login connects and calls Trellis, and a
/// WebSocket upgrade on the same origin reaches NATS.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn development_mode_signs_in_and_relays_nats_on_the_trellis_port() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let server = std::env::var_os("TRELLIS_TEST_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target/release/trellis-server"));
    let reserved: Vec<_> = (0..5)
        .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
        .collect();
    let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
    let config_dir = root.join("config");
    let generated = tokio::process::Command::new(&server)
        .args(["init", "--out"])
        .arg(&config_dir)
        .args(["--external-nats", "--dev"])
        .args(["--trellis-port", &ports[0].to_string()])
        .args(["--console-port", &ports[4].to_string()])
        .args(["--nats-port", &ports[1].to_string()])
        .args(["--nats-monitor-port", &ports[2].to_string()])
        .args(["--nats-ws-port", &ports[3].to_string()])
        .output()
        .await
        .unwrap();
    assert!(
        generated.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let (config, _) = crate::RuntimeConfig::load_from_path_with_defaults(
        config_dir.join("config.toml"),
        crate::RuntimePathDefaults {
            data: root.join("data"),
            state: root.join("state"),
            cache: root.join("cache"),
            runtime: root.join("runtime"),
            logs: root.join("logs"),
        },
    )
    .unwrap();
    crate::platform::seed_admin_credentials(&config, "admin", "a strong bootstrap password")
        .await
        .unwrap();
    let mut broker = start_broker(root, &config_dir, [ports[1], ports[2], ports[3]]).await;
    let ca = config_dir.join("tls/ca.crt");
    let origin = format!("https://localhost:{}", ports[0]);
    let issuer = origin.clone();
    let (stop, mut runtime) = start_runtime(&config, &origin, std::slice::from_ref(&ca)).await;

    // No CA file is installed: the client accepts the development
    // certificate by its issuer's fingerprint, as a browser user would.
    let ca_fingerprint = {
        use rustls::pki_types::pem::PemObject as _;
        use sha2::Digest as _;
        let der = rustls::pki_types::CertificateDer::from_pem_file(&ca).unwrap();
        sha2::Sha256::digest(der.as_ref())
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    };
    let acceptance = CertificateAcceptance {
        directory: root.join("accepted"),
        fingerprint: None,
        interactive: false,
    };
    match discover_accepting_certificate(&issuer, &acceptance).await {
        Err(CertificateAcceptanceError::Untrusted { fingerprint, .. }) => {
            assert_eq!(fingerprint, ca_fingerprint);
        }
        other => panic!("an unvouched certificate was not refused: {other:?}"),
    }
    let wrong = CertificateAcceptance {
        fingerprint: Some("00".repeat(32)),
        ..acceptance.clone()
    };
    assert!(matches!(
        discover_accepting_certificate(&issuer, &wrong).await,
        Err(CertificateAcceptanceError::Mismatch { .. })
    ));
    let accepting = CertificateAcceptance {
        fingerprint: Some(ca_fingerprint.clone()),
        ..acceptance.clone()
    };
    discover_accepting_certificate(&issuer, &accepting)
        .await
        .unwrap_or_else(|error| panic!("accepting the presented certificate failed: {error}"));
    // Remembered: the next discovery needs no fingerprint.
    let discovered = discover_accepting_certificate(&issuer, &acceptance)
        .await
        .unwrap_or_else(|error| panic!("the remembered certificate was not used: {error}"));
    assert_eq!(discovered.issuer, issuer);
    let native = NativeOAuthClient::open(
        root.join("custody"),
        NativeCredentialScope {
            issuer: discovered.issuer.clone(),
            instance_id: discovered.trust.instance_id.clone(),
            account: discovered.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::new(),
        },
        discovered.trust.clone(),
    )
    .await
    .unwrap();
    let device = native
        .start_device(&serde_json::json!([
        {"type":"trellis_capabilities","required":["trellis.auth.v1:account.read"],"optional":[]}
        ]))
        .await
        .unwrap();

    // The Portal, as a browser drives it, on the Trellis origin.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(&ca).unwrap()).unwrap())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let cookies = |response: &reqwest::Response| {
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .map(|header| {
                header
                    .to_str()
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let response = http
        .post(format!("{issuer}/portal/session"))
        .header("Origin", &origin)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let session_cookies = cookies(&response);
    let csrf = session_cookies
        .iter()
        .find_map(|cookie| cookie.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap()
        .to_owned();
    let response = http
        .post(format!("{issuer}/portal/login"))
        .header("Origin", &origin)
        .header("Cookie", session_cookies.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("username", "admin"),
            ("password", "a strong bootstrap password"),
            ("request_id", &ulid::Ulid::new().to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let cookie = cookies(&response).join("; ");
    let response = http
        .post(format!("{issuer}/portal/device"))
        .header("Origin", &origin)
        .header("Cookie", &cookie)
        .form(&[("csrf", csrf.as_str()), ("user_code", device.user_code())])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_redirection(), "{}", response.status());
    let location = url::Url::parse(&issuer)
        .unwrap()
        .join(response.headers()["location"].to_str().unwrap())
        .unwrap();
    let transaction = location
        .query_pairs()
        .find(|(key, _)| key == "transaction")
        .unwrap()
        .1
        .into_owned();
    let view: serde_json::Value = http
        .get(format!("{issuer}/portal/authorization/{transaction}"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let response = http
        .post(format!("{issuer}/portal/approve"))
        .header("Origin", &origin)
        .header("Cookie", &cookie)
        .form(&[
            ("csrf", csrf.as_str()),
            ("transaction", transaction.as_str()),
            ("presentation", view["presentationId"].as_str().unwrap()),
            ("decision", "approve"),
            ("approved", "trellis.auth.v1:account.read"),
        ])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_redirection(), "{}", response.status());

    let session = device.complete().await.unwrap();
    let principal = session.admission.credential.principal_id.clone();
    let client = trellis_rs::client::TrellisClient::connect_user(UserConnectOptions::new(
        discovered.nats_servers.clone(),
        30_000,
        session,
    ))
    .await
    .unwrap();
    let me = client
        .request_json_value(
            trellis_runtime_apis::apis::trellis_auth_v1::rpc::SessionsMe::API_ID,
            trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Rpc,
                name: "Sessions.Me".into(),
                direction: trellis_protocol::PermissionAction::Call,
            },
            trellis_runtime_apis::apis::trellis_auth_v1::rpc::SessionsMe::SUBJECT,
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert!(me.to_string().contains(&principal), "{me}");
    drop(client);

    // A WebSocket upgrade on the Trellis origin, at its root like any path,
    // reaches the broker. Upgrades are an HTTP/1.1 mechanism.
    let upgraded = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(&ca).unwrap()).unwrap())
        .build()
        .unwrap()
        .get(format!("{origin}/"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(upgraded.status(), reqwest::StatusCode::SWITCHING_PROTOCOLS);

    // A server whose certificate no longer comes from the accepted issuer,
    // such as another installation's, is refused.
    let other = root.join("other");
    let generated = tokio::process::Command::new(&server)
        .args(["init", "--out"])
        .arg(&other)
        .args(["--external-nats", "--dev"])
        .output()
        .await
        .unwrap();
    assert!(generated.status.success());
    let remembered = std::fs::read_dir(root.join("accepted"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::copy(other.join("tls/ca.crt"), &remembered).unwrap();
    match discover_accepting_certificate(&issuer, &acceptance).await {
        Err(CertificateAcceptanceError::Changed { .. }) => {}
        other => panic!("a changed certificate was not refused: {other:?}"),
    }

    stop.stop();
    tokio::time::timeout(Duration::from_secs(30), &mut runtime)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::task::spawn_blocking(move || broker.stop().unwrap())
        .await
        .unwrap();
    drop(reserved);
}
