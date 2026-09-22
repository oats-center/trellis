use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use trellis_protocol::ParticipantResourceKind;

use super::common::{
    decode_enum, decode_json, encode_enum, encode_json, map_write_error, sql_error,
};
use super::grants::{load_grant_binding, load_installed_participant};
use super::SqliteAuthorizationStore;
use crate::platform::auth::domain::{
    ApprovedResource, AuthorizationResourceKind, GrantBindingState, GrantOwnerKind,
    ResourceBindingEvidence, ResourceBindingState, ResourceCommitment, ResourceProviderIdentity,
};
use crate::platform::auth::resources::{
    physical_id, reconcile_action_payload, resource_id, DestroyResourcePayload,
    ReconcileResourcePayload, ResourceActual, ResourceCatalogRecord, ResourceCatalogState,
    ResourceReconcileRequest,
};
use crate::platform::auth::{
    AuthorizationStateError, PostCommitActionKind, PostCommitActionRecord,
};

const DETACHED: &str = "detached";

pub(in crate::platform::auth) fn reconcile_sql_resource_catalog(
    connection: &Connection,
    binding: &crate::platform::auth::GrantBinding,
    now: i64,
) -> Result<Vec<PostCommitActionRecord>, AuthorizationStateError> {
    let owner_kind = encode_enum(binding.owner_kind)?;
    let mut statement = connection
        .prepare(
            "SELECT resource_id FROM auth_resources
             WHERE owner_kind = ?1 AND owner_id = ?2 AND participant_id = ?3",
        )
        .map_err(sql_error)?;
    let existing = statement
        .query_map(
            params![owner_kind, binding.owner_id, binding.participant_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    drop(statement);

    let mut retained = std::collections::BTreeSet::new();
    let mut actions = Vec::new();
    let approved_resources = if binding.state == GrantBindingState::Active {
        binding.approved_resources.as_slice()
    } else {
        &[]
    };
    let participant = if approved_resources.is_empty() {
        None
    } else {
        let (_, participant) = load_installed_participant(
            connection,
            &binding.participant_id,
            Some(binding.installed_revision),
        )?
        .ok_or(AuthorizationStateError::ParticipantMissing)?;
        Some(participant.resolve()?.clone())
    };
    for approved in approved_resources {
        let declaration = participant
            .as_ref()
            .ok_or(AuthorizationStateError::ParticipantMissing)?
            .resources
            .get(&approved.name)
            .filter(|resource| AuthorizationResourceKind::from(resource.kind) == approved.kind)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "approved resource is absent from the installed participant".to_owned(),
                )
            })?;
        approved.commitment.validate()?;
        validate_approved_declaration(approved, declaration)?;
        let mut commitment = approved.commitment.clone();
        commitment.desired_max_object_bytes = declaration.desired_max_object;
        commitment.desired_max_total_bytes = declaration.desired_max_total;
        commitment.desired_max_value_bytes = declaration.desired_max_value;
        let kind = participant_kind(approved.kind);
        let id = resource_id(
            binding.owner_kind,
            &binding.owner_id,
            &binding.participant_id,
            kind,
            &approved.name,
        );
        retained.insert(id.clone());
        let current = load_resource_by_identity(
            connection,
            binding.owner_kind,
            &binding.owner_id,
            &binding.participant_id,
            kind,
            &approved.name,
        )?;
        if current
            .as_ref()
            .is_some_and(|resource| resource.state == ResourceCatalogState::Destroying)
        {
            return Err(AuthorizationStateError::StorageConflict);
        }
        if let Some(resource) = current.as_ref().filter(|resource| {
            resource.state == ResourceCatalogState::Ready
                && same_hard_commitment(&resource.commitment, &commitment)
                && resource.actual.is_some()
        }) {
            connection
                .execute(
                    "UPDATE auth_resources SET commitment_json = ?1, binding_revision = ?2, updated_at = ?3
                     WHERE resource_id = ?4",
                    params![encode_json(&commitment)?, binding.revision, now, id],
                )
                .map_err(map_write_error)?;
            let evidence = resource_evidence(
                resource,
                participant
                    .as_ref()
                    .ok_or(AuthorizationStateError::ParticipantMissing)?,
                declaration,
                resource.actual.as_ref(),
                None,
                now,
            )?;
            connection
                .execute(
                    "INSERT INTO auth_resource_binding_evidence (
                         owner_kind, owner_id, participant_id, installed_revision, resource_kind,
                         local_name, binding_id, provider_identity, actual_json, state,
                         materialized_at, error
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)
                     ON CONFLICT(owner_kind, owner_id, participant_id, installed_revision,
                         resource_kind, local_name)
                     DO UPDATE SET binding_id = excluded.binding_id,
                         provider_identity = excluded.provider_identity,
                         actual_json = excluded.actual_json, state = excluded.state,
                         materialized_at = excluded.materialized_at, error = excluded.error",
                    params![
                        owner_kind,
                        binding.owner_id,
                        binding.participant_id,
                        binding.installed_revision,
                        evidence.resource_kind,
                        evidence.local_name,
                        evidence.binding_id,
                        encode_json(&evidence.provider_identity)?,
                        evidence.actual.as_ref().map(encode_json).transpose()?,
                        encode_enum(evidence.state)?,
                        now,
                    ],
                )
                .map_err(map_write_error)?;
            continue;
        }
        let revision = current
            .as_ref()
            .map(|resource| super::validation::next_version(resource.revision))
            .transpose()?
            .unwrap_or(1);
        let created_at = current.as_ref().map_or(now, |resource| resource.created_at);
        let physical = current.as_ref().map_or_else(
            || physical_id(kind, &id),
            |resource| resource.physical_id.clone(),
        );
        let actual = current
            .as_ref()
            .and_then(|resource| resource.actual.clone());
        connection.execute(
            "INSERT INTO auth_resources (
                 resource_id, owner_kind, owner_id, participant_id, kind, local_name,
                 commitment_json, physical_id, actual_json, state, readiness_reason,
                 binding_revision, revision, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', 'reconciling', ?10, ?11, ?12, ?13)
             ON CONFLICT(resource_id) DO UPDATE SET
                 commitment_json = excluded.commitment_json,
                 state = 'pending', readiness_reason = 'reconciling',
                 binding_revision = excluded.binding_revision,
                 revision = excluded.revision, updated_at = excluded.updated_at",
            params![
                id,
                owner_kind,
                binding.owner_id,
                binding.participant_id,
                resource_kind_sql(kind),
                approved.name,
                encode_json(&commitment)?,
                physical,
                actual.as_ref().map(encode_json).transpose()?,
                binding.revision,
                revision,
                created_at,
                now,
            ],
        ).map_err(map_write_error)?;
        connection
            .execute(
                "UPDATE auth_resource_binding_evidence SET state = 'stale',
                    materialized_at = ?1, error = 'resource reconciliation pending'
                 WHERE owner_kind = ?2 AND owner_id = ?3 AND participant_id = ?4
                   AND resource_kind = ?5 AND local_name = ?6",
                params![
                    now,
                    owner_kind,
                    binding.owner_id,
                    binding.participant_id,
                    provider_resource_kind(kind),
                    approved.name,
                ],
            )
            .map_err(map_write_error)?;
        insert_history(
            connection,
            &id,
            revision,
            &commitment,
            actual.as_ref(),
            ResourceCatalogState::Pending,
            now,
        )?;
        let payload = reconcile_action_payload(&id, binding.revision, revision);
        actions.push(PostCommitActionRecord {
            predecessor_action_id: None,
            action_id: trellis_protocol::digest_json(&json!({"resourceReconcile": payload}))
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
            kind: PostCommitActionKind::ResourceReconcile,
            payload,
            created_at: now,
            attempts: 0,
            next_attempt_at: now,
            claimed_until: None,
            last_error: None,
        });
    }

    for id in existing.into_iter().filter(|id| !retained.contains(id)) {
        let Some(current) = load_resource(connection, &id)? else {
            continue;
        };
        if matches!(
            current.state,
            ResourceCatalogState::Destroying | ResourceCatalogState::Detached
        ) {
            continue;
        }
        let revision = super::validation::next_version(current.revision)?;
        connection
            .execute(
                "UPDATE auth_resources SET state = 'detached', readiness_reason = ?1,
                 binding_revision = ?2, revision = ?3, updated_at = ?4
             WHERE resource_id = ?5 AND revision = ?6",
                params![
                    DETACHED,
                    binding.revision,
                    revision,
                    now,
                    id,
                    current.revision
                ],
            )
            .map_err(map_write_error)?;
        connection
            .execute(
                "UPDATE auth_resource_binding_evidence SET state = 'stale',
                    materialized_at = ?1, error = 'resource detached'
                 WHERE owner_kind = ?2 AND owner_id = ?3 AND participant_id = ?4
                   AND resource_kind = ?5 AND local_name = ?6",
                params![
                    now,
                    owner_kind,
                    binding.owner_id,
                    binding.participant_id,
                    provider_resource_kind(current.resource_kind),
                    current.local_name,
                ],
            )
            .map_err(map_write_error)?;
        insert_history(
            connection,
            &id,
            revision,
            &current.commitment,
            current.actual.as_ref(),
            ResourceCatalogState::Detached,
            now,
        )?;
    }
    Ok(actions)
}

