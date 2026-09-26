use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, OptionalExtension, Row, ToSql};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    canonicalize_json, parse_authorization_context, SignedAuthorizationContext,
};

use super::super::{
    application::repository::IdempotentOutcome,
    authority::{
        issuance_snapshot_token, IssuanceConnection, IssuanceCredential, IssuanceSnapshotToken,
    },
    domain::require_protocol_timestamp,
    sqlite::{
        common::{
            decode_enum, encode_enum, from_sql_version, map_write_error, sql_error, to_sql_version,
        },
        contexts::sqlite_issuance_snapshot,
        outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay},
        validation::next_version,
        SqliteAuthorizationStore,
    },
    AuthorizationStateError, IdempotencyResultRecord, PostCommitActionKind, PostCommitActionRecord,
};

const MAXIMUM_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Durable authorization-context lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationContextState {
    /// Signed context is within its durable lease and has not been revoked.
    Active,
    /// Context was invalidated before its signed expiry.
    Revoked,
    /// Signed expiry elapsed without a semantic revocation.
    Expired,
}

/// Stable safe reason published for an authorization-context revocation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationContextRevocationReason {
    SessionRevoked,
    SessionExpired,
    CredentialChanged,
    PrincipalChanged,
    PrincipalInactive,
    AuthorityChanged,
    AuthorityRevoked,
    MaterializationChanged,
    MaterializationUnavailable,
    DeploymentInactive,
    DeploymentChanged,
    InstanceInactive,
    InstanceChanged,
    DeviceInactive,
    DeviceChanged,
    DelegationChanged,
    ParticipantChanged,
    IssuerRevoked,
    ContextReplaced,
    AdministrativeRevoke,
}

/// Durable signed authorization context and its publication lifecycle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationContextRecord {
    pub context_digest: String,
    pub connection_id: String,
    pub session_public_key: String,
    pub inbox_prefix: String,
    pub principal_id: String,
    pub principal_kind: trellis_protocol::AuthorizationPrincipalKind,
    pub participant_id: String,
    pub owner_kind: trellis_protocol::GrantOwnerKind,
    pub owner_id: String,
    pub grant_revision: u64,
    /// Exact retained SQL snapshot; not a client-side admission assertion.
    pub installed_revision: u64,
    pub identity_key_id: Option<String>,
    pub login_session_id: Option<String>,
    pub issuer_key_id: String,
    pub signed_context_json: String,
    pub issuance_snapshot_token: String,
    pub issued_at: i64,
    pub not_before: i64,
    pub refresh_at: i64,
    pub expires_at: i64,
    pub state: AuthorizationContextState,
    pub published_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub revocation_reason: Option<AuthorizationContextRevocationReason>,
    pub version: u64,
}

impl AuthorizationContextRecord {
    pub(crate) fn signed_context(
        &self,
    ) -> Result<SignedAuthorizationContext, AuthorizationStateError> {
        let value = serde_json::from_str(&self.signed_context_json)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        parse_authorization_context(&value)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
    }
}

/// Aggregate optimistic context-issuance commit.
#[derive(Clone, Debug)]
pub struct AuthorizationContextCommit {
    pub expected_snapshot_token: IssuanceSnapshotToken,
    pub context: AuthorizationContextRecord,
    pub idempotency: IdempotencyResultRecord,
    pub now: i64,
    pub minimum_remaining_seconds: i64,
}

/// Exact durable selector for context invalidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationContextSelector {
    Login(String),
    Principal(String),
    Grant(super::super::GrantOwnerKind, String, String),
    Deployment(String),
    Instance(String),
    Issuer(String),
}

/// Permanent signed-context history and mutable liveness repository boundary.
#[async_trait]
pub(crate) trait AuthorizationContextRepository: Send + Sync {
    async fn get_context_by_digest(
        &self,
        context_digest: &str,
    ) -> Result<Option<AuthorizationContextRecord>, AuthorizationStateError>;

