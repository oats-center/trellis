//! Provisioned identity and independently consented user authority have separate lifecycles.

use serde_json::json;
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::{DeviceConnectOptions, UserConnectOptions};
use trellis_rs::generated::{AppRequestDescriptor, Client, ParticipantDescriptor};
use trellis_runtime_apis::{
    apis::trellis_auth_v1::Client as AuthClient, apps::trellis_cli::Request,
};
use trellis_test_fixture::participants::trellis_test_fixture_unsupported_device::Participant as Device;
use trellis_testkit::TrellisTestRuntime;

async fn oauth(
    runtime: &mut TrellisTestRuntime,
    admin: bool,
) -> (UserConnectOptions<'static>, String) {
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let native = NativeOAuthClient::open(
        runtime
            .workdir()
            .join(format!("device-oauth-{}", ulid::Ulid::new())),
        NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust,
    )
    .await
    .unwrap();
    let mut optional = Request::OPTIONAL_CAPABILITIES.to_vec();
    if !optional.contains(&"trellis.auth@v1::deviceCompanionsManage") {
        optional.push("trellis.auth@v1::deviceCompanionsManage");
    }
    let mut details = json!([{"type":"trellis_capabilities","required":[],"optional":optional}]);
    if admin {
        details
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"trellis_platform_privileges","required":["principals.manage","roles.manage"]}));
    }
    let flow = native.start_device(&details).await.unwrap();
    runtime
        .approve_device_login("trellis.cli", flow.user_code())
        .await
        .unwrap();
    let session = flow.complete().await.unwrap();
    let login = session.admission.credential.login_session_id.clone();
    (
        UserConnectOptions::new(server.nats_servers, 10_000, session),
        login,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typescript_device_companion_uses_independent_native_oauth() {
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use trellis_test_fixture::participants::{
        trellis_test_fixture_companion_device::Participant as Required,
        trellis_test_fixture_provider::Participant as EchoProvider,
    };
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let denied = reqwest::Client::new()
        .post(format!("{}/auth/device/enroll", runtime.oauth_origin()))
        .json(&json!({"proof":"private-enrollment-sentinel"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        denied.json::<serde_json::Value>().await.unwrap(),
        json!({"error":{"code":"device_enrollment_denied"}})
    );
    let identity = runtime
        .register_service::<EchoProvider>("ts-companion-echo")
        .await
        .unwrap();
    let mut echo = EchoProvider::connect(identity.connect_options())
        .await
        .unwrap();
    trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Provider::new(&mut echo)
        .register_echo(|_, input| async move { Ok(input) });
    let echo_task = tokio::spawn(async move { echo.run().await });
    let (controller, _) = oauth(&mut runtime, true).await;
    let admin = AuthClient::from_generated(Client::connect_user(controller).await.unwrap());
    let password = ulid::Ulid::new().to_string();
    let user = admin.users_create(&serde_json::from_value(json!({"username":format!("companion-{}",ulid::Ulid::new()),"password":password,"displayName":"Independent companion user","idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().user;
    for capability in trellis_test_fixture::apps::trellis_test_fixture_companion_device_companion::Request::REQUIRED_CAPABILITIES {
        admin.capability_grants_grant(&serde_json::from_value(json!({"principalId":user.principal_id,"capabilityId":capability,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
    }
    let (unprivileged, _) = oauth(&mut runtime, false).await;
    let unprivileged =
        AuthClient::from_generated(Client::connect_user(unprivileged).await.unwrap());
    let deployment = admin.deployments_create(&serde_json::from_value(json!({"title":"TypeScript device","kind":"device","participantId":Required::ID,"packageEvidence":Required::package_evidence(),"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().deployment;
    let provisioned = admin.devices_provision(&serde_json::from_value(json!({"deploymentId":deployment.deployment_id,"title":"TypeScript parent","idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
    let mut node = tokio::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "ts/packages/trellis/native/device_interop.ts",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = node.stdin.take().unwrap();
    input.write_all(format!("{}\n",json!({"issuer":runtime.oauth_origin(),"provisioningSecret":provisioned.provisioning_secret,"username":user.account.as_ref().unwrap().username,"password":password})).as_bytes()).await.unwrap();
    let mut output = BufReader::new(node.stdout.take().unwrap()).lines();
    let mut verified = false;
    while let Some(line) = tokio::time::timeout(Duration::from_secs(45), output.next_line())
        .await
        .expect("TypeScript wrapper progress")
        .unwrap()
    {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            eprintln!("{line}");
            continue;
        };
        match message["stage"].as_str() {
            Some("review") => {
                let review = admin.devices_reviews_get(&serde_json::from_value(json!({"reviewId":message["reviewId"],"confirmationCode":message["confirmationCode"]})).unwrap()).await.unwrap().review;
                let approved = admin.devices_reviews_decide(&serde_json::from_value(json!({"reviewId":review.review_id,"confirmationCode":message["confirmationCode"],"expectedRevision":review.revision,"decision":"approved","idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().review;
                input.write_all(format!("{}\n",json!({"clientId":approved.companion_client_id,"revision":approved.revision})).as_bytes()).await.unwrap();
            }
            Some("revoke") => {
                let revoke = serde_json::from_value(json!({"instanceId":message["instanceId"],"expectedRevision":message["revision"],"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap();
                assert!(
                    unprivileged
                        .device_companions_revoke(&revoke)
                        .await
                        .is_err(),
                    "administrator principal bypassed finite session delegation"
                );
                admin.device_companions_revoke(&revoke).await.unwrap();
                input.write_all(b"{}\n").await.unwrap();
            }
            Some("verified") => {
                verified = true;
            }
            _ => eprintln!("{line}"),
        }
    }
    assert!(
        node.wait().await.unwrap().success(),
        "TypeScript device acceptance failed"
    );
    assert!(verified, "TypeScript did not verify wrapper behavior");
    echo_task.abort();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retiring_companion_declarations_fences_new_consent_not_existing_grants() {
    use std::{collections::BTreeMap, path::PathBuf};
    use trellis_idl::{
        project::{GenerateConfig, PackageManifest, PackageMetadata},
        CanonicalMode, SourceUnit,
    };
    use trellis_test_fixture::apps::trellis_test_fixture_companion_device_companion::Request as Child;
    use trellis_test_fixture::participants::trellis_test_fixture_companion_device::Participant as Required;
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    runtime.register_service::<trellis_test_fixture::participants::trellis_test_fixture_provider::Participant>("retirement-catalog").await.unwrap();
    let (controller, _) = oauth(&mut runtime, true).await;
    let admin = AuthClient::from_generated(Client::connect_user(controller).await.unwrap());
    let user = admin
        .sessions_me(&serde_json::from_value(json!({})).unwrap())
        .await
        .unwrap();
    admin.capability_grants_grant(&serde_json::from_value(json!({"principalId":user.principal.principal_id,"capabilityId":"trellis-test-fixture.echo@v1::invoke","expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
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
            source: "device CompanionDevice {}".into(),
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
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let details = json!([{"type":"trellis_capabilities","required":Child::REQUIRED_CAPABILITIES,"optional":[]}]);
    for remove in [false, true] {
        let deployment = admin.deployments_create(&serde_json::from_value(json!({"title":"Retiring companion","kind":"device","participantId":Required::ID,"packageEvidence":Required::package_evidence(),"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().deployment;
        let client_id = format!("trellis.device.{}.companion", deployment.deployment_id);
        let scope = NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: client_id.clone(),
            credential_handle: ulid::Ulid::new(),
        };
        let path = runtime
            .workdir()
            .join(format!("retirement-{}", ulid::Ulid::new()));
        let existing = NativeOAuthClient::open(path.clone(), scope.clone(), server.trust.clone())
            .await
            .unwrap();
        let flow = existing.start_device(&details).await.unwrap();
        runtime
            .approve_device_login(&client_id, flow.user_code())
            .await
            .unwrap();
        let mut existing = flow.complete().await.unwrap();
        let pending = NativeOAuthClient::open(
            runtime
                .workdir()
                .join(format!("pending-{}", ulid::Ulid::new())),
            NativeCredentialScope {
                credential_handle: ulid::Ulid::new(),
                ..scope.clone()
            },
            server.trust.clone(),
        )
        .await
        .unwrap()
        .start_device(&details)
        .await
        .unwrap();
        let current = if remove {
            let disabled = admin.deployments_disable(&serde_json::from_value(json!({"deploymentId":deployment.deployment_id,"expectedRevision":deployment.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().deployment;
            existing.refresh(server.trust.clone()).await.unwrap();
            admin.deployments_remove(&serde_json::from_value(json!({"deploymentId":deployment.deployment_id,"expectedRevision":disabled.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().deployment
        } else {
            let apply = serde_json::from_value(json!({"deploymentId":deployment.deployment_id,"expectedRevision":deployment.revision,"packageEvidence":omitted,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap();
            let applied = admin.deployments_apply(&apply).await.unwrap().deployment;
            assert_eq!(
                admin
                    .deployments_apply(&apply)
                    .await
                    .unwrap()
                    .deployment
                    .revision,
                applied.revision
            );
            assert!(admin.deployments_apply(&serde_json::from_value(json!({"deploymentId":deployment.deployment_id,"expectedRevision":deployment.revision,"packageEvidence":omitted,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.is_err());
            applied
        };
        let new = NativeOAuthClient::open(
            runtime.workdir().join(format!("new-{}", ulid::Ulid::new())),
            NativeCredentialScope {
                credential_handle: ulid::Ulid::new(),
                ..scope.clone()
            },
            server.trust.clone(),
        )
        .await
        .unwrap();
        assert!(
            new.start_device(&details).await.is_err(),
            "retired declaration admitted a new OAuth flow"
        );
        assert!(
            runtime
                .approve_device_login(&client_id, pending.user_code())
                .await
                .is_err(),
            "retired declaration approved a pending OAuth flow"
        );
        existing.refresh(server.trust.clone()).await.unwrap();
        if !remove {
            admin.deployments_apply(&serde_json::from_value(json!({"deploymentId":deployment.deployment_id,"expectedRevision":current.revision,"packageEvidence":Required::package_evidence(),"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
            let new = NativeOAuthClient::open(
                runtime
                    .workdir()
                    .join(format!("restored-{}", ulid::Ulid::new())),
                NativeCredentialScope {
                    credential_handle: ulid::Ulid::new(),
                    ..scope
                },
                server.trust.clone(),
            )
            .await
            .unwrap();
            assert!(
                new.start_device(&details).await.is_ok(),
                "accepted declaration did not restore explicit OAuth eligibility"
            );
        }
    }
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_companions_require_their_own_consent_and_revoke_only_their_grant() {
    use trellis_test_fixture::participants::{
        trellis_test_fixture_companion_device::Participant as Required,
        trellis_test_fixture_optional_companion_device::Participant as Optional,
    };
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    use trellis_test_fixture::participants::trellis_test_fixture_provider::Participant as EchoProvider;
    let echo_identity = runtime
        .register_service::<EchoProvider>("companion-echo")
        .await
        .unwrap();
    let mut echo = EchoProvider::connect(echo_identity.connect_options())
        .await
        .unwrap();
    trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Provider::new(&mut echo)
        .register_echo(|_, input| async move { Ok(input) });
    let echo_task = tokio::spawn(async move { echo.run().await });
    let (controller, _) = oauth(&mut runtime, true).await;
    let admin = AuthClient::from_generated(Client::connect_user(controller).await.unwrap());
    let user = admin
        .sessions_me(&serde_json::from_value(json!({})).unwrap())
        .await
        .unwrap();
    admin.capability_grants_grant(&serde_json::from_value(json!({"principalId":user.principal.principal_id,"capabilityId":"trellis-test-fixture.echo@v1::invoke","expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
    let deployment = Box::pin(companion_lifecycle::<
        Required,
        trellis_test_fixture::apps::trellis_test_fixture_companion_device_companion::Request,
    >(&mut runtime, &admin, true, None))
    .await;
    Box::pin(companion_lifecycle::<
        Required,
        trellis_test_fixture::apps::trellis_test_fixture_companion_device_companion::Request,
    >(&mut runtime, &admin, true, Some(deployment)))
    .await;
    Box::pin(companion_lifecycle::<Optional,trellis_test_fixture::apps::trellis_test_fixture_optional_companion_device_companion::Request>(&mut runtime, &admin, false, None)).await;
    echo_task.abort();
    runtime.shutdown().await.unwrap();
}

async fn companion_lifecycle<P: ParticipantDescriptor, C: AppRequestDescriptor>(
    runtime: &mut TrellisTestRuntime,
    admin: &AuthClient,
    required: bool,
    deployment: Option<String>,
) -> String {
    use trellis_rs::auth::{
        check_device_activation, DeviceActivationOptions, DeviceActivationStatus,
    };
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let deployment = match deployment { Some(id) => id, None => admin.deployments_create(&serde_json::from_value(json!({"title":"Companion enrollment","kind":"device","participantId":P::ID,"packageEvidence":P::package_evidence(),"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().deployment.deployment_id.to_string() };
    let provisioned = admin.devices_provision(&serde_json::from_value(json!({"deploymentId":deployment,"title":"Enrolled parent","idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
    let secret = provisioned.provisioning_secret.unwrap();
    let (seed, _) = trellis_rs::auth::generate_session_keypair();
    let (activation_key, _) = trellis_rs::auth::generate_session_keypair();
    let options = DeviceActivationOptions::new(
        DeviceConnectOptions::<P>::new(
            runtime.oauth_origin(),
            &seed,
            server.nats_servers.clone(),
            server.trust.clone(),
        ),
        &activation_key,
    )
    .with_provisioning_secret(&secret);
    let DeviceActivationStatus::Pending(pending) = check_device_activation(&options).await.unwrap()
    else {
        panic!("unreviewed enrollment became ready")
    };
    let DeviceActivationStatus::Pending(retry) = check_device_activation(&options).await.unwrap()
    else {
        panic!("retry changed unreviewed status")
    };
    assert_eq!(
        retry.review_id, pending.review_id,
        "proof-bound retry must resume the existing review"
    );
    let (other_seed, _) = trellis_rs::auth::generate_session_keypair();
    let other = DeviceActivationOptions::new(
        DeviceConnectOptions::<P>::new(
            runtime.oauth_origin(),
            &other_seed,
            server.nats_servers.clone(),
            server.trust.clone(),
        ),
        &activation_key,
    )
    .with_provisioning_secret(&secret);
    assert!(
        check_device_activation(&other).await.is_err(),
        "one-use secret admitted a second key"
    );
    assert!(
        Client::connect_device(
            DeviceConnectOptions::<P>::new(
                runtime.oauth_origin(),
                &seed,
                server.nats_servers.clone(),
                server.trust.clone()
            )
            .with_timeout_ms(2000)
        )
        .await
        .is_err(),
        "device connected before review"
    );
    let review = admin
        .devices_reviews_get(
            &serde_json::from_value(
                json!({"reviewId":pending.review_id,"confirmationCode":pending.confirmation_code}),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .review;
    assert!(admin.devices_reviews_decide(&serde_json::from_value(json!({"reviewId":pending.review_id,"confirmationCode":"00000000","decision":"approved","expectedRevision":review.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.is_err());
    let review = admin.devices_reviews_decide(&serde_json::from_value(json!({"reviewId":pending.review_id,"confirmationCode":pending.confirmation_code,"decision":"approved","expectedRevision":review.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap().review;
    assert!(matches!(
        check_device_activation(&options).await.unwrap(),
        DeviceActivationStatus::Ready(_)
    ));
    let without_user = Client::connect_device(DeviceConnectOptions::<P>::new(
        runtime.oauth_origin(),
        &seed,
        server.nats_servers.clone(),
        server.trust.clone(),
    ))
    .await;
    assert_eq!(without_user.is_err(), required);
    drop(without_user);
    let attachment = json!({"reviewId":pending.review_id,"confirmationCode":pending.confirmation_code,"expectedRevision":review.revision,"idempotencyKey":ulid::Ulid::new().to_string()});
    assert!(
        admin
            .device_companions_attach(&serde_json::from_value(attachment.clone()).unwrap())
            .await
            .is_err(),
        "an unrelated administrator OAuth grant substituted for companion consent"
    );
    let client_id = review.companion_client_id.unwrap().to_string();
    let path = runtime
        .workdir()
        .join(format!("companion-{}", ulid::Ulid::new()));
    let scope = NativeCredentialScope {
        issuer: server.issuer.clone(),
        instance_id: server.trust.instance_id.clone(),
        account: server.trust.account.clone(),
        client_id: client_id.clone(),
        credential_handle: ulid::Ulid::new(),
    };
    let native = NativeOAuthClient::open(path.clone(), scope.clone(), server.trust.clone())
        .await
        .unwrap();
    assert!(native.start_device(&json!([{"type":"trellis_capabilities","required":["trellis.auth@v1::usersResolve"],"optional":[]}])).await.is_err(),"deployment client accepted a different application request");
    let native = NativeOAuthClient::open(path.clone(), scope.clone(), server.trust.clone())
        .await
        .unwrap();
    let flow = native
        .start_device(
            &json!([{"type":"trellis_capabilities","required":C::REQUIRED_CAPABILITIES,"optional":C::OPTIONAL_CAPABILITIES}]),
        )
        .await
        .unwrap();
    runtime
        .approve_device_login(&client_id, flow.user_code())
        .await
        .unwrap();
    let session = flow.complete().await.unwrap();
    let grant = session.admission.credential.oauth_grant_id.clone();
    let association = {
        let user = Client::connect_user(UserConnectOptions::new(
            server.nats_servers.clone(),
            10000,
            session,
        ))
        .await
        .unwrap();
        let auth = AuthClient::from_generated(user);
        auth.device_companions_attach(&serde_json::from_value(attachment).unwrap())
            .await
            .unwrap()
    };
    assert_eq!(association.oauth_grant_id.to_string(), grant);
    let session = NativeOAuthClient::open(path.clone(), scope.clone(), server.trust.clone())
        .await
        .unwrap()
        .resume()
        .await
        .unwrap();
    let parent = Client::connect_device(
        DeviceConnectOptions::<P>::new(
            runtime.oauth_origin(),
            &seed,
            server.nats_servers.clone(),
            server.trust.clone(),
        )
        .with_companion(UserConnectOptions::new(
            server.nats_servers.clone(),
            10000,
            session,
        )),
    )
    .await
    .unwrap();
    let child = parent.companion().expect("assigned OAuth companion");
    child.refresh_authorization_context().await.unwrap();
    if required {
        let device = trellis_test_fixture::participants::trellis_test_fixture_companion_device::Client::from_generated(parent.clone());
        let user = trellis_test_fixture::apps::trellis_test_fixture_companion_device_companion::Client::from_generated(child.clone());
        let device_state = device.saved().unwrap();
        assert!(
            device_state.get().await.unwrap().is_none(),
            "new device inherited another device's State in the same deployment"
        );
        let parent_as_app = trellis_test_fixture::apps::trellis_test_fixture_companion_device_companion::Client::from_generated(parent.clone());
        assert!(
            parent_as_app
                .saved("0", ulid::Ulid::new().to_string().parse().unwrap())
                .await
                .is_err(),
            "device authority registered a user State declaration"
        );
        let (user_state, _) = user
            .saved("0", ulid::Ulid::new().to_string().parse().unwrap())
            .await
            .unwrap();
        device_state
            .set(&trellis_test_fixture::types::Value {
                value: "device-owned".into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
        user_state
            .set(&trellis_test_fixture::types::Value {
                value: "user-owned".into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(
            device_state.get().await.unwrap().unwrap().value.value,
            "device-owned"
        );
        assert_eq!(
            user_state.get().await.unwrap().unwrap().value.value,
            "user-owned"
        );
    }
    let echo = trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Client::from_generated(
        child.clone(),
    );
    assert_eq!(
        echo.echo(&trellis_test_fixture::types::Value {
            value: "independent companion".into(),
            extra: Default::default()
        })
        .await
        .unwrap()
        .value,
        "independent companion"
    );
    let instances = admin
        .devices_list(&serde_json::from_value(json!({"deploymentId":deployment})).unwrap())
        .await
        .unwrap();
    let instance = instances
        .items
        .iter()
        .find(|value| value.instance_id == provisioned.instance.instance_id)
        .unwrap();
    let disabled = admin.devices_disable(&serde_json::from_value(json!({"instanceId":instance.instance_id,"expectedRevision":instance.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
    assert!(parent.refresh_authorization_context().await.is_err());
    child.refresh_authorization_context().await.unwrap();
    admin.devices_enable(&serde_json::from_value(json!({"instanceId":instance.instance_id,"expectedRevision":disabled.instance.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
    let auth = AuthClient::from_generated(child.clone());
    auth.device_companions_revoke(&serde_json::from_value(json!({"instanceId":instance.instance_id,"expectedRevision":association.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap()).await.unwrap();
    assert!(child.refresh_authorization_context().await.is_err());
    assert!(
        NativeOAuthClient::open(path, scope, server.trust.clone())
            .await
            .unwrap()
            .resume()
            .await
            .is_err(),
        "revoked companion refresh family minted another session"
    );
    admin
        .sessions_me(&serde_json::from_value(json!({})).unwrap())
        .await
        .unwrap();
    let reconnect = Client::connect_device(DeviceConnectOptions::<P>::new(
        runtime.oauth_origin(),
        &seed,
        server.nats_servers.clone(),
        server.trust.clone(),
    ))
    .await;
    assert_eq!(reconnect.is_err(), required);
    deployment
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provisioned_device_and_oauth_user_revoke_independently() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let (controller, _) = oauth(&mut runtime, true).await;
    let admin = AuthClient::from_generated(Client::connect_user(controller).await.unwrap());
    let deployment = admin.deployments_create(&serde_json::from_value(json!({
        "title":"Independent device", "kind":"device", "participantId":Device::ID,
        "packageEvidence":Device::package_evidence(), "idempotencyKey":ulid::Ulid::new().to_string()
    })).unwrap()).await.unwrap().deployment;
    let (seed, public_key) = trellis_rs::auth::generate_session_keypair();
    let instance = admin
        .devices_provision(
            &serde_json::from_value(json!({
                "deploymentId":deployment.deployment_id, "title":"Independent device",
                "identityPublicKey":public_key,"idempotencyKey":ulid::Ulid::new().to_string()
            }))
            .unwrap(),
        )
        .await
        .unwrap()
        .instance;
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let device = Client::connect_device(DeviceConnectOptions::<Device>::new(
        runtime.oauth_origin(),
        &seed,
        server.nats_servers.clone(),
        server.trust.clone(),
    ))
    .await
    .unwrap();
    device.refresh_authorization_context().await.unwrap();

    let (user, login_id) = oauth(&mut runtime, false).await;
    let user = Client::connect_user(user).await.unwrap();
    let user_auth = AuthClient::from_generated(user.clone());
    user_auth
        .users_resolve(&serde_json::from_value(json!({})).unwrap())
        .await
        .unwrap();
    use trellis_test_fixture::participants::trellis_test_fixture_kv_provider::{
        Participant as KvProvider, Provider,
    };
    let resource_identity = runtime
        .register_service::<KvProvider>("private-device-resources")
        .await
        .unwrap();
    let mut provider = KvProvider::connect(resource_identity.connect_options())
        .await
        .unwrap();
    let provider_client = Provider::new(&mut provider).client();
    let records = provider_client.records().await.unwrap();
    let value = trellis_test_fixture::types::Value {
        value: "service-owned".into(),
        extra: Default::default(),
    };
    records.put("private", &value).await.unwrap();
    let objects = provider_client.objects().await.unwrap();
    objects
        .write_from("private", &b"service-owned"[..], Some(13))
        .await
        .unwrap();
    // Exercise the broker ACL too: bypassing the generated resource facade must
    // not grant a user direct access to the service's actual physical bucket.
    let (raw_user, _) = oauth(&mut runtime, false).await;
    let raw_user = std::sync::Arc::new(
        trellis_rs::client::TrellisClient::connect_user(raw_user)
            .await
            .unwrap(),
    );
    let raw_owner = trellis_rs::service::LiveProviderOwner::from_connected_client(raw_user);
    let raw = async_nats::jetstream::new(raw_owner.runtime_nats());
    assert!(raw.get_key_value(&records.binding().bucket).await.is_err());
    assert!(raw.get_object_store(&objects.binding().name).await.is_err());
    let store_write = async {
        raw.publish(
            format!("$O.{}.C.foreign", objects.binding().name),
            "stolen".into(),
        )
        .await?
        .await
    };
    assert!(store_write.await.is_err());
    assert_eq!(
        objects.read("private").await.unwrap().unwrap().as_ref(),
        b"service-owned"
    );
    let subject = format!("$KV.{}.private", records.binding().bucket);
    let write = async {
        raw.publish(
            subject,
            serde_json::to_vec(&json!({"value":"stolen"}))
                .unwrap()
                .into(),
        )
        .await?
        .await
    };
    assert!(write.await.is_err());
    use trellis_test_fixture::participants::trellis_test_fixture_jobs_provider::{
        Participant as JobsProvider, Provider as JobsProviderRuntime,
    };
    let jobs_identity = runtime
        .register_service::<JobsProvider>("device-private-jobs")
        .await
        .unwrap();
    let mut jobs = JobsProvider::connect(jobs_identity.connect_options())
        .await
        .unwrap();
    let job = JobsProviderRuntime::new(&mut jobs)
        .submit_work(value.clone())
        .await
        .unwrap();
    let history = trellis_runtime_apis::apis::trellis_jobs_v1::Client::from_generated(
        jobs.generated_client(),
    )
    .history(
        &serde_json::from_value(
            json!({"queue":"work","jobId":job.identity().id,"fromSequence":"1"}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let sequence = history
        .entries
        .first()
        .expect("service's actual retained job event")
        .sequence
        .to_string()
        .parse::<u64>()
        .unwrap();
    for endpoint in ["$JS.API.DIRECT.GET.JOBS", "$JS.API.STREAM.MSG.GET.JOBS"] {
        let attempt = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            raw_owner
                .runtime_nats()
                .request(endpoint, json!({"seq":sequence}).to_string().into()),
        )
        .await;
        assert!(
            !matches!(attempt, Ok(Ok(_))),
            "user read service job data through {endpoint}"
        );
    }
    // A real OAuth caller may invoke Auth, but cannot use administrative resource
    // discovery as a deputy for another principal's raw resource bindings.
    assert!(user_auth
        .resources_list(&serde_json::from_value(json!({})).unwrap())
        .await
        .is_err());
    assert!(user
        .kv_handle::<serde_json::Value>(
            "records",
            trellis_rs::client::ResourceCodec::new(1).unwrap()
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        records.get("private").await.unwrap().unwrap().value,
        "service-owned"
    );
    admin
        .sessions_revoke(
            &serde_json::from_value(json!({
                "loginSessionId":login_id,"idempotencyKey":ulid::Ulid::new().to_string()
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(user.refresh_authorization_context().await.is_err());
    device.refresh_authorization_context().await.unwrap();

    let (second_user, _) = oauth(&mut runtime, false).await;
    let second_user = Client::connect_user(second_user).await.unwrap();
    let second_auth = AuthClient::from_generated(second_user.clone());
    admin.devices_disable(&serde_json::from_value(json!({
        "instanceId":instance.instance_id,"expectedRevision":instance.revision,"idempotencyKey":ulid::Ulid::new().to_string()
    })).unwrap()).await.unwrap();
    assert!(device.refresh_authorization_context().await.is_err());
    second_user.refresh_authorization_context().await.unwrap();
    second_auth
        .users_resolve(&serde_json::from_value(json!({})).unwrap())
        .await
        .unwrap();
    assert!(Client::connect_device(
        DeviceConnectOptions::<Device>::new(
            runtime.oauth_origin(),
            &seed,
            server.nats_servers,
            server.trust
        )
        .with_timeout_ms(2_000)
    )
    .await
    .is_err());
    runtime.shutdown().await.unwrap();
}
