//! Trusted startup provisioning for built-in live providers.
//!
//! Built-in roles that serve a live surface (`Health.Watch`, `Jobs.Watch`,
//! `Events.Watch`, Platform Operation observation) need a normal authenticated
//! provider connection with a current signed context. Those providers sign every offer and data frame with their
//! runtime key, so they must not reuse a fixed event digest or present an
//! unsigned process identity.
//!
//! This module owns **provisioning only**: it ensures each role's long-lived
//! provisioned identity exists under its reserved deployment, materializes the
//! matching authority and resource bindings, and returns the native identity
//! seed the runtime later connects with through the ordinary service bootstrap
//! and Auth Callout. It never constructs unchecked callers or issues a context
//! itself.

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::Value;
use trellis_protocol::{digest_json, GrantOwnerKind};
use trellis_rs::client::SessionAuth;

use super::auth::{
    self, AuthService, AuthorityEvidenceRepository, DelegationCeiling, DeploymentRepository,
    GrantBindingReplacement, GrantBindingState, IdempotencyResultRecord, PrincipalKind,
    PrincipalRecord, PrincipalState, ProvisionedIdentityKind, ProvisionedIdentityRecord,
    ProvisionedIdentityState, ProvisioningRepository, ResourceBindingEvidence,
    RuntimeInstanceRecord, RuntimeInstanceState, SqliteAuthorizationStore,
};
use super::{ensure_builtin_provider_deployment, RuntimeError};

/// Built-in roles that serve a live observation surface.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LiveProviderRole {
    /// Platform Operation provider for the Auth/Core/State public routes.
    Platform,
    /// Health API live provider.
    Health,
    /// Jobs API live provider.
    Jobs,
    /// Events API live provider.
    Events,
}

impl LiveProviderRole {
    /// Return the exact configuration key for this role's seed file.
    #[must_use]
    pub const fn config_key(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Health => "health",
            Self::Jobs => "jobs",
            Self::Events => "events",
        }
    }

    /// Return every built-in live provider role in startup order.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Platform, Self::Health, Self::Jobs, Self::Events]
    }

    /// Return the roles whose subsystem runs in the selected runtime mode.
    #[must_use]
    pub fn roles_for_mode(mode: crate::RuntimeMode) -> Vec<Self> {
        mode.subsystems()
            .iter()
            .map(|subsystem| match subsystem {
                crate::SubsystemName::Platform => Self::Platform,
                crate::SubsystemName::Jobs => Self::Jobs,
                crate::SubsystemName::Health => Self::Health,
                crate::SubsystemName::Events => Self::Events,
            })
            .collect()
    }

    /// Return the reserved deployment that owns this role's live surface.
    #[must_use]
    pub const fn deployment_id(self) -> &'static str {
        match self {
            Self::Platform => "dep_trellis_auth_runtime",
            Self::Health => "dep_trellis_health_runtime",
            Self::Jobs => "dep_trellis_jobs_runtime",
            Self::Events => "dep_trellis_events_runtime",
        }
    }

    /// Return the human-readable deployment name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Platform => "Trellis Platform Live Provider",
            Self::Health => "Trellis Health Live Provider",
            Self::Jobs => "Trellis Jobs Live Provider",
            Self::Events => "Trellis Events Live Provider",
        }
    }

    /// Return the generated participant id and package evidence for this role.
    fn participant_binding(self, now: i64) -> Result<auth::ParticipantBindingRecord, RuntimeError> {
        let binding = match self {
            Self::Platform => auth::auth_runtime_participant_binding(now),
            Self::Health => auth::health_runtime_participant_binding(now),
            Self::Jobs => auth::jobs_runtime_participant_binding(now),
            Self::Events => auth::events_runtime_participant_binding(now),
        };
        binding.map_err(|error| RuntimeError::Platform(error.to_string()))
    }
}

