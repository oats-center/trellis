use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trellis_protocol::{GrantSet, PlatformPrivilege};
use ulid::Ulid;
use url::Url;

use super::super::account::{hash_password, normalize_username};
use super::super::*;
use super::bearer_secret_digest;

/// Exact internal authority target installed by first-admin completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstAdminAuthorityTarget {
    /// Internal administration participant ID.
    pub participant_id: String,
    /// Exact installed internal participant definition.
    pub installed_revision: u64,
}

/// Exact participant authority installed atomically with a first administrator.
#[derive(Clone, Debug)]
pub struct FirstAdminBinding {
    /// Participant receiving authority.
    pub participant_id: String,
    /// Exact installed participant revision.
    pub installed_revision: u64,
    /// Exact granted atoms and platform privileges.
    pub grant_set: GrantSet,
    /// Platform privileges granted only while using this participant.
    pub platform_privileges: Vec<PlatformPrivilege>,
}

/// One-time first-administrator startup result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstAdminBootstrap {
    /// Built-in portal URL containing the one-time bearer secret, present only when first created.
    pub bootstrap_url: Option<String>,
    /// Digest of the secret stored in durable state and safe to log separately.
    pub flow_id_hash: String,
    /// Flow expiry in Unix milliseconds.
    pub expires_at: i64,
}

