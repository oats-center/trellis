//! Ordinary service workers recover durable history without shared stream access.

use trellis_rs::__internal::RuntimeClient as _;
use serde_json::json;
use std::time::Duration;
use trellis_rs::client::{ServiceConnectWithContractOptions, TrellisClient};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::apis::trellis_jobs_v1::Client as JobsClient;
use trellis_test_fixture::participants::trellis_test_fixture_jobs_provider::{
    Participant, Provider,
};
use trellis_test_fixture::types::Value;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_worker_recovers_and_reconstructs_signed_job_history() {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("node-jobs")
        .await
        .unwrap();
    let mut node = tokio::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "ts/packages/trellis/native/jobs_interop.ts",
        ])
        .current_dir("../../..")
        .env("NODE_EXTRA_CA_CERTS", runtime.trust_roots()[0].clone())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    node.stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                "{}\n",
                json!({"trellisUrl":runtime.oauth_origin(),"seed":identity.seed()})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(40), node.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "Node Jobs failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| serde_json::from_str::<serde_json::Value>(line)
            .is_ok_and(|event| event["stage"] == "verified")));
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_jobs_execute_and_history_is_deployment_scoped() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let first_identity = runtime
        .register_service::<Participant>("jobs-first")
        .await
        .unwrap();
    let second_identity = runtime
        .register_service::<Participant>("jobs-second")
        .await
        .unwrap();
    let mut first = Participant::connect(first_identity.connect_options())
        .await
        .unwrap();
    let second = Participant::connect(second_identity.connect_options())
        .await
        .unwrap();
    let history_first = JobsClient::from_generated(first.generated_client());
    let history_second = JobsClient::from_generated(second.generated_client());
    let job = Provider::new(&mut first)
        .submit_work(Value {
            value: "retained input".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    Provider::new(&mut first)
        .register_work(|job| async move {
            for current in 0..140 {
                job.progress(trellis_rs::jobs::JobProgress {
                    current: Some(current),
                    ..Default::default()
                })
                .await
                .unwrap();
            }
            Ok::<_, trellis_rs::jobs::JobProcessError<String>>(Value {
                value: job.payload().value.clone(),
                extra: Default::default(),
            })
        })
        .await
        .unwrap();
    let terminal = tokio::time::timeout(Duration::from_secs(20), job.wait())
        .await
        .unwrap()
        .unwrap();
    let snapshot = job.get().await.unwrap();
    assert_eq!(snapshot.payload.value, "retained input");
    assert_eq!(snapshot.result.unwrap().value, "retained input");
    let input = serde_json::from_value(
        json!({"queue":"work","jobId":job.identity().id,"fromSequence":"1"}),
    )
    .unwrap();
    let page = history_first.history(&input).await.unwrap();
    assert_eq!(
        page.entries.len(),
        128,
        "history must page a real operation's retained observations"
    );
    let continuation = serde_json::from_value(json!({
        "queue":"work",
        "jobId":job.identity().id,
        "fromSequence":page.next_sequence,
        "throughSequence":page.through_sequence
    })).unwrap();
    let end = history_first.history(&continuation).await.unwrap();
    assert_eq!(page.through_sequence, end.through_sequence);
    assert!(end.next_sequence.is_none());
    let events = page
        .entries
        .iter()
        .chain(end.entries.iter())
        .map(|entry| serde_json::from_slice::<serde_json::Value>(&entry.payload.0).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| event["eventType"] == "started"));
    assert!(events.iter().any(|event| event["eventType"] == "completed"));
    assert!(history_second
        .history(&input)
        .await
        .unwrap()
        .entries
        .is_empty());
    let user = runtime
        .register_client::<trellis_runtime_apis::apps::trellis_cli::Request>("jobs-user")
        .await
        .unwrap();
    let user = trellis_rs::generated::Client::connect_user(user.connect_options())
        .await
        .unwrap();
    assert!(JobsClient::from_generated(user)
        .history(&input)
        .await
        .is_err());
    let server =
        trellis_rs::auth::discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
            .await
            .unwrap();
    let raw = TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
        nats_servers: server.nats_servers,
        trust: server.trust,
        participant_id: Participant::ID,
        provisioned_identity_seed_base64url: second_identity.seed(),
        name: Some("raw-isolation"),
        timeout_ms: 1000,
    })
    .await
    .unwrap();
    for endpoint in ["$JS.API.DIRECT.GET.JOBS", "$JS.API.STREAM.MSG.GET.JOBS"] {
        let attempt = tokio::time::timeout(
            Duration::from_secs(1),
            raw.runtime_nats().request(
                endpoint,
                json!({"seq":page.entries[0].sequence}).to_string().into(),
            ),
        )
        .await;
        assert!(
            !matches!(attempt, Ok(Ok(_))),
            "shared stream endpoint {endpoint} leaked history"
        );
    }
    drop(terminal);
    runtime.shutdown().await.unwrap();
}
