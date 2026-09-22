use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::json;

use super::super::application::repository::{
    AccountCreation, AccountFlowCreation, AccountRepository, FirstAdminCompletion,
    IdempotentOutcome, IdentityLinkCompletion, LocalLoginAttempt, LoginPortalMutation,
    PasswordResetCompletion, PortalRepository, PortalRouteMutation, PortalRouteRemoval,
    UserAccountMutation,
};
use super::super::application::validation::{
    validate_local_credential, validate_replacement_credential,
};
use super::super::context::{
    revoke_sql_contexts, AuthorizationContextRevocationReason, AuthorizationContextSelector,
};
use super::super::{
    AccountFlowRecord, AccountFlowState, AuthorizationStateError, LocalCredentialRecord,
    LoginPortalRecord, LoginSettingsRecord, PortalRouteRecord, PrincipalKind, PrincipalRecord,
    PrincipalState, ProviderIdentityLink, UserProfileRecord,
};
use super::common::{
    decode_enum, decode_json, encode_enum, encode_json, from_sql_u32, from_sql_version,
    map_write_error, sql_error, to_sql_version,
};
use super::evidence::load_deployment;
use super::outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay};
use super::principals::load_principal;
use super::provisioning::insert_sql_principal;
use super::validation::{local_login_attempt_result, next_version, user_account_replacement};
use super::SqliteAuthorizationStore;

#[async_trait]
impl AccountRepository for SqliteAuthorizationStore {
    async fn create_user_account(
        &self,
        command: AccountCreation,
    ) -> Result<IdempotentOutcome<UserProfileRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            insert_sql_user_account(
                &transaction,
                &command.principal,
                &command.profile,
                command.credential.as_ref(),
                command.identity.as_ref(),
            )?;
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

    async fn get_user_account(
        &self,
        principal_id: &str,
    ) -> Result<Option<(PrincipalRecord, UserProfileRecord)>, AuthorizationStateError> {
        let principal_id = principal_id.to_owned();
        self.run_read(move |connection| load_user_account(connection, &principal_id))
            .await
    }

