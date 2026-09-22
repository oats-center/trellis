use serde::{Deserialize, Serialize};

mod validation;
use serde_json::Value;
use trellis_rs::client::EventDescriptor;
use trellis_runtime_apis::apis::trellis_auth_v1::events::SessionsRevoked;
pub(crate) use validation::{validate_provisioned_identity, validate_user_account_replacement};

use super::domain::PrincipalKind;
use super::AuthorizationStateError;

/// Non-authority profile data for one user principal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileRecord {
    /// Stable user principal ID.
    pub principal_id: String,
    /// Required-nullable user-selected display name.
    pub display_name: Option<String>,
    /// Required-nullable observed email address.
    pub email: Option<String>,
    /// Required-nullable profile image URL.
    pub image_url: Option<String>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Last update time in Unix milliseconds.
    pub updated_at: i64,
    /// Optimistic record version.
    pub version: u64,
}

/// Argon2id credential state for one local user account.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCredentialRecord {
    /// Stable user principal ID.
    pub principal_id: String,
    /// Canonical case-folded login name.
    pub normalized_username: String,
    /// Encoded Argon2id password hash.
    pub password_hash: String,
    /// Version of the bounded password-hash profile.
    pub hash_profile: u32,
    /// Consecutive failed authentication attempts.
    pub failed_attempts: u32,
    /// Required-nullable lock expiry in Unix milliseconds.
    pub locked_until: Option<i64>,
    /// Last password change in Unix milliseconds.
    pub password_changed_at: i64,
    /// Last record update in Unix milliseconds.
    pub updated_at: i64,
    /// Optimistic record version.
    pub version: u64,
}

/// Login portal presentation and provider policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginPortalRecord {
    /// Stable portal ID.
    pub portal_id: String,
    /// Operator-facing portal name.
    pub display_name: String,
    /// Required-nullable external portal entry URL; the built-in portal uses `None`.
    pub entry_url: Option<String>,
    /// Whether this is the non-removable built-in portal.
    pub builtin: bool,
    /// Whether new flows may select this portal.
    pub disabled: bool,
    /// Whether the portal was administratively removed.
    pub removed: bool,
    /// Whether local registration is permitted.
    pub local_registration_enabled: bool,
    /// Ordered configured provider IDs.
    pub provider_ids: Vec<String>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Last update time in Unix milliseconds.
    pub updated_at: i64,
    /// Optimistic record version.
    pub version: u64,
}

/// Login behavior settings attached to one portal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginSettingsRecord {
    /// Owning portal ID.
    pub portal_id: String,
    /// Required-nullable provider selected without a chooser.
    pub default_provider_id: Option<String>,
    /// Whether existing local credentials may authenticate.
    pub local_login_enabled: bool,
    /// Whether unknown federated identities may register accounts.
    pub federated_registration_enabled: bool,
    /// Whether users may select among configured providers.
    pub provider_selection_enabled: bool,
    /// Last update time in Unix milliseconds.
    pub updated_at: i64,
    /// Optimistic record version.
    pub version: u64,
}

/// Administrative lifecycle state for a deployment profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeploymentProfileState {
    /// The deployment may be provisioned and connected.
    Active,
    /// New and existing connections are disabled.
    Disabled,
    /// The deployment has been administratively removed.
    Removed,
}

/// Administrative review policy for device activation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceReviewMode {
    /// An authenticated user may complete activation without an administrator.
    None,
    /// A privileged reviewer must approve activation.
    Required,
}

/// Product-facing deployment metadata independent of runtime evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProfileRecord {
    /// Stable deployment and principal ID.
    pub deployment_id: String,
    /// Service or device deployment class.
    pub kind: PrincipalKind,
    /// Operator-facing name.
    pub display_name: String,
    /// Participant selected before or during provisioning.
    pub participant_id: Option<String>,
    /// Login portal used for device activation.
    pub portal_id: Option<String>,
    /// Administrative review policy for device activation; absent for services.
    pub review_mode: Option<DeviceReviewMode>,
    /// Whether device sessions require user delegation.
    pub requires_device_delegation: bool,
    /// Optional deployment expiry in Unix milliseconds.
    pub expires_at: Option<i64>,
    /// Administrative lifecycle state.
    pub state: DeploymentProfileState,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Last update time in Unix milliseconds.
    pub updated_at: i64,
    /// Optimistic record version.
    pub version: u64,
}