    async fn list_contexts(
        &self,
        after_context_digest: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError>;

    async fn list_revoked_contexts(
        &self,
        after_context_digest: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError>;

    async fn commit_context(
        &self,
        commit: AuthorizationContextCommit,
    ) -> Result<IdempotentOutcome<AuthorizationContextRecord>, AuthorizationStateError>;

    async fn mark_context_published(
        &self,
        context_digest: &str,
        published_at: i64,
    ) -> Result<AuthorizationContextRecord, AuthorizationStateError>;

    async fn expire_contexts(
        &self,
        now: i64,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError>;

    async fn delete_expired_context_idempotency(
        &self,
        now: i64,
    ) -> Result<usize, AuthorizationStateError>;
}

#[async_trait]
impl AuthorizationContextRepository for SqliteAuthorizationStore {
    async fn get_context_by_digest(
        &self,
        context_digest: &str,
    ) -> Result<Option<AuthorizationContextRecord>, AuthorizationStateError> {
        let digest = context_digest.to_owned();
        self.run(move |connection| {
            connection
                .query_row(
                    &format!("{} WHERE context_digest = ?1", CONTEXT_SELECT),
                    [&digest],
                    decode_sql_context,
                )
                .optional()
                .map_err(sql_error)
        })
        .await
    }

    async fn list_contexts(
        &self,
        after_context_digest: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError> {
        let after = after_context_digest.unwrap_or_default().to_owned();
        let limit = limit.min(256) as i64;
        self.run(move |connection| {
            query_sql_contexts(
                connection,
                "context_digest > ?1 ORDER BY context_digest LIMIT ?2",
                &[&after, &limit],
            )
        })
        .await
    }

    async fn list_revoked_contexts(
        &self,
        after_context_digest: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError> {
        let after_context_digest = after_context_digest.map(str::to_owned);
        self.run(move |connection| {
            let limit = limit.min(256) as i64;
            match after_context_digest.as_deref() {
                Some(after) => query_sql_contexts(
                    connection,
                    "state = 'revoked' AND context_digest > ?1 ORDER BY context_digest LIMIT ?2",
                    &[&after, &limit],
                ),
                None => query_sql_contexts(
                    connection,
                    "state = 'revoked' ORDER BY context_digest LIMIT ?1",
                    &[&limit],
                ),
            }
        })
        .await
    }

    async fn commit_context(
        &self,
        commit: AuthorizationContextCommit,
    ) -> Result<IdempotentOutcome<AuthorizationContextRecord>, AuthorizationStateError> {
        validate_context_record(&commit.context)?;
        require_protocol_timestamp("now", commit.now)?;
        if commit.minimum_remaining_seconds < 0
            || commit.context.issuance_snapshot_token != commit.expected_snapshot_token.0
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "invalid context commit inputs".into(),
            ));
        }
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let current_signer = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_authorization_issuers WHERE key_id = ?1 AND is_current = 1 AND revoked_at IS NULL)",
                [&commit.context.issuer_key_id], |row| row.get::<_, bool>(0),
            ).map_err(sql_error)?;
            if !current_signer {
                return Err(AuthorizationStateError::AuthorityStale);
            }
            let credential = match (&commit.context.login_session_id, &commit.context.identity_key_id) {
                (Some(id), None) => IssuanceCredential::Login(id.clone()),
                (None, Some(id)) => IssuanceCredential::Native(id.clone()),
                _ => return Err(AuthorizationStateError::NotAuthorized),
            };
            let request = IssuanceConnection {
                credential,
                connection_id: commit.context.connection_id.clone(),
                session_public_key: commit.context.session_public_key.clone(),
            };
            let snapshot = sqlite_issuance_snapshot(&transaction, &request)?;
            if issuance_snapshot_token(&snapshot)? != commit.expected_snapshot_token {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let now_ms = commit.now.checked_mul(1_000)
                .ok_or_else(|| AuthorizationStateError::InvalidRecord("timestamp overflow".into()))?;
            let current = super::super::issuance::resolve_snapshot(snapshot, now_ms)?;
            let signed = commit.context.signed_context()?;
            if !current.matches_context(&signed.unsigned, commit.context.installed_revision) {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let connection_conflict = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_authorization_contexts
                 WHERE connection_id = ?1 AND (
                    session_public_key IS NOT ?2 OR inbox_prefix IS NOT ?3
                    OR principal_id IS NOT ?4 OR principal_kind IS NOT ?5
                    OR participant_id IS NOT ?6 OR owner_kind IS NOT ?7 OR owner_id IS NOT ?8
                    OR identity_key_id IS NOT ?9 OR login_session_id IS NOT ?10))",
                params![commit.context.connection_id, commit.context.session_public_key,
                    commit.context.inbox_prefix, commit.context.principal_id,
                    encode_enum(commit.context.principal_kind)?, commit.context.participant_id,
                    encode_enum(commit.context.owner_kind)?, commit.context.owner_id,
                    commit.context.identity_key_id, commit.context.login_session_id],
                |row| row.get::<_, bool>(0),
            ).map_err(sql_error)?;
            if connection_conflict {
                return Err(AuthorizationStateError::NotAuthorized);
            }
            if let Some(replay) = sqlite_idempotency_replay(&transaction, &commit.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(replay));
            }
            let minimum_expires_at = commit.now.checked_add(commit.minimum_remaining_seconds)
                .ok_or_else(|| AuthorizationStateError::InvalidRecord("context deadline overflow".into()))?;
            let active = query_sql_contexts(
                &transaction,
                "connection_id = ?1 AND state = 'active' AND issuance_snapshot_token = ?2
                 AND issuer_key_id = ?3 AND refresh_at > ?4 AND expires_at >= ?5
                 ORDER BY expires_at DESC, context_digest DESC LIMIT 1",
                &[&commit.context.connection_id, &commit.context.issuance_snapshot_token,
                    &commit.context.issuer_key_id, &commit.now, &minimum_expires_at],
            )?;
            if let Some(existing) = active.first() {
                let mut idempotency = commit.idempotency;
                idempotency.result = json!({ "contextDigest": existing.context_digest });
                insert_sql_idempotency_and_actions(&transaction, &idempotency, &[])?;
                let existing = existing.clone();
                transaction.commit().map_err(sql_error)?;
                return Ok(IdempotentOutcome::Applied(existing));
            }
            let mut actions = Vec::new();
            let context = match load_sql_context_by_digest(&transaction, &commit.context.context_digest)? {
                Some(existing) => {
                    if existing.signed_context_json != commit.context.signed_context_json {
                        return Err(AuthorizationStateError::InvalidRecord("context digest collision".into()));
                    }
                    if existing.state != AuthorizationContextState::Active {
                        return Err(AuthorizationStateError::StorageConflict);
                    }
                    existing
                }
                None => {
                    insert_sql_context(&transaction, &commit.context)?;
                    actions.push(context_action(&commit.context, PostCommitActionKind::ContextPublish, None)?);
                    commit.context
                }
            };
            transaction.execute(
                "UPDATE auth_authorization_issuers SET live_until_seconds = MAX(live_until_seconds, ?1) WHERE key_id = ?2",
                params![context.expires_at, context.issuer_key_id],
            ).map_err(map_write_error)?;
            let mut idempotency = commit.idempotency;
            idempotency.result = json!({ "contextDigest": context.context_digest });
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(context))
        })
        .await
    }

    async fn mark_context_published(
        &self,
        context_digest: &str,
        published_at: i64,
    ) -> Result<AuthorizationContextRecord, AuthorizationStateError> {
        require_protocol_timestamp("publishedAt", published_at)?;
        let digest = context_digest.to_owned();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let current = load_sql_context_by_digest(&transaction, &digest)?.ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("context is missing".to_owned())
            })?;
            if current.published_at.is_some() {
                return Ok(current);
            }
            let version = next_version(current.version)?;
            transaction
                .execute(
                    "UPDATE auth_authorization_contexts
                     SET published_at = ?1, version = ?2
                     WHERE context_digest = ?3 AND version = ?4 AND published_at IS NULL",
                    params![
                        published_at,
                        to_sql_version(version)?,
                        digest,
                        to_sql_version(current.version)?,
                    ],
                )
                .map_err(map_write_error)
                .and_then(|changed| {
                    if changed == 1 {
                        Ok(())
                    } else {
                        Err(AuthorizationStateError::StorageConflict)
                    }
                })?;
            let updated = load_sql_context_by_digest(&transaction, &digest)?.ok_or_else(|| {
                AuthorizationStateError::Storage("published context disappeared".to_owned())
            })?;
            transaction.commit().map_err(sql_error)?;
            Ok(updated)
        })
        .await
    }

    async fn expire_contexts(
        &self,
        now: i64,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError> {
        require_protocol_timestamp("now", now)?;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let contexts = query_sql_contexts(
                &transaction,
                "state = 'active' AND expires_at <= ?1 ORDER BY expires_at, context_digest",
                &[&now],
            )?;
            for context in &contexts {
                transaction
                    .execute(
                        "UPDATE auth_authorization_contexts SET state = 'expired', version = ?1
                         WHERE context_digest = ?2 AND state = 'active' AND version = ?3",
                        params![
                            to_sql_version(next_version(context.version)?)?,
                            context.context_digest,
                            to_sql_version(context.version)?,
                        ],
                    )
                    .map_err(map_write_error)?;
            }
            let expired = contexts
                .into_iter()
                .map(|mut context| {
                    context.state = AuthorizationContextState::Expired;
                    context.version = next_version(context.version)?;
                    Ok(context)
                })
                .collect::<Result<Vec<_>, AuthorizationStateError>>()?;
            transaction.commit().map_err(sql_error)?;
            Ok(expired)
        })
        .await
    }

    async fn delete_expired_context_idempotency(
        &self,
        now: i64,
    ) -> Result<usize, AuthorizationStateError> {
        require_protocol_timestamp("now", now)?;
        let now_millis = now.checked_mul(1_000).ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(
                "idempotency cleanup timestamp overflow".to_owned(),
            )
        })?;
        self.run(move |connection| {
            connection
                .execute(
                    "DELETE FROM auth_idempotency_results
                 WHERE purpose = 'authorizationContextIssue' AND expires_at <= ?1",
                    [now_millis],
                )
                .map_err(map_write_error)
        })
        .await
    }
}