    async fn list_user_accounts(
        &self,
        cursor: Option<&(i64, String)>,
        state: Option<&str>,
        search: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(PrincipalRecord, UserProfileRecord)>, AuthorizationStateError> {
        let cursor = cursor.cloned();
        let state = state.map(str::to_owned);
        let search = search.map(str::to_owned);
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT p.principal_id, p.kind, p.state, p.created_at, p.updated_at,
                            p.version, p.disabled_at, p.revoked_at,
                            u.principal_id, u.display_name, u.email, u.image_url,
                            u.created_at, u.updated_at, u.version
                     FROM auth_principals p
                     JOIN auth_user_profiles u ON u.principal_id = p.principal_id
                     WHERE p.kind = ?1
                       AND (?2 IS NULL OR p.state = ?2)
                       AND (?3 IS NULL OR instr(lower(p.principal_id), lower(?3)) > 0
                            OR instr(lower(coalesce(u.display_name, '')), lower(?3)) > 0
                            OR instr(lower(coalesce(u.email, '')), lower(?3)) > 0)
                       AND (?4 IS NULL OR (p.created_at, p.principal_id) > (?4, ?5))
                     ORDER BY p.created_at ASC, p.principal_id ASC
                     LIMIT ?6",
                )
                .map_err(sql_error)?;
            let accounts = statement
                .query_map(
                    params![
                        encode_enum(PrincipalKind::User)?,
                        state,
                        search,
                        cursor.as_ref().map(|cursor| cursor.0),
                        cursor.as_ref().map(|cursor| cursor.1.as_str()),
                        i64::try_from(limit).map_err(|_| {
                            AuthorizationStateError::InvalidRecord(
                                "user account list limit is invalid".to_owned(),
                            )
                        })?
                    ],
                    decode_user_account,
                )
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(accounts)
        })
        .await
    }

    async fn update_user_account(
        &self,
        command: UserAccountMutation,
    ) -> Result<IdempotentOutcome<(PrincipalRecord, UserProfileRecord)>, AuthorizationStateError>
    {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let current_principal = load_principal(&transaction, &command.principal.principal_id)?
                .ok_or(AuthorizationStateError::PrincipalMissing)?;
            let current_profile = load_user_profile(&transaction, &command.principal.principal_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            let target_is_admin = principal_has_accepted_admin_authority(
                &transaction,
                &command.principal.principal_id,
                command.principal.updated_at,
            )?;
            super::grants::require_current_actor(
                &transaction,
                &command.actor,
                target_is_admin,
                command.principal.updated_at,
            )?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let (principal, profile) = user_account_replacement(
                &current_principal,
                &current_profile,
                command.principal,
                command.profile,
                command.expected_version,
            )?;
            if principal.state != super::super::PrincipalState::Active
                && transaction
                    .query_row(
                        "SELECT 1 FROM auth_bootstrap_administrator
                         WHERE singleton = 1 AND principal_id = ?1",
                        [&principal.principal_id],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(sql_error)?
                    .is_some()
            {
                return Err(AuthorizationStateError::InvalidRecord(
                    "the bootstrap administrator principal must remain active".to_owned(),
                ));
            }
            let principal_authorization_changed = current_principal.state != principal.state;
            let principal_changed = transaction
                .execute(
                    "UPDATE auth_principals SET state = ?1, updated_at = ?2, version = ?3,
                            disabled_at = ?4, revoked_at = ?5
                     WHERE principal_id = ?6 AND version = ?7",
                    params![
                        encode_enum(principal.state)?,
                        principal.updated_at,
                        to_sql_version(principal.version)?,
                        principal.disabled_at,
                        principal.revoked_at,
                        principal.principal_id,
                        to_sql_version(command.expected_version)?,
                    ],
                )
                .map_err(map_write_error)?;
            let profile_changed = transaction
                .execute(
                    "UPDATE auth_user_profiles SET display_name = ?1, email = ?2, image_url = ?3,
                            updated_at = ?4, version = ?5
                     WHERE principal_id = ?6 AND version = ?7",
                    params![
                        profile.display_name,
                        profile.email,
                        profile.image_url,
                        profile.updated_at,
                        to_sql_version(profile.version)?,
                        profile.principal_id,
                        to_sql_version(command.expected_version)?,
                    ],
                )
                .map_err(map_write_error)?;
            if principal_changed != 1 || profile_changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            if principal_authorization_changed {
                let reason = match principal.state {
                    PrincipalState::Active => {
                        AuthorizationContextRevocationReason::PrincipalChanged
                    }
                    PrincipalState::Disabled | PrincipalState::Revoked => {
                        AuthorizationContextRevocationReason::PrincipalInactive
                    }
                };
                revoke_sql_contexts(
                    &transaction,
                    &AuthorizationContextSelector::Principal(principal.principal_id.clone()),
                    reason,
                    principal.updated_at.div_euclid(1_000),
                )?;
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied((principal, profile)))
        })
        .await
    }

    async fn get_user_profile(
        &self,
        principal_id: &str,
    ) -> Result<Option<UserProfileRecord>, AuthorizationStateError> {
        let principal_id = principal_id.to_owned();
        self.run_read(move |connection| load_user_profile(connection, &principal_id))
            .await
    }

    async fn get_local_credential(
        &self,
        principal_id: &str,
    ) -> Result<Option<LocalCredentialRecord>, AuthorizationStateError> {
        let principal_id = principal_id.to_owned();
        self.run_read(move |connection| load_local_credential(connection, &principal_id))
            .await
    }

    async fn change_password(
        &self,
        mut command: super::super::application::repository::PasswordChange,
    ) -> Result<IdempotentOutcome<usize>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let active_session = transaction
                .query_row(
                    "SELECT 1 FROM auth_sessions
                     WHERE session_id = ?1 AND principal_id = ?2 AND state = 'active'
                       AND expires_at > ?3",
                    params![
                        command.current_session_id,
                        command.principal_id,
                        command.changed_at
                    ],
                    |_| Ok(()),
                )
                .optional()
                .map_err(sql_error)?;
            if active_session.is_none() {
                return Err(AuthorizationStateError::SessionRevoked);
            }
            if let Some(value) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(value));
            }
            let current =
                load_local_credential(&transaction, &command.principal_id)?.ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord("local credential not found".to_owned())
                })?;
            if current.version != command.expected_version {
                return Err(AuthorizationStateError::StorageConflict);
            }
            validate_replacement_credential(&current, &command.credential, &command.principal_id)?;
            let changed = transaction
                .execute(
                    "UPDATE auth_local_credentials SET password_hash = ?1, hash_profile = ?2,
                     failed_attempts = ?3, locked_until = ?4, password_changed_at = ?5,
                     updated_at = ?6, version = ?7 WHERE principal_id = ?8 AND version = ?9",
                    params![
                        command.credential.password_hash,
                        command.credential.hash_profile,
                        command.credential.failed_attempts,
                        command.credential.locked_until,
                        command.credential.password_changed_at,
                        command.credential.updated_at,
                        to_sql_version(command.credential.version)?,
                        command.principal_id,
                        to_sql_version(command.expected_version)?,
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let revoked = transaction
                .execute(
                    "UPDATE auth_sessions SET state = 'revoked', revoked_at = ?1,
                     version = version + 1
                     WHERE principal_id = ?2 AND session_id <> ?3 AND state = 'active'",
                    params![
                        command.changed_at,
                        command.principal_id,
                        command.current_session_id
                    ],
                )
                .map_err(map_write_error)?;
            let mut statement = transaction
                .prepare(
                    "SELECT session_id FROM auth_sessions
                     WHERE principal_id = ?1 AND session_id <> ?2",
                )
                .map_err(sql_error)?;
            let revoked_sessions = statement
                .query_map(
                    params![command.principal_id, command.current_session_id],
                    |row| row.get::<_, String>(0),
                )
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            drop(statement);
            for session_id in revoked_sessions {
                revoke_sql_contexts(
                    &transaction,
                    &AuthorizationContextSelector::Login(session_id),
                    AuthorizationContextRevocationReason::CredentialChanged,
                    command.changed_at.div_euclid(1_000),
                )?;
            }
            command.idempotency.result = json!({
                "changedAt": command.changed_at,
                "revokedSessionCount": revoked,
            });
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(revoked))
        })
        .await
    }

    async fn get_local_credential_by_username(
        &self,
        normalized_username: &str,
    ) -> Result<Option<LocalCredentialRecord>, AuthorizationStateError> {
        let normalized_username = normalized_username.to_owned();
        self.run_read(move |connection| {
            let credential = connection
                .query_row(
                    "SELECT principal_id, normalized_username, password_hash, hash_profile, failed_attempts, locked_until, password_changed_at, updated_at, version
                     FROM auth_local_credentials WHERE normalized_username = ?1",
                    [normalized_username],
                    decode_local_credential,
                )
                .optional()
                .map_err(sql_error)?;
            credential.map_or(Ok(None), |credential| {
                validate_local_credential(&credential)?;
                Ok(Some(credential))
            })
        })
        .await
    }

    async fn record_local_login_attempt(
        &self,
        attempt: LocalLoginAttempt,
    ) -> Result<LocalCredentialRecord, AuthorizationStateError> {
        let principal_id = attempt.principal_id.clone();
        self.run(move |connection| {
            let current = load_local_credential(connection, &principal_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            let next = local_login_attempt_result(&current, &attempt)?;
            if next == current {
                return Ok(current);
            }
            let changed = connection
                .execute(
                    "UPDATE auth_local_credentials SET failed_attempts = ?1, locked_until = ?2,
                        updated_at = ?3, version = ?4
                 WHERE principal_id = ?5 AND version = ?6",
                    params![
                        i64::from(next.failed_attempts),
                        next.locked_until,
                        next.updated_at,
                        to_sql_version(next.version)?,
                        next.principal_id,
                        to_sql_version(current.version)?,
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            Ok(next)
        })
        .await
    }

    async fn replace_admin_account_flow(
        &self,
        mut flow: AccountFlowRecord,
        now: i64,
        rotate: bool,
    ) -> Result<Option<AccountFlowRecord>, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("now", now)?;
        if flow.kind != super::super::AccountFlowKind::AdminAccount
            || flow.created_at > now
            || flow.expires_at <= now
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "replacement admin-account flow must be pending and unexpired".to_owned(),
            ));
        }
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let bootstrap_principal_id = transaction
                .query_row(
                    "SELECT principal_id FROM auth_bootstrap_administrator WHERE singleton = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_error)?;
            if bootstrap_principal_id.is_some() && !rotate {
                return Ok(None);
            }
            flow.target_principal_id = bootstrap_principal_id;
            let existing = transaction
                .query_row(
                    "SELECT flow_id, kind, token_hash, target_principal_id, target_provider_id,
                            return_location, payload_json, state, created_at, expires_at,
                            consumed_at, version
                     FROM auth_account_flows
                     WHERE kind = 'admin_account' AND state = 'pending' AND expires_at > ?1
                     ORDER BY created_at, flow_id LIMIT 1",
                    [now],
                    decode_account_flow,
                )
                .optional()
                .map_err(sql_error)?;
            if let Some(existing) = existing {
                if !rotate {
                    return Ok(Some(existing));
                }
                if existing.version >= super::super::MAX_PROTOCOL_INTEGER {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "admin-account flow version overflow".to_owned(),
                    ));
                }
                transaction
                    .execute(
                        "UPDATE auth_account_flows SET state = 'revoked', version = version + 1
                         WHERE flow_id = ?1 AND version = ?2",
                        params![existing.flow_id, existing.version],
                    )
                    .map_err(map_write_error)?;
            }
            transaction
                .execute(
                    "UPDATE auth_account_flows SET state = 'expired', version = version + 1
                     WHERE kind = 'admin_account' AND state = 'pending' AND expires_at <= ?1",
                    [now],
                )
                .map_err(map_write_error)?;
            insert_account_flow(&transaction, &flow)?;
            transaction.commit().map_err(sql_error)?;
            Ok(Some(flow))
        })
        .await
    }

    async fn create_account_flow(
        &self,
        command: AccountFlowCreation,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            if command.flow.kind == super::super::model::AccountFlowKind::AdminAccount {
                return Err(AuthorizationStateError::InvalidRecord(
                    "administrator-account flows must use replace_admin_account_flow".to_owned(),
                ));
            }
            insert_account_flow(&transaction, &command.flow)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.flow))
        })
        .await
    }

    async fn get_account_flow_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<AccountFlowRecord>, AuthorizationStateError> {
        let token_hash = token_hash.to_owned();
        self.run_read(move |connection| load_account_flow_by_hash(connection, &token_hash))
            .await
    }

    async fn complete_password_reset(
        &self,
        command: PasswordResetCompletion,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let flow = sqlite_pending_account_flow(
                &transaction,
                &command.token_hash,
                command.expected_flow_version,
                command.flow_kind,
                command.consumed_at,
            )?;
            let principal_id = flow.target_principal_id.clone().ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "password-reset flow has no target principal".to_owned(),
                )
            })?;
            let target_is_admin = command.flow_kind
                == super::super::model::AccountFlowKind::PasswordReset
                && (command.admin_target
                    || principal_has_accepted_admin_authority(
                        &transaction,
                        &principal_id,
                        command.consumed_at,
                    )?);
            let requester_is_admin = match command.requester_principal_id.as_deref() {
                Some(requester) => principal_has_accepted_admin_authority(
                    &transaction,
                    requester,
                    command.consumed_at,
                )?,
                None => false,
            };
            if target_is_admin && !requester_is_admin {
                return Err(AuthorizationStateError::InvalidRecord(
                    "trellis.auth::admin capability is required".to_owned(),
                ));
            }
            if command.flow_kind == super::super::model::AccountFlowKind::AdminAccount
                && transaction
                    .query_row(
                        "SELECT principal_id FROM auth_bootstrap_administrator WHERE singleton = 1",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(sql_error)?
                    .as_deref()
                    != Some(principal_id.as_str())
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            if command.flow_kind == super::super::model::AccountFlowKind::AdminAccount {
                transaction
                    .execute(
                        "UPDATE auth_principals
                         SET state = 'active', updated_at = ?1, version = version + 1,
                             disabled_at = NULL, revoked_at = NULL
                         WHERE principal_id = ?2 AND state != 'active'",
                        params![command.consumed_at, principal_id],
                    )
                    .map_err(map_write_error)?;
            }
            let current = load_local_credential(&transaction, &principal_id)?;
            if current.as_ref().map(|current| current.version)
                != command.expected_credential_version
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            if let Some(current) = current {
                super::super::application::validation::validate_local_credential(
                    &command.replacement,
                )?;
                if command.replacement.principal_id != principal_id
                    || command.replacement.version != current.version + 1
                    || command.replacement.updated_at < current.updated_at
                    || command.replacement.password_changed_at < current.password_changed_at
                {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                if command.identity.is_none()
                    && command.replacement.normalized_username != current.normalized_username
                {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                let changed = transaction
                    .execute(
                    "UPDATE auth_local_credentials SET normalized_username = ?1, password_hash = ?2, hash_profile = ?3, failed_attempts = ?4, locked_until = ?5, password_changed_at = ?6, updated_at = ?7, version = ?8
                 WHERE principal_id = ?9 AND version = ?10",
                    params![
                        command.replacement.normalized_username,
                        command.replacement.password_hash,
                        i64::from(command.replacement.hash_profile),
                        i64::from(command.replacement.failed_attempts),
                        command.replacement.locked_until,
                        command.replacement.password_changed_at,
                        command.replacement.updated_at,
                        to_sql_version(command.replacement.version)?,
                        principal_id,
                        to_sql_version(current.version)?
                    ],
                    )
                    .map_err(map_write_error)?;
                if changed != 1 {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                if let Some(identity) = command.identity.as_ref() {
                    if identity.principal_id != principal_id
                        || identity.provider != "local"
                        || identity.provider_subject != command.replacement.normalized_username
                    {
                        return Err(AuthorizationStateError::StorageConflict);
                    }
                    let changed = transaction
                        .execute(
                            "UPDATE auth_provider_identities SET provider_subject = ?1, last_seen_at = ?2
                             WHERE provider = 'local' AND principal_id = ?3",
                            params![identity.provider_subject, identity.last_seen_at, principal_id],
                        )
                        .map_err(map_write_error)?;
                    if changed != 1 {
                        return Err(AuthorizationStateError::StorageConflict);
                    }
                }
            } else {
                super::super::application::validation::validate_local_credential(
                    &command.replacement,
                )?;
                let identity = command
                    .identity
                    .as_ref()
                    .ok_or(AuthorizationStateError::StorageConflict)?;
                if identity.principal_id != principal_id
                    || identity.provider != "local"
                    || identity.provider_subject != command.replacement.normalized_username
                {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                transaction
                    .execute(
                        "INSERT INTO auth_local_credentials (principal_id, normalized_username, password_hash, hash_profile, failed_attempts, locked_until, password_changed_at, updated_at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            command.replacement.principal_id,
                            command.replacement.normalized_username,
                            command.replacement.password_hash,
                            i64::from(command.replacement.hash_profile),
                            i64::from(command.replacement.failed_attempts),
                            command.replacement.locked_until,
                            command.replacement.password_changed_at,
                            command.replacement.updated_at,
                            to_sql_version(command.replacement.version)?
                        ],
                    )
                    .map_err(map_write_error)?;
                transaction
                    .execute(
                        "INSERT INTO auth_provider_identities (provider, provider_subject, principal_id, linked_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            identity.provider,
                            identity.provider_subject,
                            identity.principal_id,
                            identity.linked_at,
                            identity.last_seen_at
                        ],
                    )
                    .map_err(map_write_error)?;
            }
            if let Some(profile) = command.profile.as_ref() {
                if profile.principal_id != principal_id || profile.version == 0 {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                let changed = transaction
                    .execute(
                        "UPDATE auth_user_profiles
                         SET display_name = ?1, email = ?2, image_url = ?3, updated_at = ?4, version = ?5
                         WHERE principal_id = ?6 AND version = ?7",
                        params![
                            profile.display_name,
                            profile.email,
                            profile.image_url,
                            profile.updated_at,
                            to_sql_version(profile.version)?,
                            principal_id,
                            to_sql_version(profile.version - 1)?
                        ],
                    )
                    .map_err(map_write_error)?;
                if changed != 1 {
                    return Err(AuthorizationStateError::StorageConflict);
                }
            }
            for binding in command.bindings {
                super::grants::replace_grant_binding(
                    &transaction,
                    binding,
                    command.consumed_at,
                )?;
            }
            let revoked_sessions = {
                let mut statement = transaction
                    .prepare(
                        "SELECT session_id, participant_id FROM auth_sessions
                         WHERE principal_id = ?1 AND state = 'active'",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(params![principal_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(sql_error)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(sql_error)?
            };
            transaction
                .execute(
                    "UPDATE auth_sessions SET state = 'revoked', revoked_at = ?1, version = version + 1
                  WHERE principal_id = ?2 AND state = 'active'",
                    params![command.consumed_at, principal_id],
                )
                .map_err(map_write_error)?;
            revoke_sql_contexts(
                &transaction,
                &AuthorizationContextSelector::Principal(principal_id.clone()),
                AuthorizationContextRevocationReason::CredentialChanged,
                command.consumed_at.div_euclid(1_000),
            )?;
            let completed = consume_sql_flow(&transaction, &flow, command.consumed_at)?;
            let mut actions = command.actions;
            for (session_id, participant_id) in &revoked_sessions {
                let action_id = trellis_protocol::digest_json(&serde_json::json!({
                    "principalId": principal_id,
                    "sessionId": session_id,
                    "event": "Auth.Sessions.Revoked",
                }))
                .map_err(|error| {
                    AuthorizationStateError::InvalidRecord(error.to_string())
                })?;
                actions.push(super::super::model::session_revoked_event(
                    session_id,
                    participant_id,
                    &principal_id,
                    Some("password_reset"),
                    command.requester_principal_id.as_deref(),
                    command.consumed_at,
                    action_id,
                )?);
            }
            let kick_id = trellis_protocol::digest_json(&serde_json::json!({
                "principalId": principal_id,
                "request": command.token_hash,
                "kick": "password_reset",
            }))
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            actions.push(super::super::model::principal_session_kick(
                &principal_id,
                "password_reset",
                command.consumed_at,
                kick_id,
            ));
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(completed))
        })
        .await
    }

    async fn complete_identity_link(
        &self,
        command: IdentityLinkCompletion,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let flow = sqlite_pending_account_flow(
                &transaction,
                &command.token_hash,
                command.expected_flow_version,
                super::super::model::AccountFlowKind::IdentityLink,
                command.consumed_at,
            )?;
            if flow.target_principal_id.as_deref() != Some(command.identity.principal_id.as_str())
                || flow
                    .target_provider_id
                    .as_deref()
                    .is_some_and(|provider| provider != command.identity.provider)
                || load_principal(&transaction, &command.identity.principal_id)?.is_none_or(
                    |principal| principal.kind != PrincipalKind::User,
                )
            {
                return Err(AuthorizationStateError::InvalidRecord(
                    "identity-link flow target does not match the supplied identity".to_owned(),
                ));
            }
            if let Some(credential) = command.credential {
                if credential.principal_id != command.identity.principal_id
                    || credential.normalized_username != command.identity.provider_subject
                    || command.identity.provider != "local"
                {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "local credential does not match the supplied identity".to_owned(),
                    ));
                }
                transaction
                    .execute(
                        "INSERT INTO auth_local_credentials (principal_id, normalized_username, password_hash, hash_profile, failed_attempts, locked_until, password_changed_at, updated_at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            credential.principal_id,
                            credential.normalized_username,
                            credential.password_hash,
                            i64::from(credential.hash_profile),
                            i64::from(credential.failed_attempts),
                            credential.locked_until,
                            credential.password_changed_at,
                            credential.updated_at,
                            to_sql_version(credential.version)?
                        ],
                    )
                    .map_err(map_write_error)?;
            }
            transaction
                .execute(
                    "INSERT INTO auth_provider_identities (provider, provider_subject, principal_id, linked_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        command.identity.provider,
                        command.identity.provider_subject,
                        command.identity.principal_id,
                        command.identity.linked_at,
                        command.identity.last_seen_at
                    ],
                )
                .map_err(map_write_error)?;
            let completed = consume_sql_flow(&transaction, &flow, command.consumed_at)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(completed))
        })
        .await
    }

    async fn complete_first_admin(
        &self,
        command: FirstAdminCompletion,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let flow = sqlite_pending_account_flow(
                &transaction,
                &command.token_hash,
                command.expected_flow_version,
                super::super::model::AccountFlowKind::AdminAccount,
                command.consumed_at,
            )?;
            if flow.target_principal_id.is_some()
                || flow.target_provider_id.is_some()
                || transaction
                    .query_row(
                        "SELECT 1 FROM auth_bootstrap_administrator WHERE singleton = 1",
                        [],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(sql_error)?
                    .is_some()
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            insert_sql_user_account(
                &transaction,
                &command.principal,
                &command.profile,
                command.credential.as_ref(),
                Some(&command.identity),
            )?;
            for binding in command.bindings {
                super::grants::replace_grant_binding(&transaction, binding, command.consumed_at)?;
            }
            transaction
                .execute(
                    "INSERT INTO auth_bootstrap_administrator (singleton, principal_id, created_at)
                     VALUES (1, ?1, ?2)",
                    params![command.principal.principal_id, command.consumed_at],
                )
                .map_err(map_write_error)?;
            let completed = consume_sql_flow(&transaction, &flow, command.consumed_at)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(completed))
        })
        .await
    }

    async fn get_provider_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<ProviderIdentityLink>, AuthorizationStateError> {
        let provider = provider.to_owned();
        let subject = subject.to_owned();
        self.run_read(move |connection| {
            connection
                .query_row(
                    "SELECT provider, provider_subject, principal_id, linked_at, last_seen_at
                     FROM auth_provider_identities
                     WHERE provider = ?1 AND provider_subject = ?2",
                    params![provider, subject],
                    |row| {
                        Ok(ProviderIdentityLink {
                            provider: row.get(0)?,
                            provider_subject: row.get(1)?,
                            principal_id: row.get(2)?,
                            linked_at: row.get(3)?,
                            last_seen_at: row.get(4)?,
                        })
                    },
                )
                .optional()
                .map_err(sql_error)
        })
        .await
    }

    async fn list_provider_identities(
        &self,
        principal_id: &str,
    ) -> Result<Vec<ProviderIdentityLink>, AuthorizationStateError> {
        let principal_id = principal_id.to_owned();
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT provider, provider_subject, principal_id, linked_at, last_seen_at
                     FROM auth_provider_identities WHERE principal_id = ?1
                     ORDER BY provider, provider_subject",
                )
                .map_err(sql_error)?;
            let identities = statement
                .query_map([principal_id], |row| {
                    Ok(ProviderIdentityLink {
                        provider: row.get(0)?,
                        provider_subject: row.get(1)?,
                        principal_id: row.get(2)?,
                        linked_at: row.get(3)?,
                        last_seen_at: row.get(4)?,
                    })
                })
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(identities)
        })
        .await
    }

    async fn unlink_provider_identity(
        &self,
        command: super::super::application::repository::ProviderIdentityUnlink,
    ) -> Result<IdempotentOutcome<bool>, AuthorizationStateError> {
        if command.provider == "local" {
            return Err(AuthorizationStateError::InvalidRecord(
                "local credentials cannot be unlinked".to_owned(),
            ));
        }
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(value) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(value));
            }
            let owner = transaction
                .query_row(
                    "SELECT principal_id FROM auth_provider_identities
                     WHERE provider = ?1 AND provider_subject = ?2",
                    params![command.provider, command.provider_subject],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_error)?
                .filter(|owner| owner == &command.principal_id)
                .ok_or(AuthorizationStateError::IdentityMissing)?;
            let method_count: i64 = transaction
                .query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM auth_local_credentials WHERE principal_id = ?1) +
                       (SELECT COUNT(*) FROM auth_provider_identities WHERE principal_id = ?1
                        AND NOT (provider = ?2 AND provider_subject = ?3))",
                    params![owner, command.provider, command.provider_subject],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if method_count == 0 {
                return Err(AuthorizationStateError::InvalidRecord(
                    "the last authentication method cannot be unlinked".to_owned(),
                ));
            }
            let changed = transaction
                .execute(
                    "DELETE FROM auth_provider_identities
                     WHERE provider = ?1 AND provider_subject = ?2 AND principal_id = ?3",
                    params![
                        command.provider,
                        command.provider_subject,
                        command.principal_id
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(true))
        })
        .await
    }

    async fn get_principal(
        &self,
        id: &str,
    ) -> Result<Option<PrincipalRecord>, AuthorizationStateError> {
        let id = id.to_owned();
        self.run_read(move |connection| load_principal(connection, &id))
            .await
    }
}

