use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::super::authority::ContextRepository;
use super::super::authority::{
    IssuanceConnection, IssuanceCredential, IssuanceCredentialRecord, IssuanceSnapshot,
};
use super::super::context::{AuthorizationContextRecord, AuthorizationContextSelector};
use super::super::{AuthorizationStateError, GrantOwnerKind, ResourceBindingEvidence};
use super::common::{decode_enum, decode_json, encode_enum, sql_error, to_sql_version};
use super::evidence::load_deployment;
use super::principals::load_principal;
use super::sessions::load_session;
use super::SqliteAuthorizationStore;

impl SqliteAuthorizationStore {
    pub(crate) async fn get_resource_bindings(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
        installed_revision: u64,
    ) -> Result<Vec<ResourceBindingEvidence>, AuthorizationStateError> {
        self.run_read(move |connection| {
            // Reading present evidence for one revision needs that revision's
            // declaration to interpret the current physical resource.
            let (_, participant) = super::grants::load_installed_participant(
                connection,
                &participant_id,
                Some(installed_revision),
            )?
            .ok_or(AuthorizationStateError::ParticipantMissing)?;
            load_resource_bindings(
                connection,
                owner_kind,
                &owner_id,
                &participant_id,
                installed_revision,
                installed_revision,
                &participant.projection,
            )
        })
        .await
    }

    /// Lists every durable context in a selector scope, revoked or not, so
    /// transport reevaluation sees attachments the same change just revoked.
    pub(crate) async fn list_contexts_by_selector(
        &self,
        selector: AuthorizationContextSelector,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            super::super::context::list_sql_contexts_by_selector(connection, &selector)
        })
        .await
    }
}

#[async_trait]
impl ContextRepository for SqliteAuthorizationStore {
    async fn load_issuance_snapshot(
        &self,
        request: &IssuanceConnection,
    ) -> Result<IssuanceSnapshot, AuthorizationStateError> {
        let request = request.clone();
        self.run_read(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let snapshot = sqlite_issuance_snapshot(&transaction, &request, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(snapshot)
        })
        .await
    }
}

