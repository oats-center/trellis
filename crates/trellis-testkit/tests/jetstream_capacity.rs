//! Trellis on a broker with little JetStream storage, as on a small VM whose
//! NATS sizes `max_file_store` from about a gigabyte of free disk.
use std::time::Duration;

use futures_util::StreamExt as _;
use runtime_trellis::participants::runtime_trellis_webhook_service::{Participant, Provider};
use serde_json::{json, Value};
use trellis_runtime_apis::apis::trellis_health_v1;
use trellis_runtime_apis::types::{
    HealthInspectRequest, HealthInspectRequestContractId, HealthInspectRequestParticipantKind,
    HealthStorageRequest,
};
use trellis_testkit::{TestNatsUser, TestServiceIdentity, TrellisTestRuntime};

const GIB: u64 = 1 << 30;
const MIB: i64 = 1 << 20;

/// The intake stream Trellis keeps for a deployment's `async` requests.
fn intake_stream(identity: &TestServiceIdentity) -> String {
    let token: String = identity
        .deployment_id()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("TRELLIS_WEBHOOKS_{token}")
}

/// The service's latest `webhook:token` check once `accept` takes it.
async fn token_check(
    health: &trellis_health_v1::Client,
    identity: &TestServiceIdentity,
    accept: impl Fn(&Value) -> bool,
) -> Value {
    let request = HealthInspectRequest {
        contract_id: HealthInspectRequestContractId(identity.participant_id().into()),
        history_limit: None,
        history_since: None,
        instance_id: None,
        participant_kind: HealthInspectRequestParticipantKind::Service,
        extra: Default::default(),
    };
    let mut last = Value::Null;
    let found = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(inspected) = health.inspect(&request).await {
                for instance in &inspected.instances {
                    let sample = serde_json::to_value(instance).unwrap();
                    if sample["deploymentId"] != identity.deployment_id() {
                        continue;
                    }
                    if let Some(check) = sample["latestSample"]["checks"]
                        .as_array()
                        .and_then(|checks| checks.iter().find(|c| c["name"] == "webhook:token"))
                    {
                        last = check.clone();
                        if accept(check) {
                            return check.clone();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("no accepted webhook:token check; the last was {last}"))
}

/// The `jetstream` section of `Health.Storage`.
async fn jetstream_storage(health: &trellis_health_v1::Client) -> Value {
    let storage = health
        .storage(&HealthStorageRequest {
            extra: Default::default(),
        })
        .await
        .unwrap();
    serde_json::to_value(storage).unwrap()["jetstream"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_small_broker_holds_every_stream_and_webhook_intake_recovers() {
    let mut runtime = TrellisTestRuntime::builder()
        .jetstream_max_file_store(GIB)
        .start()
        .await
        .unwrap();
    let values = json!({ "partnerUser": "partner", "relayRanges": ["127.0.0.0/8"] });
    let secrets = json!({
        "signing": "signing-secret",
        "hookToken": "right-token-value",
        "partnerPassword": "partner-password",
    });
    let identity = runtime
        .register_service::<Participant>("billing")
        .await
        .unwrap();
    runtime
        .set_config(&identity, values.clone(), secrets.clone())
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let provider = Provider::new(&mut service);
    let mut deliveries = provider.token().unwrap().subscribe().await.unwrap();
    let urls = runtime.webhook_urls(&identity).await.unwrap();
    let http = runtime.https_client_builder().build().unwrap();
    let send = |url: String, body: &str| {
        http.post(url)
            .header("x-hook-token", "right-token-value")
            .body(body.to_owned())
            .send()
    };

    // An `async` endpoint stores the request in the deployment's intake
    // stream before it answers, so a 200 and the delivery mean the stream
    // was created next to every runtime stream.
    let response = send(urls["token"].clone(), "first").await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let delivery = tokio::time::timeout(Duration::from_secs(20), deliveries.next())
        .await
        .expect("the Received event arrives")
        .unwrap()
        .unwrap();
    assert_eq!(delivery.body.0, b"first");

    // Together the streams reserve a small share of the gigabyte.
    let nats = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::Trellis).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let jetstream = async_nats::jetstream::new(nats);
    let account = jetstream.query_account().await.unwrap();
    assert!(
        account.reserved_storage <= GIB / 8,
        "the streams reserve {} bytes",
        account.reserved_storage
    );

    // An intake stream deleted behind Trellis's back is created again by the
    // deployment's worker, and requests flow through it again.
    let billing = intake_stream(&identity);
    jetstream.delete_stream(&billing).await.unwrap();
    tokio::time::timeout(Duration::from_secs(90), async {
        while jetstream.get_stream(&billing).await.is_err() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the worker creates the missing intake stream");
    let response = send(urls["token"].clone(), "second").await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let delivery = tokio::time::timeout(Duration::from_secs(60), deliveries.next())
        .await
        .expect("the worker verifies requests from the new stream")
        .unwrap()
        .unwrap();
    assert_eq!(delivery.body.0, b"second");

    // Health.Storage shows the broker's budget, and nothing is near it.
    let caller = runtime
        .register_client::<runtime_trellis::apps::runtime_trellis_observer::Request>(
            "capacity health reader",
        )
        .await
        .unwrap();
    let client = trellis_rs::generated::Client::connect_user(caller.connect_options())
        .await
        .unwrap();
    let health = trellis_health_v1::Client::from_generated(client.clone());
    let room = jetstream_storage(&health).await;
    assert_eq!(room["status"], "ok", "{room}");
    assert_eq!(room["serverStorage"]["limit"], GIB.to_string(), "{room}");
    assert_eq!(room["problems"], json!([]), "{room}");

    // Another stream takes all but a few MiB of the broker's storage, so a
    // new deployment's 64 MiB intake stream does not fit.
    let reserved = jetstream.query_account().await.unwrap().reserved_storage as i64;
    let mut filler = GIB as i64 - reserved - MIB;
    while jetstream
        .create_stream(async_nats::jetstream::stream::Config {
            name: "FILLER".into(),
            subjects: vec!["filler".into()],
            max_bytes: filler,
            ..Default::default()
        })
        .await
        .is_err()
    {
        filler -= 4 * MIB;
        assert!(filler > 0, "no filler fits");
    }
    let crowded = runtime
        .register_service::<Participant>("billing-crowded")
        .await
        .unwrap();
    runtime.set_config(&crowded, values, secrets).await.unwrap();
    let mut crowded_service = Participant::connect(
        crowded
            .connect_options()
            .with_health_heartbeat_interval_ms(1_000),
    )
    .await
    .unwrap();
    let crowded_provider = Provider::new(&mut crowded_service);
    let mut crowded_deliveries = crowded_provider.token().unwrap().subscribe().await.unwrap();
    let crowded_urls = runtime.webhook_urls(&crowded).await.unwrap();

    // The endpoint's health says why, with the numbers, and requests get 503
    // rather than being lost. Until Trellis serves the new endpoint, its
    // check says only that.
    let check = token_check(&health, &crowded, |check| {
        check["status"] == "degraded"
            && check["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("JetStream"))
    })
    .await;
    let summary = check["summary"].as_str().unwrap();
    let stream = intake_stream(&crowded);
    assert!(
        summary.contains("requests get 503: JetStream storage is exhausted")
            && summary.contains(&format!("intake stream {stream} asks for 64.0 MiB"))
            && summary.contains("of 1.0 GiB"),
        "{summary}"
    );
    let refused = send(crowded_urls["token"].clone(), "crowded")
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);

    // The worker keeps trying with backoff, and the log says so once rather
    // than at every attempt.
    tokio::time::sleep(Duration::from_secs(10)).await;
    let log = std::fs::read_to_string(runtime.workdir().join("logs/trellis-server.log")).unwrap();
    let lines = |needle: &str| {
        log.lines()
            .filter(|line| line.contains(needle) && line.contains(&stream))
            .count()
    };
    assert_eq!(
        lines("JetStream refused a Trellis resource"),
        1,
        "the refusal is logged once with its numbers"
    );
    assert!(
        log.lines()
            .filter(|line| line.contains("webhook worker restarting")
                && line.contains(crowded.deployment_id()))
            .count()
            <= 1,
        "the worker's restarts are logged at most once a minute"
    );

    // Health.Storage says the broker is nearly full and what it refuses.
    let crowded_storage = jetstream_storage(&health).await;
    assert_eq!(crowded_storage["status"], "degraded", "{crowded_storage}");
    let problems: Vec<&str> = crowded_storage["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        problems
            .iter()
            .any(|problem| problem.starts_with("JetStream server storage is")
                && problem.contains("% committed")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains(&format!("intake stream {stream} asks for"))),
        "{problems:?}"
    );

    // Removing a deployment whose intake stream JetStream refuses takes its
    // refusal out of Health.Storage, since nothing wants that stream now.
    let removed = runtime
        .register_service::<Participant>("billing-removed")
        .await
        .unwrap();
    let removed_stream = format!("intake stream {} asks for", intake_stream(&removed));
    let refuses = |storage: &Value, needle: &str| {
        storage["problems"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .any(|problem| problem.contains(needle))
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        while !refuses(&jetstream_storage(&health).await, &removed_stream) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Health.Storage reports the refused intake stream");
    runtime.remove_service(&removed).await.unwrap();
    let mut last = Value::Null;
    let pruned = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            last = jetstream_storage(&health).await;
            if !refuses(&last, &removed_stream) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(
        pruned.is_ok(),
        "the refusal outlives the deployment: {last}"
    );
    assert!(
        refuses(&last, &format!("intake stream {stream} asks for")),
        "the remaining deployment's refusal stays: {last}"
    );

    // Once there is room, the worker creates the stream, the check clears,
    // and requests flow.
    jetstream.delete_stream("FILLER").await.unwrap();
    token_check(&health, &crowded, |check| check["status"] == "ok").await;
    let response = send(crowded_urls["token"].clone(), "room").await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let delivery = tokio::time::timeout(Duration::from_secs(20), crowded_deliveries.next())
        .await
        .expect("the Received event arrives")
        .unwrap()
        .unwrap();
    assert_eq!(delivery.body.0, b"room");
    let recovered = jetstream_storage(&health).await;
    assert_eq!(recovered["status"], "ok", "{recovered}");

    // A larger intake size than the broker has room for leaves the stream as
    // it was, still taking requests, and the check says why. The settings of
    // the deployment's first endpoint by name size its intake stream.
    runtime
        .set_webhook_setting(&crowded, "formed", "intakeMaxBytes", Some(2 * GIB))
        .await
        .unwrap();
    let check = token_check(&health, &crowded, |check| check["status"] == "degraded").await;
    let summary = check["summary"].as_str().unwrap();
    assert!(
        summary
            .starts_with("the intake stream keeps its old limits: JetStream storage is exhausted")
            && summary.contains("asks for 2.0 GiB"),
        "{summary}"
    );
    let response = send(crowded_urls["token"].clone(), "kept").await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // A full intake stream refuses new requests, and their records say so.
    runtime
        .set_webhook_setting(&crowded, "formed", "intakeMaxBytes", Some(1 << 20))
        .await
        .unwrap();
    token_check(&health, &crowded, |check| check["status"] == "ok").await;
    let crowded_jetstream = jetstream.clone();
    let crowded_stream = stream.clone();
    tokio::time::timeout(Duration::from_secs(20), async move {
        loop {
            let mut intake = crowded_jetstream.get_stream(&crowded_stream).await.unwrap();
            if intake.info().await.unwrap().config.max_bytes == 1 << 20 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the intake stream shrinks to the new size");
    let body = "x".repeat(200 << 10);
    let mut statuses = Vec::new();
    for _ in 0..8 {
        statuses.push(
            send(crowded_urls["token"].clone(), &body)
                .await
                .unwrap()
                .status(),
        );
    }
    assert_eq!(
        statuses.last(),
        Some(&reqwest::StatusCode::SERVICE_UNAVAILABLE),
        "{statuses:?}"
    );
    let attempts = runtime.webhook_attempts(&crowded, "token").await.unwrap();
    assert_eq!(attempts[0]["outcome"], "unavailable", "{:?}", attempts[0]);
    assert_eq!(
        attempts[0]["reason"], "intake stream full",
        "{:?}",
        attempts[0]
    );
    runtime
        .set_webhook_setting(&crowded, "formed", "intakeMaxBytes", None)
        .await
        .unwrap();
    let crowded_jetstream = jetstream.clone();
    let crowded_stream = stream.clone();
    tokio::time::timeout(Duration::from_secs(20), async move {
        loop {
            let mut intake = crowded_jetstream.get_stream(&crowded_stream).await.unwrap();
            if intake.info().await.unwrap().config.max_bytes == 64 * MIB {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the intake stream returns to its default size");

    // An operator gives the Health stream a size bound the broker has no room
    // for and restarts: startup waits, says why with the numbers, and starts
    // once there is room.
    let config = runtime.workdir().join("config/config.toml");
    let mut written = std::fs::read_to_string(&config).unwrap();
    written.push_str("\n[health]\ntransport_max_bytes = 268435456\n");
    std::fs::write(&config, written).unwrap();
    let system = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::System).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let jsz: Value = serde_json::from_slice(
        &system
            .request("$SYS.REQ.SERVER.PING.JSZ", "{}".into())
            .await
            .unwrap()
            .payload,
    )
    .unwrap();
    let left = jsz["data"]["config"]["max_storage"].as_i64().unwrap()
        - jsz["data"]["reserved_storage"].as_i64().unwrap();
    jetstream
        .create_stream(async_nats::jetstream::stream::Config {
            name: "FILLER".into(),
            subjects: vec!["filler".into()],
            max_bytes: left - 16 * MIB,
            ..Default::default()
        })
        .await
        .unwrap();
    let log_path = runtime.workdir().join("logs/trellis-server.log");
    let waiting = jetstream.clone();
    let (restarted, ()) = tokio::join!(runtime.restart(), async move {
        let refusal = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                if let Some(line) = log.lines().find(|line| {
                    line.contains("JetStream refused a Trellis resource")
                        && line.contains("stream TRELLIS_HEALTH asks for 256.0 MiB")
                }) {
                    break line.to_owned();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("startup reports the stream it cannot create");
        assert!(
            refusal.contains("JetStream storage is exhausted") && refusal.contains("of 1.0 GiB"),
            "{refusal}"
        );
        waiting.delete_stream("FILLER").await.unwrap();
    });
    restarted.unwrap();
    let mut health_stream = jetstream.get_stream("TRELLIS_HEALTH").await.unwrap();
    assert_eq!(
        health_stream.info().await.unwrap().config.max_bytes,
        256 * MIB,
        "startup applies the size bound once there is room"
    );

    drop(health);
    drop(client);
    drop(crowded_deliveries);
    drop(crowded_service);
    drop(deliveries);
    drop(service);
    runtime.shutdown().await.unwrap();
}