const CONTEXT_SELECT: &str = "SELECT
    context_digest, connection_id, session_public_key, inbox_prefix, principal_id,
    principal_kind, participant_id, owner_kind, owner_id, grant_revision, installed_revision,
    identity_key_id, login_session_id, issuer_key_id, signed_context_json, issuance_snapshot_token,
    issued_at, not_before, refresh_at, expires_at,
    state, published_at, revoked_at, revocation_reason, version
    FROM auth_authorization_contexts";

fn insert_sql_context(
    connection: &rusqlite::Connection,
    context: &AuthorizationContextRecord,
) -> Result<(), AuthorizationStateError> {
    connection
        .execute(
            "INSERT INTO auth_authorization_contexts (
                context_digest, connection_id, session_public_key, inbox_prefix, principal_id,
                principal_kind, participant_id, owner_kind, owner_id, grant_revision, installed_revision,
                identity_key_id, login_session_id, issuer_key_id, signed_context_json, issuance_snapshot_token,
                issued_at, not_before, refresh_at, expires_at,
                state, published_at, revoked_at, revocation_reason, version
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
             )",
            params![
                context.context_digest,
                context.connection_id,
                context.session_public_key,
                context.inbox_prefix,
                context.principal_id,
                encode_enum(context.principal_kind)?,
                context.participant_id,
                encode_enum(context.owner_kind)?,
                context.owner_id,
                to_sql_version(context.grant_revision)?,
                to_sql_version(context.installed_revision)?,
                context.identity_key_id,
                context.login_session_id,
                context.issuer_key_id,
                context.signed_context_json,
                context.issuance_snapshot_token,
                context.issued_at,
                context.not_before,
                context.refresh_at,
                context.expires_at,
                encode_enum(context.state)?,
                context.published_at,
                context.revoked_at,
                context.revocation_reason.map(encode_enum).transpose()?,
                to_sql_version(context.version)?,
            ],
        )
        .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn load_sql_context_by_digest(
    connection: &rusqlite::Connection,
    context_digest: &str,
) -> Result<Option<AuthorizationContextRecord>, AuthorizationStateError> {
    connection
        .query_row(
            &format!("{} WHERE context_digest = ?1", CONTEXT_SELECT),
            [context_digest],
            decode_sql_context,
        )
        .optional()
        .map_err(sql_error)
}