fn same_hard_commitment(left: &ResourceCommitment, right: &ResourceCommitment) -> bool {
    left.history == right.history && left.ttl_ms == right.ttl_ms
}

impl SqliteAuthorizationStore {
    pub(crate) async fn job_namespace_has_other_resources(
        &self,
        resource_id: String,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    ) -> Result<bool, AuthorizationStateError> {
        self.run_read(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM auth_resources
                         WHERE resource_id != ?1 AND owner_kind = ?2 AND owner_id = ?3
                           AND participant_id = ?4 AND kind = 'job'
                     )",
                    params![
                        resource_id,
                        encode_enum(owner_kind)?,
                        owner_id,
                        participant_id
                    ],
                    |row| row.get(0),
                )
                .map_err(sql_error)
        })
        .await
    }

    pub(crate) async fn resource_bindings(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
        installed_revision: u64,
    ) -> Result<Vec<ResourceBindingEvidence>, AuthorizationStateError> {
        self.run_read(move |connection| {
            super::contexts::load_resource_bindings(
                connection,
                owner_kind,
                &owner_id,
                &participant_id,
                installed_revision,
            )
        })
        .await
    }

    pub(crate) async fn consent_resource_actuals(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    ) -> Result<Vec<super::super::ephemeral::ConsentResourceActualEntry>, AuthorizationStateError>
    {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT kind, local_name, actual_json FROM auth_resources
                 WHERE owner_kind = ?1 AND owner_id = ?2 AND participant_id = ?3
                   AND actual_json IS NOT NULL",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![encode_enum(owner_kind)?, owner_id, participant_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            rows.into_iter()
                .map(|(kind, name, actual)| {
                    let kind = decode_resource_kind(&kind)?;
                    let actual: ResourceActual = decode_json(actual).map_err(sql_error)?;
                    Ok(super::super::ephemeral::ConsentResourceActualEntry {
                        kind: AuthorizationResourceKind::from(kind),
                        name,
                        actual: match actual {
                            ResourceActual::State => {
                                super::super::ephemeral::ConsentResourceActual {
                                    max_object_bytes: None,
                                    max_total_bytes: None,
                                    max_value_bytes: None,
                                    history: None,
                                    representation_version: None,
                                    ttl_ms: None,
                                }
                            }
                            ResourceActual::Kv {
                                history,
                                ttl_ms,
                                max_value_bytes,
                            } => super::super::ephemeral::ConsentResourceActual {
                                max_object_bytes: None,
                                max_total_bytes: None,
                                max_value_bytes,
                                history: Some(history),
                                representation_version: None,
                                ttl_ms: Some(ttl_ms),
                            },
                            ResourceActual::Store {
                                ttl_ms,
                                max_object_bytes,
                                max_total_bytes,
                            } => super::super::ephemeral::ConsentResourceActual {
                                max_object_bytes,
                                max_total_bytes,
                                max_value_bytes: None,
                                history: None,
                                representation_version: None,
                                ttl_ms: Some(ttl_ms),
                            },
                            ResourceActual::Job | ResourceActual::Consumer => {
                                super::super::ephemeral::ConsentResourceActual {
                                    max_object_bytes: None,
                                    max_total_bytes: None,
                                    max_value_bytes: None,
                                    history: None,
                                    representation_version: None,
                                    ttl_ms: None,
                                }
                            }
                        },
                    })
                })
                .collect()
        })
        .await
    }

    pub(crate) async fn load_resource_reconcile_request(
        &self,
        payload: ReconcileResourcePayload,
    ) -> Result<Option<ResourceReconcileRequest>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let Some(catalog) = load_resource(connection, &payload.resource_id)? else {
                return Ok(None);
            };
            if catalog.revision != payload.catalog_revision
                || catalog.binding_revision != payload.binding_revision
                || catalog.state != ResourceCatalogState::Pending
            {
                return Ok(None);
            }
            let Some(binding) = load_grant_binding(
                connection,
                catalog.owner_kind,
                &catalog.owner_id,
                &catalog.participant_id,
            )?
            else {
                return Ok(None);
            };
            if binding.state != GrantBindingState::Active
                || binding.revision != payload.binding_revision
            {
                return Ok(None);
            }
            let approved = binding
                .approved_resources
                .iter()
                .find(|approved| {
                    approved.name == catalog.local_name
                        && participant_kind(approved.kind) == catalog.resource_kind
                        && approved.commitment == catalog.commitment
                })
                .cloned();
            let Some(approved) = approved else {
                return Ok(None);
            };
            let (_, participant) = load_installed_participant(
                connection,
                &binding.participant_id,
                Some(binding.installed_revision),
            )?
            .ok_or(AuthorizationStateError::ParticipantMissing)?;
            Ok(Some(ResourceReconcileRequest {
                catalog,
                participant: participant.resolve()?.clone(),
                approved,
            }))
        })
        .await
    }

    pub(crate) async fn attach_resource(
        &self,
        payload: ReconcileResourcePayload,
        actual: ResourceActual,
        now: i64,
    ) -> Result<(), AuthorizationStateError> {
        self.complete_reconcile(payload, Some(actual), None, now)
            .await
    }

    pub(crate) async fn mark_resource_unavailable(
        &self,
        payload: ReconcileResourcePayload,
        reason: String,
        now: i64,
    ) -> Result<(), AuthorizationStateError> {
        self.complete_reconcile(payload, None, Some(reason), now)
            .await
    }

    async fn complete_reconcile(
        &self,
        payload: ReconcileResourcePayload,
        actual: Option<ResourceActual>,
        reason: Option<String>,
        now: i64,
    ) -> Result<(), AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let Some(catalog) = load_resource(&transaction, &payload.resource_id)? else { return Ok(()) };
            let Some(binding) = load_grant_binding(&transaction, catalog.owner_kind, &catalog.owner_id, &catalog.participant_id)? else { return Ok(()) };
            if catalog.revision != payload.catalog_revision
                || catalog.binding_revision != payload.binding_revision
                || catalog.state != ResourceCatalogState::Pending
                || catalog.readiness_reason.as_deref() != Some("reconciling")
                || binding.revision != payload.binding_revision
                || binding.state != GrantBindingState::Active
            {
                return Ok(());
            }
            let revision = super::validation::next_version(catalog.revision)?;
            let state = if actual.is_some() { ResourceCatalogState::Ready } else { ResourceCatalogState::Failed };
            let (_, participant) = load_installed_participant(
                &transaction,
                &binding.participant_id,
                Some(binding.installed_revision),
            )?.ok_or(AuthorizationStateError::ParticipantMissing)?;
            let projection = participant.resolve()?;
            let declaration = projection.resources.get(&catalog.local_name)
                .ok_or_else(|| AuthorizationStateError::InvalidRecord("resource declaration is missing".to_owned()))?;
            transaction.execute(
                "UPDATE auth_resources SET actual_json = COALESCE(?1, actual_json), state = ?2,
                     readiness_reason = ?3, revision = ?4, updated_at = ?5
                 WHERE resource_id = ?6 AND revision = ?7 AND binding_revision = ?8",
                params![actual.as_ref().map(encode_json).transpose()?, state_sql(state), reason, revision, now, catalog.resource_id, catalog.revision, payload.binding_revision],
            ).map_err(map_write_error)?;
            insert_history(&transaction, &catalog.resource_id, revision, &catalog.commitment, actual.as_ref().or(catalog.actual.as_ref()), state, now)?;
            let mut evidence = resource_evidence(&catalog, projection, declaration, actual.as_ref(), reason.as_deref(), now)?;
            evidence.actual = actual.clone().or(catalog.actual.clone());
            transaction.execute(
                "INSERT INTO auth_resource_binding_evidence (
                    owner_kind, owner_id, participant_id, installed_revision, resource_kind,
                     local_name, binding_id, provider_identity, actual_json, state, materialized_at, error
                  ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(owner_kind, owner_id, participant_id, installed_revision, resource_kind, local_name)
                 DO UPDATE SET binding_id = excluded.binding_id,
                    provider_identity = excluded.provider_identity, actual_json = excluded.actual_json,
                    state = excluded.state,
                    materialized_at = excluded.materialized_at, error = excluded.error",
                params![encode_enum(catalog.owner_kind)?, catalog.owner_id, catalog.participant_id,
                    binding.installed_revision, evidence.resource_kind, evidence.local_name,
                    evidence.binding_id, encode_json(&evidence.provider_identity)?,
                    evidence.actual.as_ref().map(encode_json).transpose()?,
                    encode_enum(evidence.state)?, evidence.materialized_at, evidence.error],
            ).map_err(map_write_error)?;
            if actual.is_some() {
                let resources = super::contexts::load_resource_bindings(
                    &transaction,
                    binding.owner_kind,
                    &binding.owner_id,
                    &binding.participant_id,
                    binding.installed_revision,
                )?;
                let authority = super::super::policy::resolve_authority(&participant, binding.approval_mode, &binding.approved_capabilities, &binding.approved_resources, &binding.platform_privileges, &binding.delegation_ceiling, (&resources, binding.companion_approved))?;
                if authority.exact_grants != binding.grants {
                    let (_, actions) = super::grants::replace_grant_binding(
                        &transaction,
                        super::super::GrantBindingReplacement {
                            owner_kind: binding.owner_kind,
                            owner_id: binding.owner_id,
                            participant_id: binding.participant_id,
                            installed_revision: binding.installed_revision,
                            grants: authority.exact_grants,
                            approval_mode: binding.approval_mode,
                            approved_capabilities: binding.approved_capabilities,
                            approved_resources: binding.approved_resources,
                            delegation_ceiling: binding.delegation_ceiling,
                            approval_decision_digest: binding.approval_decision_digest,
                            companion_approved: binding.companion_approved,
                            platform_privileges: binding.platform_privileges,
                            expected_revision: binding.revision,
                            expected_current_installed_revision: Some(binding.installed_revision),
                            state: binding.state,
                            expires_at: binding.expires_at,
                            provenance: binding.provenance,
                        },
                        now,
                    )?;
                    super::outbox::insert_sql_post_commit_actions(&transaction, &actions)?;
                }
            }
            transaction.commit().map_err(sql_error)
        }).await
    }

    pub(crate) async fn load_detached_resource(
        &self,
        payload: DestroyResourcePayload,
    ) -> Result<Option<ResourceCatalogRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let resource = load_resource(connection, &payload.resource_id)?;
            Ok(resource.filter(|resource| {
                resource.revision == payload.catalog_revision
                    && resource.state == ResourceCatalogState::Destroying
            }))
        })
        .await
    }

    pub(crate) async fn finish_resource_destroy(
        &self,
        payload: DestroyResourcePayload,
    ) -> Result<(), AuthorizationStateError> {
        self.run(move |connection| {
            connection.execute(
                "DELETE FROM auth_resources WHERE resource_id = ?1 AND revision = ?2 AND state = 'destroying'",
                params![payload.resource_id, payload.catalog_revision],
            ).map_err(map_write_error)?;
            Ok(())
        }).await
    }

    pub(crate) async fn inspect_resource(
        &self,
        resource_id: String,
    ) -> Result<Value, AuthorizationStateError> {
        self.run_read(move |connection| {
            let resource = load_resource(connection, &resource_id)?
                .ok_or(AuthorizationStateError::NotFound)?;
            resource_value(connection, &resource)
        })
        .await
    }

    pub(crate) async fn query_resources(
        &self,
        endpoint: &'static str,
        filters: Value,
    ) -> Result<Value, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut cursor_filters = filters.clone();
            cursor_filters
                .as_object_mut()
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord("invalid filters".to_owned())
                })?
                .remove("page");
            let filter_digest =
                trellis_protocol::pagination_query_digest(endpoint, &cursor_filters).map_err(
                    |_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()),
                )?;
            let mut entries = Vec::new();
            let after = filters
                .pointer("/page/cursor")
                .map(|cursor| {
                    let cursor = cursor.as_str().ok_or_else(|| {
                        AuthorizationStateError::InvalidRecord("invalid cursor".to_owned())
                    })?;
                    trellis_protocol::decode_pagination_cursor::<String>(cursor, &filter_digest)
                        .map_err(|_| {
                            AuthorizationStateError::InvalidRecord("invalid pagination".to_owned())
                        })
                })
                .transpose()?;
            let mut statement = connection
                .prepare(
                    "SELECT resource_id FROM auth_resources
                     WHERE (?1 IS NULL OR resource_id > ?1) ORDER BY resource_id",
                )
                .map_err(sql_error)?;
            let ids = statement
                .query_map([after.as_deref()], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            let requested_limit = filters
                .pointer("/page/limit")
                .map(|limit| {
                    limit
                        .as_u64()
                        .or_else(|| limit.as_str()?.parse().ok())
                        .filter(|limit| *limit > 0)
                        .ok_or_else(|| {
                            AuthorizationStateError::InvalidRecord("invalid page limit".to_owned())
                        })
                })
                .transpose()?
                .unwrap_or(50);
            if requested_limit > 200 {
                return Err(AuthorizationStateError::InvalidRecord(
                    "invalid page limit".to_owned(),
                ));
            }
            let limit = requested_limit as usize;
            for id in ids {
                let resource = load_resource(connection, &id)?
                    .ok_or(AuthorizationStateError::StorageConflict)?;
                let projected_owner_kind = resource_owner_kind(connection, &resource)?;
                if filters
                    .get("ownerId")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value != resource.owner_id)
                    || filters
                        .get("ownerKind")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value != projected_owner_kind)
                    || filters
                        .get("participantId")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value != resource.participant_id)
                    || filters
                        .get("kind")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value != resource_kind_sql(resource.resource_kind))
                    || filters
                        .get("state")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value != state_sql(resource.state))
                {
                    continue;
                }
                entries.push(resource_value(connection, &resource)?);
                if entries.len() > limit {
                    break;
                }
            }
            let next_cursor = if entries.len() > limit {
                let resource_id = entries[limit - 1]["resourceId"]
                    .as_str()
                    .ok_or(AuthorizationStateError::StorageConflict)?;
                Some(
                    trellis_protocol::encode_pagination_cursor(&filter_digest, &resource_id)
                        .map_err(|_| {
                            AuthorizationStateError::InvalidRecord("invalid pagination".to_owned())
                        })?,
                )
            } else {
                None
            };
            entries.truncate(limit);
            let page =
                next_cursor.map_or_else(|| json!({}), |cursor| json!({"nextCursor": cursor}));
            Ok(json!({"items": entries, "page": page}))
        })
        .await
    }

    pub(crate) async fn begin_resource_destroy(
        &self,
        resource_id: String,
        expected_revision: u64,
        confirm_physical_id: String,
        now: i64,
    ) -> Result<Value, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let resource = load_resource(&transaction, &resource_id)?
                .ok_or(AuthorizationStateError::NotFound)?;
            if resource.revision != expected_revision {
                return Err(AuthorizationStateError::RevisionConflict {
                    expected: expected_revision,
                    current: resource.revision,
                });
            }
            if resource.physical_id != confirm_physical_id
                || resource.state != ResourceCatalogState::Detached
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let revision = super::validation::next_version(resource.revision)?;
            transaction
                .execute(
                    "UPDATE auth_resources SET state = 'destroying', readiness_reason = NULL,
                    revision = ?1, updated_at = ?2 WHERE resource_id = ?3 AND revision = ?4",
                    params![revision, now, resource_id, resource.revision],
                )
                .map_err(map_write_error)?;
            insert_history(
                &transaction,
                &resource_id,
                revision,
                &resource.commitment,
                resource.actual.as_ref(),
                ResourceCatalogState::Destroying,
                now,
            )?;
            let payload = json!({"resourceId": resource_id, "catalogRevision": revision});
            let action = PostCommitActionRecord {
                predecessor_action_id: None,
                action_id: trellis_protocol::digest_json(&json!({"resourceDestroy": payload}))
                    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
                kind: PostCommitActionKind::ResourceReconcile,
                payload,
                created_at: now,
                attempts: 0,
                next_attempt_at: now,
                claimed_until: None,
                last_error: None,
            };
            transaction
                .execute(
                    "INSERT INTO auth_post_commit_actions (
                    action_id, kind, payload_json, created_at, attempts, next_attempt_at,
                    claimed_until, last_error, predecessor_action_id
                 ) VALUES (?1, ?2, ?3, ?4, 0, ?4, NULL, NULL, NULL)",
                    params![
                        action.action_id,
                        encode_enum(action.kind)?,
                        encode_json(&action.payload)?,
                        now
                    ],
                )
                .map_err(map_write_error)?;
            transaction.commit().map_err(sql_error)?;
            Ok(json!({"destroyed": true}))
        })
        .await
    }
}

