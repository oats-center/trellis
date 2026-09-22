use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::super::application::repository::{
    IdempotentOutcome, SessionCreation, SessionRepository, SessionRevocation,
};
use super::super::authority::validate_persisted_session;
use super::super::context::{
    revoke_sql_contexts, AuthorizationContextRevocationReason, AuthorizationContextSelector,
};
use super::super::{AuthorizationStateError, PrincipalKind, SessionRecord, SessionState};
use super::common::{
    decode_enum, encode_enum, from_sql_version, map_write_error, sql_error, to_sql_version,
};
use super::grants::load_installed_participant;
use super::outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay};
use super::principals::load_principal;
use super::validation::next_version;
use super::SqliteAuthorizationStore;

const SESSION_SELECT: &str = "SELECT
    session_id, principal_id, participant_id, participant_kind, session_public_key,
    session_key_id, state, created_at, last_authenticated_at, expires_at,
    revoked_at, version
    FROM auth_sessions";

#[async_trait]
impl SessionRepository for SqliteAuthorizationStore {
    async fn create_session(
        &self,
        mut command: SessionCreation,
    ) -> Result<IdempotentOutcome<SessionRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            super::super::authority::validate_session(&command.session)?;
            let principal = load_principal(&transaction, &command.session.principal_id)?
                .ok_or(AuthorizationStateError::PrincipalMissing)?;
            if principal.kind != PrincipalKind::User || principal.state != super::super::PrincipalState::Active {
                return Err(AuthorizationStateError::PrincipalInactive);
            }
            let previous_id = transaction.query_row(
                "SELECT session_id FROM auth_sessions WHERE participant_id = ?1 AND session_public_key = ?2",
                params![command.session.participant_id, command.session.session_public_key],
                |row| row.get::<_, String>(0),
            ).optional().map_err(sql_error)?;
            let session = if let Some(previous_id) = previous_id {
                let mut previous = load_session(&transaction, &previous_id)?
                    .ok_or(AuthorizationStateError::SessionMissing)?;
                if previous.principal_id != command.session.principal_id
                    || previous.participant_kind != command.session.participant_kind {
                    return Err(AuthorizationStateError::NotAuthorized);
                }
                if previous.state == SessionState::Revoked {
                    return Err(AuthorizationStateError::SessionRevoked);
                }
                if previous.state == SessionState::Expired
                    || previous.expires_at.is_some_and(|expires| expires <= command.session.last_authenticated_at) {
                    return Err(AuthorizationStateError::SessionExpired);
                }
                previous.last_authenticated_at = previous.last_authenticated_at.max(command.session.last_authenticated_at);
                previous.version = next_version(previous.version)?;
                transaction.execute(
                    "UPDATE auth_sessions SET last_authenticated_at = ?1, version = ?2 WHERE session_id = ?3",
                    params![previous.last_authenticated_at, to_sql_version(previous.version)?, previous.session_id],
                ).map_err(map_write_error)?;
                previous
            } else {
                insert_sql_session(&transaction, &command.session)?;
                command.session
            };
            command.idempotency.result = serde_json::json!({"sessionId": session.session_id});
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(session))
        })
        .await
    }

    async fn revoke_session(
        &self,
        command: SessionRevocation,
    ) -> Result<IdempotentOutcome<SessionRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            super::super::domain::require_protocol_timestamp("revokedAt", command.revoked_at)?;
            let current = load_session(&transaction, &command.session_id)?
                .ok_or(AuthorizationStateError::SessionMissing)?;
            if current.version != command.expected_version
                || current.state != SessionState::Active
                || command.revoked_at < current.created_at
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let next = next_version(command.expected_version)?;
            let changed = transaction
                .execute(
                    "UPDATE auth_sessions SET state = 'revoked', revoked_at = ?1, version = ?2
                      WHERE session_id = ?3 AND state = 'active' AND version = ?4",
                    params![
                        command.revoked_at,
                        to_sql_version(next)?,
                        command.session_id,
                        to_sql_version(command.expected_version)?
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let result = load_session(&transaction, &command.session_id)?
                .ok_or(AuthorizationStateError::SessionMissing)?;
            revoke_sql_contexts(
                &transaction,
                &AuthorizationContextSelector::Login(command.session_id.clone()),
                AuthorizationContextRevocationReason::SessionRevoked,
                command.revoked_at.div_euclid(1_000),
            )?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(result))
        })
        .await
    }

    async fn get_session(
        &self,
        id: &str,
    ) -> Result<Option<SessionRecord>, AuthorizationStateError> {
        let id = id.to_owned();
        self.run_read(move |connection| load_session(connection, &id))
            .await
    }

    async fn list_sessions(&self) -> Result<Vec<SessionRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare("SELECT session_id FROM auth_sessions ORDER BY session_id")
                .map_err(sql_error)?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            ids.into_iter()
                .map(|id| {
                    load_session(connection, &id)?.ok_or(AuthorizationStateError::SessionMissing)
                })
                .collect()
        })
        .await
    }
}

pub(in crate::platform::auth) fn insert_sql_session(
    connection: &Connection,
    session: &SessionRecord,
) -> Result<(), AuthorizationStateError> {
    super::super::authority::validate_session(session)?;
    let principal = load_principal(connection, &session.principal_id)?
        .ok_or(AuthorizationStateError::PrincipalMissing)?;
    if principal.kind != PrincipalKind::User
        || principal.state != super::super::PrincipalState::Active
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "session principal kind does not match principal".to_owned(),
        ));
    }
    let (_, participant) = load_installed_participant(connection, &session.participant_id, None)?
        .ok_or(AuthorizationStateError::ParticipantMissing)?;
    if participant.participant_kind != session.participant_kind {
        return Err(AuthorizationStateError::InvalidRecord(
            "session participant kind does not match participant binding".to_owned(),
        ));
    }
    connection
        .execute(
            "INSERT INTO auth_sessions (
            session_id, principal_id, participant_id, participant_kind,
            session_public_key, session_key_id, state, created_at, last_authenticated_at, expires_at,
            revoked_at, version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                session.session_id,
                session.principal_id,
                session.participant_id,
                encode_enum(session.participant_kind)?,
                session.session_public_key,
                session.session_key_id,
                encode_enum(session.state)?,
                session.created_at,
                session.last_authenticated_at,
                session.expires_at,
                session.revoked_at,
                to_sql_version(session.version)?
            ],
        )
        .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn load_session(
    connection: &Connection,
    id: &str,
) -> Result<Option<SessionRecord>, AuthorizationStateError> {
    connection
        .query_row(
            &format!("{SESSION_SELECT} WHERE session_id = ?1"),
            [id],
            decode_session,
        )
        .optional()
        .map_err(sql_error)
        .and_then(|session| {
            session.map_or(Ok(None), |session| {
                validate_persisted_session(&session)?;
                Ok(Some(session))
            })
        })
}

pub(in crate::platform::auth) fn decode_session(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        session_id: row.get(0)?,
        principal_id: row.get(1)?,
        participant_id: row.get(2)?,
        participant_kind: decode_enum(row.get::<_, String>(3)?)?,
        session_public_key: row.get(4)?,
        session_key_id: row.get(5)?,
        state: decode_enum(row.get::<_, String>(6)?)?,
        created_at: row.get(7)?,
        last_authenticated_at: row.get(8)?,
        expires_at: row.get(9)?,
        revoked_at: row.get(10)?,
        version: from_sql_version(row.get(11)?)?,
    })
}