fn query_sql_contexts(
    connection: &rusqlite::Connection,
    predicate: &str,
    parameters: &[&dyn ToSql],
) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError> {
    let mut statement = connection
        .prepare(&format!("{} WHERE {}", CONTEXT_SELECT, predicate))
        .map_err(sql_error)?;
    let contexts = statement
        .query_map(parameters, decode_sql_context)
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    Ok(contexts)
}

fn decode_sql_context(row: &Row<'_>) -> rusqlite::Result<AuthorizationContextRecord> {
    let context = AuthorizationContextRecord {
        context_digest: row.get(0)?,
        connection_id: row.get(1)?,
        session_public_key: row.get(2)?,
        inbox_prefix: row.get(3)?,
        principal_id: row.get(4)?,
        principal_kind: decode_enum(row.get::<_, String>(5)?)?,
        participant_id: row.get(6)?,
        owner_kind: decode_enum(row.get::<_, String>(7)?)?,
        owner_id: row.get(8)?,
        grant_revision: from_sql_version(row.get(9)?)?,
        installed_revision: from_sql_version(row.get(10)?)?,
        identity_key_id: row.get(11)?,
        login_session_id: row.get(12)?,
        issuer_key_id: row.get(13)?,
        signed_context_json: row.get(14)?,
        issuance_snapshot_token: row.get(15)?,
        issued_at: row.get(16)?,
        not_before: row.get(17)?,
        refresh_at: row.get(18)?,
        expires_at: row.get(19)?,
        state: decode_enum(row.get::<_, String>(20)?)?,
        published_at: row.get(21)?,
        revoked_at: row.get(22)?,
        revocation_reason: row
            .get::<_, Option<String>>(23)?
            .map(decode_enum)
            .transpose()?,
        version: from_sql_version(row.get(24)?)?,
    };
    validate_context_record(&context).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(context)
}