fn load_resource(
    connection: &Connection,
    id: &str,
) -> Result<Option<ResourceCatalogRecord>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT resource_id, owner_kind, owner_id, participant_id, kind, local_name,
            commitment_json, physical_id, actual_json, state, readiness_reason,
            binding_revision, revision, created_at, updated_at
         FROM auth_resources WHERE resource_id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, u64>(11)?,
                    row.get::<_, u64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .map(|row| {
            Ok(ResourceCatalogRecord {
                resource_id: row.0,
                owner_kind: decode_enum(row.1).map_err(sql_error)?,
                owner_id: row.2,
                participant_id: row.3,
                resource_kind: decode_resource_kind(&row.4)?,
                local_name: row.5,
                commitment: decode_json(row.6).map_err(sql_error)?,
                physical_id: row.7.ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(
                        "catalog physical ID is missing".to_owned(),
                    )
                })?,
                actual: row.8.map(decode_json).transpose().map_err(sql_error)?,
                state: decode_state(&row.9)?,
                readiness_reason: row.10,
                binding_revision: row.11,
                revision: row.12,
                created_at: row.13,
                updated_at: row.14,
            })
        })
        .transpose()
}

fn load_resource_by_identity(
    connection: &Connection,
    owner_kind: GrantOwnerKind,
    owner_id: &str,
    participant_id: &str,
    kind: ParticipantResourceKind,
    local_name: &str,
) -> Result<Option<ResourceCatalogRecord>, AuthorizationStateError> {
    let id = resource_id(owner_kind, owner_id, participant_id, kind, local_name);
    let resource = load_resource(connection, &id)?;
    if resource.as_ref().is_some_and(|resource| {
        resource.owner_kind != owner_kind
            || resource.owner_id != owner_id
            || resource.participant_id != participant_id
            || resource.resource_kind != kind
            || resource.local_name != local_name
    }) {
        return Err(AuthorizationStateError::StorageConflict);
    }
    Ok(resource)
}