/// Per-role delivery slots for built-in live provider owners.
///
/// Subsystems subscribe before serving their live routers; the supervisor
/// installs each owner after the bootstrap listener is serving and the
/// ordinary native provider bootstrap has completed. A live-capable router
/// that never receives its owner fails startup instead of serving without
/// one.
#[derive(Clone)]
pub(crate) struct LiveProviderSlots {
    senders: std::sync::Arc<
        std::collections::BTreeMap<LiveProviderRole, tokio::sync::watch::Sender<OwnerSlot>>,
    >,
}

impl std::fmt::Debug for LiveProviderSlots {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveProviderSlots")
            .finish_non_exhaustive()
    }
}

type OwnerSlot = Option<trellis_rs::service::LiveProviderOwner>;

impl LiveProviderSlots {
    pub(crate) fn new() -> Self {
        let mut senders = std::collections::BTreeMap::new();
        for role in LiveProviderRole::all() {
            let (sender, _receiver) = tokio::sync::watch::channel(None);
            senders.insert(role, sender);
        }
        Self {
            senders: std::sync::Arc::new(senders),
        }
    }

    pub(crate) fn receiver(
        &self,
        role: LiveProviderRole,
    ) -> tokio::sync::watch::Receiver<OwnerSlot> {
        self.senders
            .get(&role)
            .expect("every live provider role has a delivery slot")
            .subscribe()
    }

    pub(crate) fn install(
        &self,
        role: LiveProviderRole,
        owner: trellis_rs::service::LiveProviderOwner,
    ) {
        if let Some(sender) = self.senders.get(&role) {
            let _ = sender.send(Some(owner));
        }
    }
}

/// Await one role's installed provider owner while honoring shutdown.
pub(crate) async fn await_live_owner(
    receiver: &mut tokio::sync::watch::Receiver<OwnerSlot>,
    stop: &crate::shutdown::StopHandle,
) -> Option<trellis_rs::service::LiveProviderOwner> {
    loop {
        if let Some(owner) = receiver.borrow_and_update().clone() {
            return Some(owner);
        }
        tokio::select! {
            changed = receiver.changed() => {
                if changed.is_err() {
                    return None;
                }
            }
            () = stop.stopped() => return None,
        }
    }
}

/// Read one role's provisioned seed without creating or provisioning it.
///
/// All-in-one startup writes managed defaults in the platform owner; split
/// processes read the operator-distributed file. A missing file is a clear
/// configuration error rather than a silent replacement identity.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] for unreadable, missing, or malformed
/// seed files.
pub fn load_live_provider_seed(
    config: &crate::RuntimeConfig,
    role: LiveProviderRole,
) -> Result<String, RuntimeError> {
    let path = resolve_live_provider_seed_file(config, role)?;
    let contents = std::fs::read_to_string(&path).map_err(|error| {
        RuntimeError::Platform(format!(
            "failed to read live provider seed '{}': {error}",
            path.display()
        ))
    })?;
    let seed = contents.trim().to_owned();
    SessionAuth::from_seed_base64url(&seed).map_err(|error| {
        RuntimeError::Platform(format!(
            "invalid live provider seed '{}': {error}",
            path.display()
        ))
    })?;
    Ok(seed)
}

/// Resolved provisioning record for one built-in live provider role.
#[derive(Clone, Debug)]
pub struct ProvisionedLiveProvider {
    /// Generated participant id installed for this role.
    pub participant_id: String,
    /// Reserved deployment the identity is provisioned under.
    pub deployment_id: String,
    /// Runtime instance id installed for this identity.
    #[allow(
        dead_code,
        reason = "recorded for split-mode diagnostics and startup reports"
    )]
    pub instance_id: String,
    /// Participant binding used for package evidence and grant selection.
    #[allow(
        dead_code,
        reason = "retained for route registration as live routers migrate"
    )]
    pub participant: auth::ParticipantBindingRecord,
}