/// Revoke using Unix seconds; the queued outbox records use Unix milliseconds.
///
/// This is the broad form: every non-revoked context in the selector scope is
/// invalidated. Lifecycle operations that invalidate an entire scope use it.
pub(crate) fn revoke_sql_contexts(
    connection: &rusqlite::Connection,
    selector: &AuthorizationContextSelector,
    reason: AuthorizationContextRevocationReason,
    revoked_at: i64,
) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError> {
    revoke_sql_contexts_matching(connection, selector, reason, revoked_at, |_| Ok(true))
}

/// Revoke only the selected contexts a predicate still considers invalid.
///
/// The predicate sees each retained context and returns whether it must be
/// revoked. It lets grant replacement evaluate every live context against the
/// replacement authority without duplicating the durable revocation machinery.
/// A predicate error aborts the whole transaction.
pub(crate) fn revoke_sql_contexts_matching<F>(
    connection: &rusqlite::Connection,
    selector: &AuthorizationContextSelector,
    reason: AuthorizationContextRevocationReason,
    revoked_at: i64,
    mut should_revoke: F,
) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError>
where
    F: FnMut(&AuthorizationContextRecord) -> Result<bool, AuthorizationStateError>,
{
    require_protocol_timestamp("revokedAt", revoked_at)?;
    let contexts = match selector {
        AuthorizationContextSelector::Login(id) => query_sql_contexts(
            connection,
            "state != 'revoked' AND login_session_id = ?1 ORDER BY context_digest",
            &[id],
        )?,
        AuthorizationContextSelector::Principal(id) => query_sql_contexts(
            connection,
            "state != 'revoked' AND principal_id = ?1 ORDER BY context_digest",
            &[id],
        )?,
        AuthorizationContextSelector::Deployment(id) => query_sql_contexts(
            connection,
            "state != 'revoked' AND owner_kind = 'deployment' AND owner_id = ?1 ORDER BY context_digest",
            &[id],
        )?,
        AuthorizationContextSelector::Grant(kind, id, participant_id) => {
            let kind = encode_enum(*kind)?;
            query_sql_contexts(connection,
                "state != 'revoked' AND owner_kind = ?1 AND owner_id = ?2
                 AND participant_id = ?3 ORDER BY context_digest",
                &[&kind, id, participant_id],
            )?
        }
        AuthorizationContextSelector::Instance(id) => query_sql_contexts(
            connection,
            "state != 'revoked' AND principal_id IN (
                SELECT principal_id FROM auth_instances WHERE instance_id = ?1
             ) ORDER BY context_digest",
            &[id],
        )?,
        AuthorizationContextSelector::Issuer(id) => query_sql_contexts(
            connection,
            "state != 'revoked' AND issuer_key_id = ?1 ORDER BY context_digest",
            &[id],
        )?,
    };
    let mut revoked = Vec::with_capacity(contexts.len());
    for mut context in contexts {
        if !should_revoke(&context)? {
            continue;
        }
        context.state = AuthorizationContextState::Revoked;
        context.revoked_at = Some(revoked_at);
        context.revocation_reason = Some(reason);
        context.version = next_version(context.version)?;
        connection
            .execute(
                "UPDATE auth_authorization_contexts
                 SET state = 'revoked', revoked_at = ?1, revocation_reason = ?2, version = ?3
                  WHERE context_digest = ?4 AND state != 'revoked' AND version = ?5",
                params![
                    revoked_at,
                    encode_enum(reason)?,
                    to_sql_version(context.version)?,
                    context.context_digest,
                    to_sql_version(context.version - 1)?,
                ],
            )
            .map_err(map_write_error)
            .and_then(|changed| {
                if changed == 1 {
                    Ok(())
                } else {
                    Err(AuthorizationStateError::StorageConflict)
                }
            })?;
        insert_context_action(
            connection,
            &context_action(&context, PostCommitActionKind::ContextRevoke, Some(reason))?,
        )?;
        revoked.push(context);
    }
    Ok(revoked)
}

