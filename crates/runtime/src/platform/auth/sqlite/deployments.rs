use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use super::super::application::repository::{
    DeploymentProfileCreation, DeploymentProfileMutation, DeploymentRepository, IdempotentOutcome,
};
use super::super::context::{
    revoke_sql_contexts, AuthorizationContextRevocationReason, AuthorizationContextSelector,
};
use super::super::{
    AuthorizationStateError, DeploymentProfileRecord, DeploymentProfileState, PrincipalKind,
    PrincipalState,
};
use super::common::{
    decode_enum, encode_enum, from_sql_version, map_write_error, sql_error, to_sql_version,
};
use super::evidence::load_deployment;
use super::outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay};
use super::provisioning::insert_sql_principal;
use super::validation::next_version;
use super::SqliteAuthorizationStore;

#[async_trait]
impl DeploymentRepository for SqliteAuthorizationStore {
    async fn create_deployment_profile(
        &self,
        command: DeploymentProfileCreation,
    ) -> Result<IdempotentOutcome<DeploymentProfileRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            if command.principal.principal_id != command.profile.deployment_id
                || command.principal.kind != command.profile.kind
                || command.principal.version != 1
                || command.profile.version != 1
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            insert_sql_principal(&transaction, &command.principal)?;
            insert_deployment_profile(&transaction, &command.profile)?;
            upsert_deployment_profile_evidence(&transaction, &command.profile)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.profile))
        })
        .await
    }

    async fn get_deployment_profile(
        &self,
        deployment_id: &str,
    ) -> Result<Option<DeploymentProfileRecord>, AuthorizationStateError> {
        let deployment_id = deployment_id.to_owned();
        self.run_read(move |connection| load_deployment_profile(connection, &deployment_id))
            .await
    }

    async fn list_deployment_profiles(
        &self,
    ) -> Result<Vec<DeploymentProfileRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT deployment_id, kind, display_name, participant_id, portal_id,
                            review_mode, requires_device_delegation, expires_at, state, created_at,
                            updated_at, version
                     FROM auth_deployment_profiles ORDER BY deployment_id",
                )
                .map_err(sql_error)?;
            let records = statement
                .query_map([], decode_deployment_profile)
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            Ok(records)
        })
        .await
    }

    async fn put_deployment_profile(
        &self,
        command: DeploymentProfileMutation,
    ) -> Result<IdempotentOutcome<DeploymentProfileRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_deployment_profile(&transaction, &command.profile.deployment_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.version != command.expected_version
                || command.profile.version != next_version(command.expected_version)?
                || current.created_at != command.profile.created_at
                || current.kind != command.profile.kind
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE auth_deployment_profiles
                     SET display_name = ?1, participant_id = ?2, portal_id = ?3,
                          review_mode = ?4, requires_device_delegation = ?5, expires_at = ?6,
                          state = ?7, updated_at = ?8, version = ?9
                      WHERE deployment_id = ?10 AND version = ?11",
                    params![
                        command.profile.display_name,
                        command.profile.participant_id,
                        command.profile.portal_id,
                        command.profile.review_mode.map(encode_enum).transpose()?,
                        command.profile.requires_device_delegation,
                        command.profile.expires_at,
                        encode_enum(command.profile.state)?,
                        command.profile.updated_at,
                        to_sql_version(command.profile.version)?,
                        command.profile.deployment_id,
                        to_sql_version(command.expected_version)?,
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let principal_state = match command.profile.state {
                DeploymentProfileState::Active => PrincipalState::Active,
                DeploymentProfileState::Disabled => PrincipalState::Disabled,
                DeploymentProfileState::Removed => PrincipalState::Revoked,
            };
            transaction
                .execute(
                    "UPDATE auth_principals SET state = ?1, updated_at = ?2,
                         disabled_at = ?3, revoked_at = ?4, version = ?5
                     WHERE principal_id = ?6 AND version = ?7",
                    params![
                        encode_enum(principal_state)?,
                        command.profile.updated_at,
                        (principal_state == PrincipalState::Disabled)
                            .then_some(command.profile.updated_at),
                        (principal_state == PrincipalState::Revoked)
                            .then_some(command.profile.updated_at),
                        to_sql_version(command.profile.version)?,
                        command.profile.deployment_id,
                        to_sql_version(command.expected_version)?,
                    ],
                )
                .map_err(map_write_error)?;
            transaction
                .execute(
                    "UPDATE auth_deployments SET state = ?1 WHERE deployment_id = ?2",
                    params![
                        match command.profile.state {
                            DeploymentProfileState::Active => "active",
                            DeploymentProfileState::Disabled => "disabled",
                            DeploymentProfileState::Removed => "revoked",
                        },
                        command.profile.deployment_id,
                    ],
                )
                .map_err(map_write_error)?;
            upsert_deployment_profile_evidence(&transaction, &command.profile)?;
            if current.participant_id != command.profile.participant_id
                || current.requires_device_delegation != command.profile.requires_device_delegation
                || current.expires_at != command.profile.expires_at
                || current.state != command.profile.state
            {
                revoke_sql_contexts(
                    &transaction,
                    &AuthorizationContextSelector::Deployment(
                        command.profile.deployment_id.clone(),
                    ),
                    AuthorizationContextRevocationReason::DeploymentChanged,
                    command.profile.updated_at.div_euclid(1_000),
                )?;
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.profile))
        })
        .await
    }
}