fn principal_has_accepted_admin_authority(
    connection: &Connection,
    principal_id: &str,
    now: i64,
) -> Result<bool, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT EXISTS (
                SELECT 1
                 FROM auth_grant_bindings AS binding,
                      json_each(binding.platform_privileges_json) AS privilege
                 WHERE binding.owner_kind = 'user'
                   AND binding.owner_id = ?1
                   AND binding.state = 'active'
                   AND (binding.expires_at IS NULL OR binding.expires_at > ?2)
                   AND privilege.value = 'trellis.auth::admin'
            )",
            params![principal_id, now],
            |row| row.get(0),
        )
        .map_err(sql_error)
}

#[async_trait]
impl PortalRepository for SqliteAuthorizationStore {
    async fn list_login_portals(&self) -> Result<Vec<LoginPortalRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare("SELECT portal_id FROM auth_login_portals ORDER BY portal_id")
                .map_err(sql_error)?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            ids.into_iter()
                .map(|portal_id| {
                    load_login_portal(connection, &portal_id)?
                        .map(|value| value.0)
                        .ok_or(AuthorizationStateError::StorageConflict)
                })
                .collect()
        })
        .await
    }

    async fn get_login_portal(
        &self,
        portal_id: &str,
    ) -> Result<Option<(LoginPortalRecord, LoginSettingsRecord)>, AuthorizationStateError> {
        let portal_id = portal_id.to_owned();
        self.run_read(move |connection| load_login_portal(connection, &portal_id))
            .await
    }

    async fn put_login_portal(
        &self,
        command: LoginPortalMutation,
    ) -> Result<IdempotentOutcome<(LoginPortalRecord, LoginSettingsRecord)>, AuthorizationStateError>
    {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            match (
                load_login_portal(&transaction, &command.portal.portal_id)?,
                command.expected_version,
            ) {
                (None, None) if command.portal.version == 1 && command.settings.version == 1 => {
                    transaction
                        .execute(
                            "INSERT INTO auth_login_portals (portal_id, display_name, entry_url, builtin, disabled, removed, local_registration_enabled, provider_ids_json, created_at, updated_at, version)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                            params![
                                command.portal.portal_id,
                                command.portal.display_name,
                                command.portal.entry_url,
                                command.portal.builtin,
                                command.portal.disabled,
                                command.portal.removed,
                                command.portal.local_registration_enabled,
                                encode_json(&command.portal.provider_ids)?,
                                command.portal.created_at,
                                command.portal.updated_at,
                                to_sql_version(command.portal.version)?
                            ],
                        )
                        .map_err(map_write_error)?;
                    transaction
                        .execute(
                            "INSERT INTO auth_login_settings (portal_id, default_provider_id, local_login_enabled, federated_registration_enabled, provider_selection_enabled, updated_at, version)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                command.settings.portal_id,
                                command.settings.default_provider_id,
                                command.settings.local_login_enabled,
                                command.settings.federated_registration_enabled,
                                command.settings.provider_selection_enabled,
                                command.settings.updated_at,
                                to_sql_version(command.settings.version)?
                            ],
                        )
                        .map_err(map_write_error)?;
                }
                (Some((current, current_settings)), Some(expected))
                    if current.version == expected
                        && current_settings.version == expected
                        && current.builtin == command.portal.builtin
                        && current.created_at == command.portal.created_at
                        && command.portal.version == next_version(expected)?
                        && command.settings.version == command.portal.version =>
                {
                    let changed = transaction
                        .execute(
                            "UPDATE auth_login_portals SET display_name = ?1, entry_url = ?2, disabled = ?3, removed = ?4, local_registration_enabled = ?5, provider_ids_json = ?6, updated_at = ?7, version = ?8
                             WHERE portal_id = ?9 AND version = ?10 AND builtin = ?11 AND created_at = ?12",
                            params![
                                command.portal.display_name,
                                command.portal.entry_url,
                                command.portal.disabled,
                                command.portal.removed,
                                command.portal.local_registration_enabled,
                                encode_json(&command.portal.provider_ids)?,
                                command.portal.updated_at,
                                to_sql_version(command.portal.version)?,
                                command.portal.portal_id,
                                to_sql_version(expected)?,
                                command.portal.builtin,
                                command.portal.created_at
                            ],
                        )
                        .map_err(map_write_error)?;
                    let settings_changed = transaction
                        .execute(
                            "UPDATE auth_login_settings SET default_provider_id = ?1, local_login_enabled = ?2, federated_registration_enabled = ?3, provider_selection_enabled = ?4, updated_at = ?5, version = ?6
                             WHERE portal_id = ?7 AND version = ?8",
                            params![
                                command.settings.default_provider_id,
                                command.settings.local_login_enabled,
                                command.settings.federated_registration_enabled,
                                command.settings.provider_selection_enabled,
                                command.settings.updated_at,
                                to_sql_version(command.settings.version)?,
                                command.settings.portal_id,
                                to_sql_version(expected)?
                            ],
                        )
                        .map_err(map_write_error)?;
                    if changed != 1 || settings_changed != 1 {
                        return Err(AuthorizationStateError::StorageConflict);
                    }
                }
                _ => return Err(AuthorizationStateError::StorageConflict),
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied((command.portal, command.settings)))
        })
        .await
    }

    async fn put_portal_route(
        &self,
        command: PortalRouteMutation,
    ) -> Result<IdempotentOutcome<PortalRouteRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            if load_login_portal(&transaction, &command.route.portal_id)?.is_none() {
                return Err(AuthorizationStateError::InvalidRecord(
                    "portal route relationships do not exist".to_owned(),
                ));
            }
            if let Some(deployment_id) = &command.route.deployment_id {
                if load_deployment(&transaction, deployment_id)?.is_none() {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "portal route relationships do not exist".to_owned(),
                    ));
                }
            }
            let duplicate: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_portal_routes
                     WHERE route_id <> ?1 AND participant_id IS ?2 AND origin IS ?3
                       AND deployment_id IS ?4 AND priority = ?5)",
                    params![
                        command.route.route_id,
                        command.route.participant_id,
                        command.route.origin,
                        command.route.deployment_id,
                        command.route.priority
                    ],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if duplicate {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let current = load_portal_route(&transaction, &command.route.route_id)?;
            match (current, command.expected_version) {
                (None, None) if command.route.version == 1 => {
                    transaction
                        .execute(
                            "INSERT INTO auth_portal_routes (route_id, portal_id, participant_id, origin, deployment_id, priority, created_at, updated_at, version)
                              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                            params![
                                command.route.route_id,
                                command.route.portal_id,
                                command.route.participant_id,
                                command.route.origin,
                                command.route.deployment_id,
                                command.route.priority,
                                command.route.created_at,
                                command.route.updated_at,
                                to_sql_version(command.route.version)?
                            ],
                        )
                        .map_err(map_write_error)?;
                }
                (Some(current), Some(expected))
                    if current.version == expected
                        && current.created_at == command.route.created_at
                        && command.route.version == next_version(expected)? =>
                {
                    let changed = transaction
                        .execute(
                            "UPDATE auth_portal_routes SET portal_id = ?1, participant_id = ?2, origin = ?3, deployment_id = ?4, priority = ?5, updated_at = ?6, version = ?7
                              WHERE route_id = ?8 AND version = ?9",
                            params![
                                command.route.portal_id,
                                command.route.participant_id,
                                command.route.origin,
                                command.route.deployment_id,
                                command.route.priority,
                                command.route.updated_at,
                                to_sql_version(command.route.version)?,
                                command.route.route_id,
                                to_sql_version(expected)?
                            ],
                        )
                        .map_err(map_write_error)?;
                    if changed != 1 {
                        return Err(AuthorizationStateError::StorageConflict);
                    }
                }
                _ => return Err(AuthorizationStateError::StorageConflict),
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.route))
        })
        .await
    }

    async fn remove_portal_route(
        &self,
        command: PortalRouteRemoval,
    ) -> Result<IdempotentOutcome<PortalRouteRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            super::super::domain::require_nonempty("routeId", &command.route_id)?;
            super::super::domain::require_positive("expectedVersion", command.expected_version)?;
            let current = load_portal_route(&transaction, &command.route_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.version != command.expected_version {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let changed = transaction
                .execute(
                    "DELETE FROM auth_portal_routes WHERE route_id = ?1 AND version = ?2",
                    params![command.route_id, to_sql_version(command.expected_version)?],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(current))
        })
        .await
    }

    async fn list_portal_routes(&self) -> Result<Vec<PortalRouteRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT route_id, portal_id, participant_id, origin, deployment_id, priority, created_at, updated_at, version
                 FROM auth_portal_routes ORDER BY priority DESC, route_id",
                )
                .map_err(sql_error)?;
            let routes = statement
                .query_map([], decode_portal_route)
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(routes)
        })
        .await
    }

    async fn get_portal_grant_override(
        &self,
        portal_id: &str,
        participant_id: &str,
    ) -> Result<Option<super::super::PortalGrantOverrideRecord>, AuthorizationStateError> {
        SqliteAuthorizationStore::get_portal_grant_override(self, portal_id, participant_id).await
    }

    async fn list_capability_groups(
        &self,
    ) -> Result<Vec<super::super::CapabilityGroupRecord>, AuthorizationStateError> {
        SqliteAuthorizationStore::list_capability_groups(self).await
    }

    async fn list_portal_grant_bindings(
        &self,
    ) -> Result<Vec<super::super::PortalGrantBindingRecord>, AuthorizationStateError> {
        SqliteAuthorizationStore::list_portal_grant_bindings(self).await
    }
}