fn insert_context_action(
    connection: &rusqlite::Connection,
    action: &PostCommitActionRecord,
) -> Result<(), AuthorizationStateError> {
    let payload = canonicalize_json(&action.payload)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    if let Some((kind, existing_payload)) = connection
        .query_row(
            "SELECT kind, payload_json FROM auth_post_commit_actions WHERE action_id = ?1",
            [&action.action_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(sql_error)?
    {
        return if kind == encode_enum(action.kind)? && existing_payload == payload {
            Ok(())
        } else {
            Err(AuthorizationStateError::StorageConflict)
        };
    }
    connection
        .execute(
            "INSERT INTO auth_post_commit_actions (
                action_id, kind, payload_json, created_at, attempts, next_attempt_at,
                claimed_until, last_error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                action.action_id,
                encode_enum(action.kind)?,
                payload,
                action.created_at,
                i64::from(action.attempts),
                action.next_attempt_at,
                action.claimed_until,
                action.last_error,
            ],
        )
        .map_err(map_write_error)?;
    Ok(())
}

fn validate_context_record(
    record: &AuthorizationContextRecord,
) -> Result<(), AuthorizationStateError> {
    digest("contextDigest", &record.context_digest)?;
    nonempty("connectionId", &record.connection_id)?;
    digest("sessionPublicKey", &record.session_public_key)?;
    nonempty("inboxPrefix", &record.inbox_prefix)?;
    nonempty("principalId", &record.principal_id)?;
    nonempty("participantId", &record.participant_id)?;
    nonempty("ownerId", &record.owner_id)?;
    digest("issuerKeyId", &record.issuer_key_id)?;
    digest("issuanceSnapshotToken", &record.issuance_snapshot_token)?;
    positive("grantRevision", record.grant_revision)?;
    positive("installedRevision", record.installed_revision)?;
    positive("version", record.version)?;
    for (name, value) in [
        ("issuedAt", record.issued_at),
        ("notBefore", record.not_before),
        ("expiresAt", record.expires_at),
        ("refreshAt", record.refresh_at),
    ] {
        require_protocol_timestamp(name, value)?;
    }
    if record.refresh_at > record.expires_at
        || record.issued_at > record.refresh_at
        || record.not_before > record.expires_at
        || (record.state == AuthorizationContextState::Revoked)
            != (record.revoked_at.is_some() && record.revocation_reason.is_some())
        || (record.state != AuthorizationContextState::Revoked
            && (record.revoked_at.is_some() || record.revocation_reason.is_some()))
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "authorization context lifecycle is inconsistent".to_owned(),
        ));
    }
    let value: Value = serde_json::from_str(&record.signed_context_json).map_err(|error| {
        AuthorizationStateError::InvalidRecord(format!("invalid signed context: {error}"))
    })?;
    let signed = parse_authorization_context(&value)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let canonical = canonicalize_json(&value)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    if canonical != record.signed_context_json
        || signed
            .digest()
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?
            != record.context_digest
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "signed context canonical digest does not match".to_owned(),
        ));
    }
    let context = signed.unsigned;
    if context.connection_id != record.connection_id
        || context.session_key != record.session_public_key
        || context.inbox_prefix != record.inbox_prefix
        || context.principal_id != record.principal_id
        || context.principal_kind != record.principal_kind
        || context.participant_id != record.participant_id
        || context.owner_kind != record.owner_kind
        || context.owner_id != record.owner_id
        || context.grant_revision != record.grant_revision
        || context.identity_key_id != record.identity_key_id
        || context.login_session_id != record.login_session_id
        || context.issuer_key_id != record.issuer_key_id
        || context.issued_at != record.issued_at
        || context.not_before != record.not_before
        || context.expires_at != record.expires_at
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "context record metadata does not match signed context".to_owned(),
        ));
    }
    Ok(())
}

