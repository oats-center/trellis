//! Built-in Events subsystem.

use std::sync::Arc;

use trellis_events_runtime::{
    secure_consumer_routes, start_dead_letter_projector, start_events_projector,
    start_exhaustion_advisory_loop, start_replay_dispatcher, ConsumerBinding,
    ConsumerBindingResolver, DeadLetterJournal, DeliveryReporter, EventsManagement, EventsQuery,
    EventsStore, ManagementAction, VerifiedEventPublisher,
};
use trellis_protocol::{ApiSurfaceKind, PermissionAction};
use trellis_rs::service::{
    internal::run_builtin_authenticated_router, RequestContext, ServerError,
};
use trellis_runtime_apis::apis::trellis_events_v1 as events;

use crate::shutdown::StopHandle;
use crate::supervisor::{RuntimeContext, RuntimeError, SubsystemHandle};
use crate::{StorageBackend, SubsystemName};

#[derive(Clone)]
struct SqliteConsumerResolver {
    path: std::path::PathBuf,
}

/// Owner-captured inventory reader; telemetry never calls the business resolver.
#[derive(Clone)]
pub(crate) struct ConsumerTelemetryReader {
    path: std::path::PathBuf,
}

impl ConsumerTelemetryReader {
    pub(crate) fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn read(&self) -> Result<Vec<ConsumerBinding>, String> {
        list_consumers(&self.path)
    }
}

impl ConsumerBindingResolver for SqliteConsumerResolver {
    fn by_resource<'a>(
        &'a self,
        resource_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>,
    > {
        let path = self.path.clone();
        let resource_id = resource_id.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || lookup_consumer(&path, Some(&resource_id), None))
                .await
                .map_err(|error| error.to_string())?
        })
    }

    fn by_consumer<'a>(
        &'a self,
        stream: &'a str,
        consumer_name: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>,
    > {
        let path = self.path.clone();
        let physical = (stream.to_owned(), consumer_name.to_owned());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || lookup_consumer(&path, None, Some(&physical)))
                .await
                .map_err(|error| error.to_string())?
        })
    }

    fn all(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<ConsumerBinding>, String>> + Send + '_>,
    > {
        let path = self.path.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || list_consumers(&path))
                .await
                .map_err(|error| error.to_string())?
        })
    }
}

fn lookup_consumer(
    path: &std::path::Path,
    resource_id: Option<&str>,
    physical: Option<&(String, String)>,
) -> Result<Option<ConsumerBinding>, String> {
    Ok(list_consumers(path)?.into_iter().find(|binding| {
        resource_id.is_none_or(|wanted| wanted == binding.resource_id)
            && physical.is_none_or(|wanted| {
                (wanted.0 == binding.stream && wanted.1 == binding.consumer_name)
                    || (wanted.0 == trellis_events_runtime::REPLAY_STREAM
                        && wanted.1 == binding.replay_consumer_name)
            })
    }))
}