fn insert_history(
    connection: &Connection,
    resource_id: &str,
    revision: u64,
    commitment: &crate::platform::auth::domain::ResourceCommitment,
    actual: Option<&ResourceActual>,
    state: ResourceCatalogState,
    now: i64,
) -> Result<(), AuthorizationStateError> {
    connection.execute(
        "INSERT INTO auth_resource_history (resource_id, revision, commitment_json, actual_json, state, changed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![resource_id, revision, encode_json(commitment)?, actual.map(encode_json).transpose()?, state_sql(state), now],
    ).map_err(map_write_error)?;
    Ok(())
}

fn resource_evidence(
    resource: &ResourceCatalogRecord,
    participant: &crate::platform::auth::evidence::ParticipantRuntimeProjection,
    declaration: &crate::platform::auth::evidence::ResourceRuntimeProjection,
    actual: Option<&ResourceActual>,
    error: Option<&str>,
    now: i64,
) -> Result<ResourceBindingEvidence, AuthorizationStateError> {
    let provider_identity = match resource.resource_kind {
        ParticipantResourceKind::State => ResourceProviderIdentity::State {
            bucket: "trellis_state".to_owned(),
        },
        ParticipantResourceKind::Kv => ResourceProviderIdentity::Kv {
            bucket: resource.physical_id.clone(),
        },
        ParticipantResourceKind::Store => ResourceProviderIdentity::Store {
            bucket: resource.physical_id.clone(),
        },
        ParticipantResourceKind::JobQueue => ResourceProviderIdentity::JobQueue {
            namespace: crate::platform::auth::resources::job_namespace(resource),
            work_stream: "JOBS_WORK".to_owned(),
            publish_prefix: format!(
                "trellis.jobs.{}.{}",
                crate::platform::auth::resources::job_namespace(resource),
                resource.local_name
            ),
            updates_prefix: declaration.update_schema.as_ref().map(|_| {
                format!(
                    "trellis.job_updates.{}.{}",
                    crate::platform::auth::resources::job_namespace(resource),
                    resource.local_name
                )
            }),
            work_subject: format!(
                "trellis.work.{}.{}",
                crate::platform::auth::resources::job_namespace(resource),
                resource.local_name
            ),
            consumer: resource.physical_id.clone(),
        },
        ParticipantResourceKind::EventConsumer => {
            let mut filter_subjects = Vec::new();
            for (api_id, events) in &declaration.consumer_events {
                let api = participant.referenced_apis.get(api_id).ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(format!(
                        "consumer API {api_id} is missing"
                    ))
                })?;
                for event in events {
                    let action = api.actions.get(&format!("event:{event}")).ok_or_else(|| {
                        AuthorizationStateError::InvalidRecord(format!(
                            "consumer event {api_id}.{event} is missing"
                        ))
                    })?;
                    filter_subjects.push(
                        trellis_protocol::derive_event_wildcard_subject(
                            api_id,
                            event,
                            action.event_parameter_count,
                        )
                        .map_err(|error| {
                            AuthorizationStateError::InvalidRecord(error.to_string())
                        })?,
                    );
                }
            }
            filter_subjects.sort();
            filter_subjects.dedup();
            ResourceProviderIdentity::EventConsumer {
                stream: "trellis".to_owned(),
                consumer: resource.physical_id.clone(),
                replay_consumer: format!("{}_replay", resource.physical_id),
                filter_subjects,
            }
        }
    };
    Ok(ResourceBindingEvidence {
        resource_kind: provider_resource_kind(resource.resource_kind).to_owned(),
        local_name: resource.local_name.clone(),
        binding_id: resource.resource_id.clone(),
        owner_participant_id: resource.participant_id.clone(),
        provider_identity,
        actual: actual.cloned(),
        state: if actual.is_some() {
            ResourceBindingState::Available
        } else {
            ResourceBindingState::Unavailable
        },
        materialized_at: now,
        error: error.map(str::to_owned),
    })
}

