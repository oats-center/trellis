use async_trait::async_trait;
use serde_json::Value;

use super::super::{
    AccountFlowKind, AccountFlowRecord, AuthorizationStateError, DeploymentProfileRecord,
    DeviceActivationReviewRecord, DeviceActivationReviewState, DeviceDelegationRecord,
    DeviceProvisioningSecretRecord, DeviceRecord, GrantBindingReplacement, IdempotencyResultRecord,
    LocalCredentialRecord, LoginPortalRecord, LoginSettingsRecord, MutationActor,
    PortalRouteRecord, PostCommitActionRecord, PrincipalRecord, ProviderIdentityLink,
    ProvisionedIdentityRecord, RuntimeInstanceRecord, SessionRecord, UserProfileRecord,
};

/// Atomic deployment-profile creation.
#[derive(Clone, Debug)]
pub(crate) struct DeploymentProfileCreation {
    /// Principal installed for the deployment.
    pub principal: PrincipalRecord,
    /// Product-facing deployment metadata.
    pub profile: DeploymentProfileRecord,
    /// Durable replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic deployment-profile lifecycle mutation.
#[derive(Clone, Debug)]
pub(crate) struct DeploymentProfileMutation {
    /// Complete replacement profile.
    pub profile: DeploymentProfileRecord,
    /// Expected current profile version.
    pub expected_version: u64,
    /// Durable replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Result of an idempotent aggregate transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IdempotentOutcome<T> {
    /// The mutation was applied and committed.
    Applied(T),
    /// The matching request had already committed this durable JSON result.
    Replayed(Value),
}

/// One optimistic local-login state transition.
#[derive(Clone, Debug)]
pub(crate) struct LocalLoginAttempt {
    /// Target local-account principal.
    pub principal_id: String,
    /// Credential version observed during password verification.
    pub expected_version: u64,
    /// Whether password verification succeeded.
    pub succeeded: bool,
    /// Attempt time in Unix milliseconds.
    pub attempted_at: i64,
    /// Consecutive failures that trigger a lock.
    pub maximum_failures: u32,
    /// Lock duration in milliseconds.
    pub lock_duration_ms: u64,
}

/// Atomic password-reset flow completion.
#[derive(Clone, Debug)]
pub(crate) struct PasswordResetCompletion {
    /// Bearer-token digest selecting the flow.
    pub token_hash: String,
    /// Expected pending flow version.
    pub expected_flow_version: u64,
    /// Expected account-flow kind.
    pub flow_kind: AccountFlowKind,
    /// Principal that requested this reset, when it was created through Auth RPC.
    pub requester_principal_id: Option<String>,
    /// Whether the target held accepted administrator authority when this flow was issued.
    pub admin_target: bool,
    /// Credential version observed before hashing; `None` requires first install.
    pub expected_credential_version: Option<u64>,
    /// Complete replacement credential with version `current + 1`.
    pub replacement: LocalCredentialRecord,
    /// Local identity installed atomically with a first credential.
    pub identity: Option<ProviderIdentityLink>,
    /// Canonical binding to restore for bootstrap-administrator recovery.
    pub bindings: Vec<GrantBindingReplacement>,
    /// Optional profile replacement committed in the same transaction.
    pub profile: Option<UserProfileRecord>,
    /// Completion time in Unix milliseconds.
    pub consumed_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic provider-identity-link flow completion.
#[derive(Clone, Debug)]
pub(crate) struct IdentityLinkCompletion {
    /// Bearer-token digest selecting the flow.
    pub token_hash: String,
    /// Expected pending flow version.
    pub expected_flow_version: u64,
    /// Exact provider identity to attach.
    pub identity: ProviderIdentityLink,
    /// Local credential installed atomically for a local identity.
    pub credential: Option<LocalCredentialRecord>,
    /// Completion time in Unix milliseconds.
    pub consumed_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic first-administrator flow completion.
#[derive(Clone, Debug)]
pub(crate) struct FirstAdminCompletion {
    /// Bearer-token digest selecting the flow.
    pub token_hash: String,
    /// Expected pending flow version.
    pub expected_flow_version: u64,
    /// New administrator principal.
    pub principal: PrincipalRecord,
    /// New administrator profile.
    pub profile: UserProfileRecord,
    /// Optional new administrator local credential; federated completion has none.
    pub credential: Option<LocalCredentialRecord>,
    /// New administrator authentication identity.
    pub identity: ProviderIdentityLink,
    /// Explicit administrator binding committed with the account.
    pub bindings: Vec<GrantBindingReplacement>,
    /// Completion time in Unix milliseconds.
    pub consumed_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic device provisioning-secret consumption.
#[derive(Clone, Debug)]
pub(crate) struct DeviceProvisioningSecretConsumption {
    /// Secret digest selecting the pending secret.
    pub secret_hash: String,
    /// Expected pending secret version.
    pub expected_version: u64,
    /// Immutable device identity created by consumption.
    pub identity: ProvisionedIdentityRecord,
    /// Consumption time in Unix milliseconds.
    pub consumed_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic terminal activation-review decision.
#[derive(Clone, Debug)]
pub(crate) struct ActivationReviewDecision {
    /// Review ID to decide.
    pub review_id: String,
    /// Expected pending review version.
    pub expected_version: u64,
    /// Approved or rejected terminal state.
    pub state: DeviceActivationReviewState,
    /// Decision time in Unix milliseconds.
    pub decided_at: i64,
    /// Stable deciding administrator.
    pub decided_by: String,
    /// Optional operator reason.
    pub reason: Option<String>,
    /// Optional approved delegation replacement; absent on rejection.
    pub delegation: Option<DeviceDelegationRecord>,
    /// Separate user session installed for the device's companion.
    pub companion_session: Option<SessionRecord>,
    /// Whether this decision satisfies every requirement for device readiness.
    pub activate_device: bool,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic user claim of one activation review.
#[derive(Clone, Debug)]
pub(crate) struct ActivationReviewClaim {
    /// Review ID to claim.
    pub review_id: String,
    /// Expected review version.
    pub expected_version: u64,
    /// Activating user principal.
    pub activated_by_user_principal_id: String,
    /// Authoritative server time in Unix milliseconds.
    pub now: i64,
    /// Active delegation created when claiming an already approved review.
    pub delegation: Option<DeviceDelegationRecord>,
    /// Separate user session installed for the device's companion.
    pub companion_session: Option<SessionRecord>,
    /// Durable operation result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic service identity provisioning.
#[derive(Clone, Debug)]
pub(crate) struct ServiceIdentityProvisioning {
    /// Existing deployment ID.
    pub deployment_id: String,
    /// Validated stable identity key ID derived from the public key.
    pub identity_key_id: String,
    /// Canonical client-generated Ed25519 identity public key.
    pub identity_public_key: String,
    /// Explicitly requested stable instance ID, when the caller supplied one.
    pub requested_instance_id: Option<String>,
    /// Principal ID proposed for the new-identity case.
    pub proposed_principal_id: String,
    /// Instance ID proposed for the new-identity case.
    pub proposed_instance_id: String,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic device and one-time secret provisioning.
#[derive(Clone, Debug)]
pub(crate) struct DeviceProvisioning {
    /// New device principal.
    pub principal: PrincipalRecord,
    /// New deployment-owned runtime instance.
    pub instance: RuntimeInstanceRecord,
    /// New deployment-scoped device record.
    pub device: DeviceRecord,
    /// Optional identity installed immediately instead of returning a secret.
    pub identity: Option<ProvisionedIdentityRecord>,
    /// Hashed one-time provisioning secret.
    pub secret: DeviceProvisioningSecretRecord,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic service/device instance lifecycle mutation.
#[derive(Clone, Debug)]
pub(crate) struct ProvisionedInstanceMutation {
    /// Replacement runtime instance.
    pub instance: RuntimeInstanceRecord,
    /// Optional replacement device lifecycle record.
    pub device: Option<DeviceRecord>,
    /// Optional immutable identity whose state follows the lifecycle.
    pub identity: Option<ProvisionedIdentityRecord>,
    /// Expected API-visible lifecycle version.
    pub expected_version: u64,
    /// Durable replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic external-identity unlink command.
#[derive(Clone, Debug)]
pub(crate) struct ProviderIdentityUnlink {
    /// Provider key.
    pub provider: String,
    /// Provider-owned stable subject.
    pub provider_subject: String,
    /// Principal that must own the link.
    pub principal_id: String,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic authenticated password change and sibling-session revocation.
#[derive(Clone, Debug)]
pub(crate) struct PasswordChange {
    /// User principal owning the credential.
    pub principal_id: String,
    /// Session that remains active after the password change.
    pub current_session_id: String,
    /// Replacement Argon2id credential.
    pub credential: LocalCredentialRecord,
    /// Expected credential version.
    pub expected_version: u64,
    /// Password-change time in Unix milliseconds.
    pub changed_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic device delegation revocation.
#[derive(Clone, Debug)]
pub(crate) struct DeviceDelegationMutation {
    /// Replacement device lifecycle record.
    pub device: DeviceRecord,
    /// Replacement delegation record.
    pub delegation: DeviceDelegationRecord,
    /// Expected device version.
    pub expected_version: u64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic user installation login creation or eligible same-key reuse.
#[derive(Clone, Debug)]
pub(crate) struct SessionCreation {
    /// New validated session record.
    pub session: SessionRecord,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic authenticated-session revocation.
#[derive(Clone, Debug)]
pub(crate) struct SessionRevocation {
    /// Stable session ID to revoke.
    pub session_id: String,
    /// Expected active session version.
    pub expected_version: u64,
    /// Revocation time in Unix milliseconds.
    pub revoked_at: i64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic event and connection-kick actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic user-account creation with optional local-login records.
#[derive(Clone, Debug)]
pub(crate) struct AccountCreation {
    /// New user principal.
    pub principal: PrincipalRecord,
    /// New user profile.
    pub profile: UserProfileRecord,
    /// Optional local credential; requires a matching local `identity`.
    pub credential: Option<LocalCredentialRecord>,
    /// Optional local or federated provider identity.
    pub identity: Option<ProviderIdentityLink>,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic user principal and profile replacement.
#[derive(Clone, Debug)]
pub(crate) struct UserAccountMutation {
    /// Verified caller state to recheck in the write transaction.
    pub actor: MutationActor,
    /// Complete replacement principal.
    pub principal: PrincipalRecord,
    /// Complete replacement profile.
    pub profile: UserProfileRecord,
    /// Expected current version shared by the principal and profile.
    pub expected_version: u64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic account-flow creation.
#[derive(Clone, Debug)]
pub(crate) struct AccountFlowCreation {
    /// New pending account flow.
    pub flow: AccountFlowRecord,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic activation-review creation.
#[derive(Clone, Debug)]
pub(crate) struct ActivationReviewCreation {
    /// New pending activation review.
    pub review: DeviceActivationReviewRecord,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic login-portal and settings mutation.
#[derive(Clone, Debug)]
pub(crate) struct LoginPortalMutation {
    /// Portal record to create or replace.
    pub portal: LoginPortalRecord,
    /// Settings record to create or replace with the portal.
    pub settings: LoginSettingsRecord,
    /// Expected current version, or `None` for creation.
    pub expected_version: Option<u64>,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic portal-route mutation.
#[derive(Clone, Debug)]
pub(crate) struct PortalRouteMutation {
    /// Route record to create or replace.
    pub route: PortalRouteRecord,
    /// Expected current version, or `None` for creation.
    pub expected_version: Option<u64>,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Atomic portal-route removal.
#[derive(Clone, Debug)]
pub(crate) struct PortalRouteRemoval {
    /// Stable route ID to remove.
    pub route_id: String,
    /// Expected current route version.
    pub expected_version: u64,
    /// Durable proof claim and replay result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Persistence contract for the user-account aggregate.
#[async_trait]
pub(crate) trait AccountRepository: Send + Sync {
    /// Load a principal by stable ID.
    async fn get_principal(
        &self,
        id: &str,
    ) -> Result<Option<PrincipalRecord>, AuthorizationStateError>;

