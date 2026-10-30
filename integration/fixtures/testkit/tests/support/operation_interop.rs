//! A generated Rust caller observes and controls a real TypeScript provider.

use super::{administrator, wire};
use futures_util::StreamExt;
use runtime_trellis::apis::runtime_trellis_optional_availability_v2::operations::ExtendContinueSignal;
use runtime_trellis::apis::runtime_trellis_optional_availability_v2::rpc::ReadInput;
use runtime_trellis::apps::runtime_trellis_native_optional_extend::{Client, Request};
use runtime_trellis::participants::runtime_trellis_native_optional_provider::Participant;
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use trellis_rs::client::OperationEvent;
use trellis_rs::generated::AppRequestDescriptor;
use trellis_rs::generated::Client as GeneratedClient;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_observes_v2_operation_updates_from_an_implementation_only_typescript_provider() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("typescript")
        .await
        .unwrap();
    let mut provider = tokio::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "ts/packages/trellis/native/operation_interop_provider.ts",
        ])
        .current_dir("../../..")
        .env("NODE_EXTRA_CA_CERTS", runtime.trust_roots()[0].clone())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = provider.stdin.take().unwrap();
    input
        .write_all(
            format!(
                "{}\n",
                json!({"trellisUrl":runtime.trellis_url(), "seed":identity.seed()})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = output.next_line().await.unwrap() {
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if event["stage"] == "ready" {
                return;
            }
        }
        panic!("TypeScript provider exited before readiness");
    })
    .await
    .unwrap();

    let provider_logs = tokio::spawn(async move {
        while let Some(line) = output.next_line().await.unwrap() {
            eprintln!("TypeScript provider: {line}");
        }
    });
    let admin = administrator(&runtime).await;
    let me = admin.sessions_me(&wire(json!({}))).await.unwrap();
    for capability in Request::REQUIRED_CAPABILITIES
        .iter()
        .chain(Request::OPTIONAL_CAPABILITIES)
    {
        admin
            .capability_grants_grant(&wire(json!({
                "principalId":me.principal.principal_id,
                "capabilityId":capability,
                "expectedRevision":"0",
                "idempotencyKey":ulid::Ulid::new().to_string()
            })))
            .await
            .unwrap();
    }
    let identity = runtime.register_client::<Request>("rust").await.unwrap();
    let base = GeneratedClient::connect_user(identity.connect_options())
        .await
        .unwrap();
    let caller = Client::from_generated(base.clone()).runtime_trellis_optional_availability_v2();
    let invocation = caller.extend();
    let accepted = invocation
        .start(&ReadInput {
            value: "Rust caller input".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let persisted = serde_json::to_vec(accepted.reference()).unwrap();
    let operation = invocation
        .control(serde_json::from_slice(&persisted).unwrap())
        .unwrap();
    drop(accepted);
    let mut watch = operation.live_with_updates().await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        watch.next().await.unwrap().unwrap();
        operation
            .signal::<ExtendContinueSignal>(&ReadInput {
                value: "emit".into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
        loop {
            if let OperationEvent::Update { update } = watch.next().await.unwrap().unwrap() {
                assert_eq!(update.update.value, "TypeScript provider update");
                break;
            }
        }
        operation
            .signal::<ExtendContinueSignal>(&ReadInput {
                value: "finish".into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
        let terminal = operation.wait().await.unwrap();
        assert_eq!(
            terminal.state,
            trellis_rs::client::OperationState::Completed,
            "{terminal:?}"
        );
        assert_eq!(terminal.output.unwrap().value, "Rust caller input");
    })
    .await
    .expect("signed cross-language update and owner-directed completion");
    watch.close().await.unwrap();
    drop(operation);
    drop(caller);
    drop(base);
    input.write_all(b"stop\n").await.unwrap();
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), provider.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    provider_logs.await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typescript_observes_v2_operation_updates_from_an_implementation_only_rust_provider() {
    use runtime_trellis::participants::runtime_trellis_native_optional_provider::Provider;
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("rust-provider")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let mut provider = Provider::new(&mut service);
    provider
        .runtime_trellis_optional_availability_v2()
        .register_read(|_, input| async move { Ok(input) });
    provider
        .runtime_trellis_optional_availability_v2()
        .register_extend(|_, input, operation| async move {
            let mut signals = operation.signals().await?;
            let emit = signals.next().await.expect("first Continue signal")?;
            operation
                .emit_update(ReadInput {
                    value: "Rust provider update".into(),
                    extra: Default::default(),
                })
                .await?;
            operation.acknowledge_signal(emit.signal_sequence).await?;
            let finish = signals.next().await.expect("second Continue signal")?;
            operation.acknowledge_signal(finish.signal_sequence).await?;
            operation.complete(input).await?;
            Ok(())
        });
    drop(provider);
    let running = tokio::spawn(service.run());
    let admin = administrator(&runtime).await;
    let me = admin.sessions_me(&wire(json!({}))).await.unwrap();
    for capability in Request::REQUIRED_CAPABILITIES
        .iter()
        .chain(Request::OPTIONAL_CAPABILITIES)
    {
        admin.capability_grants_grant(&wire(json!({
            "principalId":me.principal.principal_id,
            "capabilityId":capability,
            "expectedRevision":"0",
            "idempotencyKey":ulid::Ulid::new().to_string()
        }))).await.unwrap();
    }
    let client_id = ulid::Ulid::new().to_string();
    admin.o_auth_clients_put(&wire(json!({
        "clientId":client_id,
        "title":"Node interoperability caller",
        "description":"v2 Operation updates",
        "kind":"native",
        "redirectUris": [],
        "impliedConsent": [],
        "development":true,
        "expectedRevision":"0",
        "idempotencyKey":ulid::Ulid::new().to_string()
    }))).await.unwrap();
    let mut caller = tokio::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "ts/packages/trellis/native/operation_interop_caller.ts",
        ])
        .current_dir("../../..")
        .env("NODE_EXTRA_CA_CERTS", runtime.trust_roots()[0].clone())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = caller.stdin.take().unwrap();
    input.write_all(format!("{}\n", json!({
        "issuer":runtime.trellis_url(),
        "username":runtime.admin_username(),
        "password":runtime.admin_password(),
        "clientId":client_id
    })).as_bytes()).await.unwrap();
    drop(input);
    let result = tokio::time::timeout(Duration::from_secs(60), caller.wait_with_output())
        .await
        .expect("cross-language signed watch, update and completion")
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let complete = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|value| value["stage"] == "complete")
        });
    assert!(
        complete,
        "caller must verify the actual forwarded Rust update and terminal output"
    );
    // Stop the application-owned serve task before stopping its infrastructure.
    running.abort();
    let stopped = running.await;
    assert!(stopped.is_err_and(|error| error.is_cancelled()));
    runtime.shutdown().await.unwrap();
}
