//! Operation execution/cancellation through normal admission and generated APIs.

use super::{administrator, wire};
use futures_util::StreamExt;
use runtime_trellis::apis::runtime_trellis_optional_availability_v2::rpc::ReadInput;
use runtime_trellis::apps::runtime_trellis_native_optional_extend::{Client, Request};
use runtime_trellis::participants::runtime_trellis_native_optional_provider::{
    Participant, Provider,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use trellis_rs::client::OperationState;
use trellis_rs::generated::{AppRequestDescriptor, Client as GeneratedClient};
use trellis_rs::service::OperationCancellationReason;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transient_previews_preserve_durable_progress_through_completion() {
    use trellis_rs::client::OperationEvent;
    use trellis_test_fixture::apis::trellis_test_fixture_echo_v1::operations::WorkContinueSignal;
    use trellis_test_fixture::apps::trellis_test_fixture_agent_caller::{Client, Request};
    use trellis_test_fixture::participants::trellis_test_fixture_provider::{
        Participant, Provider,
    };
    use trellis_test_fixture::types::{Preview, Status, Value};

    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("preview-provider")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let mut provider = Provider::new(&mut service);
    provider
        .trellis_test_fixture_echo_v1()
        .register_work(|_, input, operation| async move {
            operation
                .progress(Status {
                    stage: "transcribing".into(),
                    extra: Default::default(),
                })
                .await?;
            let mut signals = operation.signals().await?;
            let preview = signals.next().await.expect("preview signal")?;
            operation
                .emit_update(Preview {
                    text: "partial preview".into(),
                    extra: Default::default(),
                })
                .await?;
            operation
                .acknowledge_signal(preview.signal_sequence)
                .await?;
            let finish = signals.next().await.expect("completion signal")?;
            operation.acknowledge_signal(finish.signal_sequence).await?;
            operation.complete(input).await?;
            Ok(())
        });
    drop(provider);
    let serving = tokio::spawn(service.run());
    let admin = administrator(&runtime).await;
    let me = admin.sessions_me(&wire(json!({}))).await.unwrap();
    for capability in Request::REQUIRED_CAPABILITIES {
        admin.capability_grants_grant(&wire(json!({
            "principalId":me.principal.principal_id,
            "capabilityId":capability,
            "expectedRevision":"0",
            "idempotencyKey":ulid::Ulid::new().to_string()
        }))).await.unwrap();
    }
    let identity = runtime
        .register_client::<Request>("preview-caller")
        .await
        .unwrap();
    let base = GeneratedClient::connect_user(identity.connect_options())
        .await
        .unwrap();
    let api = Client::from_generated(base).trellis_test_fixture_echo_v1();
    let input = Value {
        value: "final output".into(),
        extra: Default::default(),
    };
    let operation = api.work().start(&input).await.unwrap();
    let mut events = operation.live_with_updates().await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        events.next().await.unwrap().unwrap();
        operation
            .signal::<WorkContinueSignal>(&input)
            .await
            .unwrap();
        loop {
            if let OperationEvent::Update { update } = events.next().await.unwrap().unwrap() {
                assert_eq!(update.update.text, "partial preview");
                break;
            }
        }
        let durable = operation.get().await.unwrap();
        assert_eq!(durable.progress.unwrap().stage, "transcribing");
        assert!(durable.output.is_none());
        operation
            .signal::<WorkContinueSignal>(&input)
            .await
            .unwrap();
        loop {
            match events.next().await.unwrap().unwrap() {
                OperationEvent::Completed { snapshot } => {
                    assert_eq!(snapshot.progress.unwrap().stage, "transcribing");
                    assert_eq!(snapshot.output.unwrap().value, "final output");
                    break;
                }
                OperationEvent::Failed { snapshot } | OperationEvent::Cancelled { snapshot } => {
                    panic!("unexpected terminal: {snapshot:?}")
                }
                _ => {}
            }
        }
        assert!(
            events.next().await.is_none(),
            "terminal observation ends normally"
        );
    })
    .await
    .unwrap();
    let restored = api
        .work()
        .control(
            serde_json::from_slice(&serde_json::to_vec(operation.reference()).unwrap()).unwrap(),
        )
        .unwrap();
    let durable = restored.get().await.unwrap();
    assert_eq!(durable.progress.unwrap().stage, "transcribing");
    assert_eq!(durable.output.unwrap().value, "final output");
    let mut terminal_watch = restored.live().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        assert!(matches!(
            terminal_watch.next().await.unwrap().unwrap(),
            OperationEvent::Completed { snapshot } if snapshot.state == OperationState::Completed
        ));
        assert!(terminal_watch.next().await.is_none());
    }).await.unwrap();
    terminal_watch.close().await.unwrap();
    events.close().await.unwrap();
    serving.abort();
    let _ = serving.await;
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_cancel_waits_for_the_handlers_joined_blocking_child() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("joined-cleanup")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
    let cancelled_tx = Arc::new(Mutex::new(Some(cancelled_tx)));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let child_finished = Arc::new(AtomicBool::new(false));
    let handler_finished = Arc::new(AtomicBool::new(false));
    let child_done = Arc::clone(&child_finished);
    let handler_done = Arc::clone(&handler_finished);
    let mut provider = Provider::new(&mut service);
    provider
        .runtime_trellis_optional_availability_v2()
        .register_read(|_, input| async move { Ok(input) });
    provider
        .runtime_trellis_optional_availability_v2()
        .register_extend(move |_, _, operation| {
            let started = started_tx.lock().unwrap().take().unwrap();
            let cancelled = cancelled_tx.lock().unwrap().take().unwrap();
            let release = release_rx.lock().unwrap().take().unwrap();
            let child_done = Arc::clone(&child_done);
            let handler_done = Arc::clone(&handler_done);
            async move {
                let child = tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    release.recv().unwrap();
                    child_done.store(true, Ordering::Release);
                });
                assert_eq!(
                    operation.cancellation().cancelled().await,
                    OperationCancellationReason::Requested
                );
                cancelled.send(()).unwrap();
                child.await.unwrap();
                handler_done.store(true, Ordering::Release);
                Ok(())
            }
        });
    drop(provider);
    let serving = tokio::spawn(service.run());
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
    let identity = runtime
        .register_client::<Request>("cancel-caller")
        .await
        .unwrap();
    let base = GeneratedClient::connect_user(identity.connect_options())
        .await
        .unwrap();
    let caller = Client::from_generated(base).runtime_trellis_optional_availability_v2();
    let operation = caller
        .extend()
        .start(&ReadInput {
            value: "blocking cleanup".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), started_rx)
        .await
        .unwrap()
        .unwrap();
    let mut observation = operation.live().await.unwrap();
    observation.next().await.unwrap().unwrap();
    let cancellation = operation.cancel();
    tokio::pin!(cancellation);
    tokio::select! {
        result = &mut cancellation => panic!("cancel completed before handler cleanup: {result:?}"),
        observed = tokio::time::timeout(Duration::from_secs(10), cancelled_rx) => {
            observed.unwrap().unwrap();
        }
    }
    assert!(
        matches!(
            operation.get().await.unwrap().state,
            OperationState::Pending | OperationState::Running
        ),
        "cancellation cannot acknowledge unfinished child work as terminal"
    );
    assert!(!child_finished.load(Ordering::Acquire));
    assert!(!handler_finished.load(Ordering::Acquire));
    release_tx.send(()).unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(10), &mut cancellation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.state, OperationState::Cancelled);
    let terminal = tokio::time::timeout(Duration::from_secs(10), operation.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.state, OperationState::Cancelled);
    assert!(child_finished.load(Ordering::Acquire));
    assert!(handler_finished.load(Ordering::Acquire));
    observation.close().await.unwrap();
    serving.abort();
    let _ = serving.await;
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_successor_resumes_interrupted_cancellation_before_terminal_acknowledgement() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = Arc::new(
        runtime
            .register_service::<Participant>("rust-cleanup-provider")
            .await
            .unwrap(),
    );
    let first_identity = Arc::clone(&identity);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (cleanup_tx, cleanup_rx) = tokio::sync::oneshot::channel();
    let cleanup_tx = Arc::new(Mutex::new(Some(cleanup_tx)));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    // Dropping this application's executor stops all its handler/lease tasks.
    let first = std::thread::spawn(move || {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        executor.block_on(async move {
            let mut service = Participant::connect(first_identity.connect_options())
                .await
                .unwrap();
            Provider::new(&mut service)
                .runtime_trellis_optional_availability_v2()
                .register_extend(move |_, _, operation| {
                    let cleanup_tx = cleanup_tx.lock().unwrap().take().unwrap();
                    async move {
                        assert_eq!(
                            operation.cancellation().cancelled().await,
                            OperationCancellationReason::Requested
                        );
                        cleanup_tx.send(()).unwrap();
                        std::future::pending::<()>().await;
                        Ok(())
                    }
                });
            let serving = tokio::spawn(service.run());
            ready_tx.send(()).unwrap();
            stop_rx.await.unwrap();
            serving.abort();
            let _ = serving.await;
        });
    });
    ready_rx.await.unwrap();
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
    let caller_identity = runtime
        .register_client::<Request>("rust-cleanup-caller")
        .await
        .unwrap();
    let base = GeneratedClient::connect_user(caller_identity.connect_options())
        .await
        .unwrap();
    let caller = Client::from_generated(base).runtime_trellis_optional_availability_v2();
    let operation = caller
        .extend()
        .start(&ReadInput {
            value: "recover cleanup".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let persisted = serde_json::to_vec(operation.reference()).unwrap();
    let cancellation = operation.cancel();
    tokio::pin!(cancellation);
    tokio::select! {
        result = &mut cancellation => panic!("cleanup completed before interruption: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(10), cleanup_rx) => {
            result.unwrap().unwrap();
        }
    }
    stop_tx.send(()).unwrap();
    tokio::task::spawn_blocking(move || first.join().unwrap())
        .await
        .unwrap();
    let mut successor = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (recovered_tx, recovered_rx) = tokio::sync::oneshot::channel();
    let recovered_tx = Arc::new(Mutex::new(Some(recovered_tx)));
    Provider::new(&mut successor)
        .runtime_trellis_optional_availability_v2()
        .register_extend(move |context, input, operation| {
            let recovered_tx = recovered_tx.lock().unwrap().take().unwrap();
            async move {
                assert!(context.resuming);
                assert_eq!(input.value, "recover cleanup");
                assert_eq!(
                    operation.cancellation().reason(),
                    Some(OperationCancellationReason::Requested)
                );
                recovered_tx.send(()).unwrap();
                Ok(())
            }
        });
    let serving = tokio::spawn(successor.run());
    tokio::time::timeout(Duration::from_secs(65), recovered_rx)
        .await
        .unwrap()
        .unwrap();
    let restored = caller
        .extend()
        .control(serde_json::from_slice(&persisted).unwrap())
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), restored.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        OperationState::Cancelled
    );
    serving.abort();
    let _ = serving.await;
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_operation_recovers_cleanup_after_its_provider_process_is_killed() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("recover-cleanup")
        .await
        .unwrap();
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
    let caller_identity = runtime
        .register_client::<Request>("recovery-caller")
        .await
        .unwrap();
    let base = GeneratedClient::connect_user(caller_identity.connect_options())
        .await
        .unwrap();
    let caller = Client::from_generated(base).runtime_trellis_optional_availability_v2();
    // One ordinary worker process holds application cleanup; its successor runs
    // the same generated provider/identity against retained production storage.
    // Version-manager shims may fork; kill_on_drop must own the actual worker.
    let node = tokio::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .await
        .unwrap();
    assert!(node.status.success());
    let node = String::from_utf8(node.stdout).unwrap();
    let start_worker = |hold_cleanup| {
        let mut process = tokio::process::Command::new(node.trim());
        process
            .args([
                "--experimental-transform-types",
                "ts/packages/trellis/native/operation_lifecycle_worker.ts",
            ])
            .current_dir("../../..")
            .env("NODE_EXTRA_CA_CERTS", runtime.trust_roots()[0].clone())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        (
            process.spawn().unwrap(),
            json!({
                "trellisUrl":runtime.trellis_url(),
                "seed":identity.seed(),
                "holdCleanup":hold_cleanup
            }),
        )
    };
    let (mut worker, configuration) = start_worker(true);
    let mut input = worker.stdin.take().unwrap();
    input
        .write_all(format!("{configuration}\n").as_bytes())
        .await
        .unwrap();
    let mut events = BufReader::new(worker.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = events.next_line().await.unwrap() {
            if serde_json::from_str::<serde_json::Value>(&line)
                .is_ok_and(|value| value["stage"] == "ready")
            {
                return;
            }
        }
        panic!("provider process exited before readiness");
    })
    .await
    .unwrap();
    let invocation = caller.extend();
    let accepted = invocation
        .start(&ReadInput {
            value: "recover cancellation".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let saved = serde_json::to_vec(accepted.reference()).unwrap();
    let cancellation = accepted.cancel();
    tokio::pin!(cancellation);
    tokio::select! {
        result = &mut cancellation => panic!("worker cleanup must still be pending: {result:?}"),
        observed = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(line) = events.next_line().await.unwrap() {
                if serde_json::from_str::<serde_json::Value>(&line)
                    .is_ok_and(|value| value["stage"] == "cleaning")
                {
                    return;
                }
            }
            panic!("provider exited before cancellation acknowledgement");
        }) => { observed.unwrap(); }
    }
    assert!(matches!(
        accepted.get().await.unwrap().state,
        OperationState::Pending | OperationState::Running
    ));
    worker.kill().await.unwrap();
    let restored = invocation
        .control(serde_json::from_slice(&saved).unwrap())
        .unwrap();
    let (mut successor, configuration) = start_worker(false);
    let mut input = successor.stdin.take().unwrap();
    input
        .write_all(format!("{configuration}\n").as_bytes())
        .await
        .unwrap();
    let mut recovery = BufReader::new(successor.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(line) = recovery.next_line().await.unwrap() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value["stage"] == "cleaning" {
                assert_eq!(value["operationId"], restored.id());
                assert_eq!(value["resuming"], true);
                return;
            }
        }
        panic!("successor exited before resuming retained cancellation");
    })
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(15), restored.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        OperationState::Cancelled
    );
    assert_eq!(
        restored.get().await.unwrap().state,
        OperationState::Cancelled
    );
    successor.kill().await.unwrap();
    runtime.shutdown().await.unwrap();
}