/// Input for atomic first-administrator account completion.
#[derive(Clone, Debug)]
pub struct FirstAdminRegistration {
    /// Raw one-time bearer token from the startup URL.
    pub token: String,
    /// Expected pending flow version.
    pub expected_flow_version: u64,
    /// Desired local username.
    pub username: String,
    /// New plaintext password, retained only for this call.
    pub password: String,
    /// User-facing profile name.
    pub display_name: Option<String>,
    /// Required-nullable profile email.
    pub email: Option<String>,
    /// Required-nullable profile image URL.
    pub image_url: Option<String>,
    /// Exact participant authorities committed with the administrator.
    pub bindings: Vec<FirstAdminBinding>,
    /// Required-nullable authority expiry.
    pub authority_expires_at: Option<i64>,
    /// Completion time in Unix milliseconds.
    pub completed_at: i64,
    /// Durable proof claim; its result is replaced with the committed principal ID.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Input for atomic federated first-administrator account completion.
#[derive(Clone, Debug)]
pub struct FirstAdminFederatedRegistration {
    /// Raw one-time bearer token from the startup URL.
    pub token: String,
    /// Expected pending flow version.
    pub expected_flow_version: u64,
    /// Configured OIDC provider ID.
    pub provider: String,
    /// Verified immutable provider subject.
    pub provider_subject: String,
    /// Required-nullable user-facing profile name.
    pub display_name: Option<String>,
    /// Required-nullable verified profile email.
    pub email: Option<String>,
    /// Required-nullable profile image URL.
    pub image_url: Option<String>,
    /// Exact participant authorities committed with the administrator.
    pub bindings: Vec<FirstAdminBinding>,
    /// Required-nullable authority expiry.
    pub authority_expires_at: Option<i64>,
    /// Completion time in Unix milliseconds.
    pub completed_at: i64,
    /// Durable proof claim; its result is replaced with the committed principal ID.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Newly committed first-administrator account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstAdminAccount {
    /// Stable administrator principal.
    pub principal: PrincipalRecord,
    /// Current administrator profile.
    pub profile: UserProfileRecord,
}

/// Service-owned single-use account-flow input.
#[derive(Clone, Debug)]
pub struct CreateAccountFlowInput {
    /// Password reset or identity-link purpose.
    pub kind: AccountFlowKind,
    /// Required-nullable target user principal.
    pub target_principal_id: Option<String>,
    /// Required-nullable target provider.
    pub target_provider_id: Option<String>,
    /// Required-nullable validated return location.
    pub return_location: Option<String>,
    /// Immutable flow metadata.
    pub payload: Value,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Expiry time in Unix milliseconds.
    pub expires_at: i64,
    /// Durable request proof; its result excludes the raw token.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// One-time account-flow result returned only on first application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedAccountFlow {
    /// Stable non-secret flow ID.
    pub flow_id: String,
    /// Raw one-time bearer token, never stored by Trellis.
    pub token: String,
    /// Flow expiry in Unix milliseconds.
    pub expires_at: i64,
}

/// Identity-link flow completion input.
#[derive(Clone, Debug)]
pub struct CompleteIdentityLinkInput {
    /// Raw one-time flow token.
    pub token: String,
    /// Expected pending flow version.
    pub expected_flow_version: u64,
    /// Exact provider identity link.
    pub identity: ProviderIdentityLink,
    /// Password used only when linking a local identity.
    pub local_password: Option<String>,
    /// Completion time in Unix milliseconds.
    pub completed_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

impl<R> AuthService<R>
where
    R: AccountRepository + GrantRepository + Clone,
{
    /// Seed a usable first local administrator in process, with no browser or account-flow
    /// round trip: create the first-admin flow, then complete it immediately with the
    /// supplied local credentials. The storage effects are exactly those the console's
    /// first-admin route performs, so the seeded account can sign in normally.
    ///
    /// # Errors
    ///
    /// Returns a storage or validation error when the flow cannot be created, its authority
    /// targets cannot be resolved, or the credentials do not satisfy the password policy.
    pub async fn seed_local_admin(
        &self,
        portal_base_url: &str,
        authority_targets: &[FirstAdminAuthorityTarget],
        username: &str,
        password: &str,
        now: i64,
    ) -> Result<String, AuthorizationStateError> {
        let bootstrap = self
            .rotate_admin_account_flow(portal_base_url, authority_targets, now)
            .await?
            .ok_or(AuthorizationStateError::Storage(format!(
                "first-admin flow could not be created"
            )))?;
        let bootstrap_url = bootstrap
            .bootstrap_url
            .ok_or(AuthorizationStateError::Storage(format!(
                "first-admin bootstrap URL is missing"
            )))?;
        let token = Url::parse(&bootstrap_url)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "adminAccountToken")
                    .map(|(_, value)| value.into_owned())
            })
            .ok_or(AuthorizationStateError::Storage(format!(
                "first-admin bootstrap URL carries no token"
            )))?;
        let token_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(
            URL_SAFE_NO_PAD.decode(&token).map_err(|_| {
                AuthorizationStateError::Storage(
                    "first-admin token is not canonical base64url".to_owned(),
                )
            })?,
        ));
        let flow = self
            .repository
            .get_account_flow_by_hash(&token_hash)
            .await?
            .ok_or(AuthorizationStateError::Storage(format!(
                "first-admin flow not found by token"
            )))?;
        let targets =
            flow.payload["bindings"]
                .as_array()
                .ok_or(AuthorizationStateError::Storage(format!(
                    "first-admin flow declares no authority targets"
                )))?;
        let mut bindings = Vec::with_capacity(targets.len());
        for target in targets {
            let participant_id =
                target["participantId"]
                    .as_str()
                    .ok_or(AuthorizationStateError::Storage(format!(
                        "first-admin authority target is malformed"
                    )))?;
            let installed_revision =
                target["installedRevision"]
                    .as_u64()
                    .ok_or(AuthorizationStateError::Storage(format!(
                        "first-admin authority target is not installed"
                    )))?;
            let (_, installed) = self
                .repository
                .get_installed_participant_record(
                    participant_id.to_owned(),
                    Some(installed_revision),
                )
                .await?
                .ok_or(AuthorizationStateError::Storage(format!(
                    "first-admin authority target is not installed"
                )))?;
            bindings.push(FirstAdminBinding {
                participant_id: participant_id.to_owned(),
                installed_revision,
                grant_set: super::super::http::browser::complete_participant_grants(&installed)
                    .map_err(|error| AuthorizationStateError::Storage(format!("{error:?}")))?,
                platform_privileges: (participant_id
                    != crate::platform::auth::builtins::PORTAL_PARTICIPANT_ID)
                    .then_some(PlatformPrivilege::Admin)
                    .into_iter()
                    .collect(),
            });
        }
        let request_digest =
            super::super::http::digest_parts(&["local-admin", &token_hash, username]);
        let outcome = self
            .complete_first_admin(FirstAdminRegistration {
                token,
                expected_flow_version: flow.version,
                username: username.to_owned(),
                password: password.to_owned(),
                display_name: None,
                email: None,
                image_url: None,
                bindings,
                authority_expires_at: None,
                completed_at: now,
                idempotency: IdempotencyResultRecord {
                    scope_key: super::super::http::digest_parts(&[
                        &token_hash,
                        "first_admin.complete",
                        "system:first-admin",
                        &token_hash,
                    ]),
                    purpose: "first_admin.complete".to_owned(),
                    signer_id: "system:first-admin".to_owned(),
                    request_id: token_hash.clone(),
                    request_digest,
                    result: json!({}),
                    created_at: now,
                    expires_at: now + super::super::http::IDEMPOTENCY_TTL_MS,
                },
                actions: Vec::new(),
            })
            .await?;
        Ok(match outcome {
            IdempotentOutcome::Applied(account) => account.principal.principal_id,
            IdempotentOutcome::Replayed(value) => value
                .pointer("/principalId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// Complete a first-administrator flow and reconcile its exact authority.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed tokens, unsafe passwords,
    /// or inconsistent authority input, and a conflict if the flow was already
    /// consumed or an administrator became active concurrently.
    pub(crate) async fn complete_first_admin(
        &self,
        mut input: FirstAdminRegistration,
    ) -> Result<IdempotentOutcome<FirstAdminAccount>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("completedAt", input.completed_at)?;
        let token = URL_SAFE_NO_PAD.decode(&input.token).map_err(|_| {
            AuthorizationStateError::InvalidRecord(
                "first-admin token is not canonical base64url".to_owned(),
            )
        })?;
        if token.len() != 32 || URL_SAFE_NO_PAD.encode(&token) != input.token {
            return Err(AuthorizationStateError::InvalidRecord(
                "first-admin token must canonically encode 32 bytes".to_owned(),
            ));
        }
        let token_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(token));
        let flow = self
            .repository
            .get_account_flow_by_hash(&token_hash)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        if let Some(principal_id) = flow.target_principal_id.clone() {
            let account = self
                .user(&principal_id)
                .await?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            let mut bindings = Vec::with_capacity(input.bindings.len());
            for requested in &input.bindings {
                let current = self
                    .repository
                    .get_grant_binding(
                        GrantOwnerKind::User,
                        principal_id.clone(),
                        requested.participant_id.clone(),
                    )
                    .await?;
                bindings.push(GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: principal_id.clone(),
                    participant_id: requested.participant_id.clone(),
                    installed_revision: requested.installed_revision,
                    grants: requested.grant_set.clone(),
                    approval_mode: super::super::ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: super::super::DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(requested.grant_set.clone()),
                        platform_privileges: requested.platform_privileges.clone(),
                    },
                    approval_decision_digest: input.idempotency.request_digest.clone(),
                    companion_approved: false,
                    platform_privileges: requested.platform_privileges.clone(),
                    expires_at: input.authority_expires_at,
                    expected_revision: current.as_ref().map_or(0, |binding| binding.revision),
                    expected_current_installed_revision: None,
                    state: GrantBindingState::Active,
                    provenance: None,
                });
            }
            let profile = UserProfileRecord {
                display_name: input.display_name.clone(),
                email: input.email.clone(),
                image_url: input.image_url.clone(),
                updated_at: input.completed_at,
                version: account.profile.version.checked_add(1).ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord("profile version overflow".to_owned())
                })?,
                ..account.profile.clone()
            };
            let outcome = self
                .complete_password_reset(CompletePasswordResetInput {
                    token: input.token,
                    expected_flow_version: input.expected_flow_version,
                    username: Some(input.username),
                    bindings,
                    profile: Some(profile.clone()),
                    password: input.password,
                    consumed_at: input.completed_at,
                    idempotency: input.idempotency,
                    actions: input.actions,
                })
                .await?;
            return Ok(match outcome {
                IdempotentOutcome::Applied(_) => IdempotentOutcome::Applied(FirstAdminAccount {
                    principal: account.principal,
                    profile,
                }),
                IdempotentOutcome::Replayed(value) => IdempotentOutcome::Replayed(value),
            });
        }
        let username = normalize_username(&input.username)?;
        let (password_hash, hash_profile) =
            hash_password(&input.password, Some(self.config.password_min_length))?;
        let principal_id = format!("usr_{}", Ulid::new());
        let principal = PrincipalRecord {
            principal_id: principal_id.clone(),
            kind: PrincipalKind::User,
            state: PrincipalState::Active,
            created_at: input.completed_at,
            updated_at: input.completed_at,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let profile = UserProfileRecord {
            principal_id: principal_id.clone(),
            display_name: input.display_name,
            email: input.email,
            image_url: input.image_url,
            created_at: input.completed_at,
            updated_at: input.completed_at,
            version: 1,
        };
        let credential = LocalCredentialRecord {
            principal_id: principal_id.clone(),
            normalized_username: username.clone(),
            password_hash,
            hash_profile,
            failed_attempts: 0,
            locked_until: None,
            password_changed_at: input.completed_at,
            updated_at: input.completed_at,
            version: 1,
        };
        let local_identity = ProviderIdentityLink {
            provider: "local".to_owned(),
            provider_subject: username,
            principal_id: principal_id.clone(),
            linked_at: input.completed_at,
            last_seen_at: input.completed_at,
        };
        let approval_decision_digest = input.idempotency.request_digest.clone();
        let bindings = input
            .bindings
            .into_iter()
            .map(|requested| {
                let grants = requested.grant_set;
                let platform_privileges = requested.platform_privileges;
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: principal_id.clone(),
                    participant_id: requested.participant_id,
                    installed_revision: requested.installed_revision,
                    grants: grants.clone(),
                    approval_mode: super::super::ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: super::super::DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(grants),
                        platform_privileges: platform_privileges.clone(),
                    },
                    approval_decision_digest: approval_decision_digest.clone(),
                    companion_approved: false,
                    platform_privileges,
                    expires_at: input.authority_expires_at,
                    expected_revision: 0,
                    expected_current_installed_revision: None,
                    state: GrantBindingState::Active,
                    provenance: None,
                }
            })
            .collect();
        super::validation::validate_new_user_account(
            &principal,
            &profile,
            Some(&credential),
            Some(&local_identity),
        )?;
        input.idempotency.result = json!({
            "principalId": principal_id,
        });
        let outcome = self
            .repository
            .complete_first_admin(FirstAdminCompletion {
                token_hash,
                expected_flow_version: input.expected_flow_version,
                principal: principal.clone(),
                profile: profile.clone(),
                credential: Some(credential),
                identity: local_identity,
                bindings,
                consumed_at: input.completed_at,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?;
        Ok(match outcome {
            IdempotentOutcome::Applied(_) => {
                IdempotentOutcome::Applied(FirstAdminAccount { principal, profile })
            }
            IdempotentOutcome::Replayed(value) => IdempotentOutcome::Replayed(value),
        })
    }

    /// Complete a first-administrator flow with one verified federated identity.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed tokens or identity data, and
    /// a conflict if the flow was consumed or an administrator became active.
    pub(crate) async fn complete_first_admin_federated(
        &self,
        mut input: FirstAdminFederatedRegistration,
    ) -> Result<IdempotentOutcome<FirstAdminAccount>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("completedAt", input.completed_at)?;
        let token = URL_SAFE_NO_PAD.decode(&input.token).map_err(|_| {
            AuthorizationStateError::InvalidRecord(
                "first-admin token is not canonical base64url".to_owned(),
            )
        })?;
        if token.len() != 32 || URL_SAFE_NO_PAD.encode(&token) != input.token {
            return Err(AuthorizationStateError::InvalidRecord(
                "first-admin token must canonically encode 32 bytes".to_owned(),
            ));
        }
        let token_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(token));
        let principal_id = format!("usr_{}", Ulid::new());
        let principal = PrincipalRecord {
            principal_id: principal_id.clone(),
            kind: PrincipalKind::User,
            state: PrincipalState::Active,
            created_at: input.completed_at,
            updated_at: input.completed_at,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let profile = UserProfileRecord {
            principal_id: principal_id.clone(),
            display_name: input.display_name,
            email: input.email,
            image_url: input.image_url,
            created_at: input.completed_at,
            updated_at: input.completed_at,
            version: 1,
        };
        let identity = ProviderIdentityLink {
            provider: input.provider,
            provider_subject: input.provider_subject,
            principal_id: principal_id.clone(),
            linked_at: input.completed_at,
            last_seen_at: input.completed_at,
        };
        let approval_decision_digest = input.idempotency.request_digest.clone();
        let bindings = input
            .bindings
            .into_iter()
            .map(|requested| {
                let grants = requested.grant_set;
                let platform_privileges = requested.platform_privileges;
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: principal_id.clone(),
                    participant_id: requested.participant_id,
                    installed_revision: requested.installed_revision,
                    grants: grants.clone(),
                    approval_mode: super::super::ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: super::super::DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(grants),
                        platform_privileges: platform_privileges.clone(),
                    },
                    approval_decision_digest: approval_decision_digest.clone(),
                    companion_approved: false,
                    platform_privileges,
                    expires_at: input.authority_expires_at,
                    expected_revision: 0,
                    expected_current_installed_revision: None,
                    state: GrantBindingState::Active,
                    provenance: None,
                }
            })
            .collect();
        super::validation::validate_new_user_account(&principal, &profile, None, Some(&identity))?;
        input.idempotency.result = json!({
            "principalId": principal_id,
        });
        let outcome = self
            .repository
            .complete_first_admin(FirstAdminCompletion {
                token_hash,
                expected_flow_version: input.expected_flow_version,
                principal: principal.clone(),
                profile: profile.clone(),
                credential: None,
                identity,
                bindings,
                consumed_at: input.completed_at,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?;
        Ok(match outcome {
            IdempotentOutcome::Applied(_) => {
                IdempotentOutcome::Applied(FirstAdminAccount { principal, profile })
            }
            IdempotentOutcome::Replayed(value) => IdempotentOutcome::Replayed(value),
        })
    }
}