pub(in crate::platform::auth) fn insert_account_flow(
    connection: &Connection,
    flow: &AccountFlowRecord,
) -> Result<(), AuthorizationStateError> {
    connection
    .execute(
        "INSERT INTO auth_account_flows (flow_id, kind, token_hash, target_principal_id, target_provider_id, return_location, payload_json, state, created_at, expires_at, consumed_at, version)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            flow.flow_id,
            encode_enum(flow.kind)?,
            flow.token_hash,
            flow.target_principal_id,
            flow.target_provider_id,
            flow.return_location,
            encode_json(&flow.payload)?,
            encode_enum(flow.state)?,
            flow.created_at,
            flow.expires_at,
            flow.consumed_at,
            to_sql_version(flow.version)?
        ],
    )
    .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn sqlite_pending_account_flow(
    connection: &Connection,
    token_hash: &str,
    expected_version: u64,
    kind: super::super::AccountFlowKind,
    consumed_at: i64,
) -> Result<AccountFlowRecord, AuthorizationStateError> {
    super::super::domain::require_digest("tokenHash", token_hash)?;
    super::super::domain::require_protocol_timestamp("consumedAt", consumed_at)?;
    let flow = load_account_flow_by_hash(connection, token_hash)?
        .ok_or(AuthorizationStateError::StorageConflict)?;
    if flow.kind != kind
        || flow.version != expected_version
        || flow.state != AccountFlowState::Pending
        || consumed_at < flow.created_at
        || consumed_at >= flow.expires_at
    {
        return Err(AuthorizationStateError::StorageConflict);
    }
    Ok(flow)
}