pub(in crate::platform::auth) fn insert_deployment_profile(
    connection: &Connection,
    profile: &DeploymentProfileRecord,
) -> Result<(), AuthorizationStateError> {
    connection
        .execute(
            "INSERT INTO auth_deployment_profiles
          (deployment_id, kind, display_name, participant_id, portal_id, review_mode,
           requires_device_delegation, expires_at, state, created_at, updated_at, version)
          VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                profile.deployment_id,
                encode_enum(profile.kind)?,
                profile.display_name,
                profile.participant_id,
                profile.portal_id,
                profile.review_mode.map(encode_enum).transpose()?,
                profile.requires_device_delegation,
                profile.expires_at,
                encode_enum(profile.state)?,
                profile.created_at,
                profile.updated_at,
                to_sql_version(profile.version)?,
            ],
        )
        .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn upsert_deployment_profile_evidence(
    connection: &Connection,
    profile: &DeploymentProfileRecord,
) -> Result<(), AuthorizationStateError> {
    let Some(participant_id) = &profile.participant_id else {
        return Ok(());
    };
    let participant_kind = match profile.kind {
        PrincipalKind::Service => trellis_protocol::ParticipantKind::Service,
        PrincipalKind::Device => trellis_protocol::ParticipantKind::Device,
        PrincipalKind::User => {
            return Err(AuthorizationStateError::InvalidRecord(
                "deployment profile cannot use user kind".to_owned(),
            ));
        }
    };
    if let Some(existing) = load_deployment(connection, &profile.deployment_id)? {
        if existing.participant_id != *participant_id
            || existing.participant_kind != participant_kind
        {
            return Err(AuthorizationStateError::StorageConflict);
        }
    }
    let state = match profile.state {
        DeploymentProfileState::Active => "active",
        DeploymentProfileState::Disabled => "disabled",
        DeploymentProfileState::Removed => "revoked",
    };
    connection
        .execute(
            "INSERT INTO auth_deployments
         (deployment_id, participant_id, participant_kind, state, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(deployment_id) DO UPDATE SET
             state = excluded.state, expires_at = excluded.expires_at",
            params![
                profile.deployment_id,
                participant_id,
                encode_enum(participant_kind)?,
                state,
                profile.expires_at,
            ],
        )
        .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn load_deployment_profile(
    connection: &Connection,
    deployment_id: &str,
) -> Result<Option<DeploymentProfileRecord>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT deployment_id, kind, display_name, participant_id, portal_id,
                review_mode, requires_device_delegation, expires_at, state, created_at, updated_at, version
         FROM auth_deployment_profiles WHERE deployment_id = ?1",
            [deployment_id],
            decode_deployment_profile,
        )
        .optional()
        .map_err(sql_error)
}

pub(in crate::platform::auth) fn decode_deployment_profile(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<DeploymentProfileRecord> {
    Ok(DeploymentProfileRecord {
        deployment_id: row.get(0)?,
        kind: decode_enum(row.get(1)?)?,
        display_name: row.get(2)?,
        participant_id: row.get(3)?,
        portal_id: row.get(4)?,
        review_mode: row
            .get::<_, Option<String>>(5)?
            .map(decode_enum)
            .transpose()?,
        requires_device_delegation: row.get(6)?,
        expires_at: row.get(7)?,
        state: decode_enum(row.get(8)?)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        version: from_sql_version(row.get(11)?)?,
    })
}
