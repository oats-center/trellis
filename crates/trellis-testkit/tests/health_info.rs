//! A Rust service's health details and checks reach the Health projection,
//! and its checks decide its status: any failed check makes it unhealthy,
//! otherwise any degraded check makes it degraded.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use runtime_trellis::participants::runtime_trellis_provider::Participant;
use serde_json::{json, Value};
use trellis_rs::service::{ServiceHealthCheck, ServiceHealthInfo};
use trellis_runtime_apis::apis::trellis_health_v1;
use trellis_runtime_apis::types::{
    HealthInspectRequest, HealthInspectRequestContractId, HealthInspectRequestParticipantKind,
};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeats_carry_the_services_version_info_and_current_check_results() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("health info")
        .await
        .unwrap();
    let service = Participant::connect(
        identity
            .connect_options()
            .with_health_heartbeat_interval_ms(1_000),
    )
    .await
    .unwrap();
    service.health().set_info(ServiceHealthInfo {
        version: Some("1.4.2".into()),
        info: Some(json!({ "region": "lab" }).as_object().unwrap().clone()),
    });
    let database_up = Arc::new(AtomicBool::new(true));
    let database = service.health().add("database", {
        let database_up = Arc::clone(&database_up);
        move || {
            let up = database_up.load(Ordering::SeqCst);
            async move {
                if up {
                    ServiceHealthCheck::ok()
                } else {
                    ServiceHealthCheck::failed("connection refused")
                }
            }
        }
    });

    let caller = runtime
        .register_client::<runtime_trellis::apps::runtime_trellis_observer::Request>(
            "health reader",
        )
        .await
        .unwrap();
    let client = trellis_rs::generated::Client::connect_user(caller.connect_options())
        .await
        .unwrap();
    let health = trellis_health_v1::Client::from_generated(client.clone());
    let request = HealthInspectRequest {
        contract_id: HealthInspectRequestContractId(identity.participant_id().into()),
        history_limit: None,
        history_since: None,
        instance_id: None,
        participant_kind: HealthInspectRequestParticipantKind::Service,
        extra: Default::default(),
    };
    // Waits for an instance whose projected latest sample `matches`,
    // returning the instance as JSON.
    let sample = |matches: fn(&Value) -> bool| {
        let health = health.clone();
        let request = request.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let Ok(inspected) = health.inspect(&request).await {
                        if let Some(instance) = inspected
                            .instances
                            .first()
                            .map(|instance| serde_json::to_value(instance).unwrap())
                        {
                            if matches(&instance["latestSample"]) {
                                break instance;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .expect("a matching heartbeat sample")
        }
    };

    let healthy = sample(|sample| sample["checks"].as_array().is_some_and(|c| !c.is_empty())).await
        ["latestSample"]
        .clone();
    assert_eq!(healthy["participant"]["version"], "1.4.2");
    assert_eq!(healthy["participant"]["info"], json!({ "region": "lab" }));
    assert_eq!(healthy["reportedStatus"], "healthy");
    assert_eq!(healthy["checks"][0]["name"], "database");
    assert_eq!(healthy["checks"][0]["status"], "ok");

    // A degraded check, with none failed, makes the instance degraded.
    let cache = service.health().add("cache", || async {
        ServiceHealthCheck::degraded("serving from one replica")
    });
    let degraded = sample(|sample| sample["checks"].as_array().is_some_and(|c| c.len() == 2)).await;
    assert_eq!(degraded["effectiveStatus"], "degraded");
    assert_eq!(degraded["latestSample"]["reportedStatus"], "degraded");
    assert_eq!(degraded["latestSample"]["summary"], "1 check degraded");
    assert_eq!(degraded["latestSample"]["checks"][0]["name"], "cache");
    assert_eq!(degraded["latestSample"]["checks"][0]["status"], "degraded");

    // One failed check makes the instance unhealthy, however many pass or
    // are degraded.
    database_up.store(false, Ordering::SeqCst);
    let failing = sample(|sample| sample["checks"][1]["status"] == "failed").await;
    assert_eq!(failing["effectiveStatus"], "unhealthy");
    let failing = &failing["latestSample"];
    assert_eq!(failing["reportedStatus"], "unhealthy");
    assert_eq!(failing["summary"], "1 check failing");
    assert_eq!(failing["checks"][1]["error"], "connection refused");

    cache.remove();
    database.remove();
    let removed = sample(|sample| sample["checks"].as_array().is_some_and(Vec::is_empty)).await
        ["latestSample"]
        .clone();
    assert_eq!(removed["reportedStatus"], "healthy");
    assert_eq!(removed["participant"]["version"], "1.4.2");

    // A check that panics before it returns a future is reported as failed,
    // and the heartbeats carry on.
    let broken = service.health().add("broken", || {
        if true {
            panic!("the check's lock is poisoned");
        }
        std::future::ready(ServiceHealthCheck::ok())
    });
    let panicked =
        sample(|sample| sample["reportedStatus"] == "unhealthy").await["latestSample"].clone();
    assert_eq!(panicked["checks"][0]["name"], "broken");
    assert_eq!(panicked["checks"][0]["status"], "failed");
    assert_eq!(panicked["checks"][0]["error"], "health check panicked");
    broken.remove();
    sample(|sample| sample["reportedStatus"] == "healthy").await;

    drop(health);
    drop(client);
    drop(service);
    runtime.shutdown().await.unwrap();
}