/// Resolve the configured seed file for one role.
///
/// Defaults derive from the event session seed file's parent directory so a
/// managed-local deployment needs no extra manual provisioning. When neither an
/// explicit path nor a usable parent is available this returns a clear
/// configuration error rather than guessing a working directory.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] when the path cannot be derived.
pub fn resolve_live_provider_seed_file(
    config: &crate::RuntimeConfig,
    role: LiveProviderRole,
) -> Result<PathBuf, RuntimeError> {
    let explicit = config
        .live_provider_seed_files
        .as_ref()
        .and_then(|files| match role {
            LiveProviderRole::Platform => files.platform.clone(),
            LiveProviderRole::Health => files.health.clone(),
            LiveProviderRole::Jobs => files.jobs.clone(),
            LiveProviderRole::Events => files.events.clone(),
        });
    if let Some(path) = explicit {
        return Ok(path);
    }
    let parent = config
        .event_session_seed_file
        .as_ref()
        .and_then(|path| path.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            RuntimeError::Platform(format!(
                "live_provider_seed_files.{} is required when the event session seed file has no parent directory",
                role.config_key()
            ))
        })?;
    Ok(parent
        .join("live-providers")
        .join(format!("{}.seed", role.config_key())))
}

/// Ensure one role's native identity seed file exists and return its seed.
///
/// Missing managed default files are created create-exclusive with mode `0600`
/// in a `0700` directory using the existing cryptographically random identity
/// generator. An explicitly configured file must already exist and validate;
/// this function never silently replaces an operator-supplied identity. An
/// existing file whose identity is already bound incompatibly is an error.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] for unreadable files, malformed seeds, or
/// conflicting installed identities.
pub async fn ensure_live_provider_seed(
    service: &AuthService<SqliteAuthorizationStore>,
    config: &crate::RuntimeConfig,
    role: LiveProviderRole,
    now: i64,
) -> Result<ProvisionedLiveProvider, RuntimeError> {
    let path = resolve_live_provider_seed_file(config, role)?;
    let explicit = config
        .live_provider_seed_files
        .as_ref()
        .and_then(|files| match role {
            LiveProviderRole::Platform => files.platform.as_ref(),
            LiveProviderRole::Health => files.health.as_ref(),
            LiveProviderRole::Jobs => files.jobs.as_ref(),
            LiveProviderRole::Events => files.events.as_ref(),
        })
        .is_some();
    let seed = match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let seed = contents.trim().to_owned();
            // Validate before use so a corrupt file fails closed.
            SessionAuth::from_seed_base64url(&seed).map_err(|error| {
                RuntimeError::Platform(format!(
                    "invalid live provider seed '{}': {error}",
                    path.display()
                ))
            })?;
            seed
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !explicit => {
            create_managed_seed_file(&path)?
        }
        Err(error) => {
            return Err(RuntimeError::Platform(format!(
                "failed to read live provider seed '{}': {error}",
                path.display()
            )));
        }
    };
    provision_live_provider(service, role, &seed, now).await
}

fn create_managed_seed_file(path: &Path) -> Result<String, RuntimeError> {
    let parent = path.parent().ok_or_else(|| {
        RuntimeError::Platform(format!(
            "live provider seed path '{}' has no parent directory",
            path.display()
        ))
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        RuntimeError::Platform(format!(
            "failed to create live provider seed directory '{}': {error}",
            parent.display()
        ))
    })?;
    set_private_directory_mode(parent)?;
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|error| RuntimeError::Platform(format!("identity RNG failed: {error}")))?;
    let encoded = URL_SAFE_NO_PAD.encode(seed);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            set_private_file_mode(path)?;
            file.write_all(format!("{encoded}\n").as_bytes())
                .map_err(|error| {
                    RuntimeError::Platform(format!(
                        "failed to write live provider seed '{}': {error}",
                        path.display()
                    ))
                })?;
            Ok(encoded)
        }
        // Another process created it first; read and validate that identity.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let contents = std::fs::read_to_string(path).map_err(|error| {
                RuntimeError::Platform(format!(
                    "failed to read live provider seed '{}': {error}",
                    path.display()
                ))
            })?;
            let seed = contents.trim().to_owned();
            SessionAuth::from_seed_base64url(&seed).map_err(|error| {
                RuntimeError::Platform(format!(
                    "invalid live provider seed '{}': {error}",
                    path.display()
                ))
            })?;
            Ok(seed)
        }
        Err(error) => Err(RuntimeError::Platform(format!(
            "failed to create live provider seed '{}': {error}",
            path.display()
        ))),
    }
}