fn list_consumers(path: &std::path::Path) -> Result<Vec<ConsumerBinding>, String> {
    let connection = rusqlite::Connection::open(path).map_err(|error| error.to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT owner_kind,owner_id,participant_id,local_name,binding_id,provider_identity
             FROM auth_resource_binding_evidence
             WHERE resource_kind='eventConsumer' AND state='available'",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut bindings = Vec::new();
    for row in rows {
        let (owner_kind, owner_id, participant_id, local_name, binding_id, provider) =
            row.map_err(|error| error.to_string())?;
        let provider: serde_json::Value =
            serde_json::from_str(&provider).map_err(|error| error.to_string())?;
        let Some(stream) = provider.get("stream").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(consumer) = provider.get("consumer").and_then(serde_json::Value::as_str) else {
            continue;
        };
        bindings.push(ConsumerBinding {
            resource_id: binding_id,
            owner_kind,
            owner_id,
            participant_id,
            local_name,
            stream: stream.to_owned(),
            consumer_name: consumer.to_owned(),
            filter_subjects: provider
                .get("filter_subjects")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
            replay_consumer_name: provider
                .get("replay_consumer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(bindings)
}

fn runtime_error(error: ServerError) -> RuntimeError {
    RuntimeError::Nats(error.to_string())
}

fn events_forbidden(message: impl Into<String>) -> ServerError {
    ServerError::DeclaredRpc(trellis_rs::service::DeclaredRpcError::new(
        "Forbidden",
        message,
        [("code", serde_json::json!("forbidden"))],
    ))
}

pub(crate) async fn start(context: &RuntimeContext) -> Result<SubsystemHandle, RuntimeError> {
    let owner = context.owner(crate::ownership::OwnerGroup::Events)?;
    let stop = StopHandle::new();
    let task_stop = stop.clone();
    let StorageBackend::Sqlite(storage) = context
        .config
        .events_storage_backend()
        .map_err(RuntimeError::Config)?;
    let StorageBackend::Sqlite(platform_storage) = context
        .config
        .platform_storage_backend()
        .map_err(RuntimeError::Config)?;
    let store = EventsStore::open(&storage.path)
        .map_err(|error| RuntimeError::Nats(format!("failed to open Events SQLite: {error}")))?;
    let events_runtime =
        trellis_rs::service::EventsRuntime::from_nats(context.trellis_nats.clone());
    let resolver: Arc<dyn ConsumerBindingResolver> = Arc::new(SqliteConsumerResolver {
        path: platform_storage.path.clone(),
    });
    let consumer_telemetry = ConsumerTelemetryReader::new(platform_storage.path.clone());
    let query = EventsQuery::new(store.clone(), events_runtime.clone(), Arc::clone(&resolver));
    let mut router = trellis_events_runtime::build_router_with_query(query.clone());
    let event_owner = context
        .live_providers
        .receiver(crate::platform::LiveProviderRole::Events);
    let event_stop = task_stop.clone();
    let event_verifier = Arc::new(
        move |input: trellis_events_runtime::EventAuthorizationInput| {
            let mut owner = event_owner.clone();
            let stop = event_stop.clone();
            Box::pin(async move {
                use trellis_rs::service::EventVerificationFailure;
                let owner = crate::platform::await_live_owner(&mut owner, &stop)
                    .await
                    .ok_or_else(|| {
                        EventVerificationFailure::Retryable(("Events provider stopped").into())
                    })?;
                let headers = input.headers.as_ref().ok_or_else(|| {
                    EventVerificationFailure::Rejected(("missing event headers").into())
                })?;
                let header = |name: &str| {
                    headers
                        .get(name)
                        .map(|value| value.as_str())
                        .ok_or_else(|| {
                            EventVerificationFailure::Rejected(
                                ("missing event proof header").into(),
                            )
                        })
                };
                let event: trellis_protocol::SessionEvent =
                    serde_json::from_str(header("Trellis-Session-Event")?).map_err(|_| {
                        EventVerificationFailure::Rejected(("invalid event transcript").into())
                    })?;
                let descriptor = trellis_protocol::decode_event_descriptor_identity(header(
                    "Trellis-Event-Descriptor",
                )?)
                .map_err(|_| {
                    EventVerificationFailure::Rejected(("invalid event descriptor").into())
                })?;
                trellis_protocol::validate_event_descriptor_subject(&descriptor, &input.subject)
                    .map_err(|_| {
                        EventVerificationFailure::Rejected(
                            "event subject differs from descriptor".into(),
                        )
                    })?;
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
                let proof =
                    trellis_protocol::SessionEventProof::parse(header("proof")?).map_err(|_| {
                        EventVerificationFailure::Rejected(("invalid event proof").into())
                    })?;
                let authority_owner = owner.runtime_authorization_owner();
                let transport = owner.runtime_nats();
                let status = authority_owner
                    .historical_status(&transport, &event.authority_digest)
                    .await
                    .map_err(|error| {
                        EventVerificationFailure::Retryable((error.to_string()).into())
                    })?;
                let action = trellis_protocol::CatalogActionIdentity {
                    kind: ApiSurfaceKind::Event,
                    name: event.action.clone(),
                    direction: PermissionAction::Publish,
                };
                let verified = authority_owner
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
                            EventVerificationFailure::Retryable((error.to_string()).into())
                        }
                        _ => EventVerificationFailure::Rejected((error.to_string()).into()),
                    })?;
                let authority = verified.authority().authority();
                if header("session-key")? != authority.session_public_key {
                    return Err(EventVerificationFailure::Rejected(
                        ("event session key differs from authority").into(),
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
                    participant_id,
                    deployment_id,
                    instance_id,
                    principal_id: authority.principal_id.clone(),
                    connection_id: authority.authorization_session_id.clone(),
                    login_session_id: authority.login_session_id.clone(),
                    context_digest: event.authority_digest.clone(),
                    owner_contract_id: event.api_id.clone(),
                    owner_event_name: event.action.clone(),
                })
            })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<
                                    VerifiedEventPublisher,
                                    trellis_rs::service::EventVerificationFailure,
                                >,
                            > + Send,
                    >,
                >
        },
    );
    trellis_events_runtime::register_events_watch_live(
        &mut router,
        events_runtime.clone(),
        event_verifier.clone(),
    );
    let journal = DeadLetterJournal::open(context.trellis_nats.clone())
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
    let delivery = DeliveryReporter::new(
        context.trellis_nats.clone(),
        journal.clone(),
        Arc::clone(&resolver),
        event_verifier.clone(),
    );
    let management_owner = context
        .live_providers
        .receiver(crate::platform::LiveProviderRole::Events);
    let management_stop = task_stop.clone();
    let management_path = platform_storage.path.clone();
    let management_resolver = Arc::clone(&resolver);
    let authorizer = Arc::new(
        move |request: RequestContext,
              resource_id: String,
              action: ManagementAction,
              method: &'static str| {
            let mut owner = management_owner.clone();
            let stop = management_stop.clone();
            let path = management_path.clone();
            let resolver = Arc::clone(&management_resolver);
            Box::pin(async move {
                let verified = request
                    .verified_request()
                    .ok_or_else(|| events_forbidden("verified request is missing"))?;
                let caller = request
                    .caller
                    .as_ref()
                    .ok_or_else(|| events_forbidden("verified caller is missing"))?;
                let owner = crate::platform::await_live_owner(&mut owner, &stop)
                    .await
                    .ok_or_else(|| ServerError::Nats("Events provider stopped".into()))?;
                let authority_owner = owner.runtime_authorization_owner();
                let transport = owner.runtime_nats();
                let authority = authority_owner
                    .resolve_authority(&transport, &caller.authority_digest)
                    .await
                    .map_err(|_| events_forbidden("Events authority unavailable"))?;
                if !resource_id.is_empty() {
                    let binding = resolver
                        .by_resource(&resource_id)
                        .await
                        .map_err(ServerError::Nats)?
                        .ok_or_else(|| events_forbidden("Consumer resource is unavailable"))?;
                    let owns_binding = match &caller.binding {
                        trellis_protocol::SessionBinding::Service {
                            participant_id,
                            deployment_id,
                            ..
                        }
                        | trellis_protocol::SessionBinding::Device {
                            participant_id,
                            deployment_id,
                            ..
                        } => {
                            binding.owner_kind == "deployment"
                                && participant_id == &binding.participant_id
                                && deployment_id == &binding.owner_id
                        }
                        _ => false,
                    };
                    if owns_binding && action != ManagementAction::Control {
                        use base64::Engine as _;
                        use sha2::Digest as _;
                        let deployment = binding.owner_id.clone();
                        let digest = tokio::task::spawn_blocking(move || -> Result<String, String> {
                            let connection = rusqlite::Connection::open_with_flags(path,
                                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| error.to_string())?;
                            let json: String = connection.query_row(
                                "SELECT resource_commitments_json FROM auth_deployment_bindings WHERE deployment_id=?1",
                                [deployment], |row| row.get(0)).map_err(|error| error.to_string())?;
                            let value: serde_json::Value = serde_json::from_str(&json).map_err(|error| error.to_string())?;
                            let canonical = trellis_protocol::canonicalize_json(&value).map_err(|error| error.to_string())?;
                            Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(canonical.as_bytes())))
                        }).await.map_err(|error| ServerError::Nats(error.to_string()))?
                            .map_err(ServerError::Nats)?;
                        if verified
                            .authority()
                            .authority()
                            .resource_bindings_digest
                            .as_deref()
                            == Some(&digest)
                        {
                            authority_owner
                                .assert_current(&authority)
                                .map_err(|_| events_forbidden("Events authority unavailable"))?;
                            return Ok(());
                        }
                    }
                }
                if action == ManagementAction::Consume {
                    return Err(events_forbidden(
                        "Consumer delivery reporting requires exact Consume authority",
                    ));
                }
                // The implicit runtime capability may inspect only the exact
                // owned binding above. It must not become global DLQ access via
                // the generic action-capability fallback.
                if method == "DeadLetters.Inspect"
                    && !authority.authority().capabilities.iter().any(|capability| {
                        capability.capability_id == "trellis.events@v1::deadLettersInspect"
                    })
                {
                    return Err(events_forbidden(
                        "Consumer inspection requires ownership or management authority",
                    ));
                }
                let call = trellis_protocol::CatalogActionIdentity {
                    kind: ApiSurfaceKind::Rpc,
                    name: method.into(),
                    direction: PermissionAction::Call,
                };
                authority_owner
                    .authorize_action(&transport, &authority, events::API_ID, &call)
                    .await
                    .map_err(|_| events_forbidden("Events management permission denied"))?;
                Ok(())
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<(), ServerError>> + Send>,
                >
        },
    );
    secure_consumer_routes(&mut router, query, authorizer.clone());
    EventsManagement::new(store.clone(), journal.clone(), delivery.clone(), authorizer)
        .register(&mut router);
    let consumer_runtime = events_runtime.clone();
    let mut projector = start_events_projector(events_runtime, store.clone(), event_verifier)
        .await
        .map_err(runtime_error)?;
    let mut dead_letter_projector =
        start_dead_letter_projector(context.trellis_nats.clone(), store.clone())
            .await
            .map_err(runtime_error)?;
    let mut replay_dispatcher = start_replay_dispatcher(context.trellis_nats.clone(), journal)
        .await
        .map_err(runtime_error)?;
    let sampler_store = store.clone();
    let mut advisories = start_exhaustion_advisory_loop(
        context.trellis_nats.clone(),
        crate::resources::JOBS_ADVISORIES_STREAM,
        delivery,
        store,
    )
    .await
    .map_err(runtime_error)?;
    let live_owner = context
        .live_providers
        .receiver(crate::platform::LiveProviderRole::Events);
    let join = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let _owner = owner;
        let _samplers = crate::telemetry::snapshots::SamplerOwner::start(vec![
            Box::pin(crate::telemetry::snapshots::run_events_sampler(
                sampler_store.clone(),
                task_stop.clone(),
            )),
            Box::pin(crate::telemetry::snapshots::run_consumer_sampler(
                consumer_runtime,
                consumer_telemetry,
                sampler_store.clone(),
                task_stop.clone(),
            )),
        ]);
        let api_stop = task_stop.clone();
        let api_loop = async move {
            let mut live_owner = live_owner;
            let Some(owner) = crate::platform::await_live_owner(&mut live_owner, &api_stop).await
            else {
                return Ok(());
            };
            let mut router = router;
            router.set_live_owner(owner.clone());
            let validator = owner.runtime_validator(events::API_ID);
            run_builtin_authenticated_router(owner, router, validator).await
        };
        tokio::pin!(api_loop);
        let result = {
            tokio::select! {
                biased;
                () = task_stop.stopped() => Ok(()),
                result = &mut api_loop => result.map_err(runtime_error),
                result = projector.wait() => {
                    projector.discard_completed();
                    match result {
                        Ok(()) => Err(RuntimeError::Nats(
                            "Events projector loop exited unexpectedly".to_string(),
                        )),
                        Err(error) => Err(runtime_error(error)),
                    }
                },
                result = dead_letter_projector.wait() => {
                    dead_letter_projector.discard_completed();
                    match result {
                        Ok(()) => Err(RuntimeError::Nats(
                            "dead-letter projector loop exited unexpectedly".to_owned(),
                        )),
                        Err(error) => Err(runtime_error(error)),
                    }
                },
                result = replay_dispatcher.wait() => {
                    replay_dispatcher.discard_completed();
                    match result {
                        Ok(()) => Err(RuntimeError::Nats(
                            "replay dispatcher loop exited unexpectedly".to_owned(),
                        )),
                        Err(error) => Err(runtime_error(error)),
                    }
                },
                result = advisories.wait() => {
                    match result {
                        Ok(()) => Err(RuntimeError::Nats(
                            "Consumer advisory loop exited unexpectedly".to_owned(),
                        )),
                        Err(error) => Err(runtime_error(error)),
                    }
                },
            }
        };
        task_stop.stop();
        // Telemetry samplers own their tasks and never own business lifetime.
        _samplers.stop().await;
        projector.stop().await;
        dead_letter_projector.stop().await;
        replay_dispatcher.stop().await;
        advisories.stop().await;
        result
    }));

    Ok(SubsystemHandle {
        name: SubsystemName::Events,
        stop,
        join,
    })
}
