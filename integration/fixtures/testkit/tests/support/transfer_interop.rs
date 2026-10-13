//! Real Rust/TypeScript upload persistence and authenticated download.

use super::{administrator, wire};
use runtime_trellis::apps::runtime_trellis_caller::{Client, Request};
use runtime_trellis::participants::runtime_trellis_echo_provider::Participant;
use serde_json::json;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use trellis_rs::generated::{AppRequestDescriptor, Client as GeneratedClient};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_upload_to_typescript_reads_persisted_bytes() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<runtime_trellis::participants::runtime_trellis_provider::Participant>(
            "transfer-provider",
        )
        .await
        .unwrap();
    let node = tokio::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .await
        .unwrap();
    assert!(node.status.success());
    let mut provider = tokio::process::Command::new(String::from_utf8(node.stdout).unwrap().trim())
        .args([
            "--experimental-transform-types",
            "ts/packages/trellis/native/transfer_interop_provider.ts",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
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
                json!({"trellisUrl":runtime.oauth_origin(),"seed":identity.seed()})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if serde_json::from_str::<serde_json::Value>(&line)
                .is_ok_and(|value| value["stage"] == "ready")
            {
                return;
            }
        }
        panic!("provider exited before readiness");
    })
    .await
    .unwrap();
    let logs = tokio::spawn(async move {
        while let Some(line) = output.next_line().await.unwrap() {
            eprintln!("{line}");
        }
    });
    let admin = administrator(&runtime).await;
    let password = ulid::Ulid::new().to_string();
    let user = admin.users_create(&wire(json!({"username":format!("transfer-{}",ulid::Ulid::new()),"password":password,"displayName":"Transfer caller","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap().user;
    for capability in Request::REQUIRED_CAPABILITIES {
        admin.capability_grants_grant(&wire(json!({"principalId":user.principal.principal_id,"capabilityId":capability,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    }
    let client_id = ulid::Ulid::new().to_string();
    admin.o_auth_clients_put(&wire(json!({"clientId":client_id,"title":"Transfer caller","description":"Cross-language transfer","kind":"native","redirectUris":[],"impliedConsent":[],"development":true,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    let options = super::login(&runtime, &client_id, json!([{"type":"trellis_capabilities","required":Request::REQUIRED_CAPABILITIES,"optional":[]}]), &user.username, &password, None).await;
    let base = GeneratedClient::connect_user(options).await.unwrap();
    let caller = Client::from_generated(base.clone()).runtime_trellis_runtime_v1();
    let bytes = (0..5 * 1024 * 1024 + 7)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let value = wire(json!({"value":"persisted"}));
    let operation = caller.upload().start(&value).await.unwrap();
    let stored = operation.upload(&bytes).await.unwrap();
    assert_eq!(stored.size, bytes.len() as u64);
    assert_eq!(
        operation.wait().await.unwrap().output.unwrap().value,
        "persisted"
    );
    let download = caller.download(&value).await.unwrap();
    let grant = trellis_rs::client::download_transfer_grant_from_value(
        serde_json::to_value(download.transfer.unwrap()).unwrap(),
    )
    .unwrap();
    let mut tampered = grant.clone();
    let other_options = super::login(&runtime, &client_id, json!([{"type":"trellis_capabilities","required":Request::REQUIRED_CAPABILITIES,"optional":[]}]), &user.username, &password, None).await;
    let other = GeneratedClient::connect_user(other_options).await.unwrap();
    assert!(other.download_transfer(&grant).await.is_err());
    tampered.info.digest = "SHA-256=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into();
    assert!(base.download_transfer(&tampered).await.is_err());
    let download = caller.download(&value).await.unwrap();
    let grant = trellis_rs::client::download_transfer_grant_from_value(
        serde_json::to_value(download.transfer.unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(base.download_transfer(&grant).await.unwrap(), bytes);
    input.write_all(b"stop\n").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(15), provider.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    logs.await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typescript_upload_cancellation_and_live_renewal_reduction_against_rust() {
    use runtime_trellis::participants::runtime_trellis_echo_provider::Provider;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::AsyncReadExt;
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("rust-transfer")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&executions);
    Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_upload(move |_, input, operation| {
            let counted = Arc::clone(&counted);
            async move {
                if operation.cancellation().reason().is_some() {
                    assert_eq!(input.value, "cancelled");
                    assert!(operation.upload().await.is_err());
                    return Ok(());
                }
                let committed = operation
                    .upload()
                    .await?
                    .expect("durably committed transfer");
                let mut reader = operation
                    .open_staged_upload()
                    .await?
                    .expect("persisted staged object");
                let mut actual = Vec::new();
                reader
                    .read_to_end(&mut actual)
                    .await
                    .map_err(|error| trellis_rs::service::ServerError::Nats(error.to_string()))?;
                assert_eq!(actual.len(), 5 * 1024 * 1024 + 7);
                assert_eq!(actual.len() as u64, committed.size);
                assert!(actual
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| *byte == (index % 251) as u8));
                counted.fetch_add(1, Ordering::SeqCst);
                operation.complete(input).await?;
                Ok(())
            }
        });
    let counted = Arc::clone(&executions);
    Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_echo(move |_, mut input| {
            input.value = counted.load(Ordering::SeqCst).to_string();
            async move { Ok(input) }
        });
    Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_watch(|_, _| {
            futures_util::stream::unfold(0u64, |index| async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Some((Ok(wire(json!({"value":index.to_string()}))), index + 1))
            })
        });
    let serving = tokio::spawn(service.run());
    let admin = administrator(&runtime).await;
    let password = ulid::Ulid::new().to_string();
    let user = admin.users_create(&wire(json!({"username":format!("transfer-{}",ulid::Ulid::new()),"password":password,"displayName":"Transfer caller","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap().user;
    for capability in Request::REQUIRED_CAPABILITIES {
        admin.capability_grants_grant(&wire(json!({"principalId":user.principal.principal_id,"capabilityId":capability,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    }
    let client_id = ulid::Ulid::new().to_string();
    admin.o_auth_clients_put(&wire(json!({"clientId":client_id,"title":"Transfer caller","description":"Cross-language transfer","kind":"native","redirectUris":[],"impliedConsent":[],"development":true,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    let node = tokio::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .await
        .unwrap();
    assert!(node.status.success());
    let mut caller = tokio::process::Command::new(String::from_utf8(node.stdout).unwrap().trim())
        .args([
            "--experimental-transform-types",
            "ts/packages/trellis/native/transfer_interop_caller.ts",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = caller.stdin.take().unwrap();
    input.write_all(format!("{}\n", json!({"issuer":runtime.oauth_origin(),"username":user.username,"password":password,"clientId":client_id,"deploymentId":identity.deployment_id()})).as_bytes()).await.unwrap();
    let mut output = BufReader::new(caller.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = output.next_line().await.unwrap() {
            eprintln!("transfer caller: {line}");
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else { continue; };
            if event["stage"] == "revoke" {
                let grants = admin.capability_grants_list(&wire(json!({"principalId":user.principal.principal_id}))).await.unwrap();
                let grant = grants.items.iter().find(|grant| grant.capability.capability_id.0 == "runtime-trellis.runtime@v1::transport").unwrap();
                admin.capability_grants_revoke(&wire(json!({"grantId":grant.grant_id,"expectedRevision":grant.revision,"idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
            }
            if event["stage"] == "complete" { return; }
        }
        panic!("caller exited before verification completed");
    }).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(15), caller.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    serving.abort();
    let _ = serving.await;
    runtime.shutdown().await.unwrap();
}