fn context_action(
    context: &AuthorizationContextRecord,
    kind: PostCommitActionKind,
    reason: Option<AuthorizationContextRevocationReason>,
) -> Result<PostCommitActionRecord, AuthorizationStateError> {
    let payload = match kind {
        PostCommitActionKind::ContextPublish => json!({
            "format": "trellis.authorization-context-publish-action.v1",
            "contextDigest": context.context_digest,
        }),
        PostCommitActionKind::ContextRevoke => json!({
            "format": "trellis.authorization-context-revoke-action.v1",
            "contextDigest": context.context_digest,
            "reason": reason,
            "version": context.version,
        }),
        PostCommitActionKind::Event
        | PostCommitActionKind::Kick
        | PostCommitActionKind::ResourceReconcile => unreachable!(),
    };
    let canonical = canonicalize_json(&payload)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    let action_at = context
        .revoked_at
        .unwrap_or(context.issued_at)
        .checked_mul(1_000)
        .ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("context time overflow".to_owned())
        })?;
    Ok(PostCommitActionRecord {
        predecessor_action_id: None,
        action_id: URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())),
        kind,
        payload,
        created_at: action_at,
        attempts: 0,
        next_attempt_at: action_at,
        claimed_until: None,
        last_error: None,
    })
}

pub(crate) fn context_revocation_action_id(
    context: &AuthorizationContextRecord,
) -> Result<String, AuthorizationStateError> {
    Ok(context_action(
        context,
        PostCommitActionKind::ContextRevoke,
        context.revocation_reason,
    )?
    .action_id)
}

fn nonempty(name: &str, value: &str) -> Result<(), AuthorizationStateError> {
    if value.is_empty() || value.trim() != value {
        Err(AuthorizationStateError::InvalidRecord(format!(
            "{name} must be nonempty protocol-safe text"
        )))
    } else {
        Ok(())
    }
}

fn digest(name: &str, value: &str) -> Result<(), AuthorizationStateError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AuthorizationStateError::InvalidRecord(format!("{name} must be a digest")))?;
    if bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == value {
        Ok(())
    } else {
        Err(AuthorizationStateError::InvalidRecord(format!(
            "{name} must canonically encode 32 bytes"
        )))
    }
}

fn positive(name: &str, value: u64) -> Result<(), AuthorizationStateError> {
    if (1..=MAXIMUM_SAFE_INTEGER).contains(&value) {
        Ok(())
    } else {
        Err(AuthorizationStateError::InvalidRecord(format!(
            "{name} must be a positive safe integer"
        )))
    }
}
