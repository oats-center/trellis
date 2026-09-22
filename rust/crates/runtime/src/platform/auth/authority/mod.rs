use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use super::domain::{require_nonempty, require_positive, require_protocol_timestamp};
use super::{
    AuthorizationStateError, DeploymentRecord, DeviceDelegationRecord, DeviceRecord,
    ParticipantBindingRecord, PrincipalRecord, ProviderIdentityLink, ResourceBindingEvidence,
    RuntimeInstanceRecord, SessionRecord, SessionRuntimeBinding,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IssuanceSnapshotToken(pub String);

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) enum IssuanceCredential {
    Login(String),
    Native(String),
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct IssuanceConnection {
    pub credential: IssuanceCredential,
    pub connection_id: String,
    pub session_public_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) enum IssuanceCredentialRecord {
    Login(SessionRecord),
    Native(Box<NativeIssuanceCredentialRecord>),
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct NativeIssuanceCredentialRecord {
    pub identity: Box<super::ProvisionedIdentityRecord>,
    pub instance: RuntimeInstanceRecord,
    pub deployment: DeploymentRecord,
    pub device: Option<DeviceRecord>,
    pub delegation: Option<DeviceDelegationRecord>,
    pub delegation_session: Option<SessionRecord>,
    pub delegation_principal: Option<PrincipalRecord>,
    pub delegation_binding: Option<super::GrantBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct IssuanceSnapshot {
    pub connection: IssuanceConnection,
    pub credential: IssuanceCredentialRecord,
    pub principal: PrincipalRecord,
    pub binding: super::GrantBinding,
    pub participant: ParticipantBindingRecord,
    pub resources: Vec<ResourceBindingEvidence>,
    pub issuer: trellis_protocol::AuthorizationIssuerKey,
}

pub(super) fn issuance_snapshot_token(
    snapshot: &IssuanceSnapshot,
) -> Result<IssuanceSnapshotToken, AuthorizationStateError> {
    let value = serde_json::to_value(snapshot)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    let canonical = trellis_protocol::canonicalize_json(&value).map_err(|error| {
        AuthorizationStateError::Storage(format!("cannot encode issuance snapshot: {error}"))
    })?;
    Ok(IssuanceSnapshotToken(
        URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())),
    ))
}

#[async_trait]
pub(crate) trait AuthorityEvidenceRepository: Send + Sync {
    async fn list_runtime_instances(
        &self,
    ) -> Result<Vec<RuntimeInstanceRecord>, AuthorizationStateError>;
    async fn list_devices(&self) -> Result<Vec<DeviceRecord>, AuthorizationStateError>;
    async fn get_deployment_evidence(
        &self,
        deployment_id: &str,
    ) -> Result<Option<DeploymentRecord>, AuthorizationStateError>;
    async fn put_deployment_evidence(
        &self,
        deployment: DeploymentRecord,
    ) -> Result<(), AuthorizationStateError>;
    async fn get_runtime_instance(
        &self,
        instance_id: &str,
    ) -> Result<Option<RuntimeInstanceRecord>, AuthorizationStateError>;
    async fn get_device(
        &self,
        principal_id: &str,
        deployment_id: &str,
    ) -> Result<Option<DeviceRecord>, AuthorizationStateError>;
    async fn get_device_delegation(
        &self,
        principal_id: &str,
        deployment_id: &str,
    ) -> Result<Option<DeviceDelegationRecord>, AuthorizationStateError>;
    async fn get_session_runtime_binding(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionRuntimeBinding>, AuthorizationStateError>;
}

#[async_trait]
pub(crate) trait ContextRepository: Send + Sync {
    async fn load_issuance_snapshot(
        &self,
        connection: &IssuanceConnection,
    ) -> Result<IssuanceSnapshot, AuthorizationStateError>;
}

pub(crate) fn validate_runtime_instance(
    instance: &RuntimeInstanceRecord,
) -> Result<(), AuthorizationStateError> {
    require_nonempty("instanceId", &instance.instance_id)?;
    require_nonempty("deploymentId", &instance.deployment_id)?;
    require_nonempty("principalId", &instance.principal_id)?;
    require_protocol_timestamp("createdAt", instance.created_at)?;
    require_protocol_timestamp("updatedAt", instance.updated_at)?;
    require_positive("version", instance.version)
}

pub(super) fn validate_device(device: &DeviceRecord) -> Result<(), AuthorizationStateError> {
    require_nonempty("principalId", &device.principal_id)?;
    require_nonempty("deploymentId", &device.deployment_id)?;
    require_protocol_timestamp("createdAt", device.created_at)?;
    require_protocol_timestamp("updatedAt", device.updated_at)?;
    require_positive("version", device.version)
}

pub(crate) fn validate_principal(
    principal: &PrincipalRecord,
) -> Result<(), AuthorizationStateError> {
    require_nonempty("principalId", &principal.principal_id)?;
    require_protocol_timestamp("createdAt", principal.created_at)?;
    require_protocol_timestamp("updatedAt", principal.updated_at)?;
    require_positive("version", principal.version)
}

pub(crate) fn validate_persisted_principal(
    principal: &PrincipalRecord,
) -> Result<(), AuthorizationStateError> {
    validate_principal(principal)
}

pub(crate) fn validate_provider_identity(
    identity: &ProviderIdentityLink,
) -> Result<(), AuthorizationStateError> {
    require_nonempty("provider", &identity.provider)?;
    require_nonempty("providerSubject", &identity.provider_subject)?;
    require_nonempty("principalId", &identity.principal_id)?;
    require_protocol_timestamp("linkedAt", identity.linked_at)?;
    require_protocol_timestamp("lastSeenAt", identity.last_seen_at)
}

pub(crate) fn validate_session(session: &SessionRecord) -> Result<(), AuthorizationStateError> {
    require_nonempty("sessionId", &session.session_id)?;
    require_nonempty("principalId", &session.principal_id)?;
    require_nonempty("participantId", &session.participant_id)?;
    require_nonempty("sessionPublicKey", &session.session_public_key)?;
    super::validate_ed25519_public_key("sessionPublicKey", &session.session_public_key)?;
    require_protocol_timestamp("createdAt", session.created_at)?;
    require_protocol_timestamp("lastAuthenticatedAt", session.last_authenticated_at)?;
    if let Some(expires_at) = session.expires_at {
        require_protocol_timestamp("expiresAt", expires_at)?;
        if expires_at <= session.created_at {
            return Err(AuthorizationStateError::InvalidRecord(
                "session expiresAt must be after createdAt".to_owned(),
            ));
        }
    }
    require_positive("version", session.version)
}

pub(crate) fn validate_persisted_session(
    session: &SessionRecord,
) -> Result<(), AuthorizationStateError> {
    validate_session(session)
}

pub(super) fn validate_device_delegation(
    delegation: &DeviceDelegationRecord,
) -> Result<(), AuthorizationStateError> {
    require_nonempty("principalId", &delegation.principal_id)?;
    require_nonempty("deploymentId", &delegation.deployment_id)?;
    for (field, value) in [
        (
            "companionParticipantId",
            delegation.companion_participant_id.as_deref(),
        ),
        (
            "userLoginSessionId",
            delegation.user_login_session_id.as_deref(),
        ),
        (
            "installationPublicKey",
            delegation.installation_public_key.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            require_nonempty(field, value)?;
        }
    }
    if delegation.companion_participant_id.is_some() != delegation.user_login_session_id.is_some()
        || delegation.companion_participant_id.is_some()
            != delegation.installation_public_key.is_some()
        || delegation.companion_participant_id.is_some()
            != delegation.device_grant_revision.is_some()
        || delegation.companion_participant_id.is_some()
            != delegation.child_grant_revision.is_some()
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "device companion identity, login, and installation key must be stored together"
                .to_owned(),
        ));
    }
    if delegation.state == super::DeviceDelegationState::Active
        && delegation.companion_participant_id.is_none()
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "active device delegation requires complete child authority linkage".to_owned(),
        ));
    }
    if let Some(expires_at) = delegation.expires_at {
        require_protocol_timestamp("delegation.expiresAt", expires_at)?;
    }
    Ok(())
}

