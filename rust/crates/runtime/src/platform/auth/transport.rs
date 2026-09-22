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
                (deployment_id, instance_id),
                session_prefix,
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
            compile_api_surface(api, api_bindings, atom, &mut publish, &mut subscribe)?;
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

fn compile_provider_action(
    api_id: &str,
    provider: (&str, &str),
    session_prefix: &str,
    name: &str,
    action: &super::evidence::ActionRuntimeProjection,
    publish: &mut BTreeSet<String>,
    subscribe: &mut BTreeSet<String>,
) -> Result<(), AuthorizationStateError> {
    let (deployment_id, instance_id) = provider;
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
            if action.upload {
                subscribe.insert(format!("transfer.v1.upload.{session_prefix}.*"));
            }
        }
        RuntimeActionKind::Feed => {
            let subject = trellis_protocol::derive_bound_feed_subject(api_id, deployment_id, name)
                .map_err(|error| invalid_error(error.to_string()))?;
            subscribe.insert(subject.clone());
            subscribe.insert(trellis_protocol::derive_feed_control_subject(
                &subject,
                instance_id,
            ));
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
        (ApiSurfaceKind::Feed, PermissionAction::Subscribe) => {
            let deployment_id = &api_bindings
                .get(api_id)
                .ok_or_else(|| invalid_error(format!("API {api_id} is not bound")))?
                .provider_deployment_id;
            let subject = trellis_protocol::derive_bound_feed_subject(api_id, deployment_id, name)
                .map_err(|error| invalid_error(error.to_string()))?;
            publish.insert(subject.clone());
            publish.insert(format!("{subject}.control.*.*"));
        }
        _ => return invalid("grant action does not match API surface"),
    }
    Ok(())
}

fn surface_name(surface: ApiSurfaceKind) -> &'static str {
    match surface {
        ApiSurfaceKind::Rpc => "rpc",
        ApiSurfaceKind::Operation => "operation",
        ApiSurfaceKind::Event => "event",
        ApiSurfaceKind::Feed => "feed",
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
        compile_resource, resource_binding,
    };
    use crate::platform::auth::{
        evidence::{ActionRuntimeProjection, ApiRuntimeProjection, RuntimeActionKind},
        ResourceBindingEvidence, ResourceBindingState, ResourceProviderIdentity,
    };
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
            ("sites-deployment", "sites-instance"),
            "session-prefix",
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
        assert_eq!(
            subscribe,
            BTreeSet::from([
                subject.clone(),
                format!("{subject}.control"),
                format!("{subject}.updates.*"),
                "transfer.v1.upload.session-prefix.*".to_owned(),
            ])
        );
        assert!(!subscribe.contains(&format!("{subject}.>")));
        assert!(!subscribe.contains("operations.>"));
        assert_eq!(publish, BTreeSet::from([format!("{subject}.updates.*")]));
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
    fn feed_provider_subscribes_only_to_its_bound_open_and_owner_control_routes() {
        let action = ActionRuntimeProjection {
            kind: RuntimeActionKind::Feed,
            upload: false,
            download: false,
            event_parameter_count: 0,
        };
        let mut publish = BTreeSet::new();
        let mut subscribe = BTreeSet::new();

        compile_provider_action(
            "fieldops.sites@v1",
            ("sites-deployment", "sites-instance"),
            "session-prefix",
            "Watch",
            &action,
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        let subject = trellis_protocol::derive_bound_feed_subject(
            "fieldops.sites@v1",
            "sites-deployment",
            "Watch",
        )
        .unwrap();
        let emitted_subscription_subject =
            trellis_protocol::derive_feed_control_subject(&subject, "sites-instance");
        assert_eq!(
            subscribe,
            BTreeSet::from([subject.clone(), emitted_subscription_subject.clone()])
        );
        assert!(subscribe.contains(&emitted_subscription_subject));
        assert!(
            !subscribe.contains(&trellis_protocol::derive_feed_control_subject(
                &subject,
                "sibling-instance",
            ))
        );
        assert!(!subscribe.contains(&format!("{subject}.control.>")));
        assert!(!subscribe.contains("feed.v1.Watch"));
    }

    #[test]
    fn feed_caller_publishes_only_to_bound_open_and_owner_control_routes() {
        let api_id = "fieldops.sites@v1";
        let api = ApiRuntimeProjection {
            digest: "digest".to_owned(),
            major: 1,
            actions: BTreeMap::from([(
                "feed:Watch".to_owned(),
                ActionRuntimeProjection {
                    kind: RuntimeActionKind::Feed,
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
            &api_permission(
                api_id,
                ApiSurfaceKind::Feed,
                "Watch",
                PermissionAction::Subscribe,
            ),
            &mut publish,
            &mut subscribe,
        )
        .unwrap();

        let subject =
            trellis_protocol::derive_bound_feed_subject(api_id, "sites-deployment", "Watch")
                .unwrap();
        assert_eq!(
            publish,
            BTreeSet::from([subject.clone(), format!("{subject}.control.*.*")])
        );
        assert!(subscribe.is_empty());
        assert!(!publish.contains("feed.v1.Watch"));
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