    /// Load a provider identity by its exact provider and subject.
    async fn get_provider_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<ProviderIdentityLink>, AuthorizationStateError>;

    /// List provider identities owned by one principal.
    async fn list_provider_identities(
        &self,
        principal_id: &str,
    ) -> Result<Vec<ProviderIdentityLink>, AuthorizationStateError>;

    /// Atomically unlink one external identity while preserving a login method.
    async fn unlink_provider_identity(
        &self,
        command: ProviderIdentityUnlink,
    ) -> Result<IdempotentOutcome<bool>, AuthorizationStateError>;

    /// Atomically create a user principal and profile, optionally with local-login records.
    async fn create_user_account(
        &self,
        command: AccountCreation,
    ) -> Result<IdempotentOutcome<UserProfileRecord>, AuthorizationStateError>;

    /// Load one user principal and its required profile.
    async fn get_user_account(
        &self,
        principal_id: &str,
    ) -> Result<Option<(PrincipalRecord, UserProfileRecord)>, AuthorizationStateError>;

    /// List filtered user accounts after an optional exclusive stable-sort cursor.
    async fn list_user_accounts(
        &self,
        cursor: Option<&(i64, String)>,
        state: Option<&str>,
        search: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(PrincipalRecord, UserProfileRecord)>, AuthorizationStateError>;

    /// Atomically replace one user principal and profile using optimistic versioning.
    async fn update_user_account(
        &self,
        command: UserAccountMutation,
    ) -> Result<IdempotentOutcome<(PrincipalRecord, UserProfileRecord)>, AuthorizationStateError>;

    /// Load one user profile by principal ID.
    async fn get_user_profile(
        &self,
        principal_id: &str,
    ) -> Result<Option<UserProfileRecord>, AuthorizationStateError>;

    /// Load one local credential by principal ID.
    async fn get_local_credential(
        &self,
        principal_id: &str,
    ) -> Result<Option<LocalCredentialRecord>, AuthorizationStateError>;

    /// Atomically replace a password and revoke sibling sessions.
    async fn change_password(
        &self,
        command: PasswordChange,
    ) -> Result<IdempotentOutcome<usize>, AuthorizationStateError>;

    /// Load one local credential by canonical username.
    async fn get_local_credential_by_username(
        &self,
        normalized_username: &str,
    ) -> Result<Option<LocalCredentialRecord>, AuthorizationStateError>;

    /// Atomically record one successful or failed local login attempt.
    async fn record_local_login_attempt(
        &self,
        attempt: LocalLoginAttempt,
    ) -> Result<LocalCredentialRecord, AuthorizationStateError>;

    /// Create the startup first-admin flow, or return the existing unexpired pending flow.
    async fn replace_admin_account_flow(
        &self,
        flow: AccountFlowRecord,
        now: i64,
        rotate: bool,
    ) -> Result<Option<AccountFlowRecord>, AuthorizationStateError>;

    /// Revoke prior pending password resets for one principal and insert the replacement.
    /// Create one pending flow with a globally unique token hash.
    async fn create_account_flow(
        &self,
        command: AccountFlowCreation,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError>;

    /// Load a flow by its bearer-token digest.
    async fn get_account_flow_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<AccountFlowRecord>, AuthorizationStateError>;

    /// Complete a password reset atomically with proof result and actions.
    async fn complete_password_reset(
        &self,
        command: PasswordResetCompletion,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError>;

    /// Complete an identity link atomically with proof result and actions.
    async fn complete_identity_link(
        &self,
        command: IdentityLinkCompletion,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError>;

    /// Complete first-administrator creation atomically.
    async fn complete_first_admin(
        &self,
        command: FirstAdminCompletion,
    ) -> Result<IdempotentOutcome<AccountFlowRecord>, AuthorizationStateError>;
}

/// Persistence contract for login portals, settings, and deterministic routes.
#[async_trait]
pub(crate) trait PortalRepository: Send + Sync {
    /// List login portals in stable ID order.
    async fn list_login_portals(&self) -> Result<Vec<LoginPortalRecord>, AuthorizationStateError>;

    /// Load one portal and its required settings.
    async fn get_login_portal(
        &self,
        portal_id: &str,
    ) -> Result<Option<(LoginPortalRecord, LoginSettingsRecord)>, AuthorizationStateError>;

    /// Create or compare-and-swap a portal and settings atomically.
    async fn put_login_portal(
        &self,
        command: LoginPortalMutation,
    ) -> Result<IdempotentOutcome<(LoginPortalRecord, LoginSettingsRecord)>, AuthorizationStateError>;

    /// Create or compare-and-swap one portal route.
    async fn put_portal_route(
        &self,
        command: PortalRouteMutation,
    ) -> Result<IdempotentOutcome<PortalRouteRecord>, AuthorizationStateError>;

    /// Remove one route with an optimistic version guard.
    async fn remove_portal_route(
        &self,
        command: PortalRouteRemoval,
    ) -> Result<IdempotentOutcome<PortalRouteRecord>, AuthorizationStateError>;

    /// List routes in deterministic selection order.
    async fn list_portal_routes(&self) -> Result<Vec<PortalRouteRecord>, AuthorizationStateError>;

    /// Load one trusted portal policy for a participant.
    async fn get_portal_grant_override(
        &self,
        portal_id: &str,
        participant_id: &str,
    ) -> Result<Option<super::super::PortalGrantOverrideRecord>, AuthorizationStateError>;

    /// List capability groups for trusted portal policy resolution.
    async fn list_capability_groups(
        &self,
    ) -> Result<Vec<super::super::CapabilityGroupRecord>, AuthorizationStateError>;

    /// List durable trusted-portal authority provenance in stable order.
    async fn list_portal_grant_bindings(
        &self,
    ) -> Result<Vec<super::super::PortalGrantBindingRecord>, AuthorizationStateError>;
}

/// Persistence contract for authenticated sessions and their runtime selection.
#[async_trait]
pub(crate) trait SessionRepository: Send + Sync {
    /// Create a session with any desired authority or runtime binding atomically.
    async fn create_session(
        &self,
        command: SessionCreation,
    ) -> Result<IdempotentOutcome<SessionRecord>, AuthorizationStateError>;

    /// Revoke one active session with proof result and actions atomically.
    async fn revoke_session(
        &self,
        command: SessionRevocation,
    ) -> Result<IdempotentOutcome<SessionRecord>, AuthorizationStateError>;

    /// Load a session by stable ID.
    async fn get_session(&self, id: &str)
        -> Result<Option<SessionRecord>, AuthorizationStateError>;

    /// List all sessions in stable session-id order.
    async fn list_sessions(&self) -> Result<Vec<SessionRecord>, AuthorizationStateError>;
}

/// Persistence contract for deployment profiles and deployment evidence.
#[async_trait]
pub(crate) trait DeploymentRepository: Send + Sync {
    /// Create a deployment principal and profile atomically.
    async fn create_deployment_profile(
        &self,
        command: DeploymentProfileCreation,
    ) -> Result<IdempotentOutcome<DeploymentProfileRecord>, AuthorizationStateError>;

    /// Load one deployment profile.
    async fn get_deployment_profile(
        &self,
        deployment_id: &str,
    ) -> Result<Option<DeploymentProfileRecord>, AuthorizationStateError>;

    /// List deployment profiles in stable ID order.
    async fn list_deployment_profiles(
        &self,
    ) -> Result<Vec<DeploymentProfileRecord>, AuthorizationStateError>;

    /// Replace profile lifecycle state and matching runtime evidence atomically.
    async fn put_deployment_profile(
        &self,
        command: DeploymentProfileMutation,
    ) -> Result<IdempotentOutcome<DeploymentProfileRecord>, AuthorizationStateError>;
}

/// Persistence contract for provisioned identities and runtime evidence.
#[async_trait]
pub(crate) trait ProvisioningRepository: Send + Sync {
    async fn get_device_provisioning_secret_by_hash(
        &self,
        secret_hash: &str,
    ) -> Result<Option<super::super::DeviceProvisioningSecretRecord>, AuthorizationStateError>;
    /// List provisioned identities in stable key order.
    async fn list_provisioned_identities(
        &self,
    ) -> Result<Vec<ProvisionedIdentityRecord>, AuthorizationStateError>;

    /// Load provisioned identity metadata by key ID.
    async fn get_provisioned_identity(
        &self,
        identity_key_id: &str,
    ) -> Result<Option<ProvisionedIdentityRecord>, AuthorizationStateError>;

    /// Consume a device secret and create its immutable identity atomically.
    async fn consume_device_provisioning_secret(
        &self,
        command: DeviceProvisioningSecretConsumption,
    ) -> Result<IdempotentOutcome<DeviceProvisioningSecretRecord>, AuthorizationStateError>;

    /// Create a pending activation review after exact device relationship checks.
    async fn create_activation_review(
        &self,
        command: ActivationReviewCreation,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError>;

    /// Load one activation review by stable ID.
    async fn get_activation_review(
        &self,
        review_id: &str,
    ) -> Result<Option<DeviceActivationReviewRecord>, AuthorizationStateError>;

    /// Expire every pending activation review due at `now`.
    async fn expire_due_activation_reviews(
        &self,
        now: i64,
    ) -> Result<Vec<DeviceActivationReviewRecord>, AuthorizationStateError>;

    /// Claim one activation review for an authenticated user.
    async fn claim_activation_review(
        &self,
        command: ActivationReviewClaim,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError>;

    /// List device activation reviews in stable ID order.
    async fn list_activation_reviews(
        &self,
    ) -> Result<Vec<DeviceActivationReviewRecord>, AuthorizationStateError>;

    /// Decide a review and approved device state atomically.
    async fn decide_activation_review(
        &self,
        command: ActivationReviewDecision,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError>;

    /// Provision a service principal, instance, identity, proof result, and actions atomically.
    async fn provision_service_identity(
        &self,
        command: ServiceIdentityProvisioning,
    ) -> Result<IdempotentOutcome<ProvisionedIdentityRecord>, AuthorizationStateError>;

    /// Provision a device principal, instance, record, secret, proof result, and actions atomically.
    async fn provision_device(
        &self,
        command: DeviceProvisioning,
    ) -> Result<IdempotentOutcome<DeviceProvisioningSecretRecord>, AuthorizationStateError>;

    /// Mutate one provisioned service/device lifecycle atomically.
    async fn mutate_provisioned_instance(
        &self,
        command: ProvisionedInstanceMutation,
    ) -> Result<IdempotentOutcome<RuntimeInstanceRecord>, AuthorizationStateError>;

    /// Atomically replace device delegation state and durable side effects.
    async fn mutate_device_delegation(
        &self,
        command: DeviceDelegationMutation,
    ) -> Result<IdempotentOutcome<DeviceRecord>, AuthorizationStateError>;
}

/// Persistence contract for durable state-changing request results and actions.
#[async_trait]
pub(crate) trait OutboxRepository: Send + Sync {
    /// Load one durable proof result by its authenticated request scope.
    async fn get_idempotency_result(
        &self,
        purpose: &str,
        signer_id: &str,
        request_id: &str,
    ) -> Result<Option<IdempotencyResultRecord>, AuthorizationStateError>;

    /// List dispatchable actions by next-attempt time and action ID.
    async fn list_ready_post_commit_actions(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<PostCommitActionRecord>, AuthorizationStateError>;

    /// Claim one ready action until the supplied protocol timestamp.
    async fn claim_post_commit_action(
        &self,
        action_id: &str,
        now: i64,
        claimed_until: i64,
    ) -> Result<Option<PostCommitActionClaim>, AuthorizationStateError>;

    /// Persist or read the immutable delivery proof for one currently claimed event action.
    async fn prepare_post_commit_event_delivery(
        &self,
        action_id: &str,
        claim_token: &str,
        delivery: Value,
    ) -> Result<Value, AuthorizationStateError>;

    /// Record a failed claimed attempt and schedule its retry.
    async fn fail_post_commit_action(
        &self,
        action_id: &str,
        claim_token: &str,
        next_attempt_at: i64,
        error: String,
    ) -> Result<PostCommitActionRecord, AuthorizationStateError>;

    /// Acknowledge and remove one action only for its current claim.
    async fn acknowledge_post_commit_action(
        &self,
        action_id: &str,
        claim_token: &str,
    ) -> Result<(), AuthorizationStateError>;
}

/// One uniquely fenced ownership claim over a post-commit action.
#[derive(Debug)]
pub(crate) struct PostCommitActionClaim {
    pub(crate) action: PostCommitActionRecord,
    pub(crate) token: String,
}