/// Deterministic route from auth-flow evidence to a login portal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalRouteRecord {
    /// Stable route ID.
    pub route_id: String,
    /// Selected portal ID.
    pub portal_id: String,
    /// Required-nullable participant selector.
    pub participant_id: Option<String>,
    /// Required-nullable exact origin selector.
    pub origin: Option<String>,
    /// Required-nullable deployment selector.
    pub deployment_id: Option<String>,
    /// Higher values take precedence.
    pub priority: i64,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Last update time in Unix milliseconds.
    pub updated_at: i64,
    /// Optimistic record version.
    pub version: u64,
}

/// Purpose of a durable single-use account flow.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountFlowKind {
    /// Create or recover the bootstrap administrator account.
    AdminAccount,
    /// Link another provider identity to an existing account.
    IdentityLink,
    /// Replace a local password after proof of account control.
    PasswordReset,
}

/// Lifecycle state of a durable account flow.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountFlowState {
    /// The flow can be consumed once.
    Pending,
    /// The flow completed successfully.
    Consumed,
    /// The flow passed its expiry without completion.
    Expired,
    /// The flow was administratively revoked.
    Revoked,
}

/// Hashed, expiring, single-use account workflow.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountFlowRecord {
    /// Stable flow ID.
    pub flow_id: String,
    /// Flow purpose.
    pub kind: AccountFlowKind,
    /// SHA-256 digest of the bearer secret.
    pub token_hash: String,
    /// Required-nullable target principal.
    pub target_principal_id: Option<String>,
    /// Required-nullable target provider.
    pub target_provider_id: Option<String>,
    /// Required-nullable validated return location.
    pub return_location: Option<String>,
    /// Purpose-specific immutable payload.
    pub payload: Value,
    /// Current lifecycle state.
    pub state: AccountFlowState,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Expiry time in Unix milliseconds.
    pub expires_at: i64,
    /// Required-nullable successful consumption time.
    pub consumed_at: Option<i64>,
    /// Optimistic record version.
    pub version: u64,
}

/// Kind of provisioned workload identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionedIdentityKind {
    /// Service instance identity.
    Service,
    /// Device instance identity.
    Device,
}

/// Lifecycle state of a provisioned workload identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionedIdentityState {
    /// Identity may authenticate.
    Active,
    /// Identity is permanently revoked.
    Revoked,
}

/// Public metadata for an immutable provisioned identity key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionedIdentityRecord {
    /// Digest-derived identity key ID.
    pub identity_key_id: String,
    /// Canonical public identity key.
    pub identity_public_key: String,
    /// Stable workload principal ID.
    pub principal_id: String,
    /// Immutable deployment assignment.
    pub deployment_id: String,
    /// Immutable instance assignment.
    pub instance_id: String,
    /// Workload identity kind.
    pub kind: ProvisionedIdentityKind,
    /// Current lifecycle state.
    pub state: ProvisionedIdentityState,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Required-nullable permanent revocation time.
    pub revoked_at: Option<i64>,
}

/// Lifecycle state of a one-time device provisioning secret.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningSecretState {
    /// Secret can be consumed once.
    Pending,
    /// Secret was consumed successfully.
    Consumed,
    /// Secret passed its expiry.
    Expired,
    /// Secret was administratively revoked.
    Revoked,
}

/// Hashed one-time device provisioning secret.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProvisioningSecretRecord {
    /// Stable secret record ID.
    pub secret_id: String,
    /// Target runtime instance ID.
    pub instance_id: String,
    /// SHA-256 digest of the raw secret.
    pub secret_hash: String,
    /// Current lifecycle state.
    pub state: ProvisioningSecretState,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Expiry time in Unix milliseconds.
    pub expires_at: i64,
    /// Required-nullable successful consumption time.
    pub consumed_at: Option<i64>,
    /// Optimistic record version.
    pub version: u64,
}

/// Lifecycle state of an administrative device activation review.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceActivationReviewState {
    /// Awaiting an administrator decision.
    Pending,
    /// Approved by an administrator.
    Approved,
    /// Rejected by an administrator.
    Rejected,
    /// Expired without a decision.
    Expired,
}

