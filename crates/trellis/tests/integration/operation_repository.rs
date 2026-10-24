use futures_util::StreamExt;
use serde_json::json;
use time::OffsetDateTime;
use trellis_rs::service::{
    operation_invocation_digest, DurableOperationRecord, DurableOperationSignal,
    KvOperationRepository, OperationRepository, OperationSnapshot, OperationState, ServerError,
    MAX_OPERATION_SIGNAL_BYTES,
};

fn record(id: String) -> DurableOperationRecord {
    let input = json!({"name": "Ada"});
    let binding = trellis_protocol::SessionBinding::Browser {
        client_id: "operations-client".to_owned(),
        origin: "https://operations.example".to_owned(),
    };
    DurableOperationRecord {
        invocation_digest: operation_invocation_digest(
            "example.api@1",
            "run",
            "principal",
            &binding,
            &input,
        )
        .unwrap(),
        invocation_id: id.clone(),
        api_id: "example.api@1".to_owned(),
        operation: "run".to_owned(),
        deployment_id: "deployment".to_owned(),
        creator_principal_id: "principal".to_owned(),
        creator_binding: binding,
        caller_session_key: "session".to_owned(),
        caller: None,
        input,
        snapshot: OperationSnapshot {
            id: Some(id),
            service: Some("service".to_owned()),
            operation: Some("run".to_owned()),
            created_at: Some("2026-09-10T00:00:00Z".to_owned()),
            updated_at: Some("2026-09-10T00:00:00Z".to_owned()),
            state: OperationState::Pending,
            revision: 1,
            ..OperationSnapshot::default()
        },
        revision: 1,
        owner_executor_id: None,
        owner_connection_id: None,
        owner_epoch: 0,
        lease_expires_at_ms: None,
        cancellation_requested: false,
        cancellation_requested_at: None,
        next_signal_sequence: 1,
        update_sequence: 0,
        signals: Vec::new(),
        transfer: None,
        telemetry: None,
    }
}

