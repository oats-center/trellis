use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::task::AbortOnDropHandle;
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::{TrellisClientError, UserConnectOptions};

async fn start_runtime(
    config: &crate::RuntimeConfig,
    origin: &str,
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
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_cold_rotation_resumes_custody_and_calls_signed_sessions_me() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cli = std::env::var_os("TRELLIS_TEST_CLI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target/release/trellis"));
    let reserved: Vec<_> = (0..5)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<_> = reserved
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect();
    let origin = format!("http://127.0.0.1:{}", ports[0]);
    let oauth_origin = format!("http://127.0.0.1:{}", ports[1]);
    let config_dir = root.join("config");
    let generated = tokio::process::Command::new(cli)
        .args(["init", "config", "--out"])
        .arg(&config_dir)
        .args([
            "--trellis-port",
            &ports[0].to_string(),
            "--nats-port",
            &ports[2].to_string(),
            "--nats-monitor-port",
            &ports[3].to_string(),
            "--nats-ws-port",
            &ports[4].to_string(),
            "--public-origin",
            &origin,
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
    let (mut config, _) = crate::RuntimeConfig::load_from_path_with_defaults(
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
    let oauth = config.auth.as_mut().unwrap().oauth.as_mut().unwrap();
    oauth.origin = oauth_origin.clone();
    oauth.port = ports[1];
    std::fs::write(
        config_dir.join("config.toml"),
        toml::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    crate::platform::seed_admin_credentials(&config, "admin", "a strong bootstrap password")
        .await
        .unwrap();
    drop(reserved);
    std::fs::create_dir_all(root.join("runtime")).unwrap();
    let nats_root = root.to_owned();
    let nats_config = config_dir.join("nats");
    let mut broker = tokio::task::spawn_blocking(move || {
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
                nats: ports[2],
                monitor: ports[3],
                websocket: ports[4],
            })
            .start()
            .unwrap()
    })
    .await
    .unwrap();
    let (mut stop, mut runtime) = start_runtime(&config, &origin).await;
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
        {"type":"trellis_capabilities","required":["trellis.auth@v1::accountRead"],"optional":[]}
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
                    ("approved", "trellis.auth@v1::accountRead"),
                    ("confirm_device", user_code.as_str()),
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
        {"type":"trellis_capabilities","required":["trellis.auth@v1::accountRead"],"optional":[]}
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
        let store =
            SqliteAuthorizationStore::open(&storage, &config, &identity.audience_nats_account)
                .unwrap();
        let rotation = crate::platform::auth::tests::rotate_issuer(
            &store,
            &identity.principal_id,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
        .unwrap();
        let expected = rotation
            .verify_successor(
                &pinned.issuer,
                &pinned.instance_id,
                &pinned.account,
                pinned.rotation_sequence,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
            )
            .unwrap();
        drop(store);
        (stop, runtime) = start_runtime(&config, &origin).await;
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