impl<R> AuthService<R>
where
    R: AccountRepository + Clone,
{
    /// Create a password-reset or identity-link flow and return its token once.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for first-admin or malformed flow input,
    /// and a repository conflict when durable identities cannot be committed.
    pub(crate) async fn create_account_flow(
        &self,
        mut input: CreateAccountFlowInput,
    ) -> Result<IdempotentOutcome<CreatedAccountFlow>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("createdAt", input.created_at)?;
        super::super::domain::require_protocol_timestamp("expiresAt", input.expires_at)?;
        if input.kind == AccountFlowKind::AdminAccount {
            return Err(AuthorizationStateError::InvalidRecord(
                "administrator-account flows use ensure_admin_account_flow".to_owned(),
            ));
        }
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| {
            AuthorizationStateError::Storage(format!(
                "account-flow secret generation failed: {error}"
            ))
        })?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        let token_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret));
        let flow_id = format!("afl_{}", Ulid::new());
        let flow = AccountFlowRecord {
            flow_id: flow_id.clone(),
            kind: input.kind,
            token_hash,
            target_principal_id: input.target_principal_id,
            target_provider_id: input.target_provider_id,
            return_location: input.return_location,
            payload: input.payload,
            state: AccountFlowState::Pending,
            created_at: input.created_at,
            expires_at: input.expires_at,
            consumed_at: None,
            version: 1,
        };
        super::validation::validate_account_flow(&flow)?;
        input.idempotency.result = json!({
            "flowId": flow_id,
            "expiresAt": input.expires_at,
        });
        match self
            .repository
            .create_account_flow(AccountFlowCreation {
                flow,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?
        {
            IdempotentOutcome::Applied(_) => Ok(IdempotentOutcome::Applied(CreatedAccountFlow {
                flow_id,
                token,
                expires_at: input.expires_at,
            })),
            IdempotentOutcome::Replayed(value) => Ok(IdempotentOutcome::Replayed(value)),
        }
    }

    /// Consume an identity-link flow and attach the exact provider identity.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed token or identity input and
    /// a conflict for expired, consumed, or mismatched flow state.
    pub(crate) async fn complete_identity_link(
        &self,
        mut input: CompleteIdentityLinkInput,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("completedAt", input.completed_at)?;
        super::super::authority::validate_provider_identity(&input.identity)?;
        let credential = match (input.identity.provider.as_str(), input.local_password) {
            ("local", Some(password)) => {
                let username = normalize_username(&input.identity.provider_subject)?;
                if username != input.identity.provider_subject {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "local provider subject must be a normalized username".to_owned(),
                    ));
                }
                let (password_hash, hash_profile) =
                    hash_password(&password, Some(self.config.password_min_length))?;
                Some(LocalCredentialRecord {
                    principal_id: input.identity.principal_id.clone(),
                    normalized_username: username,
                    password_hash,
                    hash_profile,
                    failed_attempts: 0,
                    locked_until: None,
                    password_changed_at: input.completed_at,
                    updated_at: input.completed_at,
                    version: 1,
                })
            }
            ("local", None) | (_, Some(_)) => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "local identity links require a password".to_owned(),
                ));
            }
            (_, None) => None,
        };
        let token_hash = bearer_secret_digest(&input.token)?;
        input.idempotency.result = json!({
            "principalId": input.identity.principal_id,
            "provider": input.identity.provider,
        });
        self.repository
            .complete_identity_link(IdentityLinkCompletion {
                token_hash,
                expected_flow_version: input.expected_flow_version,
                identity: input.identity,
                credential,
                consumed_at: input.completed_at,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await
    }
}

