//! Rust-owned authorization state and deterministic authority materialization.
//!
//! This module is the platform-internal ownership boundary for principals,
//! provider identities, sessions, exact participant bindings, desired identity
//! and deployment authority, runtime evidence, materialized authority, and the
//! unsigned state consumed by later authorization-context issuance.
//!
//! Desired authority records an accepted decision. Materialized authority is a
//! separate fail-closed projection of that decision against authority-scoped
//! participant, dependency, resource, and deployment evidence. Session,
//! instance, and activation eligibility is checked only during issuance. Exact
//! permissions remain [`trellis_protocol::GrantSet`] values; platform
//! capabilities never expand those permissions.
//!
//! Context signing, public auth/bootstrap routes, and transport admission are
//! implemented inside this module and composed by the platform runtime.

mod account;
mod api_bindings;
mod application;
mod authority;
mod builtin_semantics;
mod builtins;
mod compiled_evidence;
pub(crate) mod context;
mod domain;
mod ephemeral;
mod evidence;
mod grant_repository;
mod http;

mod issuance;

pub(crate) const DEVICE_ACTIVATION_REVIEW_TTL_MS: i64 = 15 * 60_000;
pub(super) use builtins::{
    auth_runtime_participant_binding, cli_participant_binding, console_participant_binding,
    events_runtime_participant_binding, health_runtime_participant_binding,
    jobs_runtime_participant_binding, portal_participant_binding,
};
pub(crate) use ephemeral::{
    validate_connection_kick_response, AuthConnectionPresence, AuthEphemeralRepository,
    ConsentApproval, ConsentRequest, NatsAuthEphemeralRepository,
};
pub(crate) use grant_repository::{
    ConsentAuthorityPreconditions, ConsentBindingPrecondition, GrantRepository,
};
pub(super) use http::{
    discover_oidc_providers, router as auth_http_router, AuthHttpOptions, NatsBootstrapIssuer,
};
mod model;
pub(crate) use model::{auth_event_subject, connection_event_action};

pub(crate) mod policy;
mod portal_reconciliation;
pub(crate) mod resources;
pub(crate) mod rpc;
mod sqlite;
mod transport;
pub(crate) mod verifier;

pub(super) use transport::{compile_transport_permissions, TransportPermissions};

pub(crate) use api_bindings::{current_api_bindings, resolve_api_bindings};

#[cfg(test)]
mod tests;

pub(crate) use application::repository::IdempotentOutcome;
pub(crate) use application::repository::{
    AccountCreation, AccountFlowCreation, AccountRepository, ActivationReviewClaim,
    ActivationReviewCreation, ActivationReviewDecision, DeploymentProfileCreation,
    DeploymentProfileMutation, DeploymentRepository, DeviceDelegationMutation, DeviceProvisioning,
    DeviceProvisioningSecretConsumption, FirstAdminCompletion, IdentityLinkCompletion,
    LocalLoginAttempt, LoginPortalMutation, OutboxRepository, PasswordChange,
    PasswordResetCompletion, PortalRepository, PortalRouteMutation, PortalRouteRemoval,
    ProviderIdentityUnlink, ProvisionedInstanceMutation, ProvisioningRepository,
    ServiceIdentityProvisioning, SessionCreation, SessionRepository, SessionRevocation,
    UserAccountMutation,
};
pub(crate) use application::validation::validate_login_portal;
pub(crate) use application::{
    AuthService, AuthServiceConfig, ChangePasswordInput, ClaimActivationReviewInput,
    CompleteIdentityLinkInput, CompletePasswordResetInput, CreateAccountFlowInput,
    CreateActivationReviewInput, CreateFederatedUserInput, CreateLocalUserInput,
    CreateSessionInput, CreateUserInput, DecideActivationReviewInput, EnrollDeviceIdentityInput,
    FirstAdminAuthorityTarget, FirstAdminBinding, FirstAdminFederatedRegistration,
    FirstAdminRegistration, LocalAuthentication, ProvisionDeviceInput,
    ProvisionServiceIdentityInput, UpdateUserInput, UserAccount,
};
pub(crate) use authority::validate_principal;
pub(crate) use authority::{AuthorityEvidenceRepository, ContextRepository};
pub(crate) use authority::{IssuanceConnection, IssuanceCredential};
pub(crate) use context::{
    AuthorizationContextBundle, AuthorizationContextIssueRequest, AuthorizationContextService,
    AuthorizationRegistryBinding,
};
pub(crate) use domain::MutationActor;
pub(crate) use domain::{
    validate_ed25519_public_key, verify_detached_ed25519_proof, GrantBindingReplacement,
};
pub use domain::{
    ApprovalMode, ApprovedCapability, ApprovedResource, AuthorizationResourceKind,
    AuthorizationStateError, DelegationCeiling, DeploymentRecord, DeviceDelegationRecord,
    DeviceDelegationState, DeviceRecord, DeviceState, GrantBinding, GrantBindingState,
    GrantOwnerKind, IssuableAuthorizationState, NewSession, ParticipantBindingRecord,
    ParticipantBindingState, PortalGrantProvenance, PrincipalKind, PrincipalRecord, PrincipalState,
    ProviderIdentityLink, ResourceBindingEvidence, ResourceBindingState, ResourceCommitment,
    ResourceProviderIdentity, RuntimeInstanceRecord, RuntimeInstanceState, SessionRecord,
    SessionState, MAX_PROTOCOL_INTEGER,
};
pub(crate) use model::PortalPolicySnapshot;
pub(crate) use model::{activation_review_event, activation_review_event_action_id};
pub use model::{
    AccountFlowKind, AccountFlowRecord, AccountFlowState, CapabilityGroupRecord,
    DeploymentProfileRecord, DeploymentProfileState, DeviceActivationReviewRecord,
    DeviceActivationReviewState, DeviceProvisioningSecretRecord, DeviceReviewMode,
    IdempotencyResultRecord, LocalCredentialRecord, LoginPortalRecord, LoginSettingsRecord,
    PortalGrantBindingRecord, PortalGrantOverrideRecord, PortalRoleMapping, PortalRouteRecord,
    PostCommitActionKind, PostCommitActionRecord, ProvisionedIdentityKind,
    ProvisionedIdentityRecord, ProvisionedIdentityState, ProvisioningSecretState,
    UserProfileRecord,
};
pub(crate) use policy::{
    participant_resource_commitments, portal_policy_snapshot, resolve_portal_authority_selection,
    ProviderLoginAttributes,
};
pub(crate) use portal_reconciliation::{
    portal_policy_reconciliation, PortalPolicyReconciliationHandle,
};
pub(crate) use sqlite::AuthTelemetrySnapshot;
pub use sqlite::SqliteAuthorizationStore;
