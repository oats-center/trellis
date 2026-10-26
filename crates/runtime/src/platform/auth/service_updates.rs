//! Participant updates a running service submits for itself.
//!
//! See `design/auth/service-updates.md`. An update that asks for no capability
//! or resource the deployment does not already hold installs at once; one that
//! does waits for an administrator. Either way the submitting instance moves to
//! the installed revision only when that revision is exactly its own build.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use trellis_idl::{api_digest, compare_implementation, compile_evidence, ParticipantKind};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo},
    apis::trellis_auth_v1::Provider,
};

use super::catalog::{resolve_api, ApiAcceptance};
use super::deployments::{SELF_HEALTH_CAPABILITY, SERVICE_UPDATE_CAPABILITY};
use super::rpc::{caller, mutation, rpc_error};
use super::sqlite::{decode, digest, id, json, timestamp, Connection};
use super::{
    blocking, Administration, AuthError, EnforcementScope, Mutation, SqliteAuthorizationStore,
};

/// Register the self-service submission and the administrator review surface.
pub(crate) fn register_service_update_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
    operational: async_nats::Client,
) {
    let submit_store = Arc::clone(&store);
    let submit_operational = operational.clone();
    provider.register_service_updates_submit(move |context, input| {
        let store = Arc::clone(&submit_store);
        let operational = submit_operational.clone();
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            let session = context
                .request()
                .verified_request()
                .map(|request| request.caller().authorization_session_id.clone())
                .ok_or_else(|| rpc_error(AuthError::Denied))?;
            let now = crate::clock::unix_now();
            let saved = Arc::clone(&store);
            let update =
                blocking(move || submit(&saved, &actor, &session, &input.package_evidence, now))
                    .await
                    .map_err(rpc_error)?;
            materialize(&store, &operational, &update, now).await?;
            Ok(AuthServiceUpdateResponse {
                update,
                extra: Default::default(),
            })
        }
    });
    let status_store = Arc::clone(&store);
    provider.register_service_updates_status(move |context, input| {
        let store = Arc::clone(&status_store);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    let (_, deployment, _) = submitting_instance(connection, &actor)?;
                    let update = read_update(connection, &store, &input.update_id.to_string())?;
                    if update.deployment_id.to_string() != deployment {
                        return Err(AuthError::NotFound);
                    }
                    Ok(AuthServiceUpdateResponse {
                        update,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let list_store = Arc::clone(&store);
    provider.register_service_updates_list(move |context, input| {
        let store = Arc::clone(&list_store);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    read_updates(connection, &store, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let get_store = Arc::clone(&store);
    provider.register_service_updates_get(move |context, input| {
        let store = Arc::clone(&get_store);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    Ok(AuthServiceUpdateResponse {
                        update: read_update(connection, &store, &input.update_id.to_string())?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_service_updates_decide(move |context, input| {
        let store = Arc::clone(&store);
        let operational = operational.clone();
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManagePrincipals),
                &input,
            )
            .map_err(rpc_error)?;
            let now = crate::clock::unix_now();
            let saved = Arc::clone(&store);
            let response = blocking(move || decide(&saved, &identity, &input, now))
                .await
                .map_err(rpc_error)?;
            materialize(&store, &operational, &response.update, now).await?;
            Ok(response)
        }
    });
}

/// Create storage the newly installed revision declares, through the same owner
/// as instance provisioning. A failure leaves the committed update applied;
/// approving it again retries the materialization.
async fn materialize(
    store: &Arc<SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    update: &AuthServiceUpdate,
    now: i64,
) -> Result<(), trellis_rs::service::ServerError> {
    if update.state != AuthServiceUpdateState::Applied {
        return Ok(());
    }
    super::resources::materialize_native_kv(
        store,
        operational,
        &update.deployment_id.to_string(),
        now,
    )
    .await
    .map(drop)
    .map_err(rpc_error)
}

/// The caller's own service instance: `(instance, deployment, installed revision)`.
fn submitting_instance(
    connection: &Connection,
    principal: &str,
) -> Result<(String, String, u64), AuthError> {
    connection
        .query_row(
            "SELECT i.instance_id,i.deployment_id,i.installed_revision \
            FROM auth_instances i \
            JOIN auth_deployments d USING(deployment_id) \
            JOIN auth_principals p ON p.principal_id=i.principal_id \
            WHERE i.principal_id=?1 AND p.kind='service' AND p.state='active' \
            AND i.state='active' AND d.state='active'",
            [principal],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(AuthError::Denied)
}

/// A compiled candidate participant and the evidence it came from.
struct Candidate {
    evidence: trellis_idl::PackageEvidence,
    graph: Arc<trellis_idl::PackageGraph>,
    participant: String,
    digest: String,
}

impl Candidate {
    fn compile(
        evidence: trellis_idl::PackageEvidence,
        participant: &str,
    ) -> Result<Self, AuthError> {
        let graph = compile_evidence(evidence.clone())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let definition = graph
            .root_package()
            .participants()
            .values()
            .find(|definition| definition.identity().as_str() == participant)
            .ok_or_else(|| {
                AuthError::Invalid(format!("the submitted IDL does not declare {participant}"))
            })?;
        if definition.kind() != ParticipantKind::Service {
            return Err(AuthError::Invalid("only services submit updates".into()));
        }
        let digest = trellis_idl::participant_digest(&graph, definition.identity())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        Ok(Self {
            evidence,
            graph,
            participant: participant.into(),
            digest,
        })
    }

    fn definition(&self) -> &trellis_idl::ParticipantDefinition {
        self.graph
            .root_package()
            .participants()
            .values()
            .find(|definition| definition.identity().as_str() == self.participant)
            .expect("compiled candidate declares its participant")
    }
}

/// What the deployment's current revision selects, declares, and provides.
struct Installed {
    revision: u64,
    binding_revision: u64,
    digest: String,
    title: String,
    capabilities: BTreeSet<String>,
    resources: BTreeMap<(String, String), trellis_idl::ResourceDefinition>,
    apis: BTreeSet<String>,
    resource_commitments: Value,
}

fn installed(connection: &Connection, deployment: &str) -> Result<Installed, AuthError> {
    let (
        participant,
        revision,
        binding_revision,
        digest,
        title,
        required,
        optional,
        provided,
        commitments,
        evidence,
    ): (
        String,
        u64,
        u64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT d.participant_id,b.installed_revision,b.revision,b.implementation_digest,\
        d.title,b.required_capabilities_json,b.optional_capabilities_json,\
        b.provided_apis_json,b.resource_commitments_json,p.projection_json \
        FROM auth_deployments d \
        JOIN auth_deployment_bindings b USING(deployment_id) \
        JOIN auth_installed_participants p ON p.participant_id=d.participant_id \
        AND p.revision=b.installed_revision \
        WHERE d.deployment_id=?1",
        [deployment],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        },
    )?;
    let graph = compile_evidence(decode(&evidence)?)
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let definition = graph
        .root_package()
        .participants()
        .values()
        .find(|definition| definition.identity().as_str() == participant)
        .ok_or(AuthError::Unavailable)?;
    let provided: Vec<Value> = decode(&provided)?;
    Ok(Installed {
        revision,
        binding_revision,
        digest,
        title,
        capabilities: decode::<Vec<String>>(&required)?
            .into_iter()
            .chain(decode::<Vec<String>>(&optional)?)
            .collect(),
        resources: definition
            .resources()
            .iter()
            .map(|(name, resource)| {
                (
                    (resource_kind(resource).into(), name.to_string()),
                    resource.clone(),
                )
            })
            .collect(),
        apis: provided
            .iter()
            .filter_map(|provider| provider["api_id"].as_str().map(str::to_owned))
            .collect(),
        resource_commitments: decode(&commitments)?,
    })
}

fn resource_kind(resource: &trellis_idl::ResourceDefinition) -> &'static str {
    match resource {
        trellis_idl::ResourceDefinition::Kv { .. } => "kv",
        trellis_idl::ResourceDefinition::Store { .. } => "store",
        trellis_idl::ResourceDefinition::State { .. } => "state",
        trellis_idl::ResourceDefinition::Job { .. } => "job",
        trellis_idl::ResourceDefinition::Consumer { .. } => "eventConsumer",
    }
}

/// Whether a redeclared resource asks for more than the old declaration: more
/// retained data or capacity, or delivery of events it did not consume before.
/// Schema changes are representation evolution, which materialization checks.
fn resource_grows(
    old: &trellis_idl::ResourceDefinition,
    new: &trellis_idl::ResourceDefinition,
) -> bool {
    use trellis_idl::ResourceDefinition as R;
    match (old, new) {
        (
            R::Kv {
                history: a,
                ttl_ms: b,
                desired_max_value: c,
                ..
            },
            R::Kv {
                history: x,
                ttl_ms: y,
                desired_max_value: z,
                ..
            },
        ) => (a, b, c) != (x, y, z),
        (
            R::Store {
                ttl_ms: a,
                desired_max_object: b,
                desired_max_total: c,
                ..
            },
            R::Store {
                ttl_ms: x,
                desired_max_object: y,
                desired_max_total: z,
                ..
            },
        ) => (a, b, c) != (x, y, z),
        (R::Consumer { events: a, .. }, R::Consumer { events: b, .. }) => !b.is_subset(a),
        (R::State { .. }, R::State { .. }) | (R::Job { .. }, R::Job { .. }) => false,
        _ => true,
    }
}

/// Compare a candidate with the deployment's current revision. An implemented
/// API must stay compatible with its accepted definition; a compatible change
/// is accepted when the update installs.
fn changes(
    connection: &Connection,
    deployment: &str,
    current: &Installed,
    candidate: &Candidate,
) -> Result<AuthServiceUpdateChanges, AuthError> {
    let definition = candidate.definition();
    let needs = candidate
        .graph
        .participant_needs(definition.identity())
        .ok_or(AuthError::Unavailable)?;
    let capabilities: BTreeSet<String> = needs
        .required_capabilities()
        .iter()
        .chain(needs.optional_capabilities())
        .map(ToString::to_string)
        .collect();
    let mut owned = connection.prepare(
        "SELECT kind,local_name FROM auth_resources \
        WHERE owner_kind='deployment' AND owner_id=?1 AND state!='destroying'",
    )?;
    let owned: BTreeSet<(String, String)> = owned
        .query_map([deployment], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let resources: BTreeMap<(String, String), &trellis_idl::ResourceDefinition> = definition
        .resources()
        .iter()
        .map(|(name, resource)| ((resource_kind(resource).into(), name.to_string()), resource))
        .collect();
    let mut added_resources = Vec::new();
    let mut reattached_resources = Vec::new();
    let mut changed_resources = Vec::new();
    for (key, resource) in &resources {
        match current.resources.get(key) {
            Some(old) if resource_grows(old, resource) => changed_resources.push(key),
            Some(_) => {}
            // A resource an earlier revision dropped is still this deployment's,
            // data included: declaring it again asks to reattach that data.
            None if owned.contains(key) => reattached_resources.push(key),
            None => added_resources.push(key),
        }
    }
    let removed_resources: Vec<_> = current
        .resources
        .keys()
        .filter(|key| !resources.contains_key(*key))
        .collect();
    let apis: BTreeSet<String> = definition
        .implements()
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut accepted_apis = Vec::new();
    let mut shared_apis = Vec::new();
    for identity in definition.implements() {
        let api = &identity.to_string();
        // Every deployment serving an API shares one queue group, so a second
        // one splits callers' requests with the first. That needs approval with
        // an explicit acknowledgement.
        let served_elsewhere: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_deployment_bindings b \
            JOIN auth_deployments d USING(deployment_id), json_each(b.provided_apis_json) p \
            WHERE d.deployment_id!=?1 AND d.state!='revoked' \
            AND json_extract(p.value,'$.api_id')=?2)",
            params![deployment, api],
            |row| row.get(0),
        )?;
        if served_elsewhere && !current.apis.contains(api) {
            shared_apis.push(api.clone());
        }
        let accepted: Option<(String, String)> = connection
            .query_row(
                "SELECT definition_digest,definition_json FROM auth_accepted_apis WHERE api_id=?1",
                [api],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let proposed = api_digest(&candidate.graph, identity)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        match accepted {
            None => accepted_apis.push(api.clone()),
            Some((digest, _)) if digest == proposed => {}
            Some((_, accepted)) => {
                let accepted = compile_evidence(decode(&accepted)?)
                    .map_err(|error| AuthError::Invalid(error.to_string()))?;
                if !compare_implementation(
                    resolve_api(&accepted, api)?,
                    resolve_api(&candidate.graph, api)?,
                )
                .compatible
                {
                    return Err(AuthError::Invalid(format!(
                        "API {api} changed incompatibly with its accepted definition; \
                        an administrator must force-replace it first"
                    )));
                }
                accepted_apis.push(api.clone());
            }
        }
    }
    let resource = |(kind, name): &&(String, String)| json!({"kind": kind, "name": name});
    Ok(serde_json::from_value(json!({
        "addedCapabilities": capabilities.difference(&current.capabilities).collect::<Vec<_>>(),
        "removedCapabilities": current.capabilities.difference(&capabilities).collect::<Vec<_>>(),
        "addedResources": added_resources.iter().map(resource).collect::<Vec<_>>(),
        "reattachedResources": reattached_resources.iter().map(resource).collect::<Vec<_>>(),
        "changedResources": changed_resources.iter().map(resource).collect::<Vec<_>>(),
        "removedResources": removed_resources.iter().map(resource).collect::<Vec<_>>(),
        "addedApis": apis.difference(&current.apis).collect::<Vec<_>>(),
        "sharedApis": shared_apis,
        "removedApis": current.apis.difference(&apis).collect::<Vec<_>>(),
        "acceptedApis": accepted_apis,
    }))?)
}

fn requests_access(changes: &AuthServiceUpdateChanges) -> bool {
    !changes.added_capabilities.is_empty()
        || !changes.added_resources.is_empty()
        || !changes.reattached_resources.is_empty()
        || !changes.changed_resources.is_empty()
        || !changes.added_apis.is_empty()
        || !changes.shared_apis.is_empty()
}

fn submit(
    store: &SqliteAuthorizationStore,
    principal: &str,
    session: &str,
    evidence: &AuthPackageEvidence,
    now: i64,
) -> Result<AuthServiceUpdate, AuthError> {
    let evidence: trellis_idl::PackageEvidence =
        serde_json::from_value(serde_json::to_value(evidence)?)?;
    store.transaction(|connection| {
        let (instance, deployment, pinned) = submitting_instance(connection, principal)?;
        let participant: String = connection.query_row(
            "SELECT participant_id FROM auth_deployments WHERE deployment_id=?1",
            [&deployment],
            |row| row.get(0),
        )?;
        let candidate = Candidate::compile(evidence, &participant)?;
        let running: String = connection.query_row(
            "SELECT participant_digest FROM auth_installed_participants \
            WHERE participant_id=?1 AND revision=?2",
            params![participant, pinned],
            |row| row.get(0),
        )?;
        if candidate.digest == running {
            // Submitting the build an instance already runs would roll its
            // deployment back to it; that is the administrator's call.
            return Err(AuthError::Invalid(
                "this instance already runs the submitted build".into(),
            ));
        }
        let current = installed(connection, &deployment)?;
        // Several instances of one deployment share one pending update.
        let pending: Option<String> = connection
            .query_row(
                "SELECT update_id FROM auth_service_updates \
                WHERE deployment_id=?1 AND participant_digest=?2 AND state='pending' \
                AND base_binding_revision=?3",
                params![deployment, candidate.digest, current.binding_revision],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(pending) = pending {
            return read_update(connection, store, &pending);
        }
        let changes = if candidate.digest == current.digest {
            // An administrator already installed this exact build.
            serde_json::from_value(json!({
                "addedCapabilities": [], "removedCapabilities": [], "addedResources": [],
                "changedResources": [], "reattachedResources": [], "removedResources": [], "addedApis": [],
                "sharedApis": [],
                "removedApis": [], "acceptedApis": []
            }))?
        } else {
            changes(connection, &deployment, &current, &candidate)?
        };
        // Deploying administration capabilities takes platform.manage, which a
        // service never holds; such an update always waits for an administrator.
        let needs_approval = requests_access(&changes)
            || (candidate.digest != current.digest
                && SqliteAuthorizationStore::require_platform_for(
                    connection,
                    principal,
                    current.capabilities.iter().map(String::as_str),
                )
                .is_err());
        let update = id();
        connection.execute(
            "UPDATE auth_service_updates SET state='superseded',decided_at=?2,revision=revision+1 \
            WHERE deployment_id=?1 AND state='pending'",
            params![deployment, now],
        )?;
        connection.execute(
            "INSERT INTO auth_service_updates(update_id,deployment_id,instance_id,participant_id,\
            participant_digest,evidence_json,base_binding_revision,changes_json,state,\
            requested_at,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,1)",
            params![
                update,
                deployment,
                instance,
                participant,
                candidate.digest,
                json(&candidate.evidence)?,
                current.binding_revision,
                json(&changes)?,
                now
            ],
        )?;
        if !needs_approval {
            let identity = Mutation {
                actor: principal.into(),
                request_id: id(),
                request_digest: digest(&update)?,
                session: None,
            };
            apply(
                connection, store, &identity, &update, &candidate, &changes, &current, now,
            )?;
            // Moving the instance retires the session that asked; it still
            // gets this reply before it is closed.
            super::revocation::defer_caller_enforcement(
                connection,
                session,
                &EnforcementScope::Deployment(deployment),
                now,
            )?;
        }
        read_update(connection, store, &update)
    })
}

/// Install the candidate as the deployment's revision and move the submitting
/// instance onto it, in the caller's transaction.
#[allow(clippy::too_many_arguments)]
fn apply(
    connection: &Connection,
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    update: &str,
    candidate: &Candidate,
    changes: &AuthServiceUpdateChanges,
    current: &Installed,
    now: i64,
) -> Result<(), AuthError> {
    let (deployment, instance): (String, String) = connection.query_row(
        "SELECT deployment_id,instance_id FROM auth_service_updates WHERE update_id=?1",
        [update],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if candidate.digest != current.digest {
        for api in &changes.accepted_apis {
            let review = store.review_api_in_transaction(
                connection,
                candidate.evidence.clone(),
                api.as_ref(),
                now,
            )?;
            store.accept_api_in_transaction(
                connection,
                identity,
                &ApiAcceptance {
                    review_id: review.review_id,
                    reviewed_digest: review.definition_digest,
                    force: false,
                    acknowledge_breakage: false,
                    confirm_consent_meanings: false,
                    materially_changed: vec![],
                    policy_edits: vec![],
                },
                now,
            )?;
        }
        let provisioning = super::rpc_deployments::deployment_input(
            connection,
            &serde_json::from_value(serde_json::to_value(&candidate.evidence)?)?,
            &candidate.participant,
            current.resource_commitments.clone(),
        )?;
        SqliteAuthorizationStore::install_deployment_in_transaction(
            connection,
            &identity.actor,
            &provisioning,
            Some(&deployment),
            &current.title,
            now,
        )?;
    }
    let installed = installed(connection, &deployment)?;
    let (principal, revision): (String, u64) = connection.query_row(
        "SELECT principal_id,installed_revision FROM auth_instances WHERE instance_id=?1",
        [&instance],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if revision != installed.revision {
        // The instance gives up what its old revision selected and the new one
        // does not, and gains what the new one adds. Grants an administrator
        // made directly are not part of either selection and stay.
        let previous = selected_capabilities(connection, &deployment, revision)?;
        for removed in previous.difference(&installed.capabilities) {
            connection.execute(
                "DELETE FROM auth_direct_capabilities WHERE principal_id=?1 AND capability_id=?2",
                params![principal, removed],
            )?;
        }
        let expiry: Option<i64> = connection.query_row(
            "SELECT expires_at FROM auth_deployments WHERE deployment_id=?1",
            [&deployment],
            |row| row.get(0),
        )?;
        for added in installed.capabilities.iter().chain(
            [
                SELF_HEALTH_CAPABILITY.to_owned(),
                SERVICE_UPDATE_CAPABILITY.to_owned(),
            ]
            .iter(),
        ) {
            let capability = super::policy::capability(connection, added)?;
            connection.execute(
                "INSERT OR IGNORE INTO auth_direct_capabilities(grant_id,principal_id,\
                capability_id,capability_generation,revision,expires_at,created_at) \
                VALUES(?1,?2,?3,?4,1,?5,?6)",
                params![
                    id(),
                    principal,
                    added,
                    capability.identity_generation.get(),
                    expiry,
                    now
                ],
            )?;
        }
        connection.execute(
            "UPDATE auth_instances SET installed_revision=?2,version=version+1,updated_at=?3 \
            WHERE instance_id=?1",
            params![instance, installed.revision, now],
        )?;
    }
    SqliteAuthorizationStore::policy_changed(
        connection,
        &EnforcementScope::Deployment(deployment),
        now,
    )?;
    connection.execute(
        "UPDATE auth_service_updates SET state='applied',decided_at=?2,decided_by=?3,\
        revision=revision+1 WHERE update_id=?1",
        params![update, now, identity.actor],
    )?;
    Ok(())
}

/// The capabilities an installed revision of this deployment selected.
fn selected_capabilities(
    connection: &Connection,
    deployment: &str,
    revision: u64,
) -> Result<BTreeSet<String>, AuthError> {
    let (participant, evidence): (String, String) = connection.query_row(
        "SELECT p.participant_id,p.projection_json FROM auth_deployments d \
        JOIN auth_installed_participants p ON p.participant_id=d.participant_id \
        WHERE d.deployment_id=?1 AND p.revision=?2",
        params![deployment, revision],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let graph = compile_evidence(decode(&evidence)?)
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let definition = graph
        .root_package()
        .participants()
        .values()
        .find(|definition| definition.identity().as_str() == participant)
        .ok_or(AuthError::Unavailable)?;
    let needs = graph
        .participant_needs(definition.identity())
        .ok_or(AuthError::Unavailable)?;
    Ok(needs
        .required_capabilities()
        .iter()
        .chain(needs.optional_capabilities())
        .map(ToString::to_string)
        .collect())
}

fn decide(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthServiceUpdateDecideRequest,
    now: i64,
) -> Result<AuthServiceUpdateResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.serviceUpdates.decide",
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_administration(
                connection,
                &identity.actor,
                Administration::ManagePrincipals,
            )?;
            let update = input.update_id.to_string();
            let (deployment, participant, evidence, base, _, state, revision): (
                String,
                String,
                String,
                u64,
                String,
                String,
                u64,
            ) = connection
                .query_row(
                    "SELECT deployment_id,participant_id,evidence_json,base_binding_revision,\
                changes_json,state,revision FROM auth_service_updates WHERE update_id=?1",
                    [&update],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            if state == "applied" && input.decision == AuthDecision::Approved {
                // Approving an installed update again only retries its storage.
                return Ok(AuthServiceUpdateResponse {
                    update: read_update(connection, store, &update)?,
                    extra: Default::default(),
                });
            }
            if state != "pending" || revision != input.expected_revision.0 .0 {
                return Err(AuthError::Conflict);
            }
            let current = installed(connection, &deployment)?;
            if current.binding_revision != base {
                // Someone changed the deployment since the service submitted; its
                // changes no longer describe what approval would do.
                connection.execute(
                    "UPDATE auth_service_updates SET state='superseded',decided_at=?2,\
                decided_by=?3,revision=revision+1 WHERE update_id=?1",
                    params![update, now, identity.actor],
                )?;
            } else if input.decision == AuthDecision::Approved {
                let candidate = Candidate::compile(decode(&evidence)?, &participant)?;
                // Recomputed, so an API another service started serving since
                // the submission is shown and acknowledged before approval.
                let changes = self::changes(connection, &deployment, &current, &candidate)?;
                if !changes.shared_apis.is_empty() && input.acknowledge_shared_apis != Some(true) {
                    return Err(AuthError::Invalid(format!(
                        "this update makes the service answer {} alongside the service that \
                        already does; callers' requests would be split between them. Approve \
                        it with acknowledgeSharedApis if that is intended",
                        changes
                            .shared_apis
                            .iter()
                            .map(|api| api.as_ref())
                            .collect::<Vec<&str>>()
                            .join(", ")
                    )));
                }
                SqliteAuthorizationStore::require_platform_for(
                    connection,
                    &identity.actor,
                    changes.added_capabilities.iter().map(AsRef::as_ref),
                )?;
                apply(
                    connection, store, identity, &update, &candidate, &changes, &current, now,
                )?;
            } else {
                connection.execute(
                    "UPDATE auth_service_updates SET state='rejected',decided_at=?2,\
                decided_by=?3,revision=revision+1 WHERE update_id=?1",
                    params![update, now, identity.actor],
                )?;
            }
            Ok(AuthServiceUpdateResponse {
                update: read_update(connection, store, &update)?,
                extra: Default::default(),
            })
        },
    )
}

fn read_update(
    connection: &Connection,
    store: &SqliteAuthorizationStore,
    update: &str,
) -> Result<AuthServiceUpdate, AuthError> {
    let (deployment, instance, participant, digest, changes, state, requested, decided, revision): (
        String, String, String, String, String, String, i64, Option<i64>, u64,
    ) = connection
        .query_row(
            "SELECT deployment_id,instance_id,participant_id,participant_digest,changes_json,\
            state,requested_at,decided_at,revision FROM auth_service_updates WHERE update_id=?1",
            [update],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let ulid = |value: &str| value.parse().map_err(|_| AuthError::Unavailable);
    Ok(AuthServiceUpdate {
        update_id: ulid(update)?,
        deployment_id: ulid(&deployment)?,
        instance_id: ulid(&instance)?,
        participant_id: participant,
        participant_digest: digest.into(),
        state: serde_json::from_value(state.into())?,
        changes: decode(&changes)?,
        review_url: format!(
            "{}/console/admin/services/{deployment}?update={update}",
            store.settings.console_origin
        ),
        requested_at: timestamp(requested)?,
        decided_at: decided.map(timestamp).transpose()?,
        revision: AuthRevision(revision.into()),
        extra: Default::default(),
    })
}

fn read_updates(
    connection: &Connection,
    store: &SqliteAuthorizationStore,
    input: &AuthServiceUpdatesListRequest,
) -> Result<AuthServiceUpdatesListResponse, AuthError> {
    let limit = input
        .page
        .as_ref()
        .and_then(|page| page.limit)
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    let limit = limit as usize;
    let cursor = input
        .page
        .as_ref()
        .and_then(|page| page.cursor.clone())
        .unwrap_or_default();
    // Newest first: update IDs are ULIDs, so the cursor walks down from the last one shown.
    let mut statement = connection.prepare(
        "SELECT update_id FROM auth_service_updates \
        WHERE (?1='' OR update_id<?1) AND (?2 IS NULL OR deployment_id=?2) \
        ORDER BY update_id DESC LIMIT ?3",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.deployment_id.as_ref().map(ToString::to_string),
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    Ok(CursorPage {
        items: ids
            .iter()
            .map(|update| read_update(connection, store, update))
            .collect::<Result<_, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}