#[cfg(unix)]
fn set_private_directory_mode(path: &Path) -> Result<(), RuntimeError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        RuntimeError::Platform(format!(
            "failed to set mode 0700 on '{}': {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_directory_mode(_path: &Path) -> Result<(), RuntimeError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_mode(path: &Path) -> Result<(), RuntimeError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        RuntimeError::Platform(format!(
            "failed to set mode 0600 on '{}': {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_file_mode(_path: &Path) -> Result<(), RuntimeError> {
    Ok(())
}

/// Persist the matching provisioned identity and materialized authority.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] when the identity is already bound
/// incompatibly or any provisioning record cannot be committed.
pub async fn provision_live_provider(
    service: &AuthService<SqliteAuthorizationStore>,
    role: LiveProviderRole,
    seed: &str,
    now: i64,
) -> Result<ProvisionedLiveProvider, RuntimeError> {
    let auth = SessionAuth::from_seed_base64url(seed)
        .map_err(|error| RuntimeError::Platform(format!("invalid live provider seed: {error}")))?;
    let participant = role.participant_binding(now)?;
    let deployment_id = role.deployment_id();
    ensure_builtin_provider_deployment(
        service.repository(),
        deployment_id,
        role.display_name(),
        &participant,
        now,
    )
    .await?;
    register_live_provider_routes(service, role, &participant, now).await?;
    let identity_key_id = auth::validate_ed25519_public_key("identityPublicKey", &auth.session_key)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;

    // A stable identity key can never be reassigned to a different deployment
    // or principal; an existing incompatible binding is an error.
    if let Some(existing) = service
        .repository()
        .get_provisioned_identity(&identity_key_id)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
    {
        if existing.principal_id != deployment_id || existing.deployment_id != deployment_id {
            return Err(RuntimeError::Platform(format!(
                "live provider seed for {} is already bound to principal '{}' deployment '{}'",
                role.config_key(),
                existing.principal_id,
                existing.deployment_id
            )));
        }
        let instance_id = existing.instance_id.clone();
        return Ok(ProvisionedLiveProvider {
            participant_id: participant.participant_id.clone(),
            deployment_id: deployment_id.to_owned(),
            instance_id,
            participant,
        });
    }

    let instance_id = ulid::Ulid::new().to_string();
    service
        .repository()
        .install_runtime_identity(
            RuntimeInstanceRecord {
                instance_id: instance_id.clone(),
                deployment_id: deployment_id.to_owned(),
                principal_id: deployment_id.to_owned(),
                state: RuntimeInstanceState::Active,
                created_at: now,
                updated_at: now,
                version: 1,
            },
            ProvisionedIdentityRecord {
                identity_key_id,
                identity_public_key: auth.session_key.clone(),
                principal_id: deployment_id.to_owned(),
                deployment_id: deployment_id.to_owned(),
                instance_id: instance_id.clone(),
                kind: ProvisionedIdentityKind::Service,
                state: ProvisionedIdentityState::Active,
                created_at: now,
                revoked_at: None,
            },
        )
        .await
        .map_err(|error| {
            RuntimeError::Platform(format!("install live provider identity: {error}"))
        })?;
    Ok(ProvisionedLiveProvider {
        participant_id: participant.participant_id.clone(),
        deployment_id: deployment_id.to_owned(),
        instance_id,
        participant,
    })
}

/// Register the built-in provider's implemented routes for transport compilation.
///
/// The generated package evidence already carries the participant's implemented
/// APIs; this records the exact projected route set the transport compiler reads
/// so a live-capable built-in provider receives its own observe/publish grants.
async fn register_live_provider_routes(
    service: &AuthService<SqliteAuthorizationStore>,
    role: LiveProviderRole,
    participant: &auth::ParticipantBindingRecord,
    now: i64,
) -> Result<(), RuntimeError> {
    let deployment_id = role.deployment_id();
    let installed_revision = service
        .repository()
        .get_installed_participant_record(participant.participant_id.clone(), None)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .map(|(revision, _)| revision)
        .ok_or_else(|| {
            RuntimeError::Platform(format!(
                "built-in participant '{}' installation is missing",
                participant.participant_id
            ))
        })?;
    let current = service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::Deployment,
            deployment_id.to_owned(),
            participant.participant_id.clone(),
        )
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let expected_revision = current.as_ref().map_or(0, |binding| binding.revision);
    if current.as_ref().is_some_and(|binding| {
        binding.installed_revision == installed_revision
            && binding.state == GrantBindingState::Active
    }) {
        return Ok(());
    }
    let grants = participant
        .resolve()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .select_grants(&[])
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let digest = digest_json(&serde_json::json!({
        "ownerId": deployment_id,
        "participantId": participant.participant_id,
        "installedRevision": installed_revision,
        "grants": grants,
    }))
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    service
        .repository()
        .set_grant_binding(
            GrantBindingReplacement {
                owner_kind: GrantOwnerKind::Deployment,
                owner_id: deployment_id.to_owned(),
                participant_id: participant.participant_id.clone(),
                installed_revision,
                grants: grants.clone(),
                approval_mode: auth::ApprovalMode::Exact,
                approved_capabilities: Vec::new(),
                approved_resources: auth::policy::participant_resource_commitments(participant)
                    .map_err(|error| RuntimeError::Platform(error.to_string()))?,
                delegation_ceiling: DelegationCeiling {
                    capabilities: Vec::new(),
                    exact_restrictions: Some(grants),
                    platform_privileges: Vec::new(),
                },
                approval_decision_digest: digest.clone(),
                companion_approved: false,
                platform_privileges: Vec::new(),
                expected_revision,
                expected_current_installed_revision: None,
                state: GrantBindingState::Active,
                expires_at: None,
                provenance: None,
            },
            IdempotencyResultRecord {
                scope_key: digest.clone(),
                purpose: "live_provider.binding.start".to_owned(),
                signer_id: deployment_id.to_owned(),
                request_id: digest.clone(),
                request_digest: digest,
                result: Value::Null,
                created_at: now,
                expires_at: auth::MAX_PROTOCOL_INTEGER as i64,
            },
        )
        .await
        .map_err(|error| RuntimeError::Platform(format!("bind live provider: {error}")))?;
    Ok(())
}

