//! Built-in Events subsystem.

use std::sync::Arc;

use trellis_events_runtime::{
    secure_consumer_routes, start_dead_letter_projector, start_events_projector,
    start_exhaustion_advisory_loop, start_replay_dispatcher, ConsumerBinding,
    ConsumerBindingResolver, DeadLetterJournal, DeliveryReporter, EventsManagement, EventsQuery,
    EventsStore, ManagementAction, VerifiedEventPublisher,
};
use trellis_protocol::{
    ApiSurfaceKind, ParticipantResourceKind, PermissionAction, PermissionAtom, PermissionTarget,
    PlatformPrivilege,
};
use trellis_rs::service::{
    internal::run_builtin_authenticated_router, RequestContext, RequestValidator, ServerError,
};
use trellis_runtime_apis::apis::trellis_events_v1::{self as events, feeds, rpc};

use crate::shutdown::StopHandle;
use crate::supervisor::{RuntimeContext, RuntimeError, SubsystemHandle};
use crate::{StorageBackend, SubsystemName};

const EVENTS_SUBJECTS: &[&str] = &[
    rpc::ConsumersInspect::SUBJECT,
    rpc::ConsumersQuery::SUBJECT,
    rpc::ConsumersReportDelivery::SUBJECT,
    rpc::DeadLettersDismiss::SUBJECT,
    rpc::DeadLettersInspect::SUBJECT,
    rpc::DeadLettersQuery::SUBJECT,
    rpc::DeadLettersReplay::SUBJECT,
    rpc::Diagnostics::SUBJECT,
    rpc::Inspect::SUBJECT,
    rpc::Metrics::SUBJECT,
    rpc::Query::SUBJECT,
    feeds::Watch::SUBJECT,
];

