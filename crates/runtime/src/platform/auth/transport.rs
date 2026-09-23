use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use trellis_protocol::{
    ApiSurfaceKind, AuthorizationPrincipalKind, ParticipantResourceKind, PermissionAction,
    PermissionAtom, UnsignedAuthorizationContext,
};
use trellis_rs::client::AuthorizationApiBinding;

use super::evidence::{ApiRuntimeProjection, RuntimeActionKind};
use super::{
    AuthorizationRegistryBinding, AuthorizationStateError, ParticipantBindingRecord,
    ResourceBindingEvidence, ResourceProviderIdentity,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TransportPermissions {
    pub publish: Vec<String>,
    pub subscribe: Vec<String>,
}

pub(crate) fn compile_transport_permissions(
    context: &UnsignedAuthorizationContext,
    binding: &ParticipantBindingRecord,
    resource_bindings: &[ResourceBindingEvidence],
    api_bindings: &std::collections::BTreeMap<String, AuthorizationApiBinding>,
    registry: &AuthorizationRegistryBinding,
) -> Result<TransportPermissions, AuthorizationStateError> {
    if binding.participant_id != context.participant_id {
        return invalid("issuable state does not match participant binding");
    }
    let resolved = binding.resolve()?;

    let mut publish = BTreeSet::new();
    let mut subscribe = BTreeSet::new();
    compile_authorization_registry_transport(
        &context.inbox_prefix,
        &registry.context_bucket,
        &mut publish,
        &mut subscribe,
    );
    if matches!(
        context.principal_kind,
        AuthorizationPrincipalKind::Service | AuthorizationPrincipalKind::Device
    ) {
        publish.insert("$JS.API.INFO".to_owned());
        if matches!(
            context.principal_kind,
            AuthorizationPrincipalKind::Service | AuthorizationPrincipalKind::Device
        ) {
            let deployment_id = context
                .deployment_id
                .as_deref()
                .ok_or_else(|| invalid_error("deployed provider is missing deployment identity"))?;
            let bucket = format!("trellis_operations_{deployment_id}");
            let stream = format!("KV_{bucket}");
            publish.insert(format!("$KV.{bucket}.>"));
            publish.insert(format!("$JS.API.STREAM.CREATE.{stream}"));
            publish.insert(format!("$JS.API.CONSUMER.DELETE.{stream}.>"));
            publish.insert(format!("$JS.API.$KV.{bucket}.>"));
            kv_read(&bucket, &mut publish);

            let staging = format!("trellis_operation_staging_{deployment_id}");
            let staging_stream = format!("OBJ_{staging}");
            publish.insert(format!("$O.{staging}.C.>"));
            publish.insert(format!("$O.{staging}.M.>"));
            publish.insert(format!("$JS.API.STREAM.INFO.{staging_stream}"));
            publish.insert(format!("$JS.API.STREAM.MSG.GET.{staging_stream}"));
            publish.insert(format!("$JS.API.STREAM.PURGE.{staging_stream}"));
            publish.insert(format!("$JS.API.CONSUMER.CREATE.{staging_stream}"));
            publish.insert(format!("$JS.API.CONSUMER.CREATE.{staging_stream}.>"));
            publish.insert(format!("$JS.API.CONSUMER.INFO.{staging_stream}.>"));
            publish.insert(format!("$JS.API.CONSUMER.MSG.NEXT.{staging_stream}.>"));
            publish.insert(format!("$JS.API.CONSUMER.DELETE.{staging_stream}.>"));
            publish.insert(format!("$JS.FC.{staging_stream}.>"));
            publish.insert(format!("$JS.ACK.{staging_stream}.>"));
        }
        let deployment_id = context.deployment_id.as_deref().ok_or_else(|| {
            invalid_error("deployed principal is missing deployment identity".to_owned())
        })?;
        let instance_id = context.instance_id.as_deref().ok_or_else(|| {
            invalid_error("deployed principal is missing instance identity".to_owned())
        })?;
        let kind = match context.principal_kind {
            AuthorizationPrincipalKind::Service => "service",
            AuthorizationPrincipalKind::Device => "device",
            AuthorizationPrincipalKind::User => unreachable!(),
        };
        publish.insert(format!(
            "health.v1.heartbeat.{kind}.{}.{}.{}.{}.{}",
            URL_SAFE_NO_PAD.encode(context.participant_id.as_bytes()),
            URL_SAFE_NO_PAD.encode(binding.participant_digest.as_bytes()),
            URL_SAFE_NO_PAD.encode(deployment_id.as_bytes()),
            URL_SAFE_NO_PAD.encode(instance_id.as_bytes()),
            context.session_key,
        ));
    }

    let deployment_id = context.deployment_id.as_deref();
    for (api_id, api) in &resolved.implemented_apis {
        let deployment_id = deployment_id
            .ok_or_else(|| invalid_error("API provider is missing deployment identity"))?;
        let instance_id = context
            .instance_id
            .as_deref()
            .ok_or_else(|| invalid_error("API provider is missing instance identity"))?;
        let session_prefix = &context.session_key[..16.min(context.session_key.len())];
        for (key, action) in &api.actions {
            let name = key.split_once(':').map_or(key.as_str(), |(_, name)| name);
            compile_provider_action(
                api_id,
                ProviderActionIdentity {
                    deployment_id,
                    instance_id,
                    session_prefix,
                    connection_id: &context.connection_id,
                },
                name,
                action,
                &mut publish,
                &mut subscribe,
            )?;
        }
    }

    for atom in context.grants.permissions() {
        if let Some((api_id, _, _)) = atom.target().as_api_surface() {
            let api = resolved
                .referenced_apis
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("grant references unknown API {api_id}")))?;
            compile_api_surface(
                api,
                api_bindings,
                &context.connection_id,
                atom,
                &mut publish,
                &mut subscribe,
            )?;
        } else if let Some((api_id, operation, _signal)) = atom.target().as_operation_signal() {
            let api = resolved
                .referenced_apis
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("grant references unknown API {api_id}")))?;
            if !api.actions.contains_key(&format!("operation:{operation}")) {
                return invalid("operation signal grant references unknown operation");
            }
            if atom.action() != PermissionAction::Control {
                return invalid("operation signal grant must use control action");
            }
            let deployment_id = &api_bindings
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("API {api_id} is not bound")))?
                .provider_deployment_id;
            let subject =
                trellis_protocol::derive_bound_operation_subject(api_id, deployment_id, operation)
                    .map_err(|error| invalid_error(error.to_string()))?;
            publish.insert(format!("{subject}.control"));
        } else if let Some((participant_id, kind, name)) = atom.target().as_participant_resource() {
            if participant_id != context.participant_id {
                return invalid("resource grant belongs to another participant");
            }
            let resource = resource_binding(resource_bindings, kind, name)?;
            if kind == ParticipantResourceKind::State {
                let binding = api_bindings
                    .get(trellis_runtime_apis::apis::trellis_state_v1::API_ID)
                    .ok_or_else(|| invalid_error("State API binding is unavailable"))?;
                let action = match atom.action() {
                    PermissionAction::Read => "Get",
                    PermissionAction::Write => "Put",
                    PermissionAction::Delete => "Delete",
                    _ => return invalid("resource action does not match State binding"),
                };
                publish.insert(
                    trellis_protocol::derive_bound_rpc_subject(
                        trellis_runtime_apis::apis::trellis_state_v1::API_ID,
                        &binding.provider_deployment_id,
                        action,
                    )
                    .map_err(|error| invalid_error(error.to_string()))?,
                );
            } else if kind == ParticipantResourceKind::EventConsumer {
                let binding = api_bindings
                    .get(trellis_runtime_apis::apis::trellis_events_v1::API_ID)
                    .ok_or_else(|| invalid_error("Events API binding is unavailable"))?;
                let methods: &[&str] = match atom.action() {
                    PermissionAction::Consume => &["Consumers.ReportDelivery"],
                    PermissionAction::Read => &[
                        "Consumers.Query",
                        "Consumers.Inspect",
                        "DeadLetters.Query",
                        "DeadLetters.Inspect",
                    ],
                    PermissionAction::Control => &["DeadLetters.Replay", "DeadLetters.Dismiss"],
                    _ => return invalid("resource action does not match Consumer binding"),
                };
                for method in methods {
                    publish.insert(
                        trellis_protocol::derive_bound_rpc_subject(
                            trellis_runtime_apis::apis::trellis_events_v1::API_ID,
                            &binding.provider_deployment_id,
                            method,
                        )
                        .map_err(|error| invalid_error(error.to_string()))?,
                    );
                }
                if atom.action() == PermissionAction::Consume {
                    compile_resource(resource, atom.action(), &mut publish, &mut subscribe)?;
                }
            } else {
                compile_resource(resource, atom.action(), &mut publish, &mut subscribe)?;
            }
        }
    }

    Ok(TransportPermissions {
        publish: publish.into_iter().collect(),
        subscribe: subscribe.into_iter().collect(),
    })
}