pub(in crate::platform::auth) fn sqlite_issuance_snapshot(
    connection: &Connection,
    request: &IssuanceConnection,
    pinned_revision: Option<u64>,
) -> Result<IssuanceSnapshot, AuthorizationStateError> {
    let connection_id = request.connection_id.parse::<ulid::Ulid>().map_err(|_| {
        AuthorizationStateError::InvalidRecord("connectionId must be a ULID".to_owned())
    })?;
    if connection_id.to_string() != request.connection_id {
        return Err(AuthorizationStateError::InvalidRecord(
            "connectionId must be canonical".to_owned(),
        ));
    }
    super::super::domain::validate_ed25519_public_key("sessionKey", &request.session_public_key)?;
    let (credential, principal_id, owner_kind, owner_id, participant_id, participant_revision) =
        match &request.credential {
            IssuanceCredential::Login(id) => {
                let login =
                    load_session(connection, id)?.ok_or(AuthorizationStateError::SessionMissing)?;
                // A user login is immutable session identity: a caller-supplied pin
                // that disagrees with the login's own revision fails closed rather
                // than silently adopting another vocabulary.
                if pinned_revision.is_some_and(|revision| revision != login.installed_revision) {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "login participant revision does not match its session".to_owned(),
                    ));
                }
                // An immutable user login selects the participant revision whose
                // consent created it; a later binding change moves present
                // authority, not the vocabulary this login runs.
                (
                    IssuanceCredentialRecord::Login(login.clone()),
                    login.principal_id.clone(),
                    super::super::GrantOwnerKind::User,
                    login.principal_id,
                    login.participant_id,
                    Some(login.installed_revision),
                )
            }
            IssuanceCredential::Native(id) => {
                let identity = super::provisioning::load_provisioned_identity(connection, id)?
                    .ok_or(AuthorizationStateError::IdentityMissing)?;
                let instance =
                    super::evidence::load_runtime_instance(connection, &identity.instance_id)?
                        .ok_or(AuthorizationStateError::InstanceInactive)?;
                let deployment = load_deployment(connection, &identity.deployment_id)?
                    .ok_or(AuthorizationStateError::DeploymentInactive)?;
                let device = super::evidence::load_device(
                    connection,
                    &identity.principal_id,
                    &identity.deployment_id,
                )?;
                let delegation = super::evidence::load_device_delegation(
                    connection,
                    &identity.principal_id,
                    &identity.deployment_id,
                )?;
                let delegation_session = delegation
                    .as_ref()
                    .and_then(|delegation| delegation.user_login_session_id.as_deref())
                    .map(|session_id| load_session(connection, session_id))
                    .transpose()?
                    .flatten();
                let delegation_binding = delegation_session
                    .as_ref()
                    .map(|session| {
                        super::grants::load_grant_binding(
                            connection,
                            GrantOwnerKind::User,
                            &session.principal_id,
                            &session.participant_id,
                        )
                    })
                    .transpose()?
                    .flatten();
                let delegation_principal = delegation_session
                    .as_ref()
                    .map(|session| load_principal(connection, &session.principal_id))
                    .transpose()?
                    .flatten();
                let principal_id = identity.principal_id.clone();
                let owner_id = deployment.deployment_id.clone();
                let participant_id = deployment.participant_id.clone();
                // A native instance is evaluated against the participant revision it
                // is actually running, not the deployment's current desired revision.
                // An explicit pinned revision comes from the durable context being
                // reevaluated, whose scope must not drift with later adoptions.
                let participant_revision = pinned_revision.unwrap_or(instance.installed_revision);
                (
                    IssuanceCredentialRecord::Native(Box::new(
                        super::super::authority::NativeIssuanceCredentialRecord {
                            identity: Box::new(identity),
                            instance,
                            deployment,
                            device,
                            delegation,
                            delegation_session,
                            delegation_principal,
                            delegation_binding,
                        },
                    )),
                    principal_id,
                    super::super::GrantOwnerKind::Deployment,
                    owner_id,
                    participant_id,
                    Some(participant_revision),
                )
            }
        };
    let principal = load_principal(connection, &principal_id)?
        .ok_or(AuthorizationStateError::PrincipalMissing)?;
    let binding =
        super::grants::load_grant_binding(connection, owner_kind, &owner_id, &participant_id)?
            .ok_or(AuthorizationStateError::NotAuthorized)?;
    let participant_revision = participant_revision.unwrap_or(binding.installed_revision);
    let (_, participant) = super::grants::load_installed_participant(
        connection,
        &participant_id,
        Some(participant_revision),
    )?
    .ok_or(AuthorizationStateError::ParticipantMissing)?;
    // Resource evidence is keyed by the participant revision whose vocabulary
    // interprets the single current physical resource: it proves that the
    // physical resource that exists now is usable as that revision expects. The
    // pin therefore selects how present materialization is interpreted; it never
    // selects an old physical resource. The current binding remains the
    // authority ceiling that decides which of those declarations survive.
    let resources = load_resource_bindings(
        connection,
        owner_kind,
        &owner_id,
        &participant_id,
        participant_revision,
        binding.installed_revision,
        &participant.projection,
    )?;
    // Include the mutable selected API provider bindings in the snapshot so
    // the issuance concurrency token covers the exact policy inputs.
    let binding_scope = match owner_kind {
        GrantOwnerKind::User => participant_id.as_str(),
        GrantOwnerKind::Deployment => owner_id.as_str(),
    };
    let api_bindings = {
        let mut statement = connection
            .prepare(
                "SELECT api_id, provider_deployment_id FROM auth_api_bindings
                     WHERE participant_id = ?1 ORDER BY api_id",
            )
            .map_err(sql_error)?;
        let rows = statement
            .query_map([binding_scope], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        rows.into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let issuer = connection.query_row(
        "SELECT key_id, public_key FROM auth_authorization_issuers WHERE is_current = 1 AND revoked_at IS NULL",
        [], |row| Ok(trellis_protocol::AuthorizationIssuerKey {
            key_id: row.get(0)?, public_key: row.get(1)?, state: trellis_protocol::AuthorizationIssuerState::Active,
        }),
    ).optional().map_err(sql_error)?.ok_or(AuthorizationStateError::IssuerMissing)?;
    issuer
        .verifying_key()
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    Ok(IssuanceSnapshot {
        connection: request.clone(),
        credential,
        principal,
        binding,
        participant,
        participant_revision,
        resources,
        api_bindings,
        issuer,
    })
}

pub(in crate::platform::auth) fn load_eligible_authorization_issuer(
    connection: &Connection,
    key_id: &str,
    now_ms: i64,
) -> Result<trellis_protocol::AuthorizationIssuerKey, AuthorizationStateError> {
    let issuer = connection
        .query_row(
            "SELECT public_key, is_current, live_until_seconds, revoked_at FROM auth_authorization_issuers WHERE key_id = ?1",
            [key_id],
            |row| {
                let state = if row.get::<_, Option<i64>>(3)?.is_some() {
                    trellis_protocol::AuthorizationIssuerState::Revoked
                } else if row.get::<_, bool>(1)?
                    || row.get::<_, i64>(2)? >= now_ms.div_euclid(1_000)
                {
                    trellis_protocol::AuthorizationIssuerState::Active
                } else {
                    trellis_protocol::AuthorizationIssuerState::Retired
                };
                Ok(trellis_protocol::AuthorizationIssuerKey {
                    key_id: key_id.to_owned(),
                    public_key: row.get(0)?,
                    state,
                })
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or(AuthorizationStateError::IssuerMissing)?;
    issuer
        .verifying_key()
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    Ok(issuer)
}

pub(in crate::platform::auth) fn load_resource_bindings(
    connection: &Connection,
    owner_kind: super::super::GrantOwnerKind,
    owner_id: &str,
    participant_id: &str,
    installed_revision: u64,
    binding_installed_revision: u64,
    participant: &super::super::evidence::ParticipantRuntimeProjection,
) -> Result<Vec<ResourceBindingEvidence>, AuthorizationStateError> {
    let mut statement = connection
        .prepare(
            "SELECT resource_kind, local_name, binding_id, participant_id, provider_identity,
                actual_json, state, materialized_at, error FROM auth_resource_binding_evidence
         WHERE owner_kind = ?1 AND owner_id = ?2 AND participant_id = ?3 AND installed_revision = ?4
         ORDER BY resource_kind, local_name",
        )
        .map_err(sql_error)?;
    let mut resources = statement
        .query_map(
            params![
                encode_enum(owner_kind)?,
                owner_id,
                participant_id,
                to_sql_version(installed_revision)?
            ],
            decode_resource,
        )
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    // A credential pinned below the current binding interprets present
    // materialization through its own vocabulary. Its evidence must therefore be
    // re-derived from the current physical catalog rather than copied from
    // another revision's row, and it must never claim historical
    // materialization. The projection requires all of:
    //   * the pinned participant still declares the resource;
    //   * the one current physical resource for that identity is ready;
    //   * it was reconciled at the present binding revision, i.e. present
    //     authority still permits the pinned resource semantics.
    // Anything else falls back to the stored pinned row, which cannot loosen
    // authority because it was already scoped to this revision.
    if installed_revision != binding_installed_revision {
        for resource in &mut resources {
            let declared = participant.resources.contains_key(&resource.local_name);
            if !declared {
                continue;
            }
            let current = connection
                .query_row(
                    "SELECT state, binding_revision, actual_json FROM auth_resources
                     WHERE owner_kind = ?1 AND owner_id = ?2 AND participant_id = ?3
                       AND kind = ?4 AND local_name = ?5",
                    params![
                        encode_enum(owner_kind)?,
                        owner_id,
                        participant_id,
                        resource.resource_kind,
                        resource.local_name
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?;
            let Some((state, reconciled_revision, actual)) = current else {
                continue;
            };
            if state != "ready"
                || reconciled_revision != to_sql_version(binding_installed_revision)?
            {
                continue;
            }
            resource.state = super::super::ResourceBindingState::Available;
            resource.actual = actual.map(decode_json).transpose().map_err(sql_error)?;
            resource.error = None;
        }
    }
    Ok(resources)
}

pub(in crate::platform::auth) fn decode_resource(
    row: &Row<'_>,
) -> rusqlite::Result<ResourceBindingEvidence> {
    Ok(ResourceBindingEvidence {
        resource_kind: row.get(0)?,
        local_name: row.get(1)?,
        binding_id: row.get(2)?,
        owner_participant_id: row.get(3)?,
        provider_identity: decode_json(row.get(4)?)?,
        actual: row
            .get::<_, Option<String>>(5)?
            .map(decode_json)
            .transpose()?,
        state: decode_enum(row.get::<_, String>(6)?)?,
        materialized_at: row.get(7)?,
        error: row.get(8)?,
    })
}
