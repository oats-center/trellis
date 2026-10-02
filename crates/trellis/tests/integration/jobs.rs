use trellis_rs::jobs::bindings::{JobKeyStalePolicy, JobQueueWhenFull};
use trellis_rs::jobs::keys::{
    coordination_key, AcquireSlotInput, AcquireSlotOutcome, JobKeyCoordinator, JobKeyPolicy,
    JobKeyState, LeaseMutationOutcome, NatsKeyCoordinator,
};
use trellis_rs::jobs::JobContext;

#[tokio::test]
async fn displaced_cleanup_survives_reopen_and_old_tokens_cannot_release_the_new_owner() {
    let source = tempfile::tempdir().unwrap();
    trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
        source.path(),
    ))
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut nats = trellis_local_nats::LocalNats::builder()
        .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
        .cache_dir(state.path().join("cache"))
        .source(source.path())
        .temporary_state()
        .ephemeral_ports()
        .output(trellis_local_nats::NatsOutput::Log {
            path: state.path().join("nats.log"),
            mirror: false,
        })
        .start()
        .unwrap();
    let client = async_nats::ConnectOptions::new()
        .credentials_file(source.path().join("creds/trellis-auth.creds"))
        .await
        .unwrap()
        .connect(nats.nats_url())
        .await
        .unwrap();
    let store = async_nats::jetstream::new(client.clone())
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "JOBS_KEYS_takeover".into(),
            history: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    let policy = JobKeyPolicy {
        service: "takeover".into(),
        job_type: "work".into(),
        key: "shared".into(),
        key_hash: "shared-hash".into(),
        max_active: 1,
        max_queued_per_key: 2,
        when_full: JobQueueWhenFull::Reject,
        stale_policy: JobKeyStalePolicy::FailStale,
    };
    let original = NatsKeyCoordinator::open_for_service(client.clone(), "takeover")
        .await
        .unwrap();
    let input = AcquireSlotInput {
        job_id: "A".into(),
        slot_token: "A-old".into(),
        instance_id: "crashed".into(),
        started_at: "2026-10-01T00:00:00Z".into(),
        lease_expires_at: "2026-10-01T00:00:01Z".into(),
        tries: 1,
        context: JobContext {
            trace_id: "11111111111111111111111111111111".into(),
            request_id: "request".into(),
            traceparent: "00-11111111111111111111111111111111-1111111111111111-01".into(),
            tracestate: None,
        },
    };
    assert!(matches!(
        original
            .acquire(policy.clone(), input.clone())
            .await
            .unwrap(),
        AcquireSlotOutcome::Acquired { .. }
    ));
    let next = AcquireSlotInput {
        job_id: "B".into(),
        slot_token: "B-new".into(),
        instance_id: "successor".into(),
        started_at: "2026-10-01T00:00:02Z".into(),
        lease_expires_at: "2026-10-01T00:00:03Z".into(),
        ..input.clone()
    };
    assert!(matches!(
        original.acquire(policy.clone(), next).await.unwrap(),
        AcquireSlotOutcome::Acquired { .. }
    ));
    drop(original);
    let recovered = NatsKeyCoordinator::open_for_service(client.clone(), "takeover")
        .await
        .unwrap();
    let key = coordination_key(&policy.service, &policy.job_type, &policy.key_hash);
    let entry = store.get(&key).await.unwrap().unwrap();
    let persisted: JobKeyState = serde_json::from_slice(&entry).unwrap();
    assert!(persisted.cleanup_pending.contains(&"A".to_owned()));
    assert!(matches!(
        recovered
            .release(
                policy.clone(),
                "A".into(),
                "A-old".into(),
                "2026-10-01T00:00:02Z".into()
            )
            .await
            .unwrap(),
        LeaseMutationOutcome::Lost { .. }
    ));
    let unchanged: JobKeyState =
        serde_json::from_slice(&store.get(&key).await.unwrap().unwrap()).unwrap();
    assert_eq!(unchanged.active[0].job_id, "B");
    assert_eq!(unchanged.active[0].slot_token, "B-new");
    assert!(matches!(
        recovered
            .release(
                policy.clone(),
                "B".into(),
                "B-new".into(),
                "2026-10-01T00:00:02Z".into()
            )
            .await
            .unwrap(),
        LeaseMutationOutcome::Released { .. }
    ));
    let reacquired = AcquireSlotInput {
        slot_token: "A-recovered".into(),
        instance_id: "cleanup".into(),
        started_at: "2026-10-01T00:00:02Z".into(),
        lease_expires_at: "2026-10-01T00:00:03Z".into(),
        ..input
    };
    let result = recovered.acquire(policy.clone(), reacquired).await.unwrap();
    let AcquireSlotOutcome::Acquired { state, slot, .. } = result else {
        panic!("recovered owner could not acquire capacity")
    };
    assert_eq!(slot.slot_token, "A-recovered");
    assert!(state.cleanup_pending.contains(&"A".to_owned()));
    // Releasing an interrupted cleanup attempt must not discharge its obligation.
    recovered
        .release(
            policy,
            "A".into(),
            "A-recovered".into(),
            "2026-10-01T00:00:02Z".into(),
        )
        .await
        .unwrap();
    let retained: JobKeyState =
        serde_json::from_slice(&store.get(&key).await.unwrap().unwrap()).unwrap();
    assert!(retained.cleanup_pending.contains(&"A".to_owned()));
    nats.stop().unwrap();
}