pub(in crate::platform::auth) fn consume_sql_flow(
    connection: &Connection,
    flow: &AccountFlowRecord,
    consumed_at: i64,
) -> Result<AccountFlowRecord, AuthorizationStateError> {
    let next = next_version(flow.version)?;
    let changed = connection
        .execute(
            "UPDATE auth_account_flows SET state = 'consumed', consumed_at = ?1, version = ?2
     WHERE flow_id = ?3 AND version = ?4 AND state = 'pending' AND expires_at > ?1",
            params![
                consumed_at,
                to_sql_version(next)?,
                flow.flow_id,
                to_sql_version(flow.version)?
            ],
        )
        .map_err(map_write_error)?;
    if changed != 1 {
        return Err(AuthorizationStateError::StorageConflict);
    }
    load_account_flow_by_hash(connection, &flow.token_hash)?
        .ok_or(AuthorizationStateError::StorageConflict)
}

pub(in crate::platform::auth) fn insert_sql_user_account(
    connection: &Connection,
    principal: &PrincipalRecord,
    profile: &UserProfileRecord,
    credential: Option<&LocalCredentialRecord>,
    identity: Option<&ProviderIdentityLink>,
) -> Result<(), AuthorizationStateError> {
    insert_sql_principal(connection, principal)?;
    connection
    .execute(
        "INSERT INTO auth_user_profiles (principal_id, display_name, email, image_url, created_at, updated_at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            profile.principal_id,
            profile.display_name,
            profile.email,
            profile.image_url,
            profile.created_at,
            profile.updated_at,
            to_sql_version(profile.version)?
        ],
    )
    .map_err(map_write_error)?;
    if let Some(credential) = credential {
        connection
        .execute(
            "INSERT INTO auth_local_credentials (principal_id, normalized_username, password_hash, hash_profile, failed_attempts, locked_until, password_changed_at, updated_at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                credential.principal_id,
                credential.normalized_username,
                credential.password_hash,
                i64::from(credential.hash_profile),
                i64::from(credential.failed_attempts),
                credential.locked_until,
                credential.password_changed_at,
                credential.updated_at,
                to_sql_version(credential.version)?
            ],
        )
        .map_err(map_write_error)?;
    }
    if let Some(identity) = identity {
        connection
        .execute(
            "INSERT INTO auth_provider_identities (provider, provider_subject, principal_id, linked_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                identity.provider,
                identity.provider_subject,
                identity.principal_id,
                identity.linked_at,
                identity.last_seen_at
            ],
        )
        .map_err(map_write_error)?;
    }
    Ok(())
}