/// Administrative review distinct from user delegation state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceActivationReviewRecord {
    /// Stable review ID.
    pub review_id: String,
    /// Device principal under review.
    pub principal_id: String,
    /// Deployment under review.
    pub deployment_id: String,
    /// Runtime instance under review.
    pub instance_id: String,
    /// Digest of the original activation request.
    pub request_digest: String,
    /// Immutable request payload.
    pub payload: Value,
    /// Current review state.
    pub state: DeviceActivationReviewState,
    /// Request time in Unix milliseconds.
    pub requested_at: i64,
    /// Authoritative server expiry in Unix milliseconds.
    pub expires_at: i64,
    /// Required-nullable user who claimed this activation through Resolve.
    pub activated_by_user_principal_id: Option<String>,
    /// Required-nullable decision time.
    pub decided_at: Option<i64>,
    /// Required-nullable deciding administrator.
    pub decided_by: Option<String>,
    /// Required-nullable decision reason.
    pub reason: Option<String>,
    /// Optimistic record version.
    pub version: u64,
}

/// Reusable administrative capability selection macro.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityGroupRecord {
    /// Stable canonical group key.
    pub group_key: String,
    /// Operator-facing group name.
    pub display_name: String,
    /// Operator-facing group description.
    pub description: String,
    /// Direct concrete Trellis capabilities.
    pub capabilities: Vec<String>,
    /// Transitively included capability-group keys.
    pub included_groups: Vec<String>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Last update time in Unix milliseconds.
    pub updated_at: i64,
    /// Positive optimistic record version.
    pub version: u64,
}

/// Exact provider role mapping in one trusted portal policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalRoleMapping {
    /// Exact configured provider ID.
    pub provider_id: String,
    /// Exact case-sensitive provider role.
    pub role: String,
    /// Direct concrete Trellis capabilities.
    pub direct_capabilities: Vec<String>,
    /// Reusable capability-group keys.
    pub capability_group_keys: Vec<String>,
}

/// Trusted first-party portal preauthorization policy for one participant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalGrantOverrideRecord {
    /// Selected login portal ID.
    pub portal_id: String,
    /// Exact app or agent participant ID.
    pub participant_id: String,
    /// Base direct concrete capabilities.
    pub direct_capabilities: Vec<String>,
    /// Base reusable capability-group keys.
    pub capability_group_keys: Vec<String>,
    /// Exact provider-role mappings.
    pub role_mappings: Vec<PortalRoleMapping>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Last update time in Unix milliseconds.
    pub updated_at: i64,
    /// Positive optimistic record version.
    pub version: u64,
}

/// Administrative provenance for one portal-managed identity authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalGrantBindingRecord {
    /// Stable user principal ID.
    pub principal_id: String,
    /// Exact app or agent participant ID.
    pub participant_id: String,
    /// Portal that last established the authority.
    pub portal_id: String,
    /// Provider observed during the last trusted login.
    pub provider_id: String,
    /// Last verified provider roles.
    pub roles: Vec<String>,
    /// Semantic effective portal-policy digest.
    pub effective_policy_digest: String,
    /// GrantBinding revision established by this portal decision.
    pub grant_revision: u64,
    /// Last binding update time in Unix milliseconds.
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PortalPolicySnapshot {
    pub portal_id: String,
    pub portal_version: u64,
    pub portal_fingerprint: String,
    pub login_settings_version: u64,
    pub login_settings_fingerprint: String,
    pub participant_id: String,
    pub policy_version: Option<u64>,
    pub policy_fingerprint: Option<String>,
    pub capability_group_versions: Vec<(String, u64)>,
    pub capability_group_fingerprints: Vec<(String, String)>,
}

/// Durable result for one authenticated state-changing request ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdempotencyResultRecord {
    /// Digest of purpose, signer, and request ID.
    pub scope_key: String,
    /// Exact proof purpose.
    pub purpose: String,
    /// Authenticated principal or key ID.
    pub signer_id: String,
    /// Caller-generated request ID.
    pub request_id: String,
    /// Digest of the exact canonical request.
    pub request_digest: String,
    /// Replayable successful response.
    pub result: Value,
    /// Commit time in Unix milliseconds.
    pub created_at: i64,
    /// Retention expiry in Unix milliseconds.
    pub expires_at: i64,
}

