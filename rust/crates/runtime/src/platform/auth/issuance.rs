use trellis_protocol::{
    ApiSurfaceKind, AuthorizationPrincipalKind, GrantSet, ParticipantKind, ParticipantResourceKind,
    PermissionAction, PermissionAtom, PermissionTarget,
};

use super::authority::{IssuanceCredentialRecord, IssuanceSnapshot};
use super::{
    AuthorizationStateError, DeviceDelegationState, DeviceState, GrantBindingState, GrantOwnerKind,
    IssuableAuthorizationState, PrincipalKind, PrincipalState, ProvisionedIdentityKind,
    ProvisionedIdentityState, ResourceBindingState, RuntimeInstanceState, SessionState,
};

pub(super) fn resolve_snapshot(
    snapshot: IssuanceSnapshot,
    now: i64,
) -> Result<IssuableAuthorizationState, AuthorizationStateError> {
    super::domain::require_protocol_timestamp("now", now)?;
    let IssuanceSnapshot {
        connection,
        credential,
        principal,
        binding,
        participant,
        resources,
        issuer,
    } = snapshot;
    if principal.state != PrincipalState::Active {
        return Err(AuthorizationStateError::PrincipalInactive);
    }
    if issuer.state != trellis_protocol::AuthorizationIssuerState::Active {
        return Err(AuthorizationStateError::IssuerMissing);
    }
    if binding.state != GrantBindingState::Active
        || binding.expires_at.is_some_and(|expiry| now >= expiry)
    {
        return Err(AuthorizationStateError::NotAuthorized);
    }
    if participant.participant_id != binding.participant_id {
        return Err(AuthorizationStateError::ParticipantMissing);
    }
    let participant_projection = participant.resolve()?;
    let mut expiries = vec![binding.expires_at];
    let (
        principal_kind,
        login_session_id,
        identity_key_id,
        deployment_id,
        instance_id,
        companion_available,
    ) = match credential {
        IssuanceCredentialRecord::Login(login) => {
            if principal.kind != PrincipalKind::User
                || login.principal_id != principal.principal_id
                || login.participant_id != participant.participant_id
                || login.participant_kind != participant.participant_kind
                || !matches!(
                    participant.participant_kind,
                    ParticipantKind::App | ParticipantKind::Agent
                )
                || binding.owner_kind != GrantOwnerKind::User
                || binding.owner_id != principal.principal_id
            {
                return Err(AuthorizationStateError::NotAuthorized);
            }
            match login.state {
                SessionState::Active => {}
                SessionState::Expired => return Err(AuthorizationStateError::SessionExpired),
                SessionState::Revoked => return Err(AuthorizationStateError::SessionRevoked),
            }
            if login.expires_at.is_some_and(|expiry| now >= expiry) {
                return Err(AuthorizationStateError::SessionExpired);
            }
            expiries.push(login.expires_at);
            (
                AuthorizationPrincipalKind::User,
                Some(login.session_id),
                None,
                None,
                None,
                true,
            )
        }
        IssuanceCredentialRecord::Native(native) => {
            let super::authority::NativeIssuanceCredentialRecord {
                identity,
                instance,
                deployment,
                device,
                delegation,
                delegation_session,
                delegation_principal,
                delegation_binding,
            } = *native;
            if identity.state != ProvisionedIdentityState::Active || identity.revoked_at.is_some() {
                return Err(AuthorizationStateError::IdentityMissing);
            }
            if identity.principal_id != principal.principal_id
                || identity.instance_id != instance.instance_id
                || identity.deployment_id != deployment.deployment_id
                || instance.principal_id != principal.principal_id
                || instance.deployment_id != deployment.deployment_id
                || deployment.participant_id != participant.participant_id
                || deployment.participant_kind != participant.participant_kind
                || binding.owner_kind != GrantOwnerKind::Deployment
                || binding.owner_id != deployment.deployment_id
            {
                return Err(AuthorizationStateError::NotAuthorized);
            }
            if instance.state != RuntimeInstanceState::Active {
                return Err(AuthorizationStateError::InstanceInactive);
            }
            if !deployment.active || deployment.expires_at.is_some_and(|expiry| now >= expiry) {
                return Err(AuthorizationStateError::DeploymentInactive);
            }
            expiries.push(deployment.expires_at);
            let (kind, companion_available) =
                match (principal.kind, identity.kind, participant.participant_kind) {
                    (
                        PrincipalKind::Service,
                        ProvisionedIdentityKind::Service,
                        ParticipantKind::Service,
                    ) => (AuthorizationPrincipalKind::Service, true),
                    (
                        PrincipalKind::Device,
                        ProvisionedIdentityKind::Device,
                        ParticipantKind::Device,
                    ) => {
                        let device = device.ok_or(AuthorizationStateError::DeviceInactive)?;
                        if device.state != DeviceState::Active
                            || device.principal_id != principal.principal_id
                            || device.deployment_id != deployment.deployment_id
                        {
                            return Err(AuthorizationStateError::DeviceInactive);
                        }
                        let companion_available = if let Some(delegation) = delegation {
                            if delegation.principal_id != principal.principal_id
                                || delegation.deployment_id != deployment.deployment_id
                            {
                                return Err(AuthorizationStateError::ActivationMissing);
                            }
                            if delegation.state != DeviceDelegationState::Active {
                                false
                            } else {
                                let available = (|| {
                                    if delegation.expires_at.is_some_and(|expiry| now >= expiry) {
                                        return Err(AuthorizationStateError::DelegationExpired);
                                    }
                                    if delegation.companion_participant_id
                                        != participant_projection.companion_participant_id
                                        || delegation.required
                                            != participant_projection.companion_required
                                        || delegation.device_grant_revision
                                            != Some(binding.revision)
                                    {
                                        return Err(AuthorizationStateError::ActivationMissing);
                                    }
                                    let session = delegation_session
                                        .as_ref()
                                        .ok_or(AuthorizationStateError::ActivationMissing)?;
                                    if Some(session.session_id.as_str())
                                        != delegation.user_login_session_id.as_deref()
                                        || Some(session.participant_id.as_str())
                                            != delegation.companion_participant_id.as_deref()
                                        || Some(session.participant_kind)
                                            != participant_projection.companion_participant_kind
                                        || session.state != SessionState::Active
                                        || session.expires_at.is_some_and(|expiry| now >= expiry)
                                    {
                                        return Err(AuthorizationStateError::ActivationMissing);
                                    }
                                    let child_principal = delegation_principal
                                        .as_ref()
                                        .ok_or(AuthorizationStateError::ActivationMissing)?;
                                    if child_principal.principal_id != session.principal_id
                                        || child_principal.kind != PrincipalKind::User
                                        || child_principal.state != PrincipalState::Active
                                    {
                                        return Err(AuthorizationStateError::ActivationMissing);
                                    }
                                    let child_binding = delegation_binding
                                        .as_ref()
                                        .ok_or(AuthorizationStateError::ActivationMissing)?;
                                    if child_binding.owner_kind != GrantOwnerKind::User
                                        || child_binding.owner_id != session.principal_id
                                        || child_binding.participant_id != session.participant_id
                                        || delegation.child_grant_revision
                                            != Some(child_binding.revision)
                                        || child_binding.state != GrantBindingState::Active
                                        || child_binding
                                            .expires_at
                                            .is_some_and(|expiry| now >= expiry)
                                    {
                                        return Err(AuthorizationStateError::ActivationMissing);
                                    }
                                    Ok((
                                        delegation.installation_public_key.is_some(),
                                        [
                                            delegation.expires_at,
                                            session.expires_at,
                                            child_binding.expires_at,
                                        ],
                                    ))
                                })();
                                match available {
                                    Ok((available, companion_expiries)) => {
                                        if available {
                                            expiries.extend(companion_expiries);
                                        }
                                        available
                                    }
                                    Err(error) if participant_projection.companion_required => {
                                        return Err(error);
                                    }
                                    Err(_) => false,
                                }
                            }
                        } else {
                            false
                        };
                        if participant_projection.companion_required && !companion_available {
                            return Err(AuthorizationStateError::ActivationMissing);
                        }
                        (AuthorizationPrincipalKind::Device, companion_available)
                    }
                    _ => return Err(AuthorizationStateError::WrongPrincipalKind),
                };
            (
                kind,
                None,
                Some(identity.identity_key_id),
                Some(deployment.deployment_id),
                Some(instance.instance_id),
                companion_available,
            )
        }
    };
    let authority = super::policy::resolve_authority(
        &participant,
        binding.approval_mode,
        &binding.approved_capabilities,
        &binding.approved_resources,
        &binding.platform_privileges,
        &binding.delegation_ceiling,
        (
            &resources,
            companion_available && binding.companion_approved,
        ),
    )?;
    if authority.exact_grants != binding.grants {
        return Err(AuthorizationStateError::NotAuthorized);
    }
    if !authority.readiness {
        if let Some(resource) = authority
            .missing_required
            .iter()
            .find_map(|reason| reason.strip_prefix("resource:"))
        {
            return Err(AuthorizationStateError::RequiredResourceUnavailable(
                resource.to_owned(),
            ));
        }
        return Err(AuthorizationStateError::NotAuthorized);
    }
    let mut selected_resources = Vec::new();
    for permission in authority.exact_grants.permissions() {
        let PermissionTarget::ParticipantResource {
            participant: owner,
            resource,
            name,
        } = permission.target()
        else {
            continue;
        };
        let kind = match resource {
            ParticipantResourceKind::Kv => "kv",
            ParticipantResourceKind::Store => "store",
            ParticipantResourceKind::JobQueue => "jobQueue",
            ParticipantResourceKind::EventConsumer => "eventConsumer",
            ParticipantResourceKind::State => "state",
        };
        let evidence = resources
            .iter()
            .find(|evidence| {
                evidence.resource_kind == kind
                    && evidence.local_name == *name
                    && evidence.owner_participant_id == *owner
                    && evidence.state == ResourceBindingState::Available
            })
            .cloned();
        let Some(evidence) = evidence else { continue };
        if !selected_resources.contains(&evidence) {
            selected_resources.push(evidence);
        }
    }
    let session_key_id =
        super::domain::validate_ed25519_public_key("sessionKey", &connection.session_public_key)?;
    // Installation keys can be shared by tabs; inboxes belong to logical connections.
    let inbox_prefix = format!("_INBOX.{}", connection.connection_id);
    let mut grants = authority.exact_grants.permissions().to_vec();
    if principal_kind == AuthorizationPrincipalKind::Service {
        for (api_id, api) in &participant_projection.implemented_apis {
            for (key, action) in &api.actions {
                if action.kind == super::evidence::RuntimeActionKind::Event {
                    let name = key.split_once(':').map_or(key.as_str(), |(_, name)| name);
                    grants.push(
                        PermissionAtom::new(
                            PermissionTarget::api_surface(api_id, ApiSurfaceKind::Event, name)
                                .map_err(|error| {
                                    AuthorizationStateError::InvalidRecord(error.to_string())
                                })?,
                            PermissionAction::Publish,
                        )
                        .map_err(|error| {
                            AuthorizationStateError::InvalidRecord(error.to_string())
                        })?,
                    );
                }
            }
        }
    }
    Ok(IssuableAuthorizationState {
        principal_id: principal.principal_id,
        principal_kind,
        connection_id: connection.connection_id,
        login_session_id,
        identity_key_id,
        session_public_key: connection.session_public_key,
        session_key_id,
        inbox_prefix,
        participant,
        binding,
        deployment_id,
        instance_id,
        grant_set: GrantSet::new(grants),
        resource_bindings: selected_resources,
        expires_at: expiries.into_iter().flatten().min(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::SigningKey;
    use sha2::{Digest as _, Sha256};
    use trellis_protocol::{
        AuthorizationIssuerKey, AuthorizationIssuerState, GrantSet, ParticipantKind,
    };

    use super::*;
    use crate::platform::auth::authority::{
        ContextRepository, IssuanceConnection, IssuanceCredential,
    };
    use crate::platform::auth::evidence::ParticipantRuntimeProjection;
    use crate::platform::auth::{
        ApprovalMode, DelegationCeiling, ParticipantBindingState, ProvisionedIdentityRecord,
    };

    fn device_snapshot(companion_required: bool) -> IssuanceSnapshot {
        let now = 1_000;
        let device_id = "dev_1".to_owned();
        let deployment_id = "deployment_1".to_owned();
        let participant_id = "package.device".to_owned();
        let grants = GrantSet::new(Vec::new());
        let session_public_key =
            URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes());
        IssuanceSnapshot {
            connection: IssuanceConnection {
                credential: IssuanceCredential::Native("identity_1".to_owned()),
                connection_id: "connection_1".to_owned(),
                session_public_key,
            },
            credential: IssuanceCredentialRecord::Native(Box::new(
                super::super::authority::NativeIssuanceCredentialRecord {
                    identity: Box::new(ProvisionedIdentityRecord {
                        identity_key_id: "identity_1".to_owned(),
                        identity_public_key: "unused".to_owned(),
                        principal_id: device_id.clone(),
                        deployment_id: deployment_id.clone(),
                        instance_id: "instance_1".to_owned(),
                        kind: ProvisionedIdentityKind::Device,
                        state: ProvisionedIdentityState::Active,
                        created_at: now,
                        revoked_at: None,
                    }),
                    instance: super::super::RuntimeInstanceRecord {
                        instance_id: "instance_1".to_owned(),
                        deployment_id: deployment_id.clone(),
                        principal_id: device_id.clone(),
                        state: RuntimeInstanceState::Active,
                        created_at: now,
                        updated_at: now,
                        version: 1,
                    },
                    deployment: super::super::DeploymentRecord {
                        deployment_id: deployment_id.clone(),
                        participant_id: participant_id.clone(),
                        participant_kind: ParticipantKind::Device,
                        active: true,
                        expires_at: None,
                    },
                    device: Some(super::super::DeviceRecord {
                        principal_id: device_id.clone(),
                        deployment_id: deployment_id.clone(),
                        state: DeviceState::Active,
                        created_at: now,
                        updated_at: now,
                        version: 1,
                    }),
                    delegation: Some(super::super::DeviceDelegationRecord {
                        principal_id: device_id.clone(),
                        deployment_id: deployment_id.clone(),
                        companion_participant_id: Some("package.app".to_owned()),
                        user_login_session_id: Some("session_1".to_owned()),
                        installation_public_key: Some("installation-key".to_owned()),
                        device_grant_revision: Some(1),
                        child_grant_revision: Some(1),
                        required: companion_required,
                        state: DeviceDelegationState::Active,
                        expires_at: None,
                    }),
                    delegation_session: None,
                    delegation_principal: None,
                    delegation_binding: None,
                },
            )),
            principal: super::super::PrincipalRecord {
                principal_id: device_id.clone(),
                kind: PrincipalKind::Device,
                state: PrincipalState::Active,
                created_at: now,
                updated_at: now,
                version: 1,
                disabled_at: None,
                revoked_at: None,
            },
            binding: super::super::GrantBinding {
                owner_kind: GrantOwnerKind::Deployment,
                owner_id: deployment_id,
                participant_id: participant_id.clone(),
                installed_revision: 1,
                grants: grants.clone(),
                approval_mode: ApprovalMode::Exact,
                approved_capabilities: Vec::new(),
                approved_resources: Vec::new(),
                delegation_ceiling: DelegationCeiling {
                    capabilities: Vec::new(),
                    exact_restrictions: Some(grants),
                    platform_privileges: Vec::new(),
                },
                approval_decision_digest: "decision".to_owned(),
                approval_expected_grant_revision: 0,
                companion_approved: true,
                platform_privileges: Vec::new(),
                revision: 1,
                state: GrantBindingState::Active,
                expires_at: None,
                provenance: None,
                created_at: now,
                updated_at: now,
            },
            participant: super::super::ParticipantBindingRecord {
                participant_id: participant_id.clone(),
                participant_kind: ParticipantKind::Device,
                participant_digest: "d".repeat(43),
                needs_digest: "n".repeat(43),
                package_digest: "p".repeat(43),
                evidence_digest: "e".repeat(43),
                participant_path: "device".to_owned(),
                projection: ParticipantRuntimeProjection {
                    participant_id,
                    participant_kind: ParticipantKind::Device,
                    display_name: "Device".to_owned(),
                    implemented_apis: BTreeMap::new(),
                    referenced_apis: BTreeMap::new(),
                    resources: BTreeMap::new(),
                    required_grants: GrantSet::new(Vec::new()),
                    optional_grant_bundles: BTreeMap::new(),
                    required_capabilities: Vec::new(),
                    optional_capability_definitions: BTreeMap::new(),
                    companion_participant_id: Some("package.app".to_owned()),
                    companion_participant_kind: Some(ParticipantKind::App),
                    companion_required,
                },
                resolved_at: now,
                state: ParticipantBindingState::Resolved,
                error: None,
            },
            resources: Vec::new(),
            issuer: AuthorizationIssuerKey {
                key_id: "issuer".to_owned(),
                public_key: "unused".to_owned(),
                state: AuthorizationIssuerState::Active,
            },
        }
    }

    #[test]
    fn optional_stale_companion_degrades_but_required_companion_fails() {
        assert!(resolve_snapshot(device_snapshot(false), 2_000).is_ok());
        assert_eq!(
            resolve_snapshot(device_snapshot(true), 2_000),
            Err(AuthorizationStateError::ActivationMissing)
        );
    }

    #[test]
    fn device_delegation_requires_the_current_shared_child_grant() {
        let mut snapshot = device_snapshot(true);
        let IssuanceCredentialRecord::Native(native) = &mut snapshot.credential else {
            unreachable!()
        };
        native.delegation_session = Some(super::super::SessionRecord {
            session_id: "session_1".to_owned(),
            principal_id: "user_1".to_owned(),
            participant_id: "package.app".to_owned(),
            participant_kind: ParticipantKind::App,
            session_key_id: "key_1".to_owned(),
            session_public_key: "installation-key".to_owned(),
            state: SessionState::Active,
            created_at: 1_000,
            last_authenticated_at: 1_000,
            expires_at: None,
            revoked_at: None,
            version: 1,
        });
        native.delegation_principal = Some(super::super::PrincipalRecord {
            principal_id: "user_1".to_owned(),
            kind: PrincipalKind::User,
            state: PrincipalState::Active,
            created_at: 1_000,
            updated_at: 1_000,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        });
        native.delegation_binding = Some(super::super::GrantBinding {
            owner_kind: GrantOwnerKind::User,
            owner_id: "user_1".to_owned(),
            participant_id: "package.app".to_owned(),
            installed_revision: 1,
            grants: GrantSet::new(Vec::new()),
            approval_mode: ApprovalMode::Capabilities,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(GrantSet::new(Vec::new())),
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: "second-device-consent".to_owned(),
            approval_expected_grant_revision: 1,
            companion_approved: false,
            platform_privileges: Vec::new(),
            revision: 2,
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
            created_at: 1_000,
            updated_at: 2_000,
        });

        assert_eq!(
            resolve_snapshot(snapshot.clone(), 2_000),
            Err(AuthorizationStateError::ActivationMissing)
        );
        let IssuanceCredentialRecord::Native(native) = &mut snapshot.credential else {
            unreachable!()
        };
        native.delegation.as_mut().unwrap().child_grant_revision = Some(2);
        assert!(resolve_snapshot(snapshot.clone(), 2_000).is_ok());
        let IssuanceCredentialRecord::Native(native) = &mut snapshot.credential else {
            unreachable!()
        };
        native.delegation_binding.as_mut().unwrap().state = GrantBindingState::Revoked;
        assert_eq!(
            resolve_snapshot(snapshot, 2_000),
            Err(AuthorizationStateError::ActivationMissing)
        );
    }

    #[tokio::test]
    async fn auth_runtime_bootstrap_is_immediately_issuable() {
        let store = super::super::SqliteAuthorizationStore::open_in_memory().expect("open store");
        let issuer_key = SigningKey::from_bytes(&[9; 32]).verifying_key();
        store
            .activate_issuer(
                AuthorizationIssuerKey {
                    key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(issuer_key.as_bytes())),
                    public_key: URL_SAFE_NO_PAD.encode(issuer_key.as_bytes()),
                    state: AuthorizationIssuerState::Active,
                },
                1,
            )
            .await
            .expect("activate issuer");
        let participant = super::super::auth_runtime_participant_binding(1).expect("participant");
        store
            .put_participant_binding(participant.clone())
            .await
            .expect("install participant");
        let service = super::super::AuthService::new(
            store.clone(),
            super::super::AuthServiceConfig::default(),
        )
        .expect("create service");
        let (session, _, identity_key_id, connection_id) =
            crate::platform::ensure_auth_event_session(&service, &participant, 1)
                .await
                .expect("initialize Auth event identity");
        let snapshot = store
            .load_issuance_snapshot(&IssuanceConnection {
                credential: IssuanceCredential::Native(identity_key_id),
                connection_id,
                session_public_key: session.session_key,
            })
            .await
            .expect("load issuance snapshot");

        resolve_snapshot(snapshot, 1).expect("authorize issuance snapshot");
    }
}
