//! Generated native KV access must survive admission and reconnecting the same
//! provisioned identity without replacing its authenticated physical binding.

use serde::de::DeserializeOwned;
use serde_json::{json, Value as Json};
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::generated::{AppRequestDescriptor, Client as GeneratedClient};
use trellis_runtime_apis::{
    apis::trellis_auth_v1::Client as AuthClient, apps::trellis_cli::Request as CliRequest,
};
use trellis_test_fixture::participants::trellis_test_fixture_kv_provider::{Participant, Provider};
use trellis_test_fixture::types::Value;
use trellis_testkit::TrellisTestRuntime;

fn wire<T: DeserializeOwned>(value: Json) -> T {
    serde_json::from_value(value).unwrap()
}

// Exercise the real public portal's selective consent, while the native Rust
// SDK retains credential custody and performs its own DPoP token exchange.
async fn login(
    runtime: &TrellisTestRuntime,
    client_id: &str,
    details: Json,
    username: &str,
    password: &str,
    approved: Option<&[&str]>,
) -> UserConnectOptions<'static> {
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let oauth = NativeOAuthClient::open(
        runtime
            .workdir()
            .join(format!("availability-oauth-{}", ulid::Ulid::new())),
        NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: client_id.into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust,
    )
    .await
    .unwrap();
    let device = oauth.start_device(&details).await.unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let origin = runtime.oauth_origin();
    let cookies = |response: &reqwest::Response| {
        response
            .headers()
            .get_all("set-cookie")
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
    let response = http
        .post(format!("{origin}/portal/session"))
        .header("Origin", origin)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let initial = cookies(&response);
    let csrf = initial
        .iter()
        .find_map(|cookie| cookie.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap()
        .to_owned();
    let response = http
        .post(format!("{origin}/portal/login"))
        .header("Origin", origin)
        .header("Cookie", initial.join("; "))
        .form(&[
            ("csrf", csrf.as_str()),
            ("username", username),
            ("password", password),
            ("request_id", &ulid::Ulid::new().to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let cookie = cookies(&response).join("; ");
    let response = http
        .post(format!("{origin}/portal/device"))
        .header("Origin", origin)
        .header("Cookie", &cookie)
        .form(&[("csrf", csrf.as_str()), ("user_code", device.user_code())])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let location = reqwest::Url::parse(origin)
        .unwrap()
        .join(response.headers()["location"].to_str().unwrap())
        .unwrap();
    assert_eq!(location.origin().ascii_serialization(), origin);
    let transaction = location
        .query_pairs()
        .find(|(key, _)| key == "transaction")
        .unwrap()
        .1
        .into_owned();
    let view: Json = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["clientId"], client_id);
    let mut form = vec![
        ("csrf", csrf.as_str()),
        ("transaction", transaction.as_str()),
        ("presentation", view["presentationId"].as_str().unwrap()),
        ("decision", "approve"),
        ("confirm_device", device.user_code()),
    ];
    for (field, key, target) in [
        ("capabilities", "capabilityId", "approved"),
        ("platformPrivileges", "privilege", "privilege"),
    ] {
        for item in view[field].as_array().unwrap() {
            assert!(
                !(item["required"] == true && item["available"] != true),
                "required authority unavailable: {item}"
            );
            let identity = item[key].as_str().unwrap();
            if item["available"] == true
                && (field != "capabilities"
                    || approved.is_none_or(|selected| selected.contains(&identity)))
            {
                form.push((target, identity));
            }
        }
    }
    let response = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", origin)
        .header("Cookie", &cookie)
        .form(&form)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_redirection(),
        "consent returned {}",
        response.status()
    );
    let session = device.complete().await.unwrap();
    UserConnectOptions::new(server.nats_servers, 10_000, session)
}

async fn administrator(runtime: &TrellisTestRuntime) -> AuthClient {
    let options = login(runtime, "trellis.cli", json!([
        {"type":"trellis_capabilities", "required":[], "optional": CliRequest::OPTIONAL_CAPABILITIES},
        {"type":"trellis_platform_privileges", "required":["principals.manage","roles.manage","apis.accept","clients.manage"]}
    ]), runtime.admin_username(), runtime.admin_password(), None).await;
    AuthClient::from_generated(GeneratedClient::connect_user(options).await.unwrap())
}

// Disconnect the actual broker-side socket through NATS's production system
// API. Keeping the same generated client and resource handle exercises recovery,
// rather than constructing a replacement logical connection.
async fn broker_connections(runtime: &TrellisTestRuntime) -> Vec<u64> {
    let connections: Json = reqwest::get(format!("{}/connz", runtime.monitor_url()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|connection| connection["cid"].as_u64().unwrap())
        .collect()
}

async fn kick_connection(runtime: &TrellisTestRuntime, before: &[u64]) {
    let http = reqwest::Client::new();
    let connections: Json = http
        .get(format!("{}/connz", runtime.monitor_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let connection = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|connection| !before.contains(&connection["cid"].as_u64().unwrap()))
        .max_by_key(|connection| connection["cid"].as_u64().unwrap())
        .unwrap_or_else(|| {
            panic!("the connected fixture must have a broker socket: {connections}")
        });
    let cid = connection["cid"].as_u64().unwrap();
    let system = async_nats::ConnectOptions::new()
        .credentials_file(runtime.workdir().join("config/nats/creds/system.creds"))
        .await
        .unwrap()
        .connect(runtime.nats_url())
        .await
        .unwrap();
    let varz: Json = http
        .get(format!("{}/varz", runtime.monitor_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let response = system
        .request(
            format!(
                "$SYS.REQ.SERVER.{}.KICK",
                varz["server_id"].as_str().unwrap()
            ),
            serde_json::to_vec(&json!({"cid":cid})).unwrap().into(),
        )
        .await
        .unwrap();
    let response: Json = serde_json::from_slice(&response.payload).unwrap();
    assert!(
        response.get("error").is_none(),
        "broker kick failed: {response}"
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let current: Json = http
                .get(format!("{}/connz", runtime.monitor_url()))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if !current["connections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|connection| connection["cid"].as_u64() == Some(cid))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("broker must disconnect the selected live socket");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn optional_operation_approval_decline_reduction_and_authorized_expansion() {
    use runtime_trellis::apis::runtime_trellis_optional_availability_v1::rpc::ReadInput as OperationValue;
    use runtime_trellis::apps::runtime_trellis_native_optional_extend::{Client, Request};
    use runtime_trellis::participants::runtime_trellis_native_optional_provider as native_provider;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let admin = administrator(&runtime).await;
    let provider_identity = runtime
        .register_service::<native_provider::Participant>("optional")
        .await
        .unwrap();
    let mut service = native_provider::Participant::connect(provider_identity.connect_options())
        .await
        .unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let observed = executions.clone();
    let mut provider = native_provider::Provider::new(&mut service);
    provider
        .runtime_trellis_optional_availability_v1()
        .register_read(|_, input| async move { Ok(input) });
    provider
        .runtime_trellis_optional_availability_v1()
        .register_extend(move |_, input, operation| {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                operation.complete(input).await?;
                Ok(())
            }
        });
    drop(provider);
    let running = tokio::spawn(service.run());
    let client_id = format!("optional-{}", ulid::Ulid::new());
    admin.o_auth_clients_put(&wire(json!({"clientId":client_id,"title":"Optional fixture","description":"Selective native consent","kind":"native","redirectUris":[],"impliedConsent":[],"development":true,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    let password = ulid::Ulid::new().to_string();
    let user = admin.users_create(&wire(json!({"username":format!("optional-{}",ulid::Ulid::new()),"password":password,"displayName":"Optional fixture","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap().user;
    let principal = user.principal.principal_id.to_string();
    let read = Request::REQUIRED_CAPABILITIES[0];
    let extend = Request::OPTIONAL_CAPABILITIES[0];
    for capability in [read, extend] {
        admin.capability_grants_grant(&wire(json!({"principalId":principal,"capabilityId":capability,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    }
    let details = json!([{"type":"trellis_capabilities","required":Request::REQUIRED_CAPABILITIES,"optional":Request::OPTIONAL_CAPABILITIES}]);
    let before = broker_connections(&runtime).await;
    let base = GeneratedClient::connect_user(
        login(
            &runtime,
            &client_id,
            details.clone(),
            &user.username,
            &password,
            Some(&[read, extend]),
        )
        .await
        .with_name("r2-approved"),
    )
    .await
    .unwrap();
    let approved = Client::from_generated(base.clone()).runtime_trellis_optional_availability_v1();
    let retained = approved.extend();
    let input = OperationValue {
        value: "approved".into(),
        extra: Default::default(),
    };
    let operation = retained.start(&input).await.unwrap();
    assert_eq!(
        operation.wait().await.unwrap().output.unwrap().value,
        "approved"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    kick_connection(&runtime, &before).await;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            match approved.read(&input).await {
                Ok(output) => {
                    assert_eq!(output.value, "approved");
                    break;
                }
                Err(trellis_rs::client::CallError::AuthorizationUnavailable(_)) => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await
                }
                outcome => panic!("required read must recover on the same connection: {outcome:?}"),
            }
        }
    })
    .await
    .expect("the logical caller must recover after transport readiness");
    let reconnected = retained
        .start(&OperationValue {
            value: "reconnected".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        reconnected.wait().await.unwrap().output.unwrap().value,
        "reconnected"
    );
    let declined_base = GeneratedClient::connect_user(
        login(
            &runtime,
            &client_id,
            details,
            &user.username,
            &password,
            Some(&[read]),
        )
        .await,
    )
    .await
    .unwrap();
    let declined =
        Client::from_generated(declined_base.clone()).runtime_trellis_optional_availability_v1();
    assert_eq!(declined.read(&input).await.unwrap().value, "approved");
    assert!(declined.extend().start(&input).await.is_err());
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    let grants = admin
        .capability_grants_list(&wire(
            json!({"principalId":principal,"capabilityId":extend}),
        ))
        .await
        .unwrap();
    let grant = grants
        .items
        .iter()
        .find(|item| item.capability.capability_id.as_ref() == extend)
        .unwrap();
    admin.capability_grants_revoke(&wire(json!({"grantId":grant.grant_id,"expectedRevision":grant.revision,"idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let _ = base.refresh_authorization_context().await;
            if approved.read(&input).await.is_ok() && retained.start(&input).await.is_err() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    admin.capability_grants_grant(&wire(json!({"principalId":principal,"capabilityId":extend,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    base.refresh_authorization_context().await.unwrap();
    let expanded = retained
        .start(&OperationValue {
            value: "expanded".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        expanded.wait().await.unwrap().output.unwrap().value,
        "expanded"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 3);
    declined_base.refresh_authorization_context().await.unwrap();
    assert!(declined.extend().start(&input).await.is_err());
    assert_eq!(executions.load(Ordering::SeqCst), 3);
    running.abort();
    let _ = running.await;
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_kv_is_usable_after_readiness_and_reconnection_preserves_data() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime.register_service::<Participant>("kv").await.unwrap();
    let before = broker_connections(&runtime).await;
    let mut first = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let first_client = Provider::new(&mut first).client();
    let records = first_client.records().await.unwrap();
    records
        .put(
            "retained",
            &Value {
                value: "before reconnect".into(),
                extra: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        records.get("retained").await.unwrap().unwrap().value,
        "before reconnect"
    );
    kick_connection(&runtime, &before).await;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            match records.get("retained").await {
                Ok(Some(value)) => {
                    assert_eq!(value.value, "before reconnect");
                    break;
                }
                Err(trellis_rs::service::KvResourceReadError::Unavailable) => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await
                }
                outcome => panic!("retained binding must recover its original value: {outcome:?}"),
            }
        }
    })
    .await
    .expect("the same KV handle must recover after transport readiness");
    assert_eq!(
        records.get("retained").await.unwrap().unwrap().value,
        "before reconnect"
    );
    drop(first_client);
    drop(first);
    assert!(records.get("retained").await.is_err());

    let mut reconnected = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let client = Provider::new(&mut reconnected).client();
    let current = client.records().await.unwrap();
    assert_eq!(
        current.get("retained").await.unwrap().unwrap().value,
        "before reconnect"
    );
    current
        .put(
            "retained",
            &Value {
                value: "after reconnect".into(),
                extra: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        current.get("retained").await.unwrap().unwrap().value,
        "after reconnect"
    );
    let admin = administrator(&runtime).await;
    // Authored native IDL removes the declared KV; this is a real deployment
    // revision and explicit detached-allocation deletion, not a synthetic SDK
    // snapshot. Applying new software intentionally makes old instances stale.
    use std::{collections::BTreeMap, path::PathBuf};
    use trellis_idl::{
        project::{GenerateConfig, PackageManifest, PackageMetadata},
        CanonicalMode, SourceUnit,
    };
    use trellis_rs::generated::ParticipantDescriptor;
    let graph = trellis_idl::compile_project(
        &PackageManifest {
            package: PackageMetadata {
                name: "trellis-test-fixture".into(),
                version: "0.100.0".parse().unwrap(),
            },
            sources: BTreeMap::from([("main".into(), "main.trellis".into())]),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        },
        vec![SourceUnit {
            alias: "main".into(),
            path: PathBuf::from("main.trellis"),
            source: "service KvProvider {}".into(),
        }],
        BTreeMap::new(),
    )
    .unwrap();
    let omitted = trellis_idl::PackageEvidence {
        root_package: graph.root().to_string(),
        root_digest: graph.root_digest().into(),
        packages: vec![trellis_idl::PackageSourceEvidence {
            name: graph.root().to_string(),
            version: graph.root_package().version().clone(),
            digest: graph.root_digest().into(),
            source: trellis_idl::canonical_package(
                &graph,
                graph.root(),
                CanonicalMode::Presentation,
            )
            .unwrap(),
        }],
    };
    let deployment = admin
        .deployments_get(&wire(json!({"deploymentId":identity.deployment_id()})))
        .await
        .unwrap()
        .deployment;
    admin.deployments_apply(&wire(json!({"deploymentId":identity.deployment_id(),"expectedRevision":deployment.revision,"packageEvidence":omitted,"idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while current.get("retained").await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("detaching the authenticated binding must retire the old handle");
    let allocations = admin
        .resources_list(&wire(
            json!({"ownerKind":"deployment","ownerId":identity.deployment_id()}),
        ))
        .await
        .unwrap();
    let allocation = allocations
        .items
        .iter()
        .find(|resource| resource.local_name == "records")
        .unwrap();
    admin.resources_remove(&wire(json!({"resourceId":allocation.resource_id,"expectedRevision":allocation.revision,"idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    let deployment = admin
        .deployments_get(&wire(json!({"deploymentId":identity.deployment_id()})))
        .await
        .unwrap()
        .deployment;
    admin.deployments_apply(&wire(json!({"deploymentId":identity.deployment_id(),"expectedRevision":deployment.revision,"packageEvidence":Participant::package_evidence(),"idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    let (seed, public_key) = trellis_rs::auth::generate_session_keypair();
    admin.service_instances_provision(&wire(json!({"deploymentId":identity.deployment_id(),"identityPublicKey":public_key,"title":"replacement incarnation","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let mut replacement = Participant::connect(trellis_rs::service::ServiceConnectOptions::new(
        server.nats_servers,
        server.trust,
        &seed,
    ))
    .await
    .unwrap();
    let replacement_client = Provider::new(&mut replacement).client();
    let replacement_records = replacement_client.records().await.unwrap();
    assert!(replacement_records.get("retained").await.unwrap().is_none());
    replacement_records
        .put(
            "retained",
            &Value {
                value: "new incarnation".into(),
                extra: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        replacement_records
            .get("retained")
            .await
            .unwrap()
            .unwrap()
            .value,
        "new incarnation"
    );
    assert!(records.get("retained").await.is_err());
    assert!(current.get("retained").await.is_err());
    drop(replacement_records);
    drop(replacement_client);
    drop(replacement);
    drop(current);
    drop(records);
    drop(client);
    drop(reconnected);
    runtime.shutdown().await.unwrap();
}
