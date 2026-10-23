//! Revision-pinned native authority over current deployment materialization.

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use trellis_idl::{
    ActionKind, ActionSelection, InteractionDirection, PackageGraph, ResourceDefinition,
};

use super::authorization_sessions::ProviderImplementation;
use super::catalog::{action_identity, resolve_api};
use super::sqlite::{decode, AuthError, Connection};

#[derive(Debug)]
pub(super) struct NativeInstanceProjection {
    pub deployment_id: String,
    pub participant_id: String,
    pub participant_digest: String,
    pub principal_id: String,
    /// Compiled only when a pinned revision was compared with its replacement.
    pub graph: Option<Arc<PackageGraph>>,
    pub evidence: String,
    pub resources: Value,
    pub required: Vec<String>,
    pub optional: Vec<String>,
    pub provided: Vec<ProviderImplementation>,
}

impl NativeInstanceProjection {
    /// The installed revision's package graph.
    pub(super) fn graph(&self) -> Result<Arc<PackageGraph>, AuthError> {
        match &self.graph {
            Some(graph) => Ok(Arc::clone(graph)),
            None => trellis_idl::compile_evidence(decode(&self.evidence)?)
                .map_err(|error| AuthError::Invalid(error.to_string())),
        }
    }
}

/// Project the immutable instance vocabulary without granting a newer binary's
/// actions or resources. Current policy and physical materialization still fence it.
pub(super) fn project_instance(
    connection: &Connection,
    instance: &str,
    now: i64,
) -> Result<NativeInstanceProjection, AuthError> {
    let row = connection
        .query_row(
            "SELECT i.deployment_id,d.participant_id,i.principal_id,\
                p.participant_digest,p.projection_json,c.projection_json,\
                b.resource_commitments_json,i.installed_revision=b.installed_revision,\
                b.required_capabilities_json,b.optional_capabilities_json,\
                b.provided_apis_json
         FROM auth_instances i \
                JOIN auth_principals u ON u.principal_id=i.principal_id
         JOIN auth_deployments d ON d.deployment_id=i.deployment_id
         JOIN auth_deployment_bindings b ON b.deployment_id=i.deployment_id
         JOIN auth_installed_participants p ON \
                p.participant_id=d.participant_id \
                AND p.revision=i.installed_revision
         JOIN auth_installed_participants c ON \
                c.participant_id=d.participant_id \
                AND c.revision=b.installed_revision
         WHERE i.instance_id=?1 \
                AND i.state='active' \
                AND u.state='active' \
                AND d.state='active' \
                AND (d.expires_at IS NULL \
                OR d.expires_at>?2)",
            params![instance, now],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?;
    let (
        deployment_id,
        participant_id,
        principal_id,
        participant_digest,
        pinned,
        current,
        resources,
        is_current,
        required,
        optional,
        provided,
    ) = row.ok_or(AuthError::Denied)?;
    // The current revision already has transactionally compiled commitments.
    // Recompile only when comparing a pinned revision against its replacement.
    if is_current {
        return Ok(NativeInstanceProjection {
            deployment_id,
            participant_id,
            principal_id,
            participant_digest,
            graph: None,
            evidence: pinned,
            resources: decode(&resources)?,
            required: decode(&required)?,
            optional: decode(&optional)?,
            provided: decode(&provided)?,
        });
    }
    let graph = trellis_idl::compile_evidence(decode(&pinned)?)
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let current = trellis_idl::compile_evidence(decode(&current)?)
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let installed = graph
        .packages()
        .values()
        .flat_map(|p| p.participants().values())
        .find(|p| p.identity().as_str() == participant_id)
        .ok_or(AuthError::Denied)?;
    let latest = current
        .packages()
        .values()
        .flat_map(|p| p.participants().values())
        .find(|p| p.identity().as_str() == participant_id)
        .ok_or(AuthError::Denied)?;
    let needs = graph
        .participant_needs(installed.identity())
        .ok_or(AuthError::Denied)?;
    let current_needs = current
        .participant_needs(latest.identity())
        .ok_or(AuthError::Denied)?;
    let allowed: std::collections::BTreeSet<_> = current_needs
        .required_capabilities()
        .iter()
        .chain(current_needs.optional_capabilities())
        .collect();
    if needs
        .required_capabilities()
        .iter()
        .any(|capability| !allowed.contains(capability))
    {
        return Err(AuthError::Denied);
    }
    let required = needs
        .required_capabilities()
        .iter()
        .map(ToString::to_string)
        .collect();
    let optional = needs
        .optional_capabilities()
        .iter()
        .filter(|capability| allowed.contains(capability))
        .map(ToString::to_string)
        .collect();
    let mut provided = Vec::new();
    for api in installed.implements() {
        if !latest.implements().contains(api) {
            return Err(AuthError::Denied);
        }
        let (generation, evidence): (u64, String) = connection.query_row(
            "SELECT generation,definition_json FROM auth_accepted_apis WHERE api_id=?1",
            [api.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let accepted = trellis_idl::compile_evidence(decode(&evidence)?)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let implementation = graph.api(api).ok_or(AuthError::Denied)?;
        if !trellis_idl::compare_implementation(
            implementation,
            resolve_api(&accepted, api.as_str())?,
        )
        .compatible
        {
            return Err(AuthError::Denied);
        }
        let implemented_actions = implementation
            .definition()
            .actions()
            .keys()
            .flat_map(|identity| {
                action_identity(&ActionSelection {
                    action: identity.clone(),
                    direction: match identity.kind {
                        ActionKind::Rpc => InteractionDirection::Call,
                        ActionKind::Operation => InteractionDirection::Invoke,
                        ActionKind::Event => InteractionDirection::Publish,
                        ActionKind::Live => InteractionDirection::Subscribe,
                    },
                })
            })
            .collect();
        provided.push(ProviderImplementation {
            api_id: api.to_string(),
            generation,
            implementation_digest: trellis_idl::api_digest(&graph, api)
                .map_err(|error| AuthError::Invalid(error.to_string()))?,
            implemented_actions,
        });
    }
    let mut resources: Value = decode(&resources)?;
    for (group, kind) in [("state", 0), ("kv", 1), ("store", 2), ("eventConsumers", 4)] {
        if let Some(entries) = resources.get_mut(group).and_then(Value::as_object_mut) {
            entries.retain(|name, _| {
                installed.resources().iter().any(|(local, definition)| {
                    local.as_str() == name
                        && match definition {
                            ResourceDefinition::State { .. } => kind == 0,
                            ResourceDefinition::Kv { .. } => kind == 1,
                            ResourceDefinition::Store { .. } => kind == 2,
                            ResourceDefinition::Consumer { .. } => kind == 4,
                            _ => false,
                        }
                })
            });
        }
    }
    if let Some(jobs) = resources.get_mut("jobs") {
        if let Some(queues) = jobs.get_mut("queues").and_then(Value::as_object_mut) {
            queues.retain(|name, _| {
                installed.resources().iter().any(|(local, definition)| {
                    local.as_str() == name && matches!(definition, ResourceDefinition::Job { .. })
                })
            });
            if queues.is_empty() {
                resources
                    .as_object_mut()
                    .ok_or(AuthError::Unavailable)?
                    .remove("jobs");
            }
        }
    }
    for (name, definition) in installed.resources() {
        let group = match definition {
            ResourceDefinition::State { .. } => "state",
            ResourceDefinition::Kv { .. } => "kv",
            ResourceDefinition::Store { .. } => "store",
            ResourceDefinition::Job { .. } => "jobs",
            ResourceDefinition::Consumer { .. } => "eventConsumers",
        };
        let mut entries = resources.get_mut(group);
        if group == "jobs" {
            entries = entries.and_then(|jobs| jobs.get_mut("queues"));
        }
        let entries = entries.and_then(Value::as_object_mut);
        let compatible = latest.resources().get(name).is_some_and(|replacement| {
            trellis_idl::compare_resource_evolution(
                graph
                    .resource(installed.identity(), name)
                    .expect("installed resource exists"),
                current
                    .resource(latest.identity(), name)
                    .expect("current resource exists"),
            )
            .compatible
                && match (definition, replacement) {
                    (
                        ResourceDefinition::Job {
                            payload,
                            result,
                            update,
                            key_concurrency,
                            ..
                        },
                        ResourceDefinition::Job {
                            payload: new_payload,
                            result: new_result,
                            update: new_update,
                            key_concurrency: new_key,
                            ..
                        },
                    ) => {
                        trellis_idl::json_schema(&graph, payload).ok()
                            == trellis_idl::json_schema(&current, new_payload).ok()
                            && result
                                .as_ref()
                                .map(|ty| trellis_idl::json_schema(&graph, ty).ok())
                                == new_result
                                    .as_ref()
                                    .map(|ty| trellis_idl::json_schema(&current, ty).ok())
                            && update
                                .as_ref()
                                .map(|ty| trellis_idl::json_schema(&graph, ty).ok())
                                == new_update
                                    .as_ref()
                                    .map(|ty| trellis_idl::json_schema(&current, ty).ok())
                            && key_concurrency == new_key
                    }
                    (
                        ResourceDefinition::Consumer { events, .. },
                        ResourceDefinition::Consumer {
                            events: new_events, ..
                        },
                    ) => events == new_events,
                    _ => true,
                }
        });
        let present = entries
            .as_ref()
            .is_some_and(|entries| entries.contains_key(name.as_str()));
        if !compatible || !present {
            if !definition.optional() {
                return Err(AuthError::RequiredMissing);
            }
            if let Some(entries) = entries {
                entries.remove(name.as_str());
            }
        }
    }
    // Empty optional State is omitted by the existing runtime binding format.
    if resources
        .get("state")
        .and_then(Value::as_object)
        .is_some_and(|state| state.is_empty())
    {
        resources
            .as_object_mut()
            .ok_or(AuthError::Unavailable)?
            .remove("state");
    }
    Ok(NativeInstanceProjection {
        deployment_id,
        participant_id,
        participant_digest,
        principal_id,
        graph: Some(graph),
        evidence: pinned,
        resources,
        required,
        optional,
        provided,
    })
}