fn resource_value(
    connection: &Connection,
    resource: &ResourceCatalogRecord,
) -> Result<Value, AuthorizationStateError> {
    let mut statement = connection
        .prepare(
            "SELECT revision, commitment_json, actual_json, state, changed_at
         FROM auth_resource_history WHERE resource_id = ?1 ORDER BY revision",
        )
        .map_err(sql_error)?;
    let history_rows = statement
        .query_map([&resource.resource_id], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    let history = history_rows
        .into_iter()
        .map(|(revision, commitment, actual, state, changed_at)| {
            let commitment: ResourceCommitment = decode_json(commitment).map_err(sql_error)?;
            let actual = actual
                .map(decode_json::<ResourceActual>)
                .transpose()
                .map_err(sql_error)?;
            Ok(json!({
                "version": revision.to_string(),
                "desired": {"commitment": commitment_value(&commitment)},
                "actual": actual.as_ref().map(actual_value),
                "state": state,
                "changedAt": timestamp_value(changed_at)?,
            }))
        })
        .collect::<Result<Vec<_>, AuthorizationStateError>>()?;
    Ok(json!({
        "resourceId": resource.resource_id, "ownerKind": resource_owner_kind(connection, resource)?,
        "ownerId": resource.owner_id, "participantId": resource.participant_id,
        "kind": resource_kind_sql(resource.resource_kind), "localName": resource.local_name,
        "commitment": commitment_value(&resource.commitment),
        "desired": {"commitment": commitment_value(&resource.commitment)},
        "physicalId": resource.physical_id, "actual": resource.actual.as_ref().map(actual_value),
        "state": state_sql(resource.state),
        "readiness": {"ready": resource.state == ResourceCatalogState::Ready, "reason": resource.readiness_reason},
        "bindingRevision": resource.binding_revision.to_string(), "version": resource.revision.to_string(),
        "createdAt": timestamp_value(resource.created_at)?, "updatedAt": timestamp_value(resource.updated_at)?,
        "history": history,
    }))
}

fn resource_owner_kind(
    connection: &Connection,
    resource: &ResourceCatalogRecord,
) -> Result<&'static str, AuthorizationStateError> {
    let kind = connection
        .query_row(
            "SELECT participant_kind FROM auth_installed_participants
             WHERE participant_id = ?1 ORDER BY revision DESC LIMIT 1",
            [&resource.participant_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or(AuthorizationStateError::ParticipantMissing)?;
    match (resource.owner_kind, kind.as_str()) {
        (GrantOwnerKind::Deployment, "service") => Ok("service"),
        (GrantOwnerKind::Deployment, "device") => Ok("device"),
        (GrantOwnerKind::User, "app") => Ok("app"),
        (GrantOwnerKind::User, "agent") => Ok("agent"),
        _ => Err(AuthorizationStateError::InvalidRecord(
            "resource authority owner does not match participant kind".to_owned(),
        )),
    }
}

fn actual_value(actual: &ResourceActual) -> Value {
    match actual {
        ResourceActual::State => json!({}),
        ResourceActual::Kv {
            history,
            ttl_ms,
            max_value_bytes,
        } => json!({
            "history": history.to_string(), "ttlMs": ttl_ms.to_string(),
            "maxValueBytes": max_value_bytes.map(|value| value.to_string()),
        }),
        ResourceActual::Store {
            ttl_ms,
            max_object_bytes,
            max_total_bytes,
        } => json!({
            "ttlMs": ttl_ms.to_string(),
            "maxObjectBytes": max_object_bytes.map(|value| value.to_string()),
            "maxTotalBytes": max_total_bytes.map(|value| value.to_string()),
        }),
        ResourceActual::Job | ResourceActual::Consumer => json!({}),
    }
}

fn commitment_value(commitment: &ResourceCommitment) -> Value {
    json!({
        "desiredMaxObjectBytes": commitment.desired_max_object_bytes.map(|value| value.to_string()),
        "desiredMaxTotalBytes": commitment.desired_max_total_bytes.map(|value| value.to_string()),
        "desiredMaxValueBytes": commitment.desired_max_value_bytes.map(|value| value.to_string()),
        "history": commitment.history.map(|value| value.to_string()),
        "ttlMs": commitment.ttl_ms.map(|value| value.to_string()),
    })
}

fn timestamp_value(milliseconds: i64) -> Result<String, AuthorizationStateError> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(milliseconds) * 1_000_000)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
}