pub(super) fn validate_session_runtime_binding(
    binding: &SessionRuntimeBinding,
) -> Result<(), AuthorizationStateError> {
    require_nonempty("sessionId", &binding.session_id)?;
    require_nonempty("deploymentId", &binding.deployment_id)?;
    require_nonempty("instanceId", &binding.instance_id)
}

pub(crate) fn validate_deployment_evidence(
    deployment: &DeploymentRecord,
) -> Result<(), AuthorizationStateError> {
    require_nonempty("deploymentId", &deployment.deployment_id)?;
    require_nonempty("participantId", &deployment.participant_id)?;
    if !matches!(
        deployment.participant_kind,
        trellis_protocol::ParticipantKind::Service | trellis_protocol::ParticipantKind::Device
    ) {
        return Err(AuthorizationStateError::InvalidRecord(
            "deployment evidence requires a service or device participant".to_owned(),
        ));
    }
    if let Some(expires_at) = deployment.expires_at {
        require_protocol_timestamp("deployment.expiresAt", expires_at)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_device_delegation;
    use crate::platform::auth::{DeviceDelegationRecord, DeviceDelegationState};

    #[test]
    fn active_companion_delegation_requires_one_complete_child_session_link() {
        let complete = DeviceDelegationRecord {
            principal_id: "dev_01".to_owned(),
            deployment_id: "dep_01".to_owned(),
            companion_participant_id: Some("acme.Sensor.Companion".to_owned()),
            user_login_session_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
            installation_public_key: Some("A".repeat(43)),
            device_grant_revision: Some(2),
            child_grant_revision: Some(3),
            required: true,
            state: DeviceDelegationState::Active,
            expires_at: None,
        };
        assert!(validate_device_delegation(&complete).is_ok());
        assert!(validate_device_delegation(&DeviceDelegationRecord {
            user_login_session_id: None,
            ..complete
        })
        .is_err());
    }
}