struct ProviderActionIdentity<'a> {
    deployment_id: &'a str,
    instance_id: &'a str,
    session_prefix: &'a str,
    connection_id: &'a str,
}

fn compile_provider_action(
    api_id: &str,
    provider: ProviderActionIdentity<'_>,
    name: &str,
    action: &super::evidence::ActionRuntimeProjection,
    publish: &mut BTreeSet<String>,
    subscribe: &mut BTreeSet<String>,
) -> Result<(), AuthorizationStateError> {
    let ProviderActionIdentity {
        deployment_id,
        instance_id,
        session_prefix,
        connection_id,
    } = provider;
    let own_token = URL_SAFE_NO_PAD.encode(connection_id.as_bytes());
    match action.kind {
        RuntimeActionKind::Rpc => {
            subscribe.insert(
                trellis_protocol::derive_bound_rpc_subject(api_id, deployment_id, name)
                    .map_err(|error| invalid_error(error.to_string()))?,
            );
            if action.download {
                subscribe.insert(format!("transfer.v1.download.{session_prefix}.*"));
            }
        }
        RuntimeActionKind::Operation => {
            let subject =
                trellis_protocol::derive_bound_operation_subject(api_id, deployment_id, name)
                    .map_err(|error| invalid_error(error.to_string()))?;
            subscribe.insert(subject.clone());
            subscribe.insert(format!("{subject}.control"));
            subscribe.insert(format!("{subject}.updates.*"));
            publish.insert(format!("{subject}.updates.*"));
            // The observer replica answers owner-directed live controls and
            // publishes its signed frames over the live delivery family.
            subscribe.insert(format!("{subject}.observe.{own_token}.*"));
            publish.insert(format!("live.v1.data.{own_token}.*.*"));
            if action.upload {
                subscribe.insert(format!("transfer.v1.upload.{session_prefix}.*"));
            }
        }
        RuntimeActionKind::Live => {
            let subject = trellis_protocol::derive_bound_live_subject(api_id, deployment_id, name)
                .map_err(|error| invalid_error(error.to_string()))?;
            subscribe.insert(subject.clone());
            subscribe.insert(trellis_protocol::derive_live_control_subject(
                &subject,
                instance_id,
            ));
            // Live sessions replace the retained-reply model: the provider
            // owns exact nonqueued controls for this connection and publishes
            // signed frames only to its own connection-scoped data prefix.
            subscribe.insert(format!("{subject}.observe.{own_token}.*"));
            publish.insert(format!("live.v1.data.{own_token}.*.*"));
        }
        RuntimeActionKind::Event => {}
    }
    Ok(())
}

fn compile_authorization_registry_transport(
    inbox_prefix: &str,
    context_bucket: &str,
    publish: &mut BTreeSet<String>,
    subscribe: &mut BTreeSet<String>,
) {
    // Contexts and exact revocation watches are public protocol evidence, not
    // participant resources. Issuer keys are resolved over the configured HTTPS origin.
    let context_stream = format!("KV_{context_bucket}");
    publish.insert("$JS.API.INFO".to_owned());
    publish.insert(format!("$JS.FC.{context_stream}.>"));
    publish.insert(format!(
        "$JS.API.DIRECT.GET.{context_stream}.$KV.{context_bucket}.*"
    ));
    publish.insert(format!(
        "$JS.API.CONSUMER.CREATE.{context_stream}.*.$KV.{context_bucket}.revocation.*"
    ));
    publish.insert(format!("$JS.API.CONSUMER.INFO.{context_stream}.*"));
    subscribe.insert(format!("{inbox_prefix}.>"));
}

fn compile_api_surface(
    api: &ApiRuntimeProjection,
    api_bindings: &std::collections::BTreeMap<String, AuthorizationApiBinding>,
    connection_id: &str,
    permission: &PermissionAtom,
    publish: &mut BTreeSet<String>,
    subscribe: &mut BTreeSet<String>,
) -> Result<(), AuthorizationStateError> {
    let (api_id, surface, name) = permission
        .target()
        .as_api_surface()
        .ok_or_else(|| invalid_error("permission does not target an API surface"))?;
    let projected = api
        .actions
        .get(&format!("{}:{name}", surface_name(surface)))
        .ok_or_else(|| invalid_error(format!("unknown {surface:?} {name}")))?;
    match (surface, permission.action()) {
        (ApiSurfaceKind::Rpc, PermissionAction::Call) => {
            let deployment_id = &api_bindings
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("API {api_id} is not bound")))?
                .provider_deployment_id;
            publish.insert(
                trellis_protocol::derive_bound_rpc_subject(api_id, deployment_id, name)
                    .map_err(|error| invalid_error(error.to_string()))?,
            );
            if projected.download {
                publish.insert("transfer.v1.download.*.*".to_owned());
            }
        }
        (ApiSurfaceKind::Operation, PermissionAction::Invoke) => {
            let deployment_id = &api_bindings
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("API {api_id} is not bound")))?
                .provider_deployment_id;
            let subject =
                trellis_protocol::derive_bound_operation_subject(api_id, deployment_id, name)
                    .map_err(|error| invalid_error(error.to_string()))?;
            publish.insert(subject.clone());
            publish.insert(format!("{subject}.control"));
            if projected.upload {
                publish.insert("transfer.v1.upload.*.*".to_owned());
            }
        }
        (ApiSurfaceKind::Operation, PermissionAction::Observe) => {
            let deployment_id = &api_bindings
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("API {api_id} is not bound")))?
                .provider_deployment_id;
            let subject =
                trellis_protocol::derive_bound_operation_subject(api_id, deployment_id, name)
                    .map_err(|error| invalid_error(error.to_string()))?;
            publish.insert(format!("{subject}.control"));
            // Exact Observe authority is what allows this caller to own live
            // observations of the bound route and receive its own delivery.
            publish.insert(format!("{subject}.observe.*.*"));
            subscribe.insert(consumer_live_data_subscription(connection_id));
        }
        (ApiSurfaceKind::Operation, PermissionAction::Cancel | PermissionAction::Control) => {
            let deployment_id = &api_bindings
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("API {api_id} is not bound")))?
                .provider_deployment_id;
            let subject =
                trellis_protocol::derive_bound_operation_subject(api_id, deployment_id, name)
                    .map_err(|error| invalid_error(error.to_string()))?;
            publish.insert(format!("{subject}.control"));
        }
        (ApiSurfaceKind::Event, PermissionAction::Publish) => {
            publish.insert(
                trellis_protocol::derive_event_wildcard_subject(
                    api_id,
                    name,
                    projected.event_parameter_count,
                )
                .map_err(|error| invalid_error(error.to_string()))?,
            );
        }
        (ApiSurfaceKind::Event, PermissionAction::Subscribe) => {
            subscribe.insert(
                trellis_protocol::derive_event_wildcard_subject(
                    api_id,
                    name,
                    projected.event_parameter_count,
                )
                .map_err(|error| invalid_error(error.to_string()))?,
            );
        }
        (ApiSurfaceKind::Live, PermissionAction::Subscribe) => {
            let deployment_id = &api_bindings
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("API {api_id} is not bound")))?
                .provider_deployment_id;
            let subject = trellis_protocol::derive_bound_live_subject(api_id, deployment_id, name)
                .map_err(|error| invalid_error(error.to_string()))?;
            publish.insert(subject.clone());
            publish.insert(format!("{subject}.control.*.*"));
            // The exact Live Subscribe grant also authorizes this caller's own
            // live observation controls and connection-scoped delivery.
            publish.insert(format!("{subject}.observe.*.*"));
            subscribe.insert(consumer_live_data_subscription(connection_id));
        }
        _ => return invalid("grant action does not match API surface"),
    }
    Ok(())
}