pub(in crate::platform::auth) fn decode_local_credential(
    row: &Row<'_>,
) -> rusqlite::Result<LocalCredentialRecord> {
    Ok(LocalCredentialRecord {
        principal_id: row.get(0)?,
        normalized_username: row.get(1)?,
        password_hash: row.get(2)?,
        hash_profile: from_sql_u32(row.get(3)?)?,
        failed_attempts: from_sql_u32(row.get(4)?)?,
        locked_until: row.get(5)?,
        password_changed_at: row.get(6)?,
        updated_at: row.get(7)?,
        version: from_sql_version(row.get(8)?)?,
    })
}

pub(in crate::platform::auth) fn load_user_profile(
    connection: &Connection,
    principal_id: &str,
) -> Result<Option<UserProfileRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT principal_id, display_name, email, image_url, created_at, updated_at, version FROM auth_user_profiles WHERE principal_id = ?1",
        [principal_id],
        |row| {
            Ok(UserProfileRecord {
                principal_id: row.get(0)?,
                display_name: row.get(1)?,
                email: row.get(2)?,
                image_url: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                version: from_sql_version(row.get(6)?)?,
            })
        },
    )
    .optional()
    .map_err(sql_error)
}

pub(in crate::platform::auth) fn load_user_account(
    connection: &Connection,
    principal_id: &str,
) -> Result<Option<(PrincipalRecord, UserProfileRecord)>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT p.principal_id, p.kind, p.state, p.created_at, p.updated_at,
                p.version, p.disabled_at, p.revoked_at,
                u.principal_id, u.display_name, u.email, u.image_url,
                u.created_at, u.updated_at, u.version
         FROM auth_principals p
         JOIN auth_user_profiles u ON u.principal_id = p.principal_id
         WHERE p.principal_id = ?1 AND p.kind = ?2",
            params![principal_id, encode_enum(PrincipalKind::User)?],
            decode_user_account,
        )
        .optional()
        .map_err(sql_error)
}

