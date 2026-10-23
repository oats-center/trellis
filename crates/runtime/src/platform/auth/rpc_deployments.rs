//! Typed catalog and provisioning reads over the authoritative SQLite aggregates.

use super::sqlite::timestamp;
use std::sync::Arc;
use trellis_rs::generated::TrellisError;
use trellis_rs::service::{DeclaredRpcError, ServerError};
use trellis_runtime_apis::apis::trellis_auth_v1::errors::ValidationError as AuthValidationError;

use crate::platform::auth::sqlite::Connection;
use rusqlite::{params, OptionalExtension};
use trellis_protocol::PlatformPrivilege;
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo, CursorQuery},
    apis::trellis_auth_v1::Provider,
};

use super::{
    catalog::{ApiAcceptance, ApiReview},
    rpc::{caller, mutation, rpc_error},
    AuthError, Mutation, PolicyMutation, SqliteAuthorizationStore,
};

/// Register supported catalog operations and deployment/instance projections.
pub(crate) fn register_deployment_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
    operational: async_nats::Client,
    oauth: Arc<super::OAuthEngine>,
) {
    macro_rules! read {
        ($registration:ident, $privilege:ident, $name:literal, $operation:expr) => {{
            let store = Arc::clone(&store);
            provider.$registration(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let actor =
                        caller(&context, Some(PlatformPrivilege::$privilege)).map_err(rpc_error)?;
                    tokio::task::spawn_blocking(move || {
                        store.transaction(|connection| {
                            SqliteAuthorizationStore::require_privilege(connection, &actor, $name)?;
                            ($operation)(connection, &input)
                        })
                    })
                    .await
                    .map_err(|_| rpc_error(AuthError::Unavailable))?
                    .map_err(rpc_error)
                }
            });
        }};
    }
    read!(register_apis_list, ApisAccept, "apis.accept", read_apis);
    read!(register_apis_get, ApisAccept, "apis.accept", read_api);
    read!(
        register_deployments_list,
        PrincipalsManage,
        "principals.manage",
        read_deployments
    );
    read!(
        register_deployments_get,
        PrincipalsManage,
        "principals.manage",
        |connection: &Connection, input: &AuthDeploymentGetRequest| {
            Ok(AuthDeploymentGetResponse {
                deployment: read_deployment(connection, &input.deployment_id.to_string())?,
                extra: Default::default(),
            })
        }
    );
    read!(
        register_service_instances_list,
        PrincipalsManage,
        "principals.manage",
        |connection: &Connection, input: &AuthInstancesListRequest| read_instances(
            connection, input, "service"
        )
    );
    read!(
        register_devices_list,
        PrincipalsManage,
        "principals.manage",
        |connection: &Connection, input: &AuthInstancesListRequest| read_instances(
            connection, input, "device"
        )
    );
    let reviews = Arc::clone(&store);
    provider.register_apis_review(move |context, input| {
        let store = Arc::clone(&reviews);
        async move {
            let actor = caller(&context, Some(PlatformPrivilege::ApisAccept)).map_err(rpc_error)?;
            tokio::task::spawn_blocking(move || {
                review_api(&store, &actor, &input, crate::clock::unix_now())
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
    macro_rules! accept {
        ($registration:ident, $privilege:ident, $operation:ident) => {{
            let store = Arc::clone(&store);
            provider.$registration(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let identity = mutation(
                        &context,
                        &input.idempotency_key,
                        Some(PlatformPrivilege::$privilege),
                        &input,
                    )
                    .map_err(rpc_error)?;
                    tokio::task::spawn_blocking(move || {
                        $operation(&store, &identity, &input, crate::clock::unix_now())
                    })
                    .await
                    .map_err(|_| rpc_error(AuthError::Unavailable))?
                    .map_err(rpc_error)
                }
            });
        }};
    }
    accept!(register_apis_accept, ApisAccept, accept_api);
    accept!(
        register_apis_force_replace,
        ApisForceReplace,
        force_replace_api
    );
    macro_rules! provision {
        ($registration:ident, $operation:expr) => {{
            let store = Arc::clone(&store);
            provider.$registration(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let identity = mutation(
                        &context,
                        &input.idempotency_key,
                        Some(PlatformPrivilege::PrincipalsManage),
                        &input,
                    )
                    .map_err(rpc_error)?;
                    tokio::task::spawn_blocking(move || {
                        ($operation)(&store, &identity, &input, crate::clock::unix_now())
                    })
                    .await
                    .map_err(|_| rpc_error(AuthError::Unavailable))?
                    .map_err(|error| match error {
                        AuthError::Invalid(ref code) if code == "identity_public_key_required" => {
                            ServerError::DeclaredRpc(DeclaredRpcError::new(
                                <AuthValidationError as TrellisError>::TYPE,
                                "Identity public key required",
                                [(
                                    "code",
                                    serde_json::Value::String(
                                        "identity_public_key_required".into(),
                                    ),
                                )],
                            ))
                        }
                        error => rpc_error(error),
                    })
                }
            });
        }};
    }
    provision!(register_deployments_create, create_deployment);
    provision!(register_deployments_apply, apply_deployment);
    provision!(
        register_deployments_enable,
        |store, identity, input, now| change_deployment(store, identity, input, "active", now)
    );
    provision!(
        register_deployments_disable,
        |store, identity, input, now| change_deployment(store, identity, input, "disabled", now)
    );
    provision!(
        register_deployments_remove,
        |store, identity, input, now| change_deployment(store, identity, input, "revoked", now)
    );
    let instances = Arc::clone(&store);
    let service_operational = operational.clone();
    provider.register_service_instances_provision(move |context, input| {
        let store = Arc::clone(&instances);
        let operational = service_operational.clone();
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(PlatformPrivilege::PrincipalsManage),
                &input,
            )
            .map_err(rpc_error)?;
            provision_service_instance(
                &store,
                &operational,
                &identity,
                &input,
                crate::clock::unix_now(),
            )
            .await
            .map_err(|error| match error {
                AuthError::Invalid(ref code) if code == "identity_public_key_required" => {
                    ServerError::DeclaredRpc(DeclaredRpcError::new(
                        <AuthValidationError as TrellisError>::TYPE,
                        "Identity public key required",
                        [(
                            "code",
                            serde_json::Value::String("identity_public_key_required".into()),
                        )],
                    ))
                }
                error => rpc_error(error),
            })
        }
    });
    let devices = Arc::clone(&store);
    provider.register_devices_provision(move |context, input| {
        let store = devices.clone();
        let operational = operational.clone();
        let oauth = oauth.clone();
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(PlatformPrivilege::PrincipalsManage),
                &input,
            )
            .map_err(rpc_error)?;
            let deployment = input.deployment_id.to_string();
            let now = crate::clock::unix_now();
            let active = input.identity_public_key.is_some();
            let response = tokio::task::spawn_blocking(move || {
                super::oauth::enrollment::provision(oauth.server.store(), &identity, &input, now)
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(|error| match error {
                AuthError::Invalid(ref code) if code == "identity_public_key_required" => {
                    ServerError::DeclaredRpc(DeclaredRpcError::new(
                        <AuthValidationError as TrellisError>::TYPE,
                        "Identity public key required",
                        [(
                            "code",
                            serde_json::Value::String("identity_public_key_required".into()),
                        )],
                    ))
                }
                error => rpc_error(error),
            })?;
            if active {
                super::resources::materialize_native_kv(&store, &operational, &deployment, now)
                    .await
                    .map_err(rpc_error)?;
            }
            Ok(response)
        }
    });
    provision!(
        register_service_instances_enable,
        |store, identity, input, now| change_instance(
            store, identity, input, "service", "active", now
        )
    );
    provision!(
        register_service_instances_disable,
        |store, identity, input, now| change_instance(
            store, identity, input, "service", "disabled", now
        )
    );
    provision!(
        register_service_instances_remove,
        |store, identity, input, now| change_instance(
            store, identity, input, "service", "revoked", now
        )
    );
    provision!(register_devices_enable, |store, identity, input, now| {
        change_instance(store, identity, input, "device", "active", now)
    });
    provision!(register_devices_disable, |store, identity, input, now| {
        change_instance(store, identity, input, "device", "disabled", now)
    });
    provision!(register_devices_remove, |store, identity, input, now| {
        change_instance(store, identity, input, "device", "revoked", now)
    });
}

