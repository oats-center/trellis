use serde_json::{json, Value};
use ulid::Ulid;

use super::super::account::{hash_password, normalize_username, verify_password};
use super::super::*;
use super::bearer_secret_digest;

/// Uniform result of local username/password authentication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalAuthentication {
    /// Credentials and current account lifecycle are valid.
    Authenticated {
        /// Stable active user principal.
        principal: PrincipalRecord,
    },
    /// Credentials or current account lifecycle are not eligible.
    Denied,
}

/// Administrator input for a user account, optionally bound to a local login.
#[derive(Clone, Debug)]
pub struct CreateUserInput {
    /// Required-nullable display name.
    pub name: Option<String>,
    /// Required-nullable email address.
    pub email: Option<String>,
    /// Required-nullable image URL.
    pub image: Option<String>,
    /// Optional bound local identity username, created without a password.
    pub username: Option<String>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Durable request proof and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Browser input for atomic local-identity registration.
#[derive(Clone, Debug)]
pub struct CreateLocalUserInput {
    /// Canonicalizable local username.
    pub username: String,
    /// Plaintext password retained only for this call.
    pub password: String,
    /// Required-nullable display name.
    pub name: Option<String>,
    /// Required-nullable email address.
    pub email: Option<String>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Durable request proof and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Input for atomically completing a local password account flow.
#[derive(Clone, Debug)]
pub struct CompletePasswordResetInput {
    /// Plaintext account-flow token retained only for this call.
    pub token: String,
    /// Expected durable account-flow version.
    pub expected_flow_version: u64,
    /// Username required only when installing the first local credential.
    pub username: Option<String>,
    /// Canonical participant bindings restored by an admin-account flow.
    pub bindings: Vec<GrantBindingReplacement>,
    /// Optional profile replacement committed by administrator recovery.
    pub profile: Option<UserProfileRecord>,
    /// Plaintext password retained only for this call.
    pub password: String,
    /// Completion time in Unix milliseconds.
    pub consumed_at: i64,
    /// Durable request proof and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Input for an authenticated password change and sibling-session revocation.
#[derive(Clone, Debug)]
pub struct ChangePasswordInput {
    /// User principal owning the credential.
    pub principal_id: String,
    /// Session that remains active after the password change.
    pub current_session_id: String,
    /// Plaintext current password retained only for this call.
    pub current_password: String,
    /// Plaintext replacement password retained only for this call.
    pub new_password: String,
    /// Password-change time in Unix milliseconds.
    pub changed_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// OIDC-authenticated user registration input.
#[derive(Clone, Debug)]
pub struct CreateFederatedUserInput {
    /// Stable provider ID.
    pub provider: String,
    /// Stable provider subject.
    pub provider_subject: String,
    /// Required-nullable display name.
    pub name: Option<String>,
    /// Required-nullable email address.
    pub email: Option<String>,
    /// Required-nullable image URL.
    pub image: Option<String>,
    /// Registration time in Unix milliseconds.
    pub created_at: i64,
    /// Durable callback claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Administrator input for an optimistic user-account replacement.
#[derive(Clone, Debug)]
pub struct UpdateUserInput {
    /// Verified caller state to recheck in the write transaction.
    pub(crate) actor: MutationActor,
    /// Stable user principal ID.
    pub principal_id: String,
    /// Expected principal and profile version.
    pub expected_version: u64,
    /// Required-nullable display name.
    pub name: Option<String>,
    /// Required-nullable email address.
    pub email: Option<String>,
    /// Required-nullable image URL.
    pub image: Option<String>,
    /// Requested active, disabled, or revoked lifecycle state.
    pub state: PrincipalState,
    /// Update time in Unix milliseconds.
    pub updated_at: i64,
    /// Durable request proof and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// User principal joined with its non-authority profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserAccount {
    /// Durable user principal.
    pub principal: PrincipalRecord,
    /// Required user profile.
    pub profile: UserProfileRecord,
}

impl<R> AuthService<R>
where
    R: AccountRepository + Clone,
{
    pub(crate) async fn change_password(
        &self,
        input: ChangePasswordInput,
    ) -> Result<IdempotentOutcome<usize>, AuthorizationStateError> {
        let ChangePasswordInput {
            principal_id,
            current_session_id,
            current_password,
            new_password,
            changed_at,
            idempotency,
            actions,
        } = input;
        super::validation::validate_idempotency_and_actions(&idempotency, &actions)?;
        super::super::domain::require_protocol_timestamp("changedAt", changed_at)?;
        let credential = self
            .repository
            .get_local_credential(&principal_id)
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("local credential not found".to_owned())
            })?;
        if !verify_password(&credential.password_hash, &current_password) {
            return Err(AuthorizationStateError::InvalidRecord(
                "current password is invalid".to_owned(),
            ));
        }
        if verify_password(&credential.password_hash, &new_password) {
            return Err(AuthorizationStateError::InvalidRecord(
                "new password must differ from current password".to_owned(),
            ));
        }
        let (password_hash, hash_profile) =
            hash_password(&new_password, Some(self.config.password_min_length))?;
        let replacement = LocalCredentialRecord {
            password_hash,
            hash_profile,
            failed_attempts: 0,
            locked_until: None,
            password_changed_at: changed_at,
            updated_at: changed_at,
            version: credential.version.checked_add(1).ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("credential version overflow".to_owned())
            })?,
            ..credential.clone()
        };
        super::validation::validate_replacement_credential(
            &credential,
            &replacement,
            &principal_id,
        )?;
        self.repository
            .change_password(PasswordChange {
                principal_id,
                current_session_id,
                credential: replacement,
                expected_version: credential.version,
                changed_at,
                idempotency,
                actions,
            })
            .await
    }
}

impl<R> AuthService<R>
where
    R: AccountRepository + Clone,
{
    /// Authenticate a local account with uniform caller-visible denial.
    ///
    /// # Errors
    ///
    /// Returns a repository error when credential lockout state cannot be read
    /// or committed.
    pub(crate) async fn authenticate_local(
        &self,
        username: &str,
        password: &str,
        now: i64,
    ) -> Result<LocalAuthentication, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("now", now)?;
        let Ok(username) = normalize_username(username) else {
            let _ = verify_password(&self.dummy_password_hash, password);
            return Ok(LocalAuthentication::Denied);
        };
        let credential = self
            .repository
            .get_local_credential_by_username(&username)
            .await?;
        let verified = verify_password(
            credential
                .as_ref()
                .map_or(&self.dummy_password_hash, |value| {
                    value.password_hash.as_str()
                }),
            password,
        );
        let Some(credential) = credential else {
            return Ok(LocalAuthentication::Denied);
        };
        if credential.locked_until.is_some_and(|until| until > now) {
            return Ok(LocalAuthentication::Denied);
        }
        let attempt = LocalLoginAttempt {
            principal_id: credential.principal_id.clone(),
            expected_version: credential.version,
            succeeded: verified,
            attempted_at: now,
            maximum_failures: self.config.maximum_login_failures,
            lock_duration_ms: self.config.login_lock_duration_ms,
        };
        super::validation::validate_local_login_attempt(&attempt)?;
        self.repository.record_local_login_attempt(attempt).await?;
        if !verified {
            return Ok(LocalAuthentication::Denied);
        }
        let Some(principal) = self
            .repository
            .get_principal(&credential.principal_id)
            .await?
        else {
            return Ok(LocalAuthentication::Denied);
        };
        let Some(_) = self
            .repository
            .get_user_profile(&credential.principal_id)
            .await?
        else {
            return Ok(LocalAuthentication::Denied);
        };
        if principal.kind != PrincipalKind::User || principal.state != PrincipalState::Active {
            return Ok(LocalAuthentication::Denied);
        }
        Ok(LocalAuthentication::Authenticated { principal })
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::platform::auth::account::hash_password;

    const NOW: i64 = 1_700_000_000_000;

    #[tokio::test]
    async fn sqlite_local_authentication_is_uniform_and_locks() {
        exercise_local_authentication(SqliteAuthorizationStore::open_in_memory().unwrap())
            .await
            .unwrap();
    }

    async fn exercise_local_authentication<R>(
        repository: R,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        R: AccountRepository + Clone,
    {
        let (password_hash, hash_profile) = hash_password("password1", Some(8))?;
        repository
            .create_user_account(AccountCreation {
                principal: PrincipalRecord {
                    principal_id: "usr_login".to_owned(),
                    kind: PrincipalKind::User,
                    state: PrincipalState::Active,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                    disabled_at: None,
                    revoked_at: None,
                },
                profile: UserProfileRecord {
                    principal_id: "usr_login".to_owned(),
                    display_name: Some("Login User".to_owned()),
                    email: None,
                    image_url: None,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                },
                credential: Some(LocalCredentialRecord {
                    principal_id: "usr_login".to_owned(),
                    normalized_username: "login".to_owned(),
                    password_hash,
                    hash_profile,
                    failed_attempts: 0,
                    locked_until: None,
                    password_changed_at: NOW,
                    updated_at: NOW,
                    version: 1,
                }),
                identity: Some(ProviderIdentityLink {
                    provider: "local".to_owned(),
                    provider_subject: "login".to_owned(),
                    principal_id: "usr_login".to_owned(),
                    linked_at: NOW,
                    last_seen_at: NOW,
                }),
                idempotency: IdempotencyResultRecord {
                    scope_key: "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE".to_owned(),
                    purpose: "account.create".to_owned(),
                    signer_id: "test".to_owned(),
                    request_id: "create-login-user".to_owned(),
                    request_digest: "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI".to_owned(),
                    result: serde_json::json!({ "principalId": "usr_login" }),
                    created_at: NOW,
                    expires_at: NOW + 1_000,
                },
                actions: Vec::new(),
            })
            .await?;
        let service = AuthService::new(
            repository.clone(),
            AuthServiceConfig {
                password_min_length: 8,
                maximum_login_failures: 2,
                login_lock_duration_ms: 100,
                ..AuthServiceConfig::default()
            },
        )?;
        let target = FirstAdminAuthorityTarget {
            participant_id: crate::platform::auth::builtins::CLI_PARTICIPANT_ID.to_owned(),
            installed_revision: 1,
        };

        assert!(service
            .ensure_admin_account_flow("not a URL", std::slice::from_ref(&target), NOW)
            .await
            .is_err());
        let first = service
            .ensure_admin_account_flow(
                "https://auth.example/base",
                std::slice::from_ref(&target),
                NOW,
            )
            .await?
            .ok_or("first-admin flow missing")?;
        let first_record = repository
            .get_account_flow_by_hash(&first.flow_id_hash)
            .await?
            .ok_or("first-admin record missing")?;
        assert_eq!(first_record.state, AccountFlowState::Pending);
        let bootstrap_url = url::Url::parse(
            first
                .bootstrap_url
                .as_deref()
                .ok_or("first-admin URL missing")?,
        )?;
        assert_eq!(bootstrap_url.path(), "/console/profile");
        assert!(bootstrap_url
            .query_pairs()
            .any(|(key, value)| key == "adminAccountToken" && !value.is_empty()));
        assert!(!bootstrap_url.as_str().contains(&first.flow_id_hash));
        let second = service
            .ensure_admin_account_flow(
                "https://auth.example/base",
                std::slice::from_ref(&target),
                NOW + 1,
            )
            .await?
            .ok_or("pending first-admin flow missing")?;
        assert_eq!(second.flow_id_hash, first.flow_id_hash);
        assert!(second.bootstrap_url.is_none());
        assert_eq!(
            repository
                .get_account_flow_by_hash(&first.flow_id_hash)
                .await?
                .ok_or("old first-admin record missing")?
                .state,
            AccountFlowState::Pending
        );
        let replacement = service
            .rotate_admin_account_flow(
                "https://auth.example/base",
                std::slice::from_ref(&target),
                NOW + 2,
            )
            .await?
            .ok_or("first-admin flow was not rotated")?;
        assert_ne!(replacement.flow_id_hash, first.flow_id_hash);
        assert!(replacement.bootstrap_url.is_some());
        assert_eq!(
            repository
                .get_account_flow_by_hash(&first.flow_id_hash)
                .await?
                .ok_or("rotated first-admin record missing")?
                .state,
            AccountFlowState::Revoked
        );
        let after_expiry = service
            .ensure_admin_account_flow(
                "https://auth.example/base",
                std::slice::from_ref(&target),
                replacement.expires_at + 1,
            )
            .await?
            .ok_or("expired first-admin flow was not replaced")?;
        assert_ne!(after_expiry.flow_id_hash, replacement.flow_id_hash);
        assert_eq!(
            repository
                .get_account_flow_by_hash(&replacement.flow_id_hash)
                .await?
                .ok_or("expired first-admin record missing")?
                .state,
            AccountFlowState::Expired
        );

        assert_eq!(
            service
                .authenticate_local("missing", "password1", NOW)
                .await?,
            LocalAuthentication::Denied
        );
        assert_eq!(
            service
                .authenticate_local("LOGIN", "wrong", NOW + 1)
                .await?,
            LocalAuthentication::Denied
        );
        assert_eq!(
            service
                .authenticate_local("login", "wrong", NOW + 2)
                .await?,
            LocalAuthentication::Denied
        );
        let locked = repository
            .get_local_credential("usr_login")
            .await?
            .ok_or("credential missing")?;
        assert_eq!(locked.failed_attempts, 2);
        assert_eq!(locked.locked_until, Some(NOW + 102));
        assert_eq!(
            service
                .authenticate_local("login", "password1", NOW + 3)
                .await?,
            LocalAuthentication::Denied
        );
        assert_eq!(
            repository
                .get_local_credential("usr_login")
                .await?
                .ok_or("credential missing")?
                .version,
            locked.version
        );
        assert!(matches!(
            service
                .authenticate_local("login", "password1", NOW + 102)
                .await?,
            LocalAuthentication::Authenticated { .. }
        ));
        let reset = repository
            .get_local_credential("usr_login")
            .await?
            .ok_or("credential missing")?;
        assert_eq!(reset.failed_attempts, 0);
        assert_eq!(reset.locked_until, None);
        Ok(())
    }
}

impl<R> AuthService<R>
where
    R: AccountRepository + Clone,
{
    /// Replace a local password and consume its durable account flow atomically.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for an unsafe password, or a repository
    /// conflict when the flow or credential changed concurrently.
    pub(crate) async fn complete_password_reset(
        &self,
        input: CompletePasswordResetInput,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError> {
        let CompletePasswordResetInput {
            token,
            expected_flow_version,
            username,
            bindings,
            profile,
            password,
            consumed_at,
            mut idempotency,
            actions,
        } = input;
        super::validation::validate_idempotency_and_actions(&idempotency, &actions)?;
        super::super::domain::require_protocol_timestamp("consumedAt", consumed_at)?;
        let username = username.filter(|username| !username.trim().is_empty());
        let token_hash = bearer_secret_digest(&token)?;
        let flow = self
            .repository
            .get_account_flow_by_hash(&token_hash)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        let principal_id = flow
            .target_principal_id
            .as_deref()
            .ok_or(AuthorizationStateError::StorageConflict)?;
        let current = self.repository.get_local_credential(principal_id).await?;
        if current
            .as_ref()
            .is_some_and(|current| verify_password(&current.password_hash, &password))
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "new password must differ from current password".to_owned(),
            ));
        }
        let (password_hash, hash_profile) =
            hash_password(&password, Some(self.config.password_min_length))?;
        let replacement = LocalCredentialRecord {
            principal_id: principal_id.to_owned(),
            normalized_username: match username {
                Some(username) => normalize_username(&username)?,
                None => current
                    .as_ref()
                    .map(|current| current.normalized_username.clone())
                    .ok_or_else(|| {
                        AuthorizationStateError::InvalidRecord(
                            "username is required when installing a local credential".to_owned(),
                        )
                    })?,
            },
            password_hash,
            hash_profile,
            failed_attempts: 0,
            locked_until: None,
            password_changed_at: consumed_at,
            updated_at: consumed_at,
            version: match &current {
                Some(current) => current.version.checked_add(1).ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord("credential version overflow".to_owned())
                })?,
                None => 1,
            },
        };
        super::validation::validate_local_credential(&replacement)?;
        if flow.kind != AccountFlowKind::AdminAccount {
            if let Some(current) = &current {
                super::validation::validate_replacement_credential(
                    current,
                    &replacement,
                    principal_id,
                )?;
            }
        }
        let existing_identity = match &current {
            Some(current) => {
                self.repository
                    .get_provider_identity("local", &current.normalized_username)
                    .await?
            }
            None => None,
        };
        let identity = Some(ProviderIdentityLink {
            provider: "local".to_owned(),
            provider_subject: replacement.normalized_username.clone(),
            principal_id: principal_id.to_owned(),
            linked_at: existing_identity
                .as_ref()
                .map_or(consumed_at, |identity| identity.linked_at),
            last_seen_at: consumed_at,
        });
        if let Some(identity) = &identity {
            super::super::authority::validate_provider_identity(identity)?;
        }
        idempotency.result = json!({ "principalId": principal_id, "completed": true });
        self.repository
            .complete_password_reset(PasswordResetCompletion {
                token_hash,
                expected_flow_version,
                flow_kind: flow.kind,
                requester_principal_id: flow
                    .payload
                    .get("requestedByPrincipalId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                admin_target: flow
                    .payload
                    .get("adminTarget")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                expected_credential_version: current.as_ref().map(|current| current.version),
                replacement,
                identity,
                bindings,
                profile,
                consumed_at,
                idempotency,
                actions,
            })
            .await
    }
}