#[derive(Clone)]
struct SqliteConsumerResolver {
    path: std::path::PathBuf,
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
    let mut validator_join =
        crate::platform::auth::verifier::ensure_read_only(context, task_stop.clone()).await?;
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
    let query = EventsQuery::new(store.clone(), events_runtime.clone(), Arc::clone(&resolver));
    let mut router = trellis_events_runtime::build_router_with_query(query.clone());
    let verifier = context.platform_verifier.get().cloned().ok_or_else(|| {
        RuntimeError::Platform("local authorization verifier is not ready".to_owned())
    })?;
    let validator: Arc<dyn RequestValidator> = Arc::new(verifier.clone());
    let event_auth_verifier = verifier.clone();
    let event_verifier = Arc::new(
        move |input: trellis_events_runtime::EventAuthorizationInput| {
            let verifier = event_auth_verifier.clone();
            Box::pin(async move {
                verifier
                    .verify_event(
                        crate::platform::auth::verifier::RuntimeAuthorizationEventVerificationInput {
                            subject: &input.subject,
                            descriptor_identity: &input.descriptor_identity,
                            payload: &input.payload,
                            session_key: &input.session_key,
                            proof: &input.proof,
                            authorization_context: &input.authorization_context,
                            event_id: &input.event_id,
                            event_time: &input.event_time,
                        },
                    )
                    .await
                    .map(|verified| VerifiedEventPublisher {
                        kind: verified.publisher.kind,
                        deployment_id: verified.publisher.deployment_id,
                        instance_id: verified.publisher.instance_id,
                        participant_id: verified.publisher.participant_id,
                        principal_id: verified.publisher.principal_id,
                        connection_id: verified.publisher.connection_id,
                        login_session_id: verified.publisher.login_session_id,
                        context_digest: input.authorization_context,
                        owner_contract_id: verified.owner_contract_id,
                        owner_event_name: verified.owner_event_name,
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
    trellis_events_runtime::register_events_watch_feed(
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
    let management_verifier = verifier.clone();
    let management_resolver = Arc::clone(&resolver);
    let authorizer = Arc::new(
        move |request: RequestContext,
              resource_id: String,
              action: ManagementAction,
              method: &'static str| {
            let verifier = management_verifier.clone();
            let resolver = Arc::clone(&management_resolver);
            Box::pin(async move {
                let caller = request
                    .caller
                    .ok_or_else(|| events_forbidden("verified caller is missing"))?;
                if !resource_id.is_empty() {
                    let binding = resolver
                        .by_resource(&resource_id)
                        .await
                        .map_err(ServerError::Nats)?
                        .ok_or_else(|| events_forbidden("Consumer resource is unavailable"))?;
                    let owns_binding = caller.participant_id == binding.participant_id
                        && match binding.owner_kind.as_str() {
                            "deployment" => {
                                caller.deployment_id.as_deref() == Some(&binding.owner_id)
                            }
                            "user" => caller.principal_id == binding.owner_id,
                            _ => false,
                        };
                    let target = PermissionTarget::participant_resource(
                        binding.participant_id,
                        ParticipantResourceKind::EventConsumer,
                        binding.local_name,
                    )
                    .map_err(|error| events_forbidden(error.to_string()))?;
                    let permission = PermissionAtom::new(
                        target,
                        match action {
                            ManagementAction::Read => PermissionAction::Read,
                            ManagementAction::Control => PermissionAction::Control,
                            ManagementAction::Consume => PermissionAction::Consume,
                        },
                    )
                    .map_err(|error| events_forbidden(error.to_string()))?;
                    if owns_binding
                        && verifier
                            .require_cached_permission(&caller.context_digest, &permission)
                            .is_ok()
                    {
                        return Ok(());
                    }
                }
                if action == ManagementAction::Consume {
                    return Err(events_forbidden(
                        "Consumer delivery reporting requires exact Consume authority",
                    ));
                }
                let call = PermissionAtom::new(
                    PermissionTarget::api_surface(events::API_ID, ApiSurfaceKind::Rpc, method)
                        .map_err(|error| events_forbidden(error.to_string()))?,
                    PermissionAction::Call,
                )
                .map_err(|error| events_forbidden(error.to_string()))?;
                if caller
                    .platform_privileges
                    .contains(&PlatformPrivilege::Admin)
                {
                    verifier
                        .require_cached_permission(&caller.context_digest, &call)
                        .map_err(|_| events_forbidden("Events management permission denied"))?;
                    Ok(())
                } else {
                    Err(events_forbidden("Events management permission denied"))
                }
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<(), ServerError>> + Send>,
                >
        },
    );
    secure_consumer_routes(&mut router, query, authorizer.clone());
    EventsManagement::new(store.clone(), journal.clone(), delivery.clone(), authorizer)
        .register(&mut router);
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
    let mut advisories = start_exhaustion_advisory_loop(
        context.trellis_nats.clone(),
        crate::resources::JOBS_ADVISORIES_STREAM,
        delivery,
        store,
    )
    .await
    .map_err(runtime_error)?;
    let nats = context.trellis_nats.clone();
    let join = tokio::spawn(async move {
        let _owner = owner;
        let api_loop = run_builtin_authenticated_router(
            nats,
            events::API_ID,
            EVENTS_SUBJECTS,
            router,
            validator,
        );
        tokio::pin!(api_loop);
        let result = {
            let validator_exit = async {
                match validator_join.as_mut() {
                    Some(join) => match join.await {
                        Ok(Ok(())) => Err(RuntimeError::Platform(
                            "authorization validator cache exited unexpectedly".to_owned(),
                        )),
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(RuntimeError::Platform(format!(
                            "authorization validator cache task failed: {error}"
                        ))),
                    },
                    None => std::future::pending().await,
                }
            };
            tokio::pin!(validator_exit);
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
                result = &mut validator_exit => result,
            }
        };
        task_stop.stop();
        projector.stop().await;
        dead_letter_projector.stop().await;
        replay_dispatcher.stop().await;
        advisories.stop().await;
        if let Some(join) = validator_join {
            let _ = join.await;
        }
        result
    });

    Ok(SubsystemHandle {
        name: SubsystemName::Events,
        stop,
        join,
    })
}