/// Kind of post-commit side effect.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PostCommitActionKind {
    /// Publish one canonical auth event.
    Event,
    /// Kick exact active NATS connections.
    Kick,
    /// Publish one immutable authorization-context registry entry.
    ContextPublish,
    /// Publish one authorization-context revocation entry.
    ContextRevoke,
    /// Reconcile one committed resource approval with its physical provider.
    ResourceReconcile,
}

/// Durable post-commit side-effect intent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostCommitActionRecord {
    /// Optional action that must be acknowledged before this action is claimable.
    pub predecessor_action_id: Option<String>,
    /// Deterministic action ID.
    pub action_id: String,
    /// Side-effect kind.
    pub kind: PostCommitActionKind,
    /// Canonical adapter payload.
    pub payload: Value,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Number of failed or abandoned claims.
    pub attempts: u32,
    /// Earliest next dispatch time.
    pub next_attempt_at: i64,
    /// Required-nullable active dispatch claim expiry.
    pub claimed_until: Option<i64>,
    /// Required-nullable most recent error.
    pub last_error: Option<String>,
}

pub(crate) fn auth_event_subject<D: EventDescriptor>(
    payload: &Value,
) -> Result<String, AuthorizationStateError> {
    trellis_rs::client::resolve_subject(D::SUBJECT, payload)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
}

pub(crate) fn connection_event_action<D: EventDescriptor>(
    connection: &super::AuthConnectionPresence,
    event_type: &str,
    suffix: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<PostCommitActionRecord, AuthorizationStateError> {
    let action_id = trellis_protocol::digest_json(&serde_json::json!({
        "connectionId": connection.connection_id,
        "event": suffix,
    }))
    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let mut payload = serde_json::json!({
        "eventType": event_type,
        "eventId": format!("evt_{action_id}"),
        "occurredAt": now.to_string(),
        "connectionId": connection.connection_id,
        "sessionId": connection.runtime_connection_id,
        "principalId": connection.principal_id,
        "participantId": connection.participant_id,
        "reason": reason,
    });
    if event_type == "Auth.Connections.Opened" {
        payload
            .as_object_mut()
            .expect("connection event payload is an object")
            .extend([
                (
                    "serverId".to_owned(),
                    serde_json::json!(connection.server_id),
                ),
                (
                    "clientId".to_owned(),
                    serde_json::json!(connection.client_id),
                ),
            ]);
        payload
            .as_object_mut()
            .expect("connection event payload is an object")
            .remove("reason");
    }
    payload["eventSubject"] = serde_json::json!(auth_event_subject::<D>(&payload)?);
    Ok(PostCommitActionRecord {
        predecessor_action_id: None,
        action_id,
        kind: PostCommitActionKind::Event,
        payload,
        created_at: now,
        attempts: 0,
        next_attempt_at: now,
        claimed_until: None,
        last_error: None,
    })
}

pub(crate) fn activation_review_event_action_id(
    review_id: &str,
    event: &str,
) -> Result<String, AuthorizationStateError> {
    trellis_protocol::digest_json(&serde_json::json!({
        "event": event,
        "reviewId": review_id,
    }))
    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
}

pub(crate) fn activation_review_event<D: EventDescriptor>(
    review: &DeviceActivationReviewRecord,
    suffix: &str,
    event_type: &str,
    now: i64,
    fields: Value,
) -> Result<PostCommitActionRecord, AuthorizationStateError> {
    let mut payload = serde_json::json!({
        "eventType": event_type,
        "eventId": format!("evt_{}_{}", review.review_id, suffix),
        "occurredAt": now,
        "deploymentId": review.deployment_id,
        "instanceId": review.instance_id,
    });
    payload
        .as_object_mut()
        .expect("activation event payload is an object")
        .extend(
            fields
                .as_object()
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(
                        "activation event fields must be an object".to_owned(),
                    )
                })?
                .clone(),
        );
    payload["eventSubject"] = serde_json::json!(auth_event_subject::<D>(&payload)?);
    Ok(PostCommitActionRecord {
        predecessor_action_id: None,
        action_id: activation_review_event_action_id(&review.review_id, suffix)?,
        kind: PostCommitActionKind::Event,
        payload,
        created_at: now,
        attempts: 0,
        next_attempt_at: now,
        claimed_until: None,
        last_error: None,
    })
}