fn consumer_live_data_subscription(connection_id: &str) -> String {
    format!(
        "live.v1.data.*.{}.*",
        URL_SAFE_NO_PAD.encode(connection_id.as_bytes())
    )
}

fn surface_name(surface: ApiSurfaceKind) -> &'static str {
    match surface {
        ApiSurfaceKind::Rpc => "rpc",
        ApiSurfaceKind::Operation => "operation",
        ApiSurfaceKind::Event => "event",
        ApiSurfaceKind::Live => "live",
        ApiSurfaceKind::State => "state",
    }
}

fn resource_binding<'a>(
    resources: &'a [ResourceBindingEvidence],
    kind: ParticipantResourceKind,
    name: &str,
) -> Result<&'a ResourceBindingEvidence, AuthorizationStateError> {
    let kind = match kind {
        ParticipantResourceKind::Kv => "kv",
        ParticipantResourceKind::Store => "store",
        ParticipantResourceKind::JobQueue => "jobQueue",
        ParticipantResourceKind::EventConsumer => "eventConsumer",
        ParticipantResourceKind::State => "state",
    };
    resources
        .iter()
        .find(|resource| {
            resource.resource_kind == kind
                && resource.local_name == name
                && resource.state == super::ResourceBindingState::Available
        })
        .ok_or_else(|| invalid_error(format!("missing physical binding for {kind} {name}")))
}