/// Ensure the reserved deployment exists and is active for a live provider role.
///
/// This is the generic form of the trusted built-in setup used by the event
/// runtime; it creates the deployment profile and deployment evidence only, and
/// never issues an authorization context.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] when the profile cannot be committed.
#[allow(
    dead_code,
    reason = "part of the built-in live-provider startup contract; consumed as routers migrate"
)]
pub async fn ensure_live_provider_deployment_profile(
    service: &AuthService<SqliteAuthorizationStore>,
    role: LiveProviderRole,
    participant: &auth::ParticipantBindingRecord,
    now: i64,
) -> Result<(), RuntimeError> {
    let deployment_id = role.deployment_id();
    if service
        .repository()
        .get_deployment_profile(deployment_id)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .is_none()
    {
        let principal = PrincipalRecord {
            principal_id: deployment_id.to_owned(),
            kind: PrincipalKind::Service,
            state: PrincipalState::Active,
            created_at: now,
            updated_at: now,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        auth::validate_principal(&principal)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let request_digest = digest_json(&serde_json::json!({
            "deploymentId": deployment_id,
            "participantId": participant.participant_id,
            "role": role.config_key(),
        }))
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        service
            .repository()
            .create_deployment_profile(auth::DeploymentProfileCreation {
                principal,
                profile: auth::DeploymentProfileRecord {
                    deployment_id: deployment_id.to_owned(),
                    kind: PrincipalKind::Service,
                    display_name: role.display_name().to_owned(),
                    participant_id: Some(participant.participant_id.clone()),
                    portal_id: None,
                    review_mode: None,
                    requires_device_delegation: false,
                    expires_at: None,
                    state: auth::DeploymentProfileState::Active,
                    created_at: now,
                    updated_at: now,
                    version: 1,
                },
                idempotency: IdempotencyResultRecord {
                    scope_key: request_digest.clone(),
                    purpose: "live_provider.deployment.start".to_owned(),
                    signer_id: "system:startup".to_owned(),
                    request_id: "builtin-v1".to_owned(),
                    request_digest,
                    result: serde_json::json!({ "deploymentId": deployment_id }),
                    created_at: now,
                    expires_at: auth::MAX_PROTOCOL_INTEGER as i64,
                },
                actions: Vec::new(),
            })
            .await
            .map_err(|error| {
                RuntimeError::Platform(format!("create live provider deployment: {error}"))
            })?;
    }
    service
        .repository()
        .put_deployment_evidence(auth::DeploymentRecord {
            deployment_id: deployment_id.to_owned(),
            participant_id: participant.participant_id.clone(),
            participant_kind: participant.participant_kind,
            active: true,
            expires_at: None,
        })
        .await
        .map_err(|error| RuntimeError::Platform(format!("store live deployment: {error}")))?;
    Ok(())
}

/// Ensure one live provider's resource commitments are recorded.
///
/// Built-in live providers declare no participant resources today; the function
/// exists so the split Health path records the same empty commitment set the
/// all-in-one path records, keeping grant identity equal across modes.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] when the commitment set cannot be stored.
pub async fn ensure_live_provider_resources(
    service: &AuthService<SqliteAuthorizationStore>,
    role: LiveProviderRole,
    provider: &ProvisionedLiveProvider,
) -> Result<(), RuntimeError> {
    if role == LiveProviderRole::Platform {
        // The Platform provider shares `dep_trellis_auth_runtime` with the Auth
        // runtime's event and operation sessions; those KV resources are bound
        // by the auth-runtime setup and must not be replaced with an empty set,
        // or the shared grant binding stops matching its resolved authority.
        return Ok(());
    }
    let resources: Vec<ResourceBindingEvidence> = Vec::new();
    let installed_revision = service
        .repository()
        .get_installed_participant_record(provider.participant_id.clone(), None)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .map(|(revision, _)| revision)
        .ok_or_else(|| {
            RuntimeError::Platform(format!(
                "built-in participant '{}' installation is missing",
                provider.participant_id
            ))
        })?;
    service
        .repository()
        .replace_resource_bindings(
            GrantOwnerKind::Deployment,
            provider.deployment_id.clone(),
            provider.participant_id.clone(),
            installed_revision,
            resources,
        )
        .await
        .map_err(|error| RuntimeError::Platform(format!("bind live provider resources: {error}")))
}

/// Return whether a resource binding list is empty for the built-in role.
///
/// Retained as the single place that answers the "does this role declare
/// resources" question for startup orchestration.
#[must_use]
#[allow(
    dead_code,
    reason = "answers the resource-declaration question for startup orchestration"
)]
pub fn live_provider_declares_resources(role: LiveProviderRole) -> bool {
    match role {
        LiveProviderRole::Platform
        | LiveProviderRole::Health
        | LiveProviderRole::Jobs
        | LiveProviderRole::Events => false,
    }
}