/// One canonical `Auth.Sessions.Revoked` event for a session that a bulk
/// revocation has transitioned to revoked.
pub(crate) fn session_revoked_event(
    session_id: &str,
    participant_id: &str,
    principal_id: &str,
    reason: Option<&str>,
    revoked_by: Option<&str>,
    now: i64,
    action_id: String,
) -> Result<PostCommitActionRecord, AuthorizationStateError> {
    let mut payload = serde_json::json!({
        "eventType": "Auth.Sessions.Revoked",
        "eventId": format!("evt_{action_id}"),
        // Generated uint64 event fields travel as decimal strings.
        "occurredAt": now.to_string(),
        "sessionId": session_id,
        "principalId": principal_id,
        "participantId": participant_id,
        "reason": reason,
        "revokedBy": revoked_by,
    });
    payload["eventSubject"] = serde_json::json!(auth_event_subject::<SessionsRevoked>(&payload)?);
    Ok(PostCommitActionRecord {
        predecessor_action_id: None,
        action_id,
        kind: PostCommitActionKind::Event,
        payload,
        created_at: now,
        attempts: 0,
        next_attempt_at: now,
        claimed_until: None,
        last_error: None,
    })
}

/// One principal-scoped kick that removes every live connection of a principal.
pub(crate) fn principal_session_kick(
    principal_id: &str,
    reason: &str,
    now: i64,
    action_id: String,
) -> PostCommitActionRecord {
    PostCommitActionRecord {
        predecessor_action_id: None,
        action_id,
        kind: PostCommitActionKind::Kick,
        payload: serde_json::json!({"principalId": principal_id, "reason": reason}),
        created_at: now,
        attempts: 0,
        next_attempt_at: now,
        claimed_until: None,
        last_error: None,
    }
}

#[cfg(test)]
mod event_subject_tests {
    use super::*;
    use trellis_runtime_apis::apis::trellis_auth_v1::events::{
        ConnectionsClosed, ConnectionsKicked, ConnectionsOpened, DeviceUserAuthoritiesApproved,
        DeviceUserAuthoritiesRequested, DeviceUserAuthoritiesResolved,
        DeviceUserAuthoritiesReviewRequested, GrantsChanged, IssuersRevoked, SessionsRevoked,
    };

    #[test]
    fn auth_post_commit_subjects_are_qualified_and_parameter_tokens_are_canonical() {
        let payload = serde_json::json!({ "deploymentId": "dep.one~* >" });
        let qualified = "events.v1.dHJlbGxpcy5hdXRoQHYx";
        assert_eq!(
            auth_event_subject::<GrantsChanged>(&payload).unwrap(),
            format!("{qualified}.Grants.Changed")
        );
        assert_eq!(
            auth_event_subject::<IssuersRevoked>(&payload).unwrap(),
            format!("{qualified}.Issuers.Revoked")
        );
        assert_eq!(
            auth_event_subject::<SessionsRevoked>(&payload).unwrap(),
            format!("{qualified}.Sessions.Revoked")
        );
        assert_eq!(
            auth_event_subject::<ConnectionsOpened>(&payload).unwrap(),
            format!("{qualified}.Connections.Opened")
        );
        assert_eq!(
            auth_event_subject::<ConnectionsClosed>(&payload).unwrap(),
            format!("{qualified}.Connections.Closed")
        );
        assert_eq!(
            auth_event_subject::<ConnectionsKicked>(&payload).unwrap(),
            format!("{qualified}.Connections.Kicked")
        );
        for subject in [
            auth_event_subject::<DeviceUserAuthoritiesApproved>(&payload).unwrap(),
            auth_event_subject::<DeviceUserAuthoritiesRequested>(&payload).unwrap(),
            auth_event_subject::<DeviceUserAuthoritiesResolved>(&payload).unwrap(),
            auth_event_subject::<DeviceUserAuthoritiesReviewRequested>(&payload).unwrap(),
        ] {
            assert!(subject.starts_with(&format!("{qualified}.DeviceUserAuthorities.")));
            assert!(subject.ends_with("ZGVwLm9uZX4qID4"));
        }
    }
}