pub(in crate::platform::auth) fn decode_user_account(
    row: &Row<'_>,
) -> rusqlite::Result<(PrincipalRecord, UserProfileRecord)> {
    Ok((
        PrincipalRecord {
            principal_id: row.get(0)?,
            kind: decode_enum(row.get::<_, String>(1)?)?,
            state: decode_enum(row.get::<_, String>(2)?)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            version: from_sql_version(row.get(5)?)?,
            disabled_at: row.get(6)?,
            revoked_at: row.get(7)?,
        },
        UserProfileRecord {
            principal_id: row.get(8)?,
            display_name: row.get(9)?,
            email: row.get(10)?,
            image_url: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
            version: from_sql_version(row.get(14)?)?,
        },
    ))
}

pub(in crate::platform::auth) fn load_local_credential(
    connection: &Connection,
    principal_id: &str,
) -> Result<Option<LocalCredentialRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT principal_id, normalized_username, password_hash, hash_profile, failed_attempts, locked_until, password_changed_at, updated_at, version FROM auth_local_credentials WHERE principal_id = ?1",
        [principal_id],
        decode_local_credential,
    )
    .optional()
    .map_err(sql_error)
    .and_then(|credential| {
        credential.map_or(Ok(None), |credential| {
            validate_local_credential(&credential)?;
            Ok(Some(credential))
        })
    })
}

pub(in crate::platform::auth) fn load_login_portal(
    connection: &Connection,
    portal_id: &str,
) -> Result<Option<(LoginPortalRecord, LoginSettingsRecord)>, AuthorizationStateError> {
    let portal = connection
    .query_row(
        "SELECT portal_id, display_name, entry_url, builtin, disabled, removed, local_registration_enabled, provider_ids_json, created_at, updated_at, version FROM auth_login_portals WHERE portal_id = ?1",
        [portal_id],
        |row| {
            Ok(LoginPortalRecord {
                portal_id: row.get(0)?,
                display_name: row.get(1)?,
                entry_url: row.get(2)?,
                builtin: row.get(3)?,
                disabled: row.get(4)?,
                removed: row.get(5)?,
                local_registration_enabled: row.get(6)?,
                provider_ids: decode_json(row.get(7)?)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                version: from_sql_version(row.get(10)?)?,
            })
        },
    )
    .optional()
    .map_err(sql_error)?;
    let Some(portal) = portal else {
        return Ok(None);
    };
    let settings = connection
    .query_row(
        "SELECT portal_id, default_provider_id, local_login_enabled, federated_registration_enabled, provider_selection_enabled, updated_at, version FROM auth_login_settings WHERE portal_id = ?1",
        [portal_id],
        |row| {
            Ok(LoginSettingsRecord {
                portal_id: row.get(0)?,
                default_provider_id: row.get(1)?,
                local_login_enabled: row.get(2)?,
                federated_registration_enabled: row.get(3)?,
                provider_selection_enabled: row.get(4)?,
                updated_at: row.get(5)?,
                version: from_sql_version(row.get(6)?)?,
            })
        },
    )
    .optional()
    .map_err(sql_error)?
    .ok_or_else(|| AuthorizationStateError::Storage("login portal settings are missing".to_owned()))?;
    Ok(Some((portal, settings)))
}

pub(in crate::platform::auth) fn decode_portal_route(
    row: &Row<'_>,
) -> rusqlite::Result<PortalRouteRecord> {
    Ok(PortalRouteRecord {
        route_id: row.get(0)?,
        portal_id: row.get(1)?,
        participant_id: row.get(2)?,
        origin: row.get(3)?,
        deployment_id: row.get(4)?,
        priority: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        version: from_sql_version(row.get(8)?)?,
    })
}

pub(in crate::platform::auth) fn load_portal_route(
    connection: &Connection,
    route_id: &str,
) -> Result<Option<PortalRouteRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT route_id, portal_id, participant_id, origin, deployment_id, priority, created_at, updated_at, version FROM auth_portal_routes WHERE route_id = ?1",
        [route_id],
        decode_portal_route,
    )
    .optional()
    .map_err(sql_error)
}

pub(in crate::platform::auth) fn decode_account_flow(
    row: &Row<'_>,
) -> rusqlite::Result<AccountFlowRecord> {
    Ok(AccountFlowRecord {
        flow_id: row.get(0)?,
        kind: decode_enum(row.get(1)?)?,
        token_hash: row.get(2)?,
        target_principal_id: row.get(3)?,
        target_provider_id: row.get(4)?,
        return_location: row.get(5)?,
        payload: decode_json(row.get(6)?)?,
        state: decode_enum(row.get(7)?)?,
        created_at: row.get(8)?,
        expires_at: row.get(9)?,
        consumed_at: row.get(10)?,
        version: from_sql_version(row.get(11)?)?,
    })
}

pub(in crate::platform::auth) fn load_account_flow_by_hash(
    connection: &Connection,
    token_hash: &str,
) -> Result<Option<AccountFlowRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT flow_id, kind, token_hash, target_principal_id, target_provider_id, return_location, payload_json, state, created_at, expires_at, consumed_at, version FROM auth_account_flows WHERE token_hash = ?1",
        [token_hash],
        decode_account_flow,
    )
    .optional()
    .map_err(sql_error)
}