impl<R> AuthService<R>
where
    R: AccountRepository + Clone,
{
    /// Create or report one pending first-administrator flow when no active admin exists.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for an invalid base URL or timestamp,
    /// and a repository or entropy error when the flow cannot be committed.
    pub(crate) async fn ensure_admin_account_flow(
        &self,
        portal_base_url: &str,
        authority_targets: &[FirstAdminAuthorityTarget],
        now: i64,
    ) -> Result<Option<FirstAdminBootstrap>, AuthorizationStateError> {
        self.first_admin_flow(portal_base_url, authority_targets, now, false)
            .await
    }

    /// Explicitly revoke an existing pending first-administrator flow and create a new one.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for an invalid base URL or timestamp,
    /// and a repository or entropy error when rotation cannot be committed.
    pub(crate) async fn rotate_admin_account_flow(
        &self,
        portal_base_url: &str,
        authority_targets: &[FirstAdminAuthorityTarget],
        now: i64,
    ) -> Result<Option<FirstAdminBootstrap>, AuthorizationStateError> {
        self.first_admin_flow(portal_base_url, authority_targets, now, true)
            .await
    }

    async fn first_admin_flow(
        &self,
        portal_base_url: &str,
        authority_targets: &[FirstAdminAuthorityTarget],
        now: i64,
        rotate: bool,
    ) -> Result<Option<FirstAdminBootstrap>, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("now", now)?;
        if authority_targets.is_empty()
            || authority_targets.iter().any(|target| {
                target.participant_id.trim().is_empty() || target.installed_revision == 0
            })
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "first-admin authority target is invalid".to_owned(),
            ));
        }
        let mut bootstrap_url = Url::parse(portal_base_url).map_err(|_| {
            AuthorizationStateError::InvalidRecord("portal base URL is invalid".to_owned())
        })?;
        let expires_at = u64::try_from(now)
            .ok()
            .and_then(|now| now.checked_add(self.config.first_admin_flow_ttl_ms))
            .filter(|expires| *expires <= super::super::MAX_PROTOCOL_INTEGER)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("first-admin expiry overflow".to_owned())
            })? as i64;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| {
            AuthorizationStateError::Storage(format!(
                "first-admin secret generation failed: {error}"
            ))
        })?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        let token_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret));
        let flow = AccountFlowRecord {
            flow_id: format!("afl_{}", Ulid::new()),
            kind: AccountFlowKind::AdminAccount,
            token_hash: token_hash.clone(),
            target_principal_id: None,
            target_provider_id: None,
            return_location: None,
            payload: json!({
                "bindings": authority_targets.iter().map(|target| json!({
                    "participantId": target.participant_id,
                    "installedRevision": target.installed_revision,
                })).collect::<Vec<_>>(),
            }),
            state: AccountFlowState::Pending,
            created_at: now,
            expires_at,
            consumed_at: None,
            version: 1,
        };
        super::validation::validate_account_flow(&flow)?;
        let stored = if let Some(stored) = self
            .repository
            .replace_admin_account_flow(flow, now, rotate)
            .await?
        {
            stored
        } else {
            return Ok(None);
        };
        let created = stored.token_hash == token_hash;
        let bootstrap_url = if created {
            bootstrap_url.set_path("/console/profile");
            bootstrap_url.set_query(None);
            bootstrap_url
                .query_pairs_mut()
                .append_pair("adminAccountToken", &token);
            Some(bootstrap_url.into())
        } else {
            None
        };
        Ok(Some(FirstAdminBootstrap {
            bootstrap_url,
            flow_id_hash: stored.token_hash,
            expires_at: stored.expires_at,
        }))
    }
}