/// Validate that a live-capable router has a live provider owner.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] when the router serves a Live or
/// Operation watch route without an installed provider owner.
#[allow(
    dead_code,
    reason = "part of the built-in live-provider startup contract; consumed as routers migrate"
)]
pub fn require_live_provider_owner(
    router_serves_live_surface: bool,
    owner_present: bool,
) -> Result<(), RuntimeError> {
    if router_serves_live_surface && !owner_present {
        return Err(RuntimeError::Platform(
            "a router serving a Live or Operation watch route requires a live provider owner"
                .to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_use_distinct_reserved_deployments_and_config_keys() {
        assert_eq!(
            LiveProviderRole::Platform.deployment_id(),
            "dep_trellis_auth_runtime"
        );
        assert_eq!(
            LiveProviderRole::Health.deployment_id(),
            "dep_trellis_health_runtime"
        );
        assert_ne!(
            LiveProviderRole::Platform.config_key(),
            LiveProviderRole::Health.config_key()
        );
        assert!(!live_provider_declares_resources(
            LiveProviderRole::Platform
        ));
        assert!(!live_provider_declares_resources(LiveProviderRole::Health));
    }

    #[test]
    fn seed_paths_default_under_the_event_seed_parent() {
        let config = crate::RuntimeConfig::from_toml_str(
            "event_session_seed_file = \"/data/runtime/event.seed\"\n",
        )
        .expect("valid config");
        assert_eq!(
            resolve_live_provider_seed_file(&config, LiveProviderRole::Platform).unwrap(),
            PathBuf::from("/data/runtime/live-providers/platform.seed")
        );
        assert_eq!(
            resolve_live_provider_seed_file(&config, LiveProviderRole::Health).unwrap(),
            PathBuf::from("/data/runtime/live-providers/health.seed")
        );
    }

    #[test]
    fn explicit_seed_paths_win_and_bare_relative_defaults_error() {
        let config = crate::RuntimeConfig::from_toml_str(
            "event_session_seed_file = \"event.seed\"\n\
             [live_provider_seed_files]\n\
             health = \"/secrets/health.seed\"\n",
        )
        .expect("valid config");
        assert_eq!(
            resolve_live_provider_seed_file(&config, LiveProviderRole::Health).unwrap(),
            PathBuf::from("/secrets/health.seed")
        );
        assert!(resolve_live_provider_seed_file(&config, LiveProviderRole::Platform).is_err());
    }

    #[test]
    fn managed_seed_file_is_private_and_stable() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("live-providers").join("platform.seed");
        let first = create_managed_seed_file(&path).expect("create managed seed");
        SessionAuth::from_seed_base64url(&first).expect("generated seed must validate");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let parent = std::fs::metadata(path.parent().expect("seed parent"))
                .expect("seed directory metadata")
                .permissions()
                .mode()
                & 0o777;
            let file = std::fs::metadata(&path)
                .expect("seed file metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(parent, 0o700, "seed directory must be private");
            assert_eq!(file, 0o600, "seed file must be private");
        }
        // Re-invocation returns the persisted identity instead of rotating it.
        let second = create_managed_seed_file(&path).expect("read persisted managed seed");
        assert_eq!(first, second, "managed seed identity must persist");
        // A corrupted existing seed is rejected, never silently replaced.
        std::fs::write(&path, "not-a-valid-seed\n").expect("corrupt managed seed");
        assert!(
            create_managed_seed_file(&path).is_err(),
            "invalid managed seed must be rejected"
        );
    }

    #[test]
    fn live_surface_requires_an_owner() {
        assert!(require_live_provider_owner(true, true).is_ok());
        assert!(require_live_provider_owner(false, false).is_ok());
        assert!(require_live_provider_owner(true, false).is_err());
    }
}