impl<R> AuthService<R>
where
    R: AccountRepository + Clone,
{
    /// Create one local user, profile, credential, and identity atomically.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for unsafe credentials or malformed
    /// profile data, and a conflict when the normalized username already exists.
    pub(crate) async fn create_local_user(
        &self,
        mut input: CreateLocalUserInput,
    ) -> Result<IdempotentOutcome<UserAccount>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("createdAt", input.created_at)?;
        let username = normalize_username(&input.username)?;
        let (password_hash, hash_profile) =
            hash_password(&input.password, Some(self.config.password_min_length))?;
        let principal_id = format!("usr_{}", Ulid::new());
        let principal = PrincipalRecord {
            principal_id: principal_id.clone(),
            kind: PrincipalKind::User,
            state: PrincipalState::Active,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let profile = UserProfileRecord {
            principal_id: principal_id.clone(),
            display_name: input.name,
            email: input.email,
            image_url: None,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
        };
        let credential = LocalCredentialRecord {
            principal_id: principal_id.clone(),
            normalized_username: username.clone(),
            password_hash,
            hash_profile,
            failed_attempts: 0,
            locked_until: None,
            password_changed_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
        };
        let identity = ProviderIdentityLink {
            provider: "local".to_owned(),
            provider_subject: username,
            principal_id: principal_id.clone(),
            linked_at: input.created_at,
            last_seen_at: input.created_at,
        };
        super::validation::validate_new_user_account(
            &principal,
            &profile,
            Some(&credential),
            Some(&identity),
        )?;
        input.idempotency.result = json!({ "principalId": principal_id });
        match self
            .repository
            .create_user_account(AccountCreation {
                principal: principal.clone(),
                profile: profile.clone(),
                credential: Some(credential),
                identity: Some(identity),
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?
        {
            IdempotentOutcome::Applied(_) => Ok(IdempotentOutcome::Applied(UserAccount {
                principal,
                profile,
            })),
            IdempotentOutcome::Replayed(value) => Ok(IdempotentOutcome::Replayed(value)),
        }
    }

    /// Create one credential-less user account through the aggregate path.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed profile data or timestamps
    /// and a conflict for duplicate durable request or principal identity.
    pub(crate) async fn create_user(
        &self,
        mut input: CreateUserInput,
    ) -> Result<IdempotentOutcome<UserAccount>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("createdAt", input.created_at)?;
        let principal_id = format!("usr_{}", Ulid::new());
        let principal = PrincipalRecord {
            principal_id: principal_id.clone(),
            kind: PrincipalKind::User,
            state: PrincipalState::Active,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let profile = UserProfileRecord {
            principal_id: principal_id.clone(),
            display_name: input.name,
            email: input.email,
            image_url: input.image,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
        };
        let (credential, identity) = match input.username.as_deref() {
            Some(username) => {
                let credential = super::super::account::bound_local_credential(
                    principal_id.clone(),
                    username,
                    input.created_at,
                )?;
                let identity = super::super::ProviderIdentityLink {
                    provider: "local".to_owned(),
                    provider_subject: credential.normalized_username.clone(),
                    principal_id: principal_id.clone(),
                    linked_at: input.created_at,
                    last_seen_at: input.created_at,
                };
                (Some(credential), Some(identity))
            }
            None => (None, None),
        };
        super::validation::validate_new_user_account(
            &principal,
            &profile,
            credential.as_ref(),
            identity.as_ref(),
        )?;
        input.idempotency.result = json!({ "principalId": principal_id });
        match self
            .repository
            .create_user_account(AccountCreation {
                principal: principal.clone(),
                profile: profile.clone(),
                credential,
                identity,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?
        {
            IdempotentOutcome::Applied(_) => Ok(IdempotentOutcome::Applied(UserAccount {
                principal,
                profile,
            })),
            IdempotentOutcome::Replayed(value) => Ok(IdempotentOutcome::Replayed(value)),
        }
    }

    /// Create one user and immutable federated provider link atomically.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed profile or provider data,
    /// and a conflict when the provider identity is already assigned.
    pub(crate) async fn create_federated_user(
        &self,
        mut input: CreateFederatedUserInput,
    ) -> Result<IdempotentOutcome<UserAccount>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("createdAt", input.created_at)?;
        let principal_id = format!("usr_{}", Ulid::new());
        let principal = PrincipalRecord {
            principal_id: principal_id.clone(),
            kind: PrincipalKind::User,
            state: PrincipalState::Active,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let profile = UserProfileRecord {
            principal_id: principal_id.clone(),
            display_name: input.name,
            email: input.email,
            image_url: input.image,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
        };
        let identity = ProviderIdentityLink {
            provider: input.provider,
            provider_subject: input.provider_subject,
            principal_id: principal_id.clone(),
            linked_at: input.created_at,
            last_seen_at: input.created_at,
        };
        super::validation::validate_new_user_account(&principal, &profile, None, Some(&identity))?;
        input.idempotency.result = json!({ "principalId": principal_id });
        match self
            .repository
            .create_user_account(AccountCreation {
                principal: principal.clone(),
                profile: profile.clone(),
                credential: None,
                identity: Some(identity),
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?
        {
            IdempotentOutcome::Applied(_) => Ok(IdempotentOutcome::Applied(UserAccount {
                principal,
                profile,
            })),
            IdempotentOutcome::Replayed(value) => Ok(IdempotentOutcome::Replayed(value)),
        }
    }

    /// Load one user account by stable principal ID.
    ///
    /// # Errors
    ///
    /// Returns a repository error when the coherent principal/profile read fails.
    pub(crate) async fn user(
        &self,
        principal_id: &str,
    ) -> Result<Option<UserAccount>, AuthorizationStateError> {
        Ok(self
            .repository
            .get_user_account(principal_id)
            .await?
            .map(|(principal, profile)| UserAccount { principal, profile }))
    }

    /// List filtered user accounts after an exclusive stable-sort cursor.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for an unsafe limit and a repository
    /// error when the coherent page cannot be read.
    pub(crate) async fn users(
        &self,
        cursor: Option<&(i64, String)>,
        state: Option<&str>,
        search: Option<&str>,
        limit: usize,
    ) -> Result<Vec<UserAccount>, AuthorizationStateError> {
        super::validation::validate_account_list(cursor, limit)?;
        Ok(self
            .repository
            .list_user_accounts(cursor, state, search, limit)
            .await?
            .into_iter()
            .map(|(principal, profile)| UserAccount { principal, profile })
            .collect())
    }

    /// Atomically replace a user lifecycle and profile.
    ///
    /// # Errors
    ///
    /// Returns a conflict when the expected version changed and an
    /// invalid-record error for already revoked or malformed replacement input.
    pub(crate) async fn update_user(
        &self,
        mut input: UpdateUserInput,
    ) -> Result<IdempotentOutcome<UserAccount>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("updatedAt", input.updated_at)?;
        let (current_principal, current_profile) = self
            .repository
            .get_user_account(&input.principal_id)
            .await?
            .ok_or(AuthorizationStateError::PrincipalMissing)?;
        if current_principal.version != input.expected_version
            || current_profile.version != input.expected_version
        {
            return Err(AuthorizationStateError::StorageConflict);
        }
        let version = input.expected_version.checked_add(1).ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("user version overflow".to_owned())
        })?;
        let principal = PrincipalRecord {
            state: input.state,
            updated_at: input.updated_at,
            version,
            disabled_at: (input.state == PrincipalState::Disabled).then_some(input.updated_at),
            revoked_at: (input.state == PrincipalState::Revoked).then_some(input.updated_at),
            ..current_principal
        };
        let profile = UserProfileRecord {
            display_name: input.name,
            email: input.email,
            image_url: input.image,
            updated_at: input.updated_at,
            version,
            ..current_profile
        };
        super::validation::validate_user_account_replacement(
            &principal,
            &profile,
            input.expected_version,
        )?;
        input.idempotency.result = json!({
            "principalId": input.principal_id,
            "version": version,
        });
        match self
            .repository
            .update_user_account(UserAccountMutation {
                actor: input.actor,
                principal: principal.clone(),
                profile: profile.clone(),
                expected_version: input.expected_version,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?
        {
            IdempotentOutcome::Applied(_) => Ok(IdempotentOutcome::Applied(UserAccount {
                principal,
                profile,
            })),
            IdempotentOutcome::Replayed(value) => Ok(IdempotentOutcome::Replayed(value)),
        }
    }
}