pub(super) fn create_deployment(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthDeploymentCreateRequest,
    now: i64,
) -> Result<AuthDeploymentMutationResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.deployments.create",
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "principals.manage",
            )?;
            let provisioning = deployment_input(
                connection,
                &input.package_evidence,
                &input.participant_id,
                serde_json::json!({}),
            )?;
            let graph = trellis_idl::compile_evidence(provisioning.evidence.clone())
                .map_err(|error| AuthError::Invalid(error.to_string()))?;
            let participant = graph
                .root_package()
                .participants()
                .values()
                .find(|participant| participant.identity().as_str() == input.participant_id)
                .ok_or(AuthError::NotFound)?;
            let kind = match participant.kind() {
                trellis_idl::ParticipantKind::Service => "service",
                trellis_idl::ParticipantKind::Device => "device",
            };
            if kind != input.kind.as_str() {
                return Err(AuthError::Invalid("participant kind mismatch".into()));
            }
            let deployment = SqliteAuthorizationStore::install_deployment_in_transaction(
                connection,
                &provisioning,
                None,
                &input.title,
                now,
            )?;
            Ok(AuthDeploymentMutationResponse {
                deployment: read_deployment(connection, &deployment)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

fn deployment_input(
    connection: &Connection,
    evidence: &AuthPackageEvidence,
    participant: &str,
    resources: serde_json::Value,
) -> Result<super::DeploymentProvisioning, AuthError> {
    let evidence: trellis_idl::PackageEvidence =
        serde_json::from_value(serde_json::to_value(evidence)?)?;
    let graph = trellis_idl::compile_evidence(evidence.clone())
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let definition = graph
        .root_package()
        .participants()
        .values()
        .find(|definition| definition.identity().as_str() == participant)
        .ok_or(AuthError::NotFound)?;
    let mut expected_api_generations = std::collections::BTreeMap::new();
    for api in definition.implements() {
        let generation = connection
            .query_row(
                "SELECT generation FROM auth_accepted_apis WHERE api_id=?1",
                [api.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(AuthError::Denied)?;
        expected_api_generations.insert(api.to_string(), generation);
    }
    Ok(super::DeploymentProvisioning {
        evidence,
        participant_id: participant.into(),
        expected_api_generations,
        identity_public_key: String::new(),
        resource_commitments: resources,
        expires_at: None,
    })
}

pub(super) fn apply_deployment(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthDeploymentApplyRequest,
    now: i64,
) -> Result<AuthDeploymentMutationResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.deployments.apply",
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "principals.manage",
            )?;
            let deployment = input.deployment_id.to_string();
            let current = read_deployment(connection, &deployment)?;
            if current.revision != input.expected_revision
                || current.state == AuthDeploymentState::Removed
            {
                return Err(AuthError::Conflict);
            }
            let resources: String = connection.query_row(
                "SELECT resource_commitments_json \
                FROM auth_deployment_bindings \
                WHERE deployment_id=?1",
                [&deployment],
                |row| row.get(0),
            )?;
            let provisioning = deployment_input(
                connection,
                &input.package_evidence,
                &current.participant_id,
                serde_json::from_str(&resources)?,
            )?;
            SqliteAuthorizationStore::install_deployment_in_transaction(
                connection,
                &provisioning,
                Some(&deployment),
                &current.title,
                now,
            )?;
            // Processes retain their installed revision. Current policy projects and
            // fences their pinned authority; a newer deployment is not a revocation.
            SqliteAuthorizationStore::policy_changed(
                connection,
                &super::EnforcementScope::Deployment(deployment.clone()),
                now,
            )?;
            Ok(AuthDeploymentMutationResponse {
                deployment: read_deployment(connection, &deployment)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

/// Complete service provisioning through the same resource owner used at startup.
/// The original typed provisioning result remains replayable after broker failure;
/// retrying its request resumes non-destructive materialization before success.
pub(super) async fn provision_service_instance(
    store: &Arc<SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    identity: &Mutation,
    input: &AuthInstanceProvisionRequest,
    now: i64,
) -> Result<AuthInstanceProvisionResponse, AuthError> {
    let saved_store = store.clone();
    let saved_identity = identity.clone();
    let saved_input = input.clone();
    let response = tokio::task::spawn_blocking(move || {
        provision_instance(&saved_store, &saved_identity, &saved_input, "service", now)
    })
    .await
    .map_err(|_| AuthError::Unavailable)??;
    super::resources::materialize_native_kv(
        store,
        operational,
        &input.deployment_id.to_string(),
        now,
    )
    .await?;
    Ok(response)
}

pub(super) fn change_deployment(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthDeploymentMutationRequest,
    state: &str,
    now: i64,
) -> Result<AuthDeploymentMutationResponse, AuthError> {
    store.mutate(
        identity,
        &format!("rpc.deployments.{state}"),
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "principals.manage",
            )?;
            let deployment = input.deployment_id.to_string();
            let current = read_deployment(connection, &deployment)?;
            if current.revision != input.expected_revision
                || current.state == AuthDeploymentState::Removed
            {
                return Err(AuthError::Conflict);
            }
            connection.execute(
                "UPDATE auth_deployments SET state=?2 WHERE deployment_id=?1",
                params![deployment, state],
            )?;
            connection.execute(
                "UPDATE auth_deployment_bindings SET revision=revision+1 WHERE deployment_id=?1",
                [&deployment],
            )?;
            if state != "revoked" {
                // Lifecycle changes retain the same materialization. Advance only
                // evidence that was current before this transition, never stale state.
                connection.execute(
                    "UPDATE auth_resources \
                    SET binding_revision=binding_revision+1 \
                    WHERE owner_kind='deployment' \
                    AND owner_id=?1 AND state='ready' \
                    AND binding_revision=?2",
                    params![deployment, current.revision.0 .0],
                )?;
            }
            if state == "revoked" {
                super::resources::detach_deployment_resources(
                    connection,
                    &deployment,
                    &Default::default(),
                    now,
                )?;
                connection.execute(
                    "UPDATE auth_deployment_bindings \
                    SET resource_commitments_json='{}' \
                    WHERE deployment_id=?1",
                    [&deployment],
                )?;
                connection.execute(
                    "UPDATE auth_provisioned_identities \
                    SET state='revoked',revoked_at=?2 \
                    WHERE deployment_id=?1 \
                    AND state='active'",
                    params![deployment, now],
                )?;
            }
            SqliteAuthorizationStore::policy_changed(
                connection,
                &super::EnforcementScope::Deployment(deployment.clone()),
                now,
            )?;
            Ok(AuthDeploymentMutationResponse {
                deployment: read_deployment(connection, &deployment)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn provision_instance(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthInstanceProvisionRequest,
    kind: &str,
    now: i64,
) -> Result<AuthInstanceProvisionResponse, AuthError> {
    store.mutate(
        identity,
        &format!("rpc.{kind}.provision"),
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "principals.manage",
            )?;
            let deployment = input.deployment_id.to_string();
            let current = read_deployment(connection, &deployment)?;
            if current.kind.as_str() != kind {
                return Err(AuthError::Invalid("deployment kind mismatch".into()));
            }
            let public_key = input
                .identity_public_key
                .as_ref()
                .ok_or_else(|| AuthError::Invalid("identity_public_key_required".into()))?;
            let provisioned = SqliteAuthorizationStore::provision_instance_in_transaction(
                connection,
                &deployment,
                Some(public_key.as_ref()),
                &input.title,
                now,
            )?;
            Ok(AuthInstanceProvisionResponse {
                instance: read_instance(connection, &provisioned.instance_id)?,
                provisioning_secret: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn change_instance(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthInstanceMutationRequest,
    kind: &str,
    state: &str,
    now: i64,
) -> Result<AuthInstanceMutationResponse, AuthError> {
    store.mutate(
        identity,
        &format!("rpc.{kind}.{state}"),
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "principals.manage",
            )?;
            let instance_id = input.instance_id.to_string();
            let instance = read_instance(connection, &instance_id)?;
            if instance.principal.kind.as_str() != kind
                || instance.revision != input.expected_revision
            {
                return Err(AuthError::Conflict);
            }
            let deployment = read_deployment(connection, &instance.deployment_id.to_string())?;
            if state == "active" {
                if deployment.state != AuthDeploymentState::Enabled {
                    return Err(AuthError::Denied);
                }
                let current: bool = connection.query_row(
                    "SELECT i.installed_revision=b.installed_revision \
                    FROM auth_instances i \
                    JOIN auth_deployment_bindings b USING(deployment_id) \
                    WHERE instance_id=?1",
                    [&instance_id],
                    |row| row.get(0),
                )?;
                if !current {
                    return Err(AuthError::Conflict);
                }
            }
            let command = PolicyMutation::PrincipalState {
                principal_id: instance.principal.principal_id.to_string(),
                expected_revision: instance.principal.revision.0 .0,
                state: state.into(),
            };
            super::policy::apply_policy_in_transaction(connection, identity, &command, now)?;
            if connection.execute(
                "UPDATE auth_instances \
                SET state=?2,version=version+1,updated_at=?3 \
                WHERE instance_id=?1 \
                AND version=?4 AND state!='revoked'",
                params![instance_id, state, now, input.expected_revision.0 .0],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            if state == "revoked" {
                connection.execute(
                    "UPDATE auth_provisioned_identities \
                    SET state='revoked',revoked_at=?2 \
                    WHERE instance_id=?1 \
                    AND state='active'",
                    params![instance_id, now],
                )?;
            }
            if kind == "device" {
                connection.execute(
                    "UPDATE auth_devices \
                    SET state=?2,updated_at=?3,version=version+1 \
                    WHERE principal_id=?1",
                    params![instance.principal.principal_id.to_string(), state, now],
                )?;
            }
            Ok(AuthInstanceMutationResponse {
                instance: read_instance(connection, &instance_id)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn read_instance(
    connection: &Connection,
    instance_id: &str,
) -> Result<AuthProvisionedInstance, AuthError> {
    let (deployment, principal, revision, key, title): (
        String,
        String,
        u64,
        Option<String>,
        String,
    ) = connection
        .query_row(
            "SELECT i.deployment_id,i.principal_id,i.version,p.identity_public_key,\
            i.title \
            FROM auth_instances i \
            LEFT JOIN auth_provisioned_identities p USING(instance_id) \
            WHERE i.instance_id=?1",
            [instance_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let (kind, state, created_at, version): (String, String, i64, u64) = connection.query_row(
        "SELECT kind,state,created_at,version FROM auth_principals WHERE principal_id=?1",
        [&principal],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    Ok(AuthProvisionedInstance {
        instance_id: instance_id.parse().map_err(|_| AuthError::Unavailable)?,
        deployment_id: deployment.parse().map_err(|_| AuthError::Unavailable)?,
        identity_public_key: key.map(Into::into),
        revision: AuthRevision(revision.into()),
        principal: AuthPrincipal {
            principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
            title,
            kind: serde_json::from_value(kind.into())?,
            enabled: state == "active",
            created_at: timestamp(created_at)?,
            revision: AuthRevision(version.into()),
            extra: Default::default(),
        },
        extra: Default::default(),
    })
}

pub(super) fn review_api(
    store: &SqliteAuthorizationStore,
    actor: &str,
    input: &AuthApiReviewRequest,
    now: i64,
) -> Result<AuthApiReviewResponse, AuthError> {
    let evidence = serde_json::from_value(serde_json::to_value(&input.package_evidence)?)?;
    store.transaction(|connection| {
        let review =
            store.review_api_in_transaction(connection, actor, evidence, &input.api_id, now)?;
        let graph = trellis_idl::compile_evidence(review.proposed.clone())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let candidate = super::catalog::resolve_api(&graph, &review.api_id)?;
        let mut capability_changes = Vec::new();
        for name in review
            .removed_capabilities
            .iter()
            .chain(&review.consent_changes)
        {
            let previous: Option<(u64, u64)> = connection
                .query_row(
                    "SELECT identity_generation,consent_revision \
                    FROM auth_capability_identities \
                    WHERE capability_id=?1 \
                    AND state='active'",
                    [name],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            capability_changes.push(AuthCapabilityChange {
                capability_id: name.clone().into(),
                description: if review.removed_capabilities.contains(name) {
                    "Removed"
                } else {
                    "Consent meaning changed"
                }
                .into(),
                previous_identity_generation: previous.map(|value| AuthRevision(value.0.into())),
                previous_consent_revision: previous.map(|value| AuthRevision(value.1.into())),
                proposed_consent_revision: candidate
                    .definition()
                    .capabilities()
                    .iter()
                    .find(|(id, _)| id.as_str() == name)
                    .map(|(_, definition)| AuthRevision(definition.consent_revision.into())),
                extra: Default::default(),
            });
        }
        // Changed actions include removed actions: resolve against both definitions.
        let previous: Option<String> = connection
            .query_row(
                "SELECT definition_json FROM auth_accepted_apis WHERE api_id=?1",
                [&review.api_id],
                |row| row.get(0),
            )
            .optional()?;
        let previous = previous
            .map(|value| super::sqlite::decode(&value))
            .transpose()?
            .map(trellis_idl::compile_evidence)
            .transpose()
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let mut action_changes = Vec::new();
        for key in &review.changed_actions {
            let identity = candidate
                .definition()
                .actions()
                .keys()
                .find(|action| format!("{:?}:{}", action.kind, action.name) == *key)
                .cloned()
                .or_else(|| {
                    previous
                        .as_ref()
                        .and_then(|graph| super::catalog::resolve_api(graph, &review.api_id).ok())
                        .and_then(|api| {
                            api.definition()
                                .actions()
                                .keys()
                                .find(|action| format!("{:?}:{}", action.kind, action.name) == *key)
                                .cloned()
                        })
                })
                .ok_or(AuthError::Unavailable)?;
            let direction = match identity.kind {
                trellis_idl::ActionKind::Rpc => trellis_idl::InteractionDirection::Call,
                trellis_idl::ActionKind::Operation => trellis_idl::InteractionDirection::Invoke,
                trellis_idl::ActionKind::Event => trellis_idl::InteractionDirection::Publish,
                trellis_idl::ActionKind::Live => trellis_idl::InteractionDirection::Subscribe,
            };
            for action in super::catalog::action_identity(&trellis_idl::ActionSelection {
                action: identity.clone(),
                direction,
            }) {
                action_changes.push(serde_json::from_value(serde_json::to_value(action)?)?);
            }
        }
        Ok(AuthApiReviewResponse {
            review: AuthApiReview {
                review_id: review
                    .review_id
                    .parse()
                    .map_err(|_| AuthError::Unavailable)?,
                api_id: review.api_id.into(),
                canonical_digest: review.definition_digest.into(),
                compatible: review.compatible,
                breakage: review.breaking_changes,
                capability_changes,
                action_changes,
                affected_role_ids: review
                    .affected_roles
                    .keys()
                    .map(|value| value.parse().map_err(|_| AuthError::Unavailable))
                    .collect::<Result<_, _>>()?,
                affected_deployment_ids: review
                    .affected_providers
                    .iter()
                    .map(|value| value.parse().map_err(|_| AuthError::Unavailable))
                    .collect::<Result<_, _>>()?,
                current_generation: (review.expected_generation != 0)
                    .then(|| AuthRevision(review.expected_generation.into())),
                current_accepted_revision: (review.expected_revision != 0)
                    .then(|| AuthRevision(review.expected_revision.into())),
                expires_at: timestamp(now.saturating_add(900))?,
                extra: Default::default(),
            },
            extra: Default::default(),
        })
    })
}

pub(super) fn accept_api(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthApiAcceptRequest,
    now: i64,
) -> Result<AuthApiAcceptResponse, AuthError> {
    store.mutate(identity, "rpc.apis.accept", input, now, |connection| {
        let acceptance = reviewed_acceptance(
            connection,
            ApiAcceptance {
                review_id: input.review_id.to_string(),
                reviewed_digest: input.canonical_digest.to_string(),
                force: false,
                acknowledge_breakage: false,
                confirm_consent_meanings: false,
                materially_changed: vec![],
                policy_edits: vec![],
            },
            input.expected_generation.0,
            input.expected_accepted_revision.0,
            &input.role_edits,
            &input.mapping_edits,
            now,
        )?;
        let snapshot = store.accept_api_in_transaction(connection, identity, &acceptance, now)?;
        Ok(AuthApiAcceptResponse {
            api: accepted_api(connection, &snapshot.api_id)?,
            enforcement: None,
            extra: Default::default(),
        })
    })
}

pub(super) fn force_replace_api(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthApiForceReplaceRequest,
    now: i64,
) -> Result<AuthApiAcceptResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.apis.forceReplace",
        input,
        now,
        |connection| {
            let acceptance = reviewed_acceptance(
                connection,
                ApiAcceptance {
                    review_id: input.review_id.to_string(),
                    reviewed_digest: input.canonical_digest.to_string(),
                    force: true,
                    acknowledge_breakage: input.acknowledge_breakage,
                    confirm_consent_meanings: input.acknowledge_consent_impact,
                    materially_changed: vec![],
                    policy_edits: vec![],
                },
                input.expected_generation.0 .0,
                input.expected_accepted_revision.0 .0,
                &input.role_edits,
                &input.mapping_edits,
                now,
            )?;
            let snapshot =
                store.accept_api_in_transaction(connection, identity, &acceptance, now)?;
            Ok(AuthApiAcceptResponse {
                api: accepted_api(connection, &snapshot.api_id)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

fn reviewed_acceptance(
    connection: &Connection,
    mut acceptance: ApiAcceptance,
    generation: u64,
    revision: u64,
    roles: &[AuthReviewedRoleEdit],
    mappings: &[AuthReviewedMappingEdit],
    now: i64,
) -> Result<ApiAcceptance, AuthError> {
    let review: String = connection
        .query_row(
            "SELECT review_json FROM auth_api_reviews \
            WHERE review_id=?1 AND expires_at>?2 \
            AND consumed_at IS NULL",
            params![acceptance.review_id, now],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AuthError::Conflict)?;
    let review: ApiReview = super::sqlite::decode(&review)?;
    if (generation, revision) != (review.expected_generation, review.expected_revision) {
        return Err(AuthError::Conflict);
    }
    let mut edits = Vec::new();
    for edit in roles {
        let role = super::rpc_policy::read_role(connection, &edit.role_id.to_string())?;
        edits.push(PolicyMutation::RolePut {
            role_id: edit.role_id.to_string(),
            expected_revision: edit.expected_revision.0 .0,
            title: role.title,
            description: role.description,
            capabilities: edit
                .capability_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
        });
    }
    for edit in mappings {
        let mapping_id = edit.mapping_id.to_string();
        match edit.role_ids.as_slice() {
            [] => edits.push(PolicyMutation::OidcMappingDelete {
                mapping_id,
                expected_revision: edit.expected_revision.0 .0,
            }),
            roles => {
                let (provider_id, claim_name, claim_value) = connection
                    .query_row(
                        "SELECT provider_id,claim_name,claim_value \
                        FROM auth_oidc_role_mappings \
                        WHERE mapping_id=?1",
                        [&mapping_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                    .ok_or(AuthError::NotFound)?;
                edits.push(PolicyMutation::OidcMappingPut {
                    mapping_id,
                    provider_id,
                    claim_name,
                    claim_value,
                    role_ids: roles.iter().map(ToString::to_string).collect(),
                    expected_revision: edit.expected_revision.0 .0,
                });
            }
        }
    }
    if acceptance.force {
        acceptance.materially_changed = review.consent_changes;
    }
    acceptance.policy_edits = edits;
    Ok(acceptance)
}

fn accepted_api(connection: &Connection, api_id: &str) -> Result<AuthAcceptedApi, AuthError> {
    let (generation, revision, digest, accepted_at): (u64, u64, String, i64) = connection
        .query_row(
            "SELECT generation,accepted_revision,definition_digest,accepted_at \
            FROM auth_accepted_apis \
            WHERE api_id=?1",
            [api_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let mut statement = connection.prepare(
        "SELECT c.capability_id,c.identity_generation,c.consent_revision,\
        c.title,c.description,c.consequence \
        FROM auth_capability_identities c \
        WHERE api_id=?1 AND state='active' \
        ORDER BY capability_id",
    )?;
    let rows = statement
        .query_map([api_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let capabilities = rows
        .into_iter()
        .map(|(id, identity, consent, title, description, consequence)| {
            let mut statement = connection.prepare(
                "SELECT a.descriptor_json \
                FROM auth_capability_actions c \
                JOIN auth_api_actions a ON a.api_id=c.api_id \
                AND a.generation=c.api_generation \
                AND a.action_key=c.action_key \
                WHERE c.capability_id=?1 \
                AND c.identity_generation=?2 \
                ORDER BY a.action_key",
            )?;
            let allows = statement
                .query_map(params![id, identity], |row| row.get::<_, String>(0))?
                .map(|row| Ok(serde_json::from_str(&row?)?))
                .collect::<Result<Vec<_>, AuthError>>()?;
            Ok(AuthCapabilityDefinition {
                identity: AuthCapabilityIdentity {
                    capability_id: id.into(),
                    identity_generation: AuthRevision(identity.into()),
                    extra: Default::default(),
                },
                consent_revision: AuthRevision(consent.into()),
                api_id: api_id.to_owned().into(),
                title,
                description,
                consequence,
                allows,
                extra: Default::default(),
            })
        })
        .collect::<Result<_, AuthError>>()?;
    Ok(AuthAcceptedApi {
        api_id: api_id.to_owned().into(),
        generation: AuthRevision(generation.into()),
        accepted_revision: AuthRevision(revision.into()),
        canonical_digest: digest.into(),
        accepted_at: timestamp(accepted_at)?,
        capabilities,
        extra: Default::default(),
    })
}

pub(super) fn read_api(
    connection: &Connection,
    input: &AuthApiGetRequest,
) -> Result<AuthApiGetResponse, AuthError> {
    let evidence: String = connection
        .query_row(
            "SELECT definition_json FROM auth_accepted_apis WHERE api_id=?1",
            [input.api_id.as_ref()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let evidence: trellis_idl::PackageEvidence = super::sqlite::decode(&evidence)?;
    let graph = trellis_idl::compile_evidence(evidence).map_err(|_| AuthError::Unavailable)?;
    let canonical_idl = trellis_idl::canonical_package(
        &graph,
        graph.root(),
        trellis_idl::CanonicalMode::Presentation,
    )
    .map_err(|_| AuthError::Unavailable)?;
    Ok(AuthApiGetResponse {
        api: accepted_api(connection, &input.api_id)?,
        canonical_idl,
        extra: Default::default(),
    })
}

pub(super) fn read_apis(
    connection: &Connection,
    input: &AuthPageRequest,
) -> Result<AuthApisListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT api_id FROM auth_accepted_apis \
        WHERE api_id>?1 AND (?2 IS NULL \
        OR instr(lower(api_id),lower(?2))>0) \
        ORDER BY api_id LIMIT ?3",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.search.as_ref().map(|value| value.as_ref()),
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
            .map(|id| accepted_api(connection, id))
            .collect::<Result<_, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

fn page_bounds(page: Option<&CursorQuery>) -> Result<(&str, usize), AuthError> {
    let limit = page.and_then(|page| page.limit).unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    Ok((
        page.and_then(|page| page.cursor.as_deref()).unwrap_or(""),
        limit as usize,
    ))
}

pub(super) fn read_deployment(
    connection: &Connection,
    deployment_id: &str,
) -> Result<AuthDeployment, AuthError> {
    let (participant, kind, state, revision, digest, required, optional, title): (
        String,
        String,
        String,
        u64,
        String,
        String,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT d.participant_id,d.participant_kind,d.state,b.revision,\
            b.implementation_digest,b.required_capabilities_json,\
            b.optional_capabilities_json,d.title \
            FROM auth_deployments d \
            JOIN auth_deployment_bindings b USING(deployment_id) \
            WHERE deployment_id=?1",
            [deployment_id],
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
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    Ok(AuthDeployment {
        deployment_id: deployment_id.parse().map_err(|_| AuthError::Unavailable)?,
        title,
        participant_id: participant,
        kind: serde_json::from_value(kind.into())?,
        state: match state.as_str() {
            "active" => AuthDeploymentState::Enabled,
            "disabled" => AuthDeploymentState::Disabled,
            "revoked" => AuthDeploymentState::Removed,
            _ => return Err(AuthError::Unavailable),
        },
        revision: AuthRevision(revision.into()),
        implementation_digest: digest.into(),
        selection: AuthCapabilitySelection {
            required_capabilities: serde_json::from_str(&required)?,
            optional_capabilities: serde_json::from_str(&optional)?,
            extra: Default::default(),
        },
        extra: Default::default(),
    })
}

pub(super) fn read_deployments(
    connection: &Connection,
    input: &AuthPageRequest,
) -> Result<CursorPage<AuthDeployment>, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT deployment_id \
        FROM auth_deployments \
        WHERE deployment_id>?1 \
        AND (?2 IS NULL OR instr(lower(participant_id),lower(?2))>0 \
        OR instr(lower(title),lower(?2))>0) \
        ORDER BY deployment_id \
        LIMIT ?3",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.search.as_ref().map(|value| value.as_ref()),
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
            .map(|id| read_deployment(connection, id))
            .collect::<Result<_, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

pub(super) fn read_instances(
    connection: &Connection,
    input: &AuthInstancesListRequest,
    kind: &str,
) -> Result<AuthInstancesListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT i.instance_id \
        FROM auth_instances i \
        JOIN auth_deployments d USING(deployment_id) \
        WHERE i.instance_id>?1 \
        AND (?2 IS NULL OR i.deployment_id=?2) \
        AND d.participant_kind=?3 \
        ORDER BY i.instance_id \
        LIMIT ?4",
    )?;
    let mut rows = statement
        .query_map(
            params![
                cursor,
                input.deployment_id.as_ref().map(ToString::to_string),
                kind,
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (rows.len() > limit).then(|| rows[limit - 1].clone());
    rows.truncate(limit);
    let items = rows
        .into_iter()
        .map(|instance| read_instance(connection, &instance))
        .collect::<Result<_, AuthError>>()?;
    Ok(CursorPage {
        items,
        page: CursorPageInfo { next_cursor },
    })
}

#[cfg(test)]
#[path = "rpc_deployments_tests.rs"]
mod tests;
