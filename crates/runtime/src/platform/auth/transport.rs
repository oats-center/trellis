//! The single broker ACL projection of accepted application authority.

use std::collections::BTreeSet;

use rusqlite::{params, Connection, OptionalExtension};
use trellis_idl::{compile_evidence, ActionDefinition, PackageEvidence};
use trellis_protocol::{
    ApiSurfaceKind, PermissionAction, SessionBinding, TransportAuthorizationV1,
    TransportResponseAuthorizationV1, UnsignedSessionAuthority, TRANSPORT_AUTHORIZATION_FORMAT_V1,
};

use super::{
    catalog::resolve_api,
    sqlite::{decode, AuthError, SqliteAuthorizationStore},
};

impl SqliteAuthorizationStore {
    pub(super) fn compile_transport(
        &self,
        connection: &Connection,
        authority: &UnsignedSessionAuthority,
    ) -> Result<TransportAuthorizationV1, AuthError> {
        let native = match &authority.binding {
            SessionBinding::Service { instance_id, .. }
            | SessionBinding::Device { instance_id, .. } => {
                Some(super::native_projection::project_instance(
                    connection,
                    instance_id,
                    authority.issued_at,
                )?)
            }
            _ => None,
        };
        let mut publish = BTreeSet::from([trellis_protocol::admission_get_subject(
            &authority.session_public_key,
        )?]);
        publish.insert("$SYS.REQ.USER.INFO".into());
        publish.insert(trellis_protocol::ISSUER_METADATA_SUBJECT.into());
        let mut subscribe = BTreeSet::from([format!("{}.>", authority.inbox_prefix)]);
        // Verification material and session cutoffs are public, read-only data.
        for bucket in [
            super::broker::MATERIAL_BUCKET,
            super::broker::REVOCATION_BUCKET,
        ] {
            stream_access(&mut publish, &format!("KV_{bucket}"), false)?;
            subscribe.insert(format!("$KV.{bucket}.>"));
        }
        let own = &authority.authorization_session_id;
        // Live data routes carry the authenticated logical runtime ID. The
        // authorization-session ID is for cutoffs, not a transport subject.
        let own_runtime = &authority.runtime_id;
        let own_token = trellis_protocol::encode_subject_token(own_runtime);
        let mut responds = false;
        let mut operation_providers = BTreeSet::new();
        for reference in &authority.apis {
            let (generation,revision,evidence):(u64,u64,String)=connection.query_row(
                "SELECT generation,accepted_revision,definition_json FROM auth_accepted_apis WHERE api_id=?1",
                [&reference.api_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            if generation != reference.generation.get()
                || revision != reference.accepted_revision.get()
            {
                return Err(AuthError::Denied);
            }
            let graph = compile_evidence(decode::<PackageEvidence>(&evidence)?)
                .map_err(|error| AuthError::Invalid(error.to_string()))?;
            let api = resolve_api(&graph, &reference.api_id)?;
            let subjects = api.definition().subjects();
            let catalog = self.catalog_in_transaction(connection, &reference.api_id)?;
            if catalog.digest()? != reference.catalog_snapshot_digest {
                return Err(AuthError::Denied);
            }
            let mut caller = Vec::new();
            for selected in &authority.capabilities {
                if let Some(capability) = catalog.capabilities.iter().find(|capability| {
                    capability.capability_id == selected.capability_id
                        && capability.identity_generation == selected.identity_generation
                        && capability.consent_revision == selected.consent_revision
                }) {
                    caller.extend(
                        capability
                            .memberships
                            .iter()
                            .filter(|membership| {
                                membership.member_since_revision.get()
                                    <= reference.accepted_revision.get()
                            })
                            .map(|membership| &membership.action),
                    );
                }
            }
            let mut providers = Vec::new();
            for binding in authority
                .provider_bindings
                .iter()
                .filter(|binding| binding.api_id == reference.api_id)
            {
                let projection = native.as_ref().ok_or(AuthError::Denied)?;
                if projection.deployment_id != binding.deployment_id {
                    return Err(AuthError::Denied);
                }
                let implementations = &projection.provided;
                let implementation = implementations
                    .iter()
                    .find(|implementation| {
                        implementation.api_id == binding.api_id
                            && implementation.generation == binding.generation.get()
                            && implementation.implementation_digest == binding.implementation_digest
                    })
                    .ok_or(AuthError::Denied)?;
                providers.extend(implementation.implemented_actions.iter().cloned());
            }
            for action in &catalog.actions {
                let identity = &action.identity;
                let can_call = caller.contains(&identity);
                let can_provide = providers.contains(identity);
                if !can_call && !can_provide {
                    continue;
                }
                match (identity.kind, identity.direction) {
                    (ApiSurfaceKind::Rpc, PermissionAction::Call) => {
                        let subject = subjects.rpc.get(&identity.name).ok_or(AuthError::Denied)?;
                        responds |= project_request(
                            subject,
                            can_call,
                            can_provide,
                            &mut publish,
                            &mut subscribe,
                        );
                    }
                    (ApiSurfaceKind::Operation, direction) => {
                        if can_provide {
                            operation_providers.extend(
                                authority
                                    .provider_bindings
                                    .iter()
                                    .filter(|binding| binding.api_id == reference.api_id)
                                    .map(|binding| binding.deployment_id.clone()),
                            );
                        }
                        let subject = subjects
                            .operations
                            .get(&identity.name)
                            .ok_or(AuthError::Denied)?;
                        if direction == PermissionAction::Invoke {
                            responds |= project_request(
                                subject,
                                can_call,
                                can_provide,
                                &mut publish,
                                &mut subscribe,
                            );
                        }
                        // Established work retains its deployment owner. The set
                        // contains all eligible owners, never a sticky selection.
                        let mut deployments=connection.prepare("SELECT DISTINCT b.deployment_id FROM auth_deployment_bindings b JOIN auth_instances i USING(deployment_id) JOIN auth_deployments d USING(deployment_id) WHERE i.state='active' AND d.state='active' AND EXISTS(SELECT 1 FROM json_each(b.provided_apis_json) WHERE json_extract(value,'$.api_id')=?1 AND json_extract(value,'$.generation')=?2)")?;
                        for deployment in deployments
                            .query_map(params![reference.api_id, generation], |row| {
                                row.get::<_, String>(0)
                            })?
                        {
                            let deployment = deployment?;
                            let base = trellis_protocol::derive_bound_operation_subject(
                                &reference.api_id,
                                &deployment,
                                &identity.name,
                            )?;
                            if can_call {
                                if matches!(
                                    direction,
                                    PermissionAction::Observe
                                        | PermissionAction::Cancel
                                        | PermissionAction::Control
                                ) {
                                    publish.insert(format!("{base}.control"));
                                }
                                if direction == PermissionAction::Observe {
                                    subscribe.insert(format!("{base}.updates.*"));
                                }
                                if direction == PermissionAction::Invoke {
                                    publish.insert(format!("{base}.observe.*.*"));
                                }
                            }
                            if can_provide
                                && authority.provider_bindings.iter().any(|provider| {
                                    provider.api_id == reference.api_id
                                        && provider.deployment_id == deployment
                                })
                            {
                                subscribe.insert(format!("{base}.control"));
                                // The implementation owns this deployment's
                                // route; observers carry their own session ID,
                                // not the provider's. Each request still passes
                                // native caller authorization at dispatch.
                                subscribe.insert(format!("{base}.observe.*.*"));
                                publish.insert(format!("{base}.updates.*"));
                                // Forward this deployment's updates to its live
                                // observers without requiring caller entitlement.
                                subscribe.insert(format!("{base}.updates.*"));
                                // Private owner reconciliation is restricted to
                                // this accepted deployment implementation.
                                publish.insert(format!("{base}.reconcile.*"));
                                subscribe.insert(format!("{base}.reconcile.{own_token}"));
                                responds = true;
                            }
                        }
                    }
                    (ApiSurfaceKind::Live, PermissionAction::Subscribe) => {
                        let subject = subjects
                            .lives
                            .get(&identity.name)
                            .ok_or(AuthError::Denied)?;
                        if can_call {
                            publish.insert(format!("{subject}.observe.*.*"));
                        }
                        responds |= project_request(
                            subject,
                            can_call,
                            can_provide,
                            &mut publish,
                            &mut subscribe,
                        );
                        if can_provide {
                            subscribe.insert(
                                trellis_protocol::derive_live_observe_wildcard_subject(
                                    subject,
                                    own_runtime,
                                )?,
                            );
                        }
                    }
                    (
                        ApiSurfaceKind::Event,
                        PermissionAction::Publish | PermissionAction::Subscribe,
                    ) => {
                        let subject = &subjects
                            .events
                            .get(&identity.name)
                            .ok_or(AuthError::Denied)?
                            .wildcard;
                        if identity.direction == PermissionAction::Publish {
                            publish.insert(subject.clone());
                        } else if can_call {
                            subscribe.insert(subject.clone());
                        }
                    }
                    _ => return Err(AuthError::Denied),
                }
                // Transfer-capable actions retain endpoint-scoped permissions.
                let definition =
                    api.definition()
                        .actions()
                        .iter()
                        .find(|(key, _)| {
                            key.name == identity.name
                                && match (key.kind, identity.kind) {
                                    (trellis_idl::ActionKind::Rpc, ApiSurfaceKind::Rpc)
                                    | (
                                        trellis_idl::ActionKind::Operation,
                                        ApiSurfaceKind::Operation,
                                    ) => true,
                                    _ => false,
                                }
                        })
                        .map(|(_, value)| value);
                if let Some(definition) = definition {
                    if matches!(
                        definition,
                        ActionDefinition::Rpc { download: true, .. }
                            | ActionDefinition::Operation { upload: true, .. }
                    ) {
                        if can_call {
                            publish.insert(trellis_protocol::transfer::derive_transfer_consumer_upload_data_wildcard_subject(own_runtime)?);
                            publish.insert(trellis_protocol::transfer::derive_transfer_consumer_control_wildcard_subject(own_runtime)?);
                            subscribe.insert(trellis_protocol::transfer::derive_transfer_consumer_download_data_wildcard_subject(own_runtime)?);
                            subscribe.insert(
                                trellis_protocol::transfer::derive_transfer_consumer_signal_wildcard_subject(
                                    own_runtime,
                                )?,
                            );
                        }
                        if can_provide {
                            subscribe.insert(trellis_protocol::transfer::derive_transfer_provider_upload_data_wildcard_subject(own_runtime)?);
                            subscribe.insert(trellis_protocol::transfer::derive_transfer_provider_control_wildcard_subject(own_runtime)?);
                            publish.insert(trellis_protocol::transfer::derive_transfer_provider_download_data_wildcard_subject(own_runtime)?);
                            publish.insert(
                                trellis_protocol::transfer::derive_transfer_provider_signal_wildcard_subject(
                                    own_runtime,
                                )?,
                            );
                        }
                    }
                }
            }
        }
        if !authority.capabilities.is_empty() {
            subscribe.insert(format!("live.v1.data.*.{own_token}.*"));
        }
        if responds {
            publish.insert(format!("live.v1.data.{own_token}.*.*"));
        }
        if matches!(
            &authority.binding,
            SessionBinding::Service { .. } | SessionBinding::Device { .. }
        ) {
            let bootstrap = self
                .native_bootstrap(connection, authority)?
                .ok_or(AuthError::Denied)?;
            let resources: trellis_rs::service::ServiceResourceBindings =
                serde_json::from_value(bootstrap["resourceRuntime"].clone())?;
            for deployment in &operation_providers {
                let (owner, instance, participant) = match &authority.binding {
                    SessionBinding::Service {
                        deployment_id,
                        instance_id,
                        participant_id,
                    }
                    | SessionBinding::Device {
                        deployment_id,
                        instance_id,
                        participant_id,
                    } => (deployment_id, instance_id, participant_id),
                    _ => return Err(AuthError::Denied),
                };
                if owner != deployment {
                    return Err(AuthError::Denied);
                }
                // Framework storage is deployment-owned, not software-revision-owned.
                // The pinned provider actions above already establish eligibility.
                let evidence: Option<String> = connection.query_row("SELECT e.actual_json FROM auth_resource_binding_evidence e JOIN auth_instances i ON i.deployment_id=e.owner_id WHERE e.owner_kind='deployment' AND e.owner_id=?1 AND e.participant_id=?2 AND e.resource_kind='framework_operation' AND e.local_name='operations' AND e.state='available' AND i.instance_id=?3 AND i.state='active'",params![deployment,participant,instance], |row|row.get(0)).optional()?;
                let storage: super::framework_storage::OperationStorage =
                    decode(&evidence.ok_or(AuthError::Unavailable)?)?;
                for (bucket, stream_prefix, subject_prefix) in [
                    (&storage.repository, "KV", "$KV"),
                    (&storage.staging, "OBJ", "$O"),
                ] {
                    stream_access(&mut publish, &format!("{stream_prefix}_{bucket}"), true)?;
                    if stream_prefix == "OBJ" {
                        // Object deletion/replacement cleans up chunks, not the allocation.
                        publish.insert(format!("$JS.API.STREAM.PURGE.OBJ_{bucket}"));
                    }
                    publish.insert(format!("{subject_prefix}.{bucket}.>"));
                    subscribe.insert(format!("{subject_prefix}.{bucket}.>"));
                }
            }
            for resource in resources.kv.values() {
                stream_access(&mut publish, &format!("KV_{}", resource.bucket), true)?;
                publish.insert(format!("$KV.{}.>", resource.bucket));
                subscribe.insert(format!("$KV.{}.>", resource.bucket));
            }
            for resource in resources.store.values() {
                stream_access(&mut publish, &format!("OBJ_{}", resource.name), true)?;
                publish.insert(format!("$JS.API.STREAM.PURGE.OBJ_{}", resource.name));
                publish.insert(format!("$O.{}.>", resource.name));
                subscribe.insert(format!("$O.{}.>", resource.name));
            }
            if let Some(jobs) = resources.jobs {
                if !jobs.queues.is_empty()
                    && matches!(&authority.binding, SessionBinding::Service { .. })
                {
                    publish.insert("rpc.v1.jobs.History".into());
                }
                publish.insert("$JS.API.STREAM.INFO.JOBS".into());
                let work_stream = jobs.work_stream.as_deref().ok_or(AuthError::Unavailable)?;
                if jobs
                    .queues
                    .values()
                    .any(|queue| queue.key_concurrency.is_some())
                {
                    let bucket = format!("JOBS_KEYS_{}", jobs.namespace);
                    stream_access(&mut publish, &format!("KV_{bucket}"), true)?;
                    publish.insert(format!("$KV.{bucket}.>"));
                    subscribe.insert(format!("$KV.{bucket}.>"));
                }
                for (job_type, queue) in &jobs.queues {
                    publish.insert(format!(
                        "trellis.jobs.workers.{}.{job_type}.*.heartbeat",
                        jobs.namespace
                    ));
                    for family in ["CONSUMER.INFO", "CONSUMER.MSG.NEXT"] {
                        publish.insert(format!(
                            "$JS.API.{family}.{work_stream}.{}",
                            queue.consumer_name
                        ));
                    }
                    publish.insert(format!("$JS.ACK.{work_stream}.{}.>", queue.consumer_name));
                    publish.insert(format!(
                        "$JS.API.DIRECT.GET.JOBS.{}.>",
                        queue.publish_prefix
                    ));
                    publish.insert(queue.work_subject.clone());
                    subscribe.insert(queue.work_subject.clone());
                    publish.insert(format!("{}.>", queue.publish_prefix));
                    subscribe.insert(format!("{}.>", queue.publish_prefix));
                    if let Some(prefix) = &queue.updates_prefix {
                        publish.insert(format!("{prefix}.>"));
                        subscribe.insert(format!("{prefix}.>"));
                    }
                }
            }
            for consumer in resources.event_consumers.values() {
                for (stream, durable) in [
                    (&consumer.stream, &consumer.consumer_name),
                    (
                        &consumer.replay_binding.stream,
                        &consumer.replay_binding.consumer_name,
                    ),
                ] {
                    publish.insert(format!("$JS.API.STREAM.INFO.{stream}"));
                    for family in ["CONSUMER.INFO", "CONSUMER.MSG.NEXT"] {
                        publish.insert(format!("$JS.API.{family}.{stream}.{durable}"));
                    }
                    publish.insert(format!("$JS.ACK.{stream}.{durable}.>"));
                }
            }
        }
        let current = super::policy::evaluate(
            connection,
            &super::authorization_sessions::session_credential(connection, own)?,
            authority.issued_at,
        )?;
        let policy = TransportAuthorizationV1 {
            format: TRANSPORT_AUTHORIZATION_FORMAT_V1.into(),
            account: self.settings.account.clone(),
            publish_allow: publish.into_iter().collect(),
            subscribe_allow: subscribe.into_iter().collect(),
            response: responds.then_some(TransportResponseAuthorizationV1 {
                max_messages: 65_535,
                ttl_ms: 120_000,
            }),
            hard_expires_at: (current.not_after != i64::MAX).then_some(current.not_after),
        };
        policy.validate()?;
        Ok(policy)
    }
}

// Used for each owned stream and the read-only verification streams. Stream
// management is not implied by owning data: no CREATE/UPDATE/DELETE stream ACL.
fn stream_access(
    publish: &mut BTreeSet<String>,
    stream: &str,
    write: bool,
) -> Result<(), AuthError> {
    if stream.is_empty()
        || stream.contains(['.', '*', '>'])
        || stream.chars().any(char::is_whitespace)
    {
        return Err(AuthError::Denied);
    }
    // Native NATS Object Store clients check JetStream availability before
    // opening their already-provisioned bucket.
    publish.insert("$JS.API.INFO".into());
    for family in ["STREAM.INFO", "STREAM.MSG.GET", "DIRECT.GET"] {
        publish.insert(format!("$JS.API.{family}.{stream}"));
    }
    publish.insert(format!("$JS.API.CONSUMER.CREATE.{stream}"));
    publish.insert(format!("$JS.API.DIRECT.GET.{stream}.>"));
    for family in [
        "CONSUMER.CREATE",
        "CONSUMER.INFO",
        "CONSUMER.DELETE",
        "CONSUMER.MSG.NEXT",
    ] {
        publish.insert(format!("$JS.API.{family}.{stream}.>"));
    }
    publish.insert(format!("$JS.ACK.{stream}.>"));
    if write {
        publish.insert(format!("$JS.API.STREAM.MSG.DELETE.{stream}"));
    }
    Ok(())
}

// The same initiation direction applies to RPC, Operation, and Live actions.
// Owning an implementation never creates a caller permission (or vice versa).
fn project_request(
    subject: &str,
    caller: bool,
    provider: bool,
    publish: &mut BTreeSet<String>,
    subscribe: &mut BTreeSet<String>,
) -> bool {
    if caller {
        publish.insert(subject.into());
    }
    if provider {
        subscribe.insert(subject.into());
    }
    provider
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_provider_and_reduction_project_distinct_acls() {
        let subject = "rpc.v1.documents.Get";
        let project = |caller, provider| {
            let mut publish = BTreeSet::new();
            let mut subscribe = BTreeSet::new();
            let responds = project_request(subject, caller, provider, &mut publish, &mut subscribe);
            (publish, subscribe, responds)
        };
        let caller = project(true, false);
        assert!(caller.0.contains(subject));
        assert!(caller.1.is_empty());
        assert!(!caller.2);
        let provider = project(false, true);
        assert!(provider.0.is_empty());
        assert!(provider.1.contains(subject));
        assert!(provider.2);
        let combined = project(true, true);
        assert!(combined.0.contains(subject));
        assert!(combined.1.contains(subject));
        assert!(combined.2);
        let removed = project(false, false);
        assert!(removed.0.is_empty());
        assert!(removed.1.is_empty());
        assert!(!removed.2);
    }
}