#[cfg(test)]
mod password_reset_action_tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::platform::auth::application::repository::{SessionCreation, SessionRepository};
    use crate::platform::auth::domain::{NewSession, SessionRecord};
    use crate::platform::auth::{builtins, AccountFlowKind, IdempotencyResultRecord};
    use trellis_protocol::ParticipantKind;

    const NOW: i64 = 1_700_000_000_000;

    fn proof(scope: &str, purpose: &str, request: &str, now: i64) -> IdempotencyResultRecord {
        IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!([scope])).unwrap(),
            purpose: purpose.to_owned(),
            signer_id: "test".to_owned(),
            request_id: request.to_owned(),
            request_digest: trellis_protocol::digest_json(&json!({ "request": request })).unwrap(),
            result: json!(null),
            created_at: now,
            expires_at: now + 900_000,
        }
    }

    async fn seed_user(store: &SqliteAuthorizationStore, principal_id: &str, username: &str) {
        let credential = crate::platform::auth::account::bound_local_credential(
            principal_id.to_owned(),
            username,
            NOW,
        )
        .unwrap();
        let identity = ProviderIdentityLink {
            provider: "local".to_owned(),
            provider_subject: username.to_owned(),
            principal_id: principal_id.to_owned(),
            linked_at: NOW,
            last_seen_at: NOW,
        };
        store
            .create_user_account(AccountCreation {
                principal: PrincipalRecord {
                    principal_id: principal_id.to_owned(),
                    kind: PrincipalKind::User,
                    state: PrincipalState::Active,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                    disabled_at: None,
                    revoked_at: None,
                },
                profile: UserProfileRecord {
                    principal_id: principal_id.to_owned(),
                    display_name: None,
                    email: None,
                    image_url: None,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                },
                credential: Some(credential),
                identity: Some(identity),
                idempotency: proof(
                    &format!("seed.account.{principal_id}"),
                    "auth.account.create",
                    principal_id,
                    NOW,
                ),
                actions: Vec::new(),
            })
            .await
            .unwrap();
    }

    async fn seed_session(
        store: &SqliteAuthorizationStore,
        principal_id: &str,
        participant_id: &str,
        participant_kind: ParticipantKind,
        index: usize,
    ) -> String {
        let (_, public_key) = trellis_rs::auth::generate_session_keypair();
        let session_id = ulid::Ulid::new().to_string();
        let session = SessionRecord::from_new(NewSession {
            session_id: session_id.clone(),
            principal_id: principal_id.to_owned(),
            participant_id: participant_id.to_owned(),
            participant_kind,
            session_public_key: public_key,
            created_at: NOW,
            expires_at: Some(NOW + 3_600_000),
        })
        .unwrap();
        let result = store
            .create_session(SessionCreation {
                session,
                idempotency: proof(
                    &format!("seed.session.{index}"),
                    "auth.session.create",
                    &session_id,
                    NOW + index as i64,
                ),
                actions: Vec::new(),
            })
            .await;
        result.unwrap();
        session_id
    }

    #[tokio::test]
    async fn password_reset_emits_one_event_per_session_and_replays_without_duplicates() {
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let principal_id = "usr_reset_target";
        seed_user(&store, principal_id, "reset-target").await;
        let mut session_ids = Vec::new();
        let mut session_participants = BTreeMap::new();
        for (index, binding) in [
            builtins::cli_participant_binding(NOW).unwrap(),
            builtins::console_participant_binding(NOW).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            let participant_id = binding.participant_id.clone();
            let participant_kind = binding.participant_kind;
            store.put_participant_binding(binding).await.unwrap();
            let session_id = seed_session(
                &store,
                principal_id,
                &participant_id,
                participant_kind,
                index + 1,
            )
            .await;
            session_participants.insert(session_id.clone(), participant_id);
            session_ids.push(session_id);
        }
        // A different principal's live session must not be touched by the reset.
        seed_user(&store, "usr_reset_other", "reset-other").await;
        let (shared_participant_id, shared_participant_kind) = session_participants
            .values()
            .next()
            .and_then(|participant| {
                session_participants
                    .iter()
                    .find(|(_, value)| *value == participant)
                    .map(|_| participant)
            })
            .map(|participant| {
                let kind = [builtins::cli_participant_binding(NOW).unwrap()]
                    .into_iter()
                    .find(|binding| binding.participant_id == *participant)
                    .expect("seeded participant is a builtin")
                    .participant_kind;
                (participant.clone(), kind)
            })
            .expect("at least one seeded session");
        let unrelated_session = seed_session(
            &store,
            "usr_reset_other",
            &shared_participant_id,
            shared_participant_kind,
            9,
        )
        .await;

        let token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let token_hash = crate::platform::auth::application::bearer_secret_digest(token).unwrap();
        store
            .create_account_flow(AccountFlowCreation {
                flow: AccountFlowRecord {
                    flow_id: "flow_reset".to_owned(),
                    kind: AccountFlowKind::PasswordReset,
                    token_hash: token_hash.clone(),
                    target_principal_id: Some(principal_id.to_owned()),
                    target_provider_id: None,
                    return_location: None,
                    payload: json!({ "requestedByPrincipalId": "usr_requester" }),
                    state: AccountFlowState::Pending,
                    created_at: NOW,
                    expires_at: NOW + 300_000,
                    consumed_at: None,
                    version: 1,
                },
                idempotency: proof("seed.flow", "auth.account.flow.create", "flow_reset", NOW),
                actions: Vec::new(),
            })
            .await
            .unwrap();

        let (hash, profile) =
            crate::platform::auth::account::hash_password("reset-password-1", None).unwrap();
        let replacement = LocalCredentialRecord {
            principal_id: principal_id.to_owned(),
            normalized_username: "reset-target".to_owned(),
            password_hash: hash,
            hash_profile: profile,
            failed_attempts: 0,
            locked_until: None,
            password_changed_at: NOW + 1_000,
            updated_at: NOW + 1_000,
            version: 2,
        };
        let completion = |idempotency: IdempotencyResultRecord| PasswordResetCompletion {
            token_hash: token_hash.clone(),
            expected_flow_version: 1,
            flow_kind: AccountFlowKind::PasswordReset,
            requester_principal_id: Some("usr_requester".to_owned()),
            admin_target: false,
            expected_credential_version: Some(1),
            replacement: replacement.clone(),
            identity: None,
            bindings: Vec::new(),
            profile: None,
            consumed_at: NOW + 1_000,
            idempotency,
            actions: Vec::new(),
        };

        let outcome = store
            .complete_password_reset(completion(proof(
                "reset.complete",
                "account.password.reset",
                token,
                NOW + 1_000,
            )))
            .await
            .unwrap();
        assert!(matches!(outcome, IdempotentOutcome::Applied(_)));

        let session_principal = principal_id.to_owned();
        let (session_states, actions) = store
            .run_read(move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT state FROM auth_sessions WHERE principal_id = ?1 ORDER BY session_id",
                    )
                    .map_err(sql_error)?;
                let states = statement
                    .query_map(params![session_principal], |row| row.get::<_, String>(0))
                    .map_err(sql_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sql_error)?;
                let mut actions_statement = connection
                    .prepare(
                        "SELECT kind, payload_json FROM auth_post_commit_actions
                         ORDER BY created_at, action_id",
                    )
                    .map_err(sql_error)?;
                let actions = actions_statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(sql_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sql_error)?;
                Ok((states, actions))
            })
            .await
            .unwrap();
        assert_eq!(
            session_states,
            vec!["revoked".to_owned(), "revoked".to_owned()]
        );

        let events = actions
            .iter()
            .filter(|(kind, _)| kind == "event")
            .collect::<Vec<_>>();
        let kicks = actions
            .iter()
            .filter(|(kind, _)| kind == "kick")
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 2, "one revoked event per session");
        assert_eq!(kicks.len(), 1, "one principal kick");
        let mut event_sessions = Vec::new();
        for (_, payload) in &events {
            let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
            assert_eq!(payload["eventType"], "Auth.Sessions.Revoked");
            assert_eq!(payload["reason"], "password_reset");
            assert_eq!(payload["revokedBy"], "usr_requester");
            assert!(payload["eventSubject"]
                .as_str()
                .is_some_and(|subject| !subject.is_empty()));
            // uint64 event fields travel as decimal strings on the wire.
            assert_eq!(payload["occurredAt"], (NOW + 1_000).to_string());
            let decoded = serde_json::from_value::<
                trellis_runtime_apis::apis::trellis_auth_v1::events::SessionsRevokedEvent,
            >(payload.clone())
            .expect("the generated Auth.Sessions.Revoked decoder accepts the payload");
            let session_id = decoded.session_id.as_str().to_owned();
            assert_eq!(decoded.principal_id.as_str(), principal_id);
            assert_eq!(
                decoded.participant_id.as_str(),
                session_participants
                    .get(&session_id)
                    .expect("event session was seeded")
                    .as_str()
            );
            assert_eq!(u64::from(decoded.occurred_at.0), (NOW + 1_000) as u64);
            event_sessions.push(session_id);
        }
        event_sessions.sort();
        session_ids.sort();
        assert_eq!(event_sessions, session_ids);

        // The unrelated principal's session stays active.
        let unrelated_state = store
            .run_read(move |connection| {
                connection
                    .query_row(
                        "SELECT state FROM auth_sessions WHERE session_id = ?1",
                        params![unrelated_session],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(sql_error)
            })
            .await
            .unwrap();
        assert_eq!(unrelated_state, "active");
        let kick_payload: serde_json::Value = serde_json::from_str(&kicks[0].1).unwrap();
        assert_eq!(kick_payload["principalId"], principal_id);

        let replay = store
            .complete_password_reset(completion(proof(
                "reset.complete",
                "account.password.reset",
                token,
                NOW + 1_000,
            )))
            .await
            .unwrap();
        assert!(matches!(replay, IdempotentOutcome::Replayed(_)));
        let action_count = store
            .run_read(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM auth_post_commit_actions", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(sql_error)
            })
            .await
            .unwrap();
        assert_eq!(action_count, 3, "replay commits no duplicate events");
    }
}