fn validate_approved_declaration(
    approved: &ApprovedResource,
    declaration: &crate::platform::auth::evidence::ResourceRuntimeProjection,
) -> Result<(), AuthorizationStateError> {
    if approved.commitment.history != declaration.history
        || approved.commitment.ttl_ms != declaration.ttl_ms
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "approved resource commitment differs from declaration".to_owned(),
        ));
    }
    Ok(())
}

fn participant_kind(kind: AuthorizationResourceKind) -> ParticipantResourceKind {
    match kind {
        AuthorizationResourceKind::Consumer => ParticipantResourceKind::EventConsumer,
        AuthorizationResourceKind::Job => ParticipantResourceKind::JobQueue,
        AuthorizationResourceKind::Kv => ParticipantResourceKind::Kv,
        AuthorizationResourceKind::State => ParticipantResourceKind::State,
        AuthorizationResourceKind::Store => ParticipantResourceKind::Store,
    }
}
fn resource_kind_sql(kind: ParticipantResourceKind) -> &'static str {
    match kind {
        ParticipantResourceKind::EventConsumer => "consumer",
        ParticipantResourceKind::JobQueue => "job",
        ParticipantResourceKind::Kv => "kv",
        ParticipantResourceKind::State => "state",
        ParticipantResourceKind::Store => "store",
    }
}
fn provider_resource_kind(kind: ParticipantResourceKind) -> &'static str {
    match kind {
        ParticipantResourceKind::EventConsumer => "eventConsumer",
        ParticipantResourceKind::JobQueue => "jobQueue",
        ParticipantResourceKind::Kv => "kv",
        ParticipantResourceKind::State => "state",
        ParticipantResourceKind::Store => "store",
    }
}
fn decode_resource_kind(value: &str) -> Result<ParticipantResourceKind, AuthorizationStateError> {
    match value {
        "consumer" => Ok(ParticipantResourceKind::EventConsumer),
        "job" => Ok(ParticipantResourceKind::JobQueue),
        "kv" => Ok(ParticipantResourceKind::Kv),
        "state" => Ok(ParticipantResourceKind::State),
        "store" => Ok(ParticipantResourceKind::Store),
        _ => Err(AuthorizationStateError::InvalidRecord(
            "unknown catalog resource kind".to_owned(),
        )),
    }
}
fn state_sql(state: ResourceCatalogState) -> &'static str {
    match state {
        ResourceCatalogState::Detached => "detached",
        ResourceCatalogState::Pending => "pending",
        ResourceCatalogState::Ready => "ready",
        ResourceCatalogState::Failed => "failed",
        ResourceCatalogState::Destroying => "destroying",
    }
}
fn decode_state(value: &str) -> Result<ResourceCatalogState, AuthorizationStateError> {
    match value {
        "detached" => Ok(ResourceCatalogState::Detached),
        "pending" => Ok(ResourceCatalogState::Pending),
        "ready" => Ok(ResourceCatalogState::Ready),
        "failed" => Ok(ResourceCatalogState::Failed),
        "destroying" => Ok(ResourceCatalogState::Destroying),
        _ => Err(AuthorizationStateError::InvalidRecord(
            "unknown catalog resource state".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_capacity_does_not_change_hard_commitment() {
        let original = ResourceCommitment {
            desired_max_object_bytes: Some(1),
            desired_max_total_bytes: Some(2),
            desired_max_value_bytes: Some(3),
            history: Some(4),
            ttl_ms: Some(5),
        };
        let varied = ResourceCommitment {
            desired_max_object_bytes: None,
            desired_max_total_bytes: Some(20),
            desired_max_value_bytes: Some(30),
            ..original.clone()
        };
        assert!(same_hard_commitment(&original, &varied));
    }

    #[tokio::test]
    async fn fresh_catalog_detach_and_destroy_are_revision_fenced() {
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        assert_eq!(
            store
                .query_resources("core.Resources.Query", json!({}))
                .await
                .unwrap()["items"],
            json!([])
        );
        assert!(matches!(
            store
                .query_resources("core.Resources.Query", json!({"page": {"limit": "0"}}))
                .await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));

        let resource_id = "A".repeat(43);
        store
            .run({
                let resource_id = resource_id.clone();
                move |connection| {
                    connection
                        .execute(
                            "INSERT INTO auth_package_evidence (
                                package_digest, platform_trusted, accepted_at
                             ) VALUES (?1, 0, 1)",
                            ["B".repeat(43)],
                        )
                        .map_err(sql_error)?;
                    connection
                        .execute(
                            "INSERT INTO auth_package_evidence_documents (
                                evidence_digest, package_digest, evidence_json, created_at
                             ) VALUES (?1, ?2, '{}', 1)",
                            params!["E".repeat(43), "B".repeat(43)],
                        )
                        .map_err(sql_error)?;
                    connection
                        .execute(
                            "INSERT INTO auth_installed_participants (
                                participant_id, revision, participant_kind, participant_digest,
                                needs_digest, package_digest, evidence_digest, participant_path, companion_required,
                                projection_json, installed_at
                             ) VALUES ('participant-1', 1, 'service', ?1, ?2, ?3, ?4, 'Service', 0, '{}', 1)",
                            params!["C".repeat(43), "D".repeat(43), "B".repeat(43), "E".repeat(43)],
                        )
                        .map_err(sql_error)?;
                    connection
                        .execute(
                            "INSERT INTO auth_resources (
                            resource_id, owner_kind, owner_id, participant_id, kind, local_name,
                            commitment_json, physical_id, actual_json, state, readiness_reason,
                            binding_revision, revision, created_at, updated_at
                         ) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'kv', 'cache',
                            '{}', 'tr_kv_test', NULL, 'ready', NULL, 1, 1, 1, 1)",
                            [&resource_id],
                        )
                        .map_err(sql_error)?;
                    Ok(())
                }
            })
            .await
            .unwrap();

        let binding = crate::platform::auth::GrantBinding {
            owner_kind: GrantOwnerKind::Deployment,
            owner_id: "deployment-1".to_owned(),
            participant_id: "participant-1".to_owned(),
            installed_revision: 1,
            grants: trellis_protocol::GrantSet::new(Vec::new()),
            approval_mode: crate::platform::auth::ApprovalMode::Exact,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: crate::platform::auth::DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(trellis_protocol::GrantSet::new(Vec::new())),
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: "A".repeat(43),
            approval_expected_grant_revision: 1,
            companion_approved: false,
            platform_privileges: Vec::new(),
            revision: 2,
            state: GrantBindingState::Revoked,
            expires_at: None,
            provenance: None,
            created_at: 1,
            updated_at: 2,
        };
        store
            .run(move |connection| {
                assert!(reconcile_sql_resource_catalog(connection, &binding, 2)?.is_empty());
                Ok(())
            })
            .await
            .unwrap();
        let detached = store.inspect_resource(resource_id.clone()).await.unwrap();
        assert_eq!(detached["readiness"]["reason"], "detached");
        assert_eq!(detached["physicalId"], "tr_kv_test");
        let queried = store
            .query_resources(
                "core.Resources.Query",
                json!({"ownerKind": "service", "state": "detached"}),
            )
            .await
            .unwrap();
        serde_json::from_value::<trellis_runtime_apis::types::ResourcesQueryResponse>(
            queried.clone(),
        )
        .unwrap();
        assert_eq!(queried["items"].as_array().unwrap().len(), 1);

        store
            .begin_resource_destroy(resource_id.clone(), 2, "tr_kv_test".to_owned(), 3)
            .await
            .unwrap();
        let inspected = store.inspect_resource(resource_id.clone()).await.unwrap();
        serde_json::from_value::<trellis_runtime_apis::types::ResourceInspection>(
            inspected.clone(),
        )
        .unwrap();
        assert_eq!(inspected["ownerKind"], "service");
        assert_eq!(inspected["state"], "destroying");
        assert_eq!(inspected["version"], "3");
        assert_eq!(
            store
                .run_read(|connection| {
                    connection
                        .query_row("SELECT kind FROM auth_post_commit_actions", [], |row| {
                            row.get::<_, String>(0)
                        })
                        .map_err(sql_error)
                })
                .await
                .unwrap(),
            "resource_reconcile"
        );
        assert!(matches!(
            store
                .begin_resource_destroy(resource_id.clone(), 1, "tr_kv_test".to_owned(), 3)
                .await,
            Err(AuthorizationStateError::RevisionConflict { current: 3, .. })
        ));

        let payload = DestroyResourcePayload {
            resource_id,
            catalog_revision: 3,
        };
        store
            .finish_resource_destroy(payload.clone())
            .await
            .unwrap();
        store.finish_resource_destroy(payload).await.unwrap();
    }
}