fn compile_resource(
    resource: &ResourceBindingEvidence,
    action: PermissionAction,
    publish: &mut BTreeSet<String>,
    subscribe: &mut BTreeSet<String>,
) -> Result<(), AuthorizationStateError> {
    match (&resource.provider_identity, action) {
        (ResourceProviderIdentity::Kv { bucket }, PermissionAction::Read) => {
            kv_read(bucket, publish);
        }
        (ResourceProviderIdentity::Kv { bucket }, PermissionAction::Write)
        | (ResourceProviderIdentity::Kv { bucket }, PermissionAction::Delete) => {
            publish.insert(format!("$KV.{bucket}.>"));
            publish.insert(format!("$JS.API.STREAM.INFO.KV_{bucket}"));
        }
        (
            ResourceProviderIdentity::State { .. },
            PermissionAction::Read | PermissionAction::Write | PermissionAction::Delete,
        ) => {}
        (ResourceProviderIdentity::Store { bucket }, PermissionAction::Read) => {
            let stream = format!("OBJ_{bucket}");
            publish.insert("$JS.API.INFO".to_owned());
            publish.insert(format!("$JS.API.STREAM.INFO.{stream}"));
            publish.insert(format!("$JS.API.STREAM.MSG.GET.{stream}"));
            publish.insert(format!("$JS.API.CONSUMER.CREATE.{stream}"));
            publish.insert(format!("$JS.API.CONSUMER.CREATE.{stream}.>"));
            publish.insert(format!("$JS.API.CONSUMER.INFO.{stream}.>"));
            publish.insert(format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.>"));
            publish.insert(format!("$JS.API.CONSUMER.DELETE.{stream}.>"));
            publish.insert(format!("$JS.FC.{stream}.>"));
            publish.insert(format!("$JS.ACK.{stream}.>"));
        }
        (
            ResourceProviderIdentity::Store { bucket },
            PermissionAction::Write | PermissionAction::Delete,
        ) => {
            publish.insert(format!("$O.{bucket}.C.>"));
            publish.insert(format!("$O.{bucket}.M.>"));
            publish.insert(format!("$JS.API.STREAM.PURGE.OBJ_{bucket}"));
        }
        (
            ResourceProviderIdentity::JobQueue {
                namespace: _,
                work_stream,
                publish_prefix,
                updates_prefix,
                ..
            },
            PermissionAction::Submit,
        ) => {
            publish.insert(format!("{publish_prefix}.>"));
            publish.insert(format!("$JS.API.CONSUMER.INFO.{work_stream}.>"));
            if let Some(prefix) = updates_prefix {
                subscribe.insert(format!("{prefix}.>"));
            }
        }
        (
            ResourceProviderIdentity::JobQueue {
                namespace,
                work_stream,
                publish_prefix,
                work_subject,
                consumer,
                updates_prefix,
                ..
            },
            PermissionAction::Process,
        ) => {
            let keys_bucket = format!("JOBS_KEYS_{namespace}");
            subscribe.insert(work_subject.clone());
            subscribe.insert(format!("{publish_prefix}.>"));
            publish.insert(format!("{publish_prefix}.>"));
            publish.insert(format!("trellis.jobs.workers.{namespace}.>"));
            publish.insert("$JS.API.DIRECT.GET.JOBS".to_owned());
            publish.insert("$JS.API.DIRECT.GET.JOBS.>".to_owned());
            publish.insert("$JS.API.STREAM.MSG.GET.JOBS".to_owned());
            publish.insert(format!("$KV.{keys_bucket}.>"));
            kv_read(&keys_bucket, publish);
            publish.insert(format!("$JS.API.STREAM.INFO.{work_stream}"));
            publish.insert(format!("$JS.API.CONSUMER.INFO.{work_stream}.{consumer}"));
            publish.insert(format!("$JS.API.CONSUMER.MSG.NEXT.{work_stream}.>"));
            publish.insert(format!("$JS.ACK.{work_stream}.>"));
            if let Some(prefix) = updates_prefix {
                publish.insert(format!("{prefix}.>"));
                subscribe.insert(format!("{prefix}.>"));
            }
        }
        (
            ResourceProviderIdentity::EventConsumer {
                stream,
                consumer,
                replay_consumer,
                ..
            },
            PermissionAction::Consume,
        ) => {
            publish.insert(format!("$JS.API.CONSUMER.INFO.{stream}.{consumer}"));
            publish.insert(format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.{consumer}"));
            publish.insert(format!("$JS.ACK.{stream}.{consumer}.>"));
            publish.insert(format!(
                "$JS.API.CONSUMER.INFO.{}.{replay_consumer}",
                trellis_events_runtime::REPLAY_STREAM
            ));
            publish.insert(format!(
                "$JS.API.CONSUMER.MSG.NEXT.{}.{replay_consumer}",
                trellis_events_runtime::REPLAY_STREAM
            ));
            publish.insert(format!(
                "$JS.ACK.{}.{replay_consumer}.>",
                trellis_events_runtime::REPLAY_STREAM
            ));
        }
        _ => return invalid("resource action does not match physical binding"),
    }
    Ok(())
}

fn kv_read(bucket: &str, publish: &mut BTreeSet<String>) {
    let stream = format!("KV_{bucket}");
    publish.insert(format!("$JS.API.STREAM.INFO.{stream}"));
    publish.insert(format!("$JS.API.STREAM.MSG.GET.{stream}"));
    publish.insert(format!("$JS.API.DIRECT.GET.{stream}"));
    publish.insert(format!("$JS.API.DIRECT.GET.{stream}.>"));
    publish.insert(format!("$JS.API.CONSUMER.CREATE.{stream}"));
    publish.insert(format!("$JS.API.CONSUMER.CREATE.{stream}.>"));
    publish.insert(format!("$JS.API.CONSUMER.INFO.{stream}.>"));
    publish.insert(format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.>"));
    publish.insert(format!("$JS.ACK.{stream}.>"));
}

fn invalid<T>(message: impl Into<String>) -> Result<T, AuthorizationStateError> {
    Err(invalid_error(message))
}

fn invalid_error(message: impl Into<String>) -> AuthorizationStateError {
    AuthorizationStateError::InvalidRecord(message.into())
}

#[cfg(test)]
mod tests {
    use super::{
        compile_api_surface, compile_authorization_registry_transport, compile_provider_action,
        compile_resource, resource_binding, ProviderActionIdentity,
    };
    use crate::platform::auth::{
        evidence::{ActionRuntimeProjection, ApiRuntimeProjection, RuntimeActionKind},
        ResourceBindingEvidence, ResourceBindingState, ResourceProviderIdentity,
    };
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use std::collections::{BTreeMap, BTreeSet};
    use trellis_protocol::{ApiSurfaceKind, PermissionAction, PermissionAtom, PermissionTarget};
    use trellis_rs::client::AuthorizationApiBinding;

    fn api_permission(
        api_id: &str,
        surface: ApiSurfaceKind,
        name: &str,
        action: PermissionAction,
    ) -> PermissionAtom {
        PermissionAtom::new(
            PermissionTarget::api_surface(api_id, surface, name).unwrap(),
            action,
        )
        .unwrap()
    }

    #[test]
    fn authorization_registry_transport_is_exact() {
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_authorization_registry_transport(
            "_INBOX.session",
            "contexts",
            &mut publish,
            &mut subscribe,
        );

        assert_eq!(
            publish,
            BTreeSet::from([
                "$JS.API.CONSUMER.CREATE.KV_contexts.*.$KV.contexts.revocation.*".to_owned(),
                "$JS.API.CONSUMER.INFO.KV_contexts.*".to_owned(),
                "$JS.API.DIRECT.GET.KV_contexts.$KV.contexts.*".to_owned(),
                "$JS.API.INFO".to_owned(),
                "$JS.FC.KV_contexts.>".to_owned(),
            ])
        );
        assert_eq!(subscribe, BTreeSet::from(["_INBOX.session.>".to_owned()]));
    }

    #[test]
    fn operation_provider_subscribes_only_to_bound_routes_it_serves() {
        let action = ActionRuntimeProjection {
            kind: RuntimeActionKind::Operation,
            upload: true,
            download: false,
            event_parameter_count: 0,
        };
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_provider_action(
            "fieldops.sites@v1",
            ProviderActionIdentity {
                deployment_id: "sites-deployment",
                instance_id: "sites-instance",
                session_prefix: "session-prefix",
                connection_id: "sites-connection",
            },
            "Refresh",
            &action,
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        let subject = trellis_protocol::derive_bound_operation_subject(
            "fieldops.sites@v1",
            "sites-deployment",
            "Refresh",
        )
        .unwrap();
        let own_token = URL_SAFE_NO_PAD.encode(b"sites-connection");
        assert_eq!(
            subscribe,
            BTreeSet::from([
                subject.clone(),
                format!("{subject}.control"),
                format!("{subject}.updates.*"),
                format!("{subject}.observe.{own_token}.*"),
                "transfer.v1.upload.session-prefix.*".to_owned(),
            ])
        );
        assert!(!subscribe.contains(&format!("{subject}.>")));
        assert!(!subscribe.contains("operations.>"));
        assert_eq!(
            publish,
            BTreeSet::from([
                format!("{subject}.updates.*"),
                format!("live.v1.data.{own_token}.*.*"),
            ])
        );
    }

    #[test]
    fn operation_caller_publishes_only_bound_caller_surfaces() {
        let api_id = "fieldops.sites@v1";
        let api = ApiRuntimeProjection {
            digest: "digest".to_owned(),
            major: 1,
            actions: BTreeMap::from([(
                "operation:Refresh".to_owned(),
                ActionRuntimeProjection {
                    kind: RuntimeActionKind::Operation,
                    upload: true,
                    download: false,
                    event_parameter_count: 0,
                },
            )]),
            capabilities: BTreeMap::new(),
        };
        let bindings = BTreeMap::from([(
            api_id.to_owned(),
            AuthorizationApiBinding {
                provider_deployment_id: "sites-deployment".to_owned(),
            },
        )]);
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_api_surface(
            &api,
            &bindings,
            "caller-connection",
            &api_permission(
                api_id,
                ApiSurfaceKind::Operation,
                "Refresh",
                PermissionAction::Invoke,
            ),
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        let subject =
            trellis_protocol::derive_bound_operation_subject(api_id, "sites-deployment", "Refresh")
                .unwrap();
        assert_eq!(
            publish,
            BTreeSet::from([
                subject.clone(),
                format!("{subject}.control"),
                "transfer.v1.upload.*.*".to_owned(),
            ])
        );
        assert!(subscribe.is_empty());
        assert!(!publish.contains("operations.v1.Refresh"));
        assert!(!publish.contains("operations.>"));
    }

    #[test]
    fn state_transport_is_exact_and_rejects_undeclared_resources() {
        let api_id = trellis_runtime_apis::apis::trellis_state_v1::API_ID;
        let api = ApiRuntimeProjection {
            digest: "digest".to_owned(),
            major: 1,
            actions: ["Get", "Put", "Delete"]
                .into_iter()
                .map(|name| {
                    (
                        format!("rpc:{name}"),
                        ActionRuntimeProjection {
                            kind: RuntimeActionKind::Rpc,
                            upload: false,
                            download: false,
                            event_parameter_count: 0,
                        },
                    )
                })
                .collect(),
            capabilities: BTreeMap::new(),
        };
        let bindings = BTreeMap::from([(
            api_id.to_owned(),
            AuthorizationApiBinding {
                provider_deployment_id: "state-deployment".to_owned(),
            },
        )]);
        let resources = [ResourceBindingEvidence {
            resource_kind: "state".to_owned(),
            local_name: "saved".to_owned(),
            binding_id: "binding".to_owned(),
            owner_participant_id: "example.Client".to_owned(),
            provider_identity: ResourceProviderIdentity::State {
                bucket: "state-bucket".to_owned(),
            },
            actual: Some(crate::platform::auth::resources::ResourceActual::State),
            state: ResourceBindingState::Available,
            materialized_at: 0,
            error: None,
        }];
        assert!(resource_binding(
            &resources,
            trellis_protocol::ParticipantResourceKind::State,
            "saved"
        )
        .is_ok());
        assert!(resource_binding(
            &resources,
            trellis_protocol::ParticipantResourceKind::State,
            "undeclared"
        )
        .is_err());

        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();
        for name in ["Get", "Put", "Delete"] {
            compile_api_surface(
                &api,
                &bindings,
                "state-connection",
                &api_permission(api_id, ApiSurfaceKind::Rpc, name, PermissionAction::Call),
                &mut publish,
                &mut subscribe,
            )
            .unwrap();
        }
        assert_eq!(
            publish,
            ["Get", "Put", "Delete"]
                .into_iter()
                .map(|name| {
                    trellis_protocol::derive_bound_rpc_subject(api_id, "state-deployment", name)
                        .unwrap()
                })
                .collect()
        );
        assert!(subscribe.is_empty());
        assert!(!publish.contains("rpc.v1.state.Put"));
    }

    #[test]
    fn live_provider_subscribes_only_to_its_bound_open_and_owner_control_routes() {
        let action = ActionRuntimeProjection {
            kind: RuntimeActionKind::Live,
            upload: false,
            download: false,
            event_parameter_count: 0,
        };
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_provider_action(
            "fieldops.sites@v1",
            ProviderActionIdentity {
                deployment_id: "sites-deployment",
                instance_id: "sites-instance",
                session_prefix: "session-prefix",
                connection_id: "sites-connection",
            },
            "Watch",
            &action,
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        let subject = trellis_protocol::derive_bound_live_subject(
            "fieldops.sites@v1",
            "sites-deployment",
            "Watch",
        )
        .unwrap();
        let emitted_subscription_subject =
            trellis_protocol::derive_live_control_subject(&subject, "sites-instance");
        let own_token = URL_SAFE_NO_PAD.encode(b"sites-connection");
        assert_eq!(
            subscribe,
            BTreeSet::from([
                subject.clone(),
                emitted_subscription_subject.clone(),
                format!("{subject}.observe.{own_token}.*"),
            ])
        );
        assert_eq!(
            publish,
            BTreeSet::from([format!("live.v1.data.{own_token}.*.*")])
        );
        assert!(subscribe.contains(&emitted_subscription_subject));
        assert!(
            !subscribe.contains(&trellis_protocol::derive_live_control_subject(
                &subject,
                "sibling-instance",
            ))
        );
        assert!(!subscribe.contains(&format!("{subject}.control.>")));
        assert!(!subscribe.contains("live.v1.route.Watch"));
    }

    #[test]
    fn live_caller_publishes_only_to_bound_open_and_owner_control_routes() {
        let api_id = "fieldops.sites@v1";
        let api = ApiRuntimeProjection {
            digest: "digest".to_owned(),
            major: 1,
            actions: BTreeMap::from([(
                "live:Watch".to_owned(),
                ActionRuntimeProjection {
                    kind: RuntimeActionKind::Live,
                    upload: false,
                    download: false,
                    event_parameter_count: 0,
                },
            )]),
            capabilities: BTreeMap::new(),
        };
        let bindings = BTreeMap::from([(
            api_id.to_owned(),
            AuthorizationApiBinding {
                provider_deployment_id: "sites-deployment".to_owned(),
            },
        )]);
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_api_surface(
            &api,
            &bindings,
            "caller-connection",
            &api_permission(
                api_id,
                ApiSurfaceKind::Live,
                "Watch",
                PermissionAction::Subscribe,
            ),
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        let subject =
            trellis_protocol::derive_bound_live_subject(api_id, "sites-deployment", "Watch")
                .unwrap();
        let own_token = URL_SAFE_NO_PAD.encode(b"caller-connection");
        assert_eq!(
            publish,
            BTreeSet::from([
                subject.clone(),
                format!("{subject}.control.*.*"),
                format!("{subject}.observe.*.*"),
            ])
        );
        assert_eq!(
            subscribe,
            BTreeSet::from([format!("live.v1.data.*.{own_token}.*")])
        );
        assert!(!publish.contains("live.v1.route.Watch"));
        assert!(!publish.contains(&format!("{subject}.control.>")));
    }

    #[test]
    fn job_process_update_subscription_is_resource_exact() {
        let resource = ResourceBindingEvidence {
            resource_kind: "jobQueue".to_owned(),
            local_name: "documents".to_owned(),
            binding_id: "binding-documents".to_owned(),
            owner_participant_id: "acme.jobs@v1".to_owned(),
            provider_identity: ResourceProviderIdentity::JobQueue {
                namespace: "tr_jobs_documents".to_owned(),
                work_stream: "JOBS_WORK_DOCUMENTS".to_owned(),
                publish_prefix: "trellis.jobs.tr_jobs_documents.process".to_owned(),
                updates_prefix: Some("trellis.job_updates.tr_jobs_documents.process".to_owned()),
                work_subject: "trellis.work.tr_jobs_documents.process".to_owned(),
                consumer: "worker-documents".to_owned(),
            },
            actual: Some(crate::platform::auth::resources::ResourceActual::Job),
            state: ResourceBindingState::Available,
            materialized_at: 1,
            error: None,
        };
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_resource(
            &resource,
            PermissionAction::Process,
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        assert_eq!(
            subscribe,
            BTreeSet::from([
                "trellis.job_updates.tr_jobs_documents.process.>".to_owned(),
                "trellis.jobs.tr_jobs_documents.process.>".to_owned(),
                "trellis.work.tr_jobs_documents.process".to_owned(),
            ])
        );
    }

    #[test]
    fn event_consumer_transport_is_bound_to_its_physical_consumer() {
        let resource = ResourceBindingEvidence {
            resource_kind: "eventConsumer".to_owned(),
            local_name: "measurements".to_owned(),
            binding_id: "binding-measurements".to_owned(),
            owner_participant_id: "acme.sensor".to_owned(),
            provider_identity: ResourceProviderIdentity::EventConsumer {
                stream: "trellis".to_owned(),
                consumer: "tr_cons_measurements".to_owned(),
                replay_consumer: "tr_cons_measurements_replay".to_owned(),
                filter_subjects: vec!["events.v1.YWNtZS5tZWFzdXJlbWVudEB2MQ.Recorded.*".to_owned()],
            },
            actual: Some(crate::platform::auth::resources::ResourceActual::Consumer),
            state: ResourceBindingState::Available,
            materialized_at: 1,
            error: None,
        };
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_resource(
            &resource,
            PermissionAction::Consume,
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        assert_eq!(
            publish,
            BTreeSet::from([
                "$JS.ACK.trellis.tr_cons_measurements.>".to_owned(),
                "$JS.API.CONSUMER.INFO.trellis.tr_cons_measurements".to_owned(),
                "$JS.API.CONSUMER.MSG.NEXT.trellis.tr_cons_measurements".to_owned(),
                "$JS.ACK.trellis_consumer_replays.tr_cons_measurements_replay.>".to_owned(),
                "$JS.API.CONSUMER.INFO.trellis_consumer_replays.tr_cons_measurements_replay"
                    .to_owned(),
                "$JS.API.CONSUMER.MSG.NEXT.trellis_consumer_replays.tr_cons_measurements_replay"
                    .to_owned(),
            ])
        );
        assert!(subscribe.is_empty());
    }
}

/// Real-broker verification of reply-permission expiry/count independence and
/// static live-namespace isolation (work-order §4, BT01–BT03).
#[cfg(test)]
mod nats_reply_permission_tests {
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use futures_util::StreamExt;
    use trellis_local_nats::{ManagedNatsServer, NatsBinarySource, NatsOutput, NatsServerBinary};
    use trellis_protocol::{
        ApiSurfaceKind, AuthorizationPrincipalKind, GrantOwnerKind, GrantSet, ParticipantKind,
        PermissionAction, PermissionAtom, PermissionTarget, UnsignedAuthorizationContext,
    };
    use trellis_rs::client::AuthorizationApiBinding;

    use super::compile_transport_permissions;
    use crate::platform::auth::{
        domain::ParticipantBindingState,
        evidence::{
            ActionRuntimeProjection, ApiRuntimeProjection, ParticipantRuntimeProjection,
            RuntimeActionKind,
        },
        AuthorizationRegistryBinding, ParticipantBindingRecord, TransportPermissions,
    };

    const API_ID: &str = "fieldops.sites@v1";
    const PROVIDER_CONNECTION: &str = "sites-connection";
    const CONSUMER_CONNECTION: &str = "caller-connection";
    const CONSUMER_SESSION_KEY: &str = "caller-session-key";
    const PROVIDER_DEPLOYMENT: &str = "sites-deployment";
    const SESSION_ID: &str = "AAECAwQFBgcICQoLDA0ODw";

    fn api_projection() -> ApiRuntimeProjection {
        ApiRuntimeProjection {
            digest: "d".repeat(43),
            major: 1,
            actions: BTreeMap::from([
                (
                    "live:Watch".to_owned(),
                    ActionRuntimeProjection {
                        kind: RuntimeActionKind::Live,
                        upload: false,
                        download: false,
                        event_parameter_count: 0,
                    },
                ),
                (
                    "operation:Inspect".to_owned(),
                    ActionRuntimeProjection {
                        kind: RuntimeActionKind::Operation,
                        upload: false,
                        download: false,
                        event_parameter_count: 0,
                    },
                ),
            ]),
            capabilities: BTreeMap::new(),
        }
    }

    fn binding(participant_id: &str, path: &str, implements: bool) -> ParticipantBindingRecord {
        let api = api_projection();
        ParticipantBindingRecord {
            participant_id: participant_id.to_owned(),
            participant_kind: ParticipantKind::Service,
            participant_digest: "1".repeat(43),
            needs_digest: "2".repeat(43),
            package_digest: "3".repeat(43),
            evidence_digest: "4".repeat(43),
            participant_path: path.to_owned(),
            projection: ParticipantRuntimeProjection {
                participant_id: participant_id.to_owned(),
                participant_kind: ParticipantKind::Service,
                display_name: participant_id.to_owned(),
                implemented_apis: if implements {
                    BTreeMap::from([(API_ID.to_owned(), api.clone())])
                } else {
                    BTreeMap::new()
                },
                referenced_apis: if implements {
                    BTreeMap::new()
                } else {
                    BTreeMap::from([(API_ID.to_owned(), api)])
                },
                resources: BTreeMap::new(),
                required_grants: GrantSet::new(Vec::new()),
                optional_grant_bundles: BTreeMap::new(),
                required_capabilities: Vec::new(),
                optional_capability_definitions: BTreeMap::new(),
                companion_participant_id: None,
                companion_participant_kind: None,
                companion_required: false,
            },
            resolved_at: 0,
            state: ParticipantBindingState::Resolved,
            error: None,
        }
    }

    fn context(
        connection_id: &str,
        session_key: &str,
        participant_id: &str,
        inbox_prefix: String,
        grants: GrantSet,
        deployment: Option<&str>,
        instance: Option<&str>,
    ) -> UnsignedAuthorizationContext {
        UnsignedAuthorizationContext {
            format: "trellis.auth.v1".to_owned(),
            issuer_key_id: "issuer".to_owned(),
            connection_id: connection_id.to_owned(),
            session_key: session_key.to_owned(),
            principal_id: format!("principal.{connection_id}"),
            principal_kind: if deployment.is_some() {
                AuthorizationPrincipalKind::Service
            } else {
                AuthorizationPrincipalKind::User
            },
            participant_id: participant_id.to_owned(),
            owner_kind: GrantOwnerKind::Deployment,
            owner_id: "deployment".to_owned(),
            grant_revision: 1,
            identity_key_id: None,
            login_session_id: None,
            deployment_id: deployment.map(str::to_owned),
            instance_id: instance.map(str::to_owned),
            inbox_prefix,
            issued_at: 0,
            not_before: 0,
            expires_at: i64::MAX,
            grants,
            platform_privileges: Vec::new(),
            extensions: serde_json::Map::new(),
            critical: Vec::new(),
        }
    }

    fn api_bindings() -> BTreeMap<String, AuthorizationApiBinding> {
        BTreeMap::from([(
            API_ID.to_owned(),
            AuthorizationApiBinding {
                provider_deployment_id: PROVIDER_DEPLOYMENT.to_owned(),
            },
        )])
    }

    fn registry() -> AuthorizationRegistryBinding {
        AuthorizationRegistryBinding {
            context_bucket: "contexts".to_owned(),
        }
    }

    fn provider_permissions() -> TransportPermissions {
        compile_transport_permissions(
            &context(
                PROVIDER_CONNECTION,
                "sites-session-key",
                "fieldops.Sites",
                "_INBOX.sites".to_owned(),
                GrantSet::new(Vec::new()),
                Some(PROVIDER_DEPLOYMENT),
                Some("sites-instance"),
            ),
            &binding("fieldops.Sites", "Sites", true),
            &[],
            &api_bindings(),
            &registry(),
        )
        .expect("provider permissions compile")
    }

    fn consumer_permissions(permission: PermissionAtom) -> TransportPermissions {
        let consumer_token = URL_SAFE_NO_PAD.encode(CONSUMER_CONNECTION.as_bytes());
        compile_transport_permissions(
            &context(
                CONSUMER_CONNECTION,
                CONSUMER_SESSION_KEY,
                "fieldops.Caller",
                format!("_INBOX.{consumer_token}"),
                GrantSet::new(vec![permission]),
                None,
                None,
            ),
            &binding("fieldops.Caller", "Caller", false),
            &[],
            &api_bindings(),
            &registry(),
        )
        .expect("consumer permissions compile")
    }

    fn live_subscribe_permission() -> PermissionAtom {
        PermissionAtom::new(
            PermissionTarget::api_surface(API_ID, ApiSurfaceKind::Live, "Watch").unwrap(),
            PermissionAction::Subscribe,
        )
        .unwrap()
    }

    fn operation_permission(action: PermissionAction) -> PermissionAtom {
        PermissionAtom::new(
            PermissionTarget::api_surface(API_ID, ApiSurfaceKind::Operation, "Inspect").unwrap(),
            action,
        )
        .unwrap()
    }

    fn compiled_permissions() -> (TransportPermissions, TransportPermissions) {
        (
            provider_permissions(),
            consumer_permissions(live_subscribe_permission()),
        )
    }

    fn nats_list(subjects: &[String]) -> String {
        let items: Vec<String> = subjects.iter().map(|s| format!("\"{s}\"")).collect();
        format!("[{}]", items.join(", "))
    }

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local addr")
            .port()
    }

    /// Reuse an already-installed pinned binary when present; otherwise download it
    /// into a private cache directory.
    fn resolve_pinned_binary() -> PathBuf {
        let name = format!(
            "nats-server-v{}",
            trellis_local_nats::pinned_version().expect("pinned nats version")
        );
        let mut candidates = Vec::new();
        if let Some(dir) = std::env::var_os("TRELLIS_CACHE_DIR") {
            candidates.push(PathBuf::from(dir).join(&name));
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            candidates.push(home.join(".cache/trellis").join(&name));
            candidates.push(home.join(".cache/trellis-test").join(&name));
        }
        for candidate in candidates {
            if candidate.is_file() {
                if let Ok(path) =
                    NatsServerBinary::resolve(&NatsBinarySource::Path(candidate), None)
                {
                    return path;
                }
            }
        }
        let cache = std::env::temp_dir().join("trellis-v2-nats-cache");
        std::fs::create_dir_all(&cache).expect("private nats cache dir");
        NatsServerBinary::resolve(&NatsBinarySource::DownloadPinned, Some(&cache))
            .expect("download pinned nats-server")
    }

    struct TestBroker {
        server: ManagedNatsServer,
        url: String,
        _dir: tempfile::TempDir,
    }

    impl TestBroker {
        fn start(response_allowance: &str) -> Self {
            Self::start_with_consumer(
                response_allowance,
                consumer_permissions(live_subscribe_permission()),
            )
        }

        fn start_with_consumer(response_allowance: &str, consumer: TransportPermissions) -> Self {
            let provider = provider_permissions();
            let binary = resolve_pinned_binary();
            // Broker tests run in parallel, so an ephemeral port observed by
            // `free_port()` can be claimed by a sibling test before this
            // nats-server binds it. Retry with fresh ports instead of failing
            // the suite on that race.
            let mut last: Option<(String, String)> = None;
            for _attempt in 0..8 {
                let dir = tempfile::tempdir().expect("temp dir");
                let config_path = dir.path().join("nats.conf");
                let nats_port = free_port();
                let http_port = free_port();
                let ws_port = free_port();
                let config = format!(
                    "port: {nats_port}\nhttp_port: {http_port}\nwebsocket {{ port: {ws_port}, no_tls: true }}\n\
                     authorization {{\n  users: [\n\
                     {{ user: \"provider\", password: \"pw\", permissions: {{ publish: {provider_publish}, subscribe: {provider_subscribe}, allow_responses: {response_allowance} }} }},\n\
                     {{ user: \"consumer\", password: \"pw\", permissions: {{ publish: {consumer_publish}, subscribe: {consumer_subscribe} }} }}\n\
                     ]\n}}\n",
                    provider_publish = nats_list(&provider.publish),
                    provider_subscribe = nats_list(&provider.subscribe),
                    consumer_publish = nats_list(&consumer.publish),
                    consumer_subscribe = nats_list(&consumer.subscribe),
                );
                std::fs::write(&config_path, &config).expect("write config");
                let pid_file = dir.path().join("nats.pid");
                let log_path = dir.path().join("nats.log");
                match ManagedNatsServer::start(
                    &binary,
                    &config_path,
                    nats_port,
                    http_port,
                    ws_port,
                    &pid_file,
                    &NatsOutput::Log {
                        path: log_path.clone(),
                        mirror: false,
                    },
                ) {
                    Ok(server) => {
                        let url = server.url();
                        return Self {
                            server,
                            url,
                            _dir: dir,
                        };
                    }
                    Err(error) => {
                        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                        last = Some((format!("{error}"), format!("{config}\n--- log ---\n{log}")));
                    }
                }
            }
            let (error, detail) = last.expect("at least one start attempt");
            panic!("start nats-server after 8 attempts: {error}\n--- config ---\n{detail}");
        }
    }

    impl Drop for TestBroker {
        fn drop(&mut self) {
            let _ = self.server.stop();
        }
    }

    type Errors = Arc<Mutex<Vec<String>>>;

    async fn connect(url: &str, user: &str) -> (async_nats::Client, Errors) {
        let errors: Errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        let client =
            async_nats::ConnectOptions::with_user_and_password(user.to_owned(), "pw".to_owned())
                .event_callback(move |event| {
                    let sink = sink.clone();
                    async move {
                        sink.lock().unwrap().push(format!("{event}"));
                    }
                })
                .connect(url)
                .await
                .expect("connect to broker");
        (client, errors)
    }

    fn live_base() -> String {
        trellis_protocol::derive_bound_live_subject(API_ID, PROVIDER_DEPLOYMENT, "Watch")
            .expect("live subject")
    }

    fn live_subject() -> String {
        trellis_protocol::derive_live_data_subject(
            PROVIDER_CONNECTION,
            CONSUMER_CONNECTION,
            SESSION_ID,
        )
        .expect("live subject")
    }

    fn operation_base() -> String {
        trellis_protocol::derive_bound_operation_subject(API_ID, PROVIDER_DEPLOYMENT, "Inspect")
            .expect("operation subject")
    }

    #[test]
    fn operation_observe_is_the_only_operation_grant_with_live_delivery() {
        let operation = operation_base();
        let live = live_base();
        let delivery = format!(
            "live.v1.data.*.{}.*",
            URL_SAFE_NO_PAD.encode(CONSUMER_CONNECTION.as_bytes())
        );

        let live_subscribe = consumer_permissions(live_subscribe_permission());
        assert!(live_subscribe.publish.contains(&live));
        assert!(live_subscribe.subscribe.contains(&delivery));
        assert!(!live_subscribe
            .publish
            .contains(&format!("{operation}.control")));

        let observe = consumer_permissions(operation_permission(PermissionAction::Observe));
        assert!(observe.publish.contains(&format!("{operation}.control")));
        assert!(observe
            .publish
            .contains(&format!("{operation}.observe.*.*")));
        assert!(observe.subscribe.contains(&delivery));
        assert!(!observe.publish.contains(&live));

        let invoke = consumer_permissions(operation_permission(PermissionAction::Invoke));
        assert!(invoke.publish.contains(&operation));
        assert!(invoke.publish.contains(&format!("{operation}.control")));
        assert!(!invoke.subscribe.contains(&delivery));
        assert!(!invoke.publish.contains(&format!("{operation}.observe.*.*")));

        let cancel = consumer_permissions(operation_permission(PermissionAction::Cancel));
        assert!(cancel.publish.contains(&format!("{operation}.control")));
        assert!(!cancel.publish.contains(&operation));
        assert!(!cancel.subscribe.contains(&delivery));
        assert!(!cancel.publish.contains(&format!("{operation}.observe.*.*")));
    }

    async fn next_within<T: Send + 'static>(
        receiver: &mut (impl futures_util::Stream<Item = T> + Unpin),
        label: &str,
    ) -> T {
        tokio::time::timeout(Duration::from_secs(5), receiver.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
            .unwrap_or_else(|| panic!("stream ended waiting for {label}"))
    }

    async fn error_mentioning(errors: &Errors, needle: &str) -> bool {
        for _ in 0..100 {
            if errors.lock().unwrap().iter().any(|e| e.contains(needle)) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    #[tokio::test]
    async fn bt01_response_expiry_is_independent_of_the_large_count_allowance() {
        let broker = TestBroker::start("{ max: 65535, expires: \"250ms\" }");
        let (provider, _provider_errors) = connect(&broker.url, "provider").await;
        let (consumer, _consumer_errors) = connect(&broker.url, "consumer").await;
        let consumer_inbox = format!(
            "_INBOX.{}",
            URL_SAFE_NO_PAD.encode(CONSUMER_CONNECTION.as_bytes())
        );
        let mut requests = provider.subscribe(live_base()).await.unwrap();
        let mut replies = consumer
            .subscribe(format!("{consumer_inbox}.*"))
            .await
            .unwrap();
        let mut live = consumer.subscribe(live_subject()).await.unwrap();
        provider.flush().await.unwrap();
        consumer.flush().await.unwrap();

        let reply = format!("{consumer_inbox}.r1");
        assert!(
            !compiled_permissions().1.publish.contains(&reply),
            "no static grant may already permit the response inbox"
        );
        consumer
            .publish_with_reply(live_base(), reply.clone(), b"req".to_vec().into())
            .await
            .unwrap();
        let request = next_within(&mut requests, "request").await;
        assert_eq!(request.reply.as_deref(), Some(reply.as_str()));

        provider
            .publish(reply.clone(), b"ok".to_vec().into())
            .await
            .unwrap();
        let received = next_within(&mut replies, "first reply").await;
        assert_eq!(received.payload.as_ref(), b"ok");

        tokio::time::sleep(Duration::from_millis(600)).await;
        provider
            .publish(reply.clone(), b"late".to_vec().into())
            .await
            .unwrap();
        assert!(
            error_mentioning(&_provider_errors, "Permissions Violation").await,
            "expired response subject must be denied"
        );

        provider
            .publish(live_subject(), b"live".to_vec().into())
            .await
            .unwrap();
        let frame = next_within(&mut live, "live frame").await;
        assert_eq!(frame.payload.as_ref(), b"live");

        let reply2 = format!("{consumer_inbox}.r2");
        consumer
            .publish_with_reply(live_base(), reply2.clone(), b"req2".to_vec().into())
            .await
            .unwrap();
        let request2 = next_within(&mut requests, "second request").await;
        assert_eq!(request2.reply.as_deref(), Some(reply2.as_str()));
        provider
            .publish(reply2, b"ok2".to_vec().into())
            .await
            .unwrap();
        let second = next_within(&mut replies, "second reply").await;
        assert_eq!(second.payload.as_ref(), b"ok2");
    }

    #[tokio::test]
    async fn bt02_response_count_is_independent_of_static_live_delivery() {
        let broker = TestBroker::start("{ max: 3, expires: \"60s\" }");
        let (provider, provider_errors) = connect(&broker.url, "provider").await;
        let (consumer, _consumer_errors) = connect(&broker.url, "consumer").await;
        let consumer_inbox = format!(
            "_INBOX.{}",
            URL_SAFE_NO_PAD.encode(CONSUMER_CONNECTION.as_bytes())
        );
        let mut requests = provider.subscribe(live_base()).await.unwrap();
        let mut replies = consumer
            .subscribe(format!("{consumer_inbox}.*"))
            .await
            .unwrap();
        let mut live = consumer.subscribe(live_subject()).await.unwrap();
        provider.flush().await.unwrap();
        consumer.flush().await.unwrap();

        let reply = format!("{consumer_inbox}.count");
        consumer
            .publish_with_reply(live_base(), reply.clone(), b"req".to_vec().into())
            .await
            .unwrap();
        let _ = next_within(&mut requests, "request").await;

        for marker in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            provider
                .publish(reply.clone(), marker.to_vec().into())
                .await
                .unwrap();
        }
        for expected in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            let frame = next_within(&mut replies, "counted reply").await;
            assert_eq!(frame.payload.as_ref(), expected);
        }
        provider
            .publish(reply.clone(), b"four".to_vec().into())
            .await
            .unwrap();
        assert!(
            error_mentioning(&provider_errors, "Permissions Violation").await,
            "the fourth response must be denied within the 60s allowance"
        );

        for index in 0..65u32 {
            provider
                .publish(
                    live_subject(),
                    format!("marker-{index}").into_bytes().into(),
                )
                .await
                .unwrap();
        }
        for index in 0..65u32 {
            let frame = next_within(&mut live, "live marker").await;
            assert_eq!(frame.payload.as_ref(), format!("marker-{index}").as_bytes());
        }
    }

    #[tokio::test]
    async fn bt03_static_namespaces_are_isolated() {
        let broker = TestBroker::start("{ max: 65535, expires: \"60s\" }");
        let (provider, provider_errors) = connect(&broker.url, "provider").await;
        let (consumer, consumer_errors) = connect(&broker.url, "consumer").await;

        let other_consumer = format!("_INBOX.{}", URL_SAFE_NO_PAD.encode(b"other-connection"));
        let _denied_subscription = consumer.subscribe(format!("{other_consumer}.>")).await;
        assert!(
            error_mentioning(&consumer_errors, "Permissions Violation").await,
            "a consumer must not subscribe to another consumer's inbox"
        );

        let foreign_live = trellis_protocol::derive_live_data_subject(
            "other-provider",
            CONSUMER_CONNECTION,
            SESSION_ID,
        )
        .unwrap();
        provider
            .publish(foreign_live, b"x".to_vec().into())
            .await
            .unwrap();
        assert!(
            error_mentioning(&provider_errors, "Permissions Violation").await,
            "a provider must not publish under another provider's live prefix"
        );

        consumer
            .publish(live_subject(), b"x".to_vec().into())
            .await
            .unwrap();
        assert!(
            error_mentioning(&consumer_errors, "Permissions Violation").await,
            "a non-provider must not publish live data"
        );
    }

    #[tokio::test]
    async fn bt04_only_operation_observe_receives_live_delivery() {
        let broker = TestBroker::start_with_consumer(
            "{ max: 65535, expires: \"60s\" }",
            consumer_permissions(operation_permission(PermissionAction::Observe)),
        );
        let (provider, _provider_errors) = connect(&broker.url, "provider").await;
        let (consumer, _consumer_errors) = connect(&broker.url, "consumer").await;
        let mut live = consumer.subscribe(live_subject()).await.unwrap();
        provider.flush().await.unwrap();
        consumer.flush().await.unwrap();
        provider
            .publish(live_subject(), b"observe".to_vec().into())
            .await
            .unwrap();
        assert_eq!(
            next_within(&mut live, "operation observation")
                .await
                .payload
                .as_ref(),
            b"observe"
        );

        for action in [PermissionAction::Invoke, PermissionAction::Cancel] {
            let broker = TestBroker::start_with_consumer(
                "{ max: 65535, expires: \"60s\" }",
                consumer_permissions(operation_permission(action)),
            );
            let (consumer, errors) = connect(&broker.url, "consumer").await;
            let _denied = consumer.subscribe(live_subject()).await.unwrap();
            assert!(
                error_mentioning(&errors, "Permissions Violation").await,
                "{action:?} must not receive operation live delivery"
            );
        }
    }
}
