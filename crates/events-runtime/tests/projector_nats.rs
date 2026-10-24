use std::sync::Arc;
use std::time::Duration;
use trellis_rs::__internal::{FromProviderNats as _, RuntimeOwner as _};

use trellis_events_runtime::{
    start_events_projector, EventVerifier, EventsRuntime, EventsStore, VerifiedEventPublisher,
};
use trellis_rs::client::{PreparedTrellisEvent, ServiceConnectWithContractOptions, TrellisClient};
use trellis_rs::service::EventVerificationFailure;
use trellis_runtime_apis::apis::trellis_health_v1::events::StatusChanged;
use trellis_runtime_apis::participants::{
    trellis_events_runtime as events_participant, trellis_health_runtime,
};
use trellis_runtime_apis::types::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_projection_preserves_order_and_durable_retention_gaps() {
    let mut runtime = trellis_testkit::TrellisTestRuntime::builder()
        .start()
        .await
        .unwrap();
    let publisher_identity = runtime
        .register_service::<trellis_health_runtime::Participant>("projector publisher")
        .await
        .unwrap();
    let receiver_identity = runtime
        .register_service::<events_participant::Participant>("projector receiver")
        .await
        .unwrap();
    let server =
        trellis_rs::auth::discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
            .await
            .unwrap();
    let publisher =
        TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
            nats_servers: server.nats_servers.clone(),
            trust: server.trust.clone(),
            participant_id: publisher_identity.participant_id(),
            provisioned_identity_seed_base64url: publisher_identity.seed(),
            name: Some(publisher_identity.name()),
            timeout_ms: 10_000,
        })
        .await
        .unwrap();
    let receiver =
        TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
            nats_servers: server.nats_servers.clone(),
            trust: server.trust,
            participant_id: receiver_identity.participant_id(),
            provisioned_identity_seed_base64url: receiver_identity.seed(),
            name: Some(receiver_identity.name()),
            timeout_ms: 10_000,
        })
        .await
        .unwrap();

    let publisher =
        trellis_rs::__internal::LiveProviderOwner::from_connected_client(Arc::new(publisher));
    let receiver =
        trellis_rs::__internal::LiveProviderOwner::from_connected_client(Arc::new(receiver));

    // The production runtime's operational credentials administer the broker.
    // Publications and their verifier use separate ordinary provisioned clients.
    let config = trellis_runtime::RuntimeConfig::load_from_path(
        runtime.workdir().join("config/config.toml"),
    )
    .unwrap();
    let credentials = config.resolve_nats_runtime().unwrap();
    let observer =
        async_nats::ConnectOptions::with_credentials_file(&credentials.trellis_creds_path)
            .await
            .unwrap()
            .add_root_certificates(runtime.trust_roots()[0].clone())
            .connect(runtime.nats_url())
            .await
            .unwrap();
    let context = async_nats::jetstream::new(observer.clone());
    let mut stream = context.get_stream("trellis").await.unwrap();
    let projection_path = runtime.workdir().join("projector.sqlite");
    let store = EventsStore::open(&projection_path).unwrap();
    let owner = receiver.runtime_authorization_owner();
    let transport = receiver.runtime_nats();
    let verify: EventVerifier = Arc::new(move |input| {
        let owner = owner.clone();
        let transport = transport.clone();
        Box::pin(async move {
            let header = |name| {
                input
                    .headers
                    .as_ref()
                    .and_then(|headers| headers.get(name))
                    .map(|value| value.as_str())
                    .ok_or_else(|| {
                        EventVerificationFailure::Rejected(("missing event header").into())
                    })
            };
            let event: trellis_protocol::SessionEvent =
                serde_json::from_str(header("Trellis-Session-Event")?)
                    .map_err(|error| EventVerificationFailure::Rejected(error.to_string()))?;
            let descriptor = trellis_protocol::decode_event_descriptor_identity(header(
                "Trellis-Event-Descriptor",
            )?)
            .map_err(|error| EventVerificationFailure::Rejected(error.to_string()))?;
            trellis_protocol::validate_event_descriptor_subject(&descriptor, &input.subject)
                .map_err(|error| EventVerificationFailure::Rejected(error.to_string()))?;
            if event.subject != input.subject
                || descriptor.api_id() != event.api_id
                || descriptor.event_name() != event.action
                || header("authorization-context")? != event.authority_digest
                || header("Nats-Msg-Id")? != event.event_id
                || header("Trellis-Event-Time")? != event.event_time
            {
                return Err(EventVerificationFailure::Rejected(
                    ("event headers differ from signed transcript").into(),
                ));
            }
            let proof = trellis_protocol::SessionEventProof::parse(header("proof")?)
                .map_err(|error| EventVerificationFailure::Rejected(error.to_string()))?;
            let status = owner
                .historical_status(&transport, &event.authority_digest)
                .await
                .map_err(|error| EventVerificationFailure::Retryable(error.to_string()))?;
            let action = trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Event,
                name: event.action.clone(),
                direction: trellis_protocol::PermissionAction::Publish,
            };
            let verified = owner
                .verify_historical_event(
                    &transport,
                    trellis_rs::client::HistoricalEventVerificationInput {
                        event: &event,
                        payload: &input.payload,
                        proof: &proof,
                        action: &action,
                        original_publication: &input.original_publication,
                        status: &status,
                    },
                )
                .await
                .map_err(|error| match error {
                    trellis_rs::client::TrellisClientError::AuthorizationUnavailable(_) => {
                        EventVerificationFailure::Retryable(error.to_string())
                    }
                    _ => EventVerificationFailure::Rejected(error.to_string()),
                })?;
            let authority = verified.authority().authority();
            if header("session-key")? != authority.session_public_key {
                return Err(EventVerificationFailure::Rejected(
                    ("event key differs from signed authority").into(),
                ));
            }
            let (kind, participant_id, deployment_id, instance_id) = match &authority.binding {
                trellis_protocol::SessionBinding::Service {
                    participant_id,
                    deployment_id,
                    instance_id,
                } => (
                    "service",
                    participant_id.clone(),
                    Some(deployment_id.clone()),
                    Some(instance_id.clone()),
                ),
                trellis_protocol::SessionBinding::Device {
                    participant_id,
                    deployment_id,
                    instance_id,
                    ..
                } => (
                    "device",
                    participant_id.clone(),
                    Some(deployment_id.clone()),
                    Some(instance_id.clone()),
                ),
                trellis_protocol::SessionBinding::Browser { client_id, .. }
                | trellis_protocol::SessionBinding::Native { client_id, .. } => {
                    ("user", client_id.clone(), None, None)
                }
            };
            Ok(VerifiedEventPublisher {
                kind: kind.into(),
                deployment_id,
                instance_id,
                participant_id,
                principal_id: authority.principal_id.clone(),
                connection_id: authority.runtime_id.clone(),
                login_session_id: authority.login_session_id.clone(),
                context_digest: event.authority_digest.clone(),
                owner_contract_id: event.api_id.clone(),
                owner_event_name: event.action.clone(),
            })
        })
    });

    let prepare = || {
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        trellis_rs::client::prepare_event::<StatusChanged>(&HealthStatusChangedEvent {
            changed_at: now.clone(),
            last_seen_at: now.clone(),
            header: HealthStatusChangedEventheader {
                id: HealthStatusChangedEventheaderId(ulid::Ulid::new().to_string()),
                time: now,
                extra: Default::default(),
            },
            participant: HealthStatusChangedEventparticipant {
                contract_id: publisher_identity.participant_id().into(),
                deployment_id: publisher_identity.deployment_id().into(),
                instance_id: publisher_identity.instance_id().into(),
                kind: HealthStatusChangedEventparticipantKind::Service,
                name: publisher_identity.name().into(),
                extra: Default::default(),
            },
            previous_status: HealthStatusChangedEventPreviousStatus::Healthy,
            reported_status: HealthStatusChangedEventReportedStatus::Healthy,
            status: HealthStatusChangedEventStatus::Healthy,
            reason: "projection acceptance".into(),
            summary: None,
            extra: Default::default(),
        })
        .unwrap()
    };
    let wait_for_event = |store: EventsStore, event: &PreparedTrellisEvent| {
        let id = event.event_id().to_owned();
        async move {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(row) = store.inspect_event(Some(&id), None).unwrap() {
                        break row["event"]["row"].clone();
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("signed event must project through real historical authorization")
        }
    };
    let event_runtime = EventsRuntime::from_nats(observer.clone());
    let projector = start_events_projector(event_runtime.clone(), store.clone(), verify.clone())
        .await
        .unwrap();
    let first_event = prepare();
    publisher.publish_prepared(&first_event).await.unwrap();
    let second_event = prepare();
    publisher.publish_prepared(&second_event).await.unwrap();
    let first = wait_for_event(store.clone(), &first_event).await;
    let second = wait_for_event(store.clone(), &second_event).await;
    let first_sequence = first["streamSequence"].as_u64().unwrap();
    let second_sequence = second["streamSequence"].as_u64().unwrap();
    assert!(first_sequence < second_sequence);
    for row in [&first, &second] {
        assert_eq!(row["verificationStatus"], "verified");
        assert_eq!(
            row["publisherDeploymentId"],
            publisher_identity.deployment_id()
        );
        assert_eq!(row["publisherInstanceId"], publisher_identity.instance_id());
    }
    assert_eq!(store.diagnostics().unwrap()["gapDetected"], false);
    projector.stop().await;
    drop(store);

    // Truncate real broker history while this projection is offline, leaving a
    // signed retained record beyond an unprojected publication.
    let discarded = prepare();
    publisher.publish_prepared(&discarded).await.unwrap();
    let retained = prepare();
    publisher.publish_prepared(&retained).await.unwrap();
    let retained_sequence = tokio::time::timeout(Duration::from_secs(10), async {
        let mut sequence = second_sequence + 1;
        loop {
            let last = stream.info().await.unwrap().state.last_sequence;
            while sequence <= last {
                let raw = stream.get_raw_message(sequence).await.unwrap();
                if raw
                    .headers
                    .get("Nats-Msg-Id")
                    .is_some_and(|value| value.as_str() == retained.event_id())
                {
                    break;
                }
                sequence += 1;
            }
            if sequence <= last {
                break sequence;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    stream.purge().sequence(retained_sequence).await.unwrap();

    let reopened = EventsStore::open(&projection_path).unwrap();
    let projector = start_events_projector(event_runtime.clone(), reopened.clone(), verify.clone())
        .await
        .unwrap();
    let replayed = wait_for_event(reopened.clone(), &retained).await;
    assert_eq!(replayed["verificationStatus"], "verified");
    assert!(reopened
        .inspect_event(Some(discarded.event_id()), None)
        .unwrap()
        .is_none());
    assert_eq!(reopened.diagnostics().unwrap()["gapDetected"], true);
    assert!(reopened
        .inspect_event(Some(first_event.event_id()), None)
        .unwrap()
        .is_some());
    projector.stop().await;
    drop(reopened);
    assert_eq!(
        EventsStore::open(&projection_path)
            .unwrap()
            .diagnostics()
            .unwrap()["gapDetected"],
        true
    );

    // The same truncation must also be detected by a completely new projection.
    let rebuilt = EventsStore::open(runtime.workdir().join("rebuilt-projector.sqlite")).unwrap();
    let projector = start_events_projector(event_runtime, rebuilt.clone(), verify.clone())
        .await
        .unwrap();
    let rebuilt_event = wait_for_event(rebuilt.clone(), &retained).await;
    assert_eq!(rebuilt_event["verificationStatus"], "verified");
    assert_eq!(rebuilt.diagnostics().unwrap()["gapDetected"], true);
    projector.stop().await;

    // Retire the real publisher session through a separately approved native
    // administrator, then compare retained broker chronology with a new publish
    // of exactly the same previously signed bytes.
    let server =
        trellis_rs::auth::discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
            .await
            .unwrap();
    let native = trellis_rs::auth::NativeOAuthClient::open(
        runtime.workdir().join("historical-admin"),
        trellis_rs::auth::NativeCredentialScope {
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
    let device = native
        .start_device(&serde_json::json!([
            {
                "type":"trellis_capabilities",
                "required": [
                    "trellis.auth.v1:users.manage",
                    "trellis.auth.v1:users.manage"
                            ],
                "optional": []
            }
        ]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    let session = device.complete().await.unwrap();
    let generated = trellis_rs::generated::Client::connect_user(
        trellis_rs::client::UserConnectOptions::new(server.nats_servers, 30_000, session),
    )
    .await
    .unwrap();
    let admin = trellis_runtime_apis::apis::trellis_auth_v1::Client::from_generated(generated);
    let sessions = admin
        .authorization_sessions_list(
            &serde_json::from_value(serde_json::json!({
                "deploymentId": publisher_identity.deployment_id(), "state": "active"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let raw = stream.get_raw_message(retained_sequence).await.unwrap();
    assert!(
        !sessions.items.is_empty(),
        "publisher has an active session"
    );
    let mut config = stream.info().await.unwrap().config.clone();
    config.duplicate_window = Duration::from_millis(100);
    let replay_after = raw.time + config.duplicate_window;
    context.update_stream(config).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        // A second-boundary cutoff alone can leave this publication less than
        // 100 ms old. Wait for both cutoff precision and broker dedup expiry.
        while time::OffsetDateTime::now_utc().unix_timestamp() <= raw.time.unix_timestamp()
            || time::OffsetDateTime::now_utc() <= replay_after
        {
            tick.tick().await;
        }
    })
    .await
    .unwrap();
    for session in sessions.items {
        admin
            .authorization_sessions_revoke(&AuthAuthorizationSessionRevokeRequest {
                authorization_session_id: session.authorization_session_id,
                idempotency_key: ulid::Ulid::new().to_string().into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
    }
    let original = trellis_protocol::OriginalEventPublication {
        published_at: raw
            .time
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        stream_sequence: trellis_protocol::U64s::new(raw.sequence),
    };
    verify(trellis_events_runtime::EventAuthorizationInput {
        subject: raw.subject.to_string(),
        payload: raw.payload.to_vec(),
        headers: Some(raw.headers.clone()),
        original_publication: original,
    })
    .await
    .expect("retirement preserves genuine pre-cutoff backlog");
    let replay_sequence = context
        .publish_with_headers(
            raw.subject.clone(),
            raw.headers.clone(),
            raw.payload.clone(),
        )
        .await
        .unwrap()
        .await
        .unwrap()
        .sequence;
    assert!(replay_sequence > retained_sequence);
    let late = stream.get_raw_message(replay_sequence).await.unwrap();
    assert!(matches!(
        verify(trellis_events_runtime::EventAuthorizationInput {
            subject: late.subject.to_string(),
            payload: late.payload.to_vec(),
            headers: Some(late.headers),
            original_publication: trellis_protocol::OriginalEventPublication {
                published_at: late
                    .time
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap(),
                stream_sequence: trellis_protocol::U64s::new(late.sequence),
            },
        })
        .await,
        Err(EventVerificationFailure::Rejected(_))
    ));
    drop(admin);
    publisher.shutdown_native_runtime().await.unwrap();
    receiver.shutdown_native_runtime().await.unwrap();
    drop(observer);
    runtime.shutdown().await.unwrap();
}