#[tokio::test]
async fn operation_records_persist_and_lease_writes_are_fenced() {
    let source = tempfile::tempdir().unwrap();
    trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
        source.path(),
    ))
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut nats = trellis_local_nats::LocalNats::builder()
        .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
        .cache_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/trellis-test-cache"),
        )
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
    let jetstream = async_nats::jetstream::new(client);
    let bucket = format!("trellis_test_operations_{}", ulid::Ulid::new());
    let store = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: bucket.clone(),
            history: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    let repository = KvOperationRepository::new(store);
    let operation = record(ulid::Ulid::new().to_string());
    let operation_id = operation.invocation_id.clone();
    let created = repository.create(operation).await.unwrap();
    assert_eq!(
        repository.create(created.record.clone()).await.unwrap(),
        created
    );
    for binding in [
        trellis_protocol::SessionBinding::Browser {
            client_id: "operations-client".to_owned(),
            origin: "https://another-origin.example".to_owned(),
        },
        trellis_protocol::SessionBinding::Browser {
            client_id: "another-client".to_owned(),
            origin: "https://operations.example".to_owned(),
        },
    ] {
        let mut conflicting = created.record.clone();
        conflicting.creator_binding = binding;
        conflicting.invocation_digest = operation_invocation_digest(
            &conflicting.api_id,
            &conflicting.operation,
            &conflicting.creator_principal_id,
            &conflicting.creator_binding,
            &conflicting.input,
        )
        .unwrap();
        assert!(repository.create(conflicting).await.is_err());
    }
    let mut conflicting = created.record.clone();
    conflicting.input = json!({"name": "Grace"});
    conflicting.invocation_digest = operation_invocation_digest(
        &conflicting.api_id,
        &conflicting.operation,
        &conflicting.creator_principal_id,
        &conflicting.creator_binding,
        &conflicting.input,
    )
    .unwrap();
    assert!(repository.create(conflicting).await.is_err());
    assert_eq!(
        repository.get(&operation_id).await.unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        repository.list_nonterminal().await.unwrap(),
        vec![created.clone()]
    );

    let first = repository
        .claim(&operation_id, "executor-a", 1_000, 31_000)
        .await
        .unwrap();
    assert!(repository
        .claim(&operation_id, "executor-b", 2_000, 32_000)
        .await
        .is_err());
    let renewed = repository
        .renew(
            &operation_id,
            "executor-a",
            first.record.owner_epoch,
            2_000,
            32_000,
        )
        .await
        .unwrap();
    assert_eq!(renewed.record.owner_epoch, first.record.owner_epoch);
    assert!(repository
        .renew(
            &operation_id,
            "executor-a",
            first.record.owner_epoch,
            32_000,
            62_000,
        )
        .await
        .is_err());

    let millisecond_fence_id = ulid::Ulid::new().to_string();
    repository
        .create(record(millisecond_fence_id.clone()))
        .await
        .unwrap();
    let now = loop {
        let now = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
        if now.rem_euclid(1_000) >= 100 {
            break now;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    let expired = repository
        .claim(&millisecond_fence_id, "executor-ms", now - 2, now - 1)
        .await
        .unwrap();
    let mut fenced = expired.record;
    fenced.revision += 1;
    assert!(repository
        .compare_exchange(expired.revision, "executor-ms", fenced.owner_epoch, fenced)
        .await
        .is_err());

    let now = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    let reclaimed_same_executor = repository
        .claim(&operation_id, "executor-a", now, now + 30_000)
        .await
        .unwrap();
    assert_eq!(
        reclaimed_same_executor.record.owner_epoch,
        first.record.owner_epoch + 1
    );
    let successor_now = now + 30_001;
    let owned = repository
        .claim(
            &operation_id,
            "executor-b",
            successor_now,
            successor_now + 30_000,
        )
        .await
        .unwrap();
    assert_eq!(
        owned.record.owner_epoch,
        reclaimed_same_executor.record.owner_epoch + 1
    );
    let mut stale_same_executor = reclaimed_same_executor.record;
    stale_same_executor.revision += 1;
    assert!(repository
        .compare_exchange(
            reclaimed_same_executor.revision,
            "executor-a",
            stale_same_executor.owner_epoch,
            stale_same_executor,
        )
        .await
        .is_err());
    let mut watch = repository.watch(&operation_id).await.unwrap();
    assert_eq!(watch.next().await.unwrap().unwrap(), owned);

    let mut signalled = owned.record.clone();
    signalled.revision += 1;
    signalled.next_signal_sequence += 1;
    signalled.signals.push(DurableOperationSignal {
        sequence: 1,
        request_id: "signal-request".to_owned(),
        name: "resume".to_owned(),
        payload: json!({"ok": true}),
        accepted_at: "2026-09-10T00:00:00Z".to_owned(),
        acknowledged: false,
    });
    let signalled = repository
        .compare_exchange(
            owned.revision,
            "executor-b",
            owned.record.owner_epoch,
            signalled,
        )
        .await
        .unwrap();
    assert_eq!(watch.next().await.unwrap().unwrap(), signalled);
    assert!(!signalled.record.signals[0].acknowledged);

    let mut acknowledged = signalled.record.clone();
    acknowledged.revision += 1;
    acknowledged.signals[0].acknowledged = true;
    let acknowledged = repository
        .compare_exchange(
            signalled.revision,
            "executor-b",
            signalled.record.owner_epoch,
            acknowledged,
        )
        .await
        .unwrap();
    assert!(acknowledged.record.signals[0].acknowledged);

    let mut cancelled = acknowledged.record.clone();
    cancelled.revision += 1;
    cancelled.cancellation_requested = true;
    cancelled.cancellation_requested_at = Some(
        OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
    );
    cancelled.snapshot.state = OperationState::Cancelled;
    let cancelled = repository
        .compare_exchange(
            acknowledged.revision,
            "executor-b",
            acknowledged.record.owner_epoch,
            cancelled,
        )
        .await
        .unwrap();
    let mut late_complete = cancelled.record.clone();
    late_complete.revision += 1;
    late_complete.snapshot.state = OperationState::Completed;
    assert!(repository
        .compare_exchange(
            cancelled.revision,
            "executor-b",
            cancelled.record.owner_epoch,
            late_complete,
        )
        .await
        .is_err());
    let mut stale = first.record;
    stale.revision += 1;
    assert!(repository
        .compare_exchange(first.revision, "executor-a", stale.owner_epoch, stale,)
        .await
        .is_err());

    let capacity_id = ulid::Ulid::new().to_string();
    repository
        .create(record(capacity_id.clone()))
        .await
        .unwrap();
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    let before_capacity_rejections = repository
        .claim_for_connection(
            &capacity_id,
            "executor-b",
            "connection-b",
            now_ms,
            now_ms + 60_000,
        )
        .await
        .unwrap();
    let mut oversized_signal = before_capacity_rejections.record.clone();
    oversized_signal.revision += 1;
    oversized_signal.next_signal_sequence += 1;
    oversized_signal.signals.push(DurableOperationSignal {
        sequence: oversized_signal.next_signal_sequence - 1,
        request_id: "oversized-signal".to_owned(),
        name: "resume".to_owned(),
        payload: json!("x".repeat(MAX_OPERATION_SIGNAL_BYTES + 1)),
        accepted_at: "2026-09-10T00:00:00Z".to_owned(),
        acknowledged: false,
    });
    assert!(matches!(
        repository
            .compare_exchange(
                before_capacity_rejections.revision,
                "executor-b",
                before_capacity_rejections.record.owner_epoch,
                oversized_signal,
            )
            .await,
        Err(ServerError::OperationCapacityExceeded {
            field: "signal",
            ..
        })
    ));
    assert_eq!(
        repository.get(&capacity_id).await.unwrap().unwrap(),
        before_capacity_rejections
    );

    let mut oversized_record = before_capacity_rejections.record.clone();
    oversized_record.revision += 1;
    oversized_record.signals = (0..20)
        .map(|index| DurableOperationSignal {
            sequence: index + 1,
            request_id: ulid::Ulid::new().to_string(),
            name: "resume".to_owned(),
            payload: json!("x".repeat(60 * 1024)),
            acknowledged: false,
            accepted_at: "2026-09-10T00:00:00Z".to_owned(),
        })
        .collect();
    oversized_record.next_signal_sequence = oversized_record.signals.len() as u64 + 1;
    assert!(matches!(
        repository
            .compare_exchange(
                before_capacity_rejections.revision,
                "executor-b",
                before_capacity_rejections.record.owner_epoch,
                oversized_record,
            )
            .await,
        Err(ServerError::OperationCapacityExceeded {
            field: "record",
            ..
        })
    ));
    assert_eq!(
        repository.get(&capacity_id).await.unwrap().unwrap(),
        before_capacity_rejections
    );

    // Production repositories are scoped by their deployment's provisioned KV
    // bucket. Identical invocation ids are not a cross-deployment dedupe key.
    let other_bucket = format!("trellis_test_operations_{}", ulid::Ulid::new());
    let other_store = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: other_bucket.clone(),
            history: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    let other_repository = KvOperationRepository::new(other_store);
    let shared_id = ulid::Ulid::new().to_string();
    let deployment_a = repository.create(record(shared_id.clone())).await.unwrap();
    let mut other_operation = record(shared_id.clone());
    other_operation.deployment_id = "other-deployment".to_owned();
    let deployment_b = other_repository.create(other_operation).await.unwrap();
    assert_eq!(
        repository
            .create(deployment_a.record.clone())
            .await
            .unwrap(),
        deployment_a
    );
    assert_eq!(
        other_repository
            .create(deployment_b.record.clone())
            .await
            .unwrap(),
        deployment_b
    );

    // Even with the destination's correct CAS revision, a control mutation
    // carrying the other deployment's immutable identity must not cross over.
    let mut misrouted_cancel = deployment_a.record.clone();
    misrouted_cancel.revision += 1;
    misrouted_cancel.cancellation_requested = true;
    misrouted_cancel.cancellation_requested_at = Some(
        OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
    );
    assert!(other_repository
        .compare_record_exchange(deployment_b.revision, misrouted_cancel)
        .await
        .is_err());
    assert_eq!(
        other_repository.get(&shared_id).await.unwrap().unwrap(),
        deployment_b
    );

    let mut cancel_b = deployment_b.record.clone();
    cancel_b.revision += 1;
    cancel_b.cancellation_requested = true;
    cancel_b.cancellation_requested_at = Some(
        OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
    );
    let cancelled_b = other_repository
        .compare_record_exchange(deployment_b.revision, cancel_b)
        .await
        .unwrap();
    assert!(cancelled_b.record.cancellation_requested);
    assert_eq!(
        repository.get(&shared_id).await.unwrap().unwrap(),
        deployment_a
    );
    assert_eq!(
        other_repository.create(deployment_b.record).await.unwrap(),
        cancelled_b
    );

    jetstream.delete_key_value(other_bucket).await.unwrap();
    jetstream.delete_key_value(bucket).await.unwrap();
    nats.stop().unwrap();
}
