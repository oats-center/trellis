use async_trait::async_trait;

use super::{
    AuthorizationStateError, GrantBinding, GrantBindingReplacement, GrantOwnerKind,
    IdempotencyResultRecord, ParticipantBindingRecord,
};
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct ConsentBindingPrecondition {
    pub(crate) owner_kind: GrantOwnerKind,
    pub(crate) owner_id: String,
    pub(crate) participant_id: String,
    pub(crate) revision: u64,
    pub(crate) expires_at: Option<i64>,
    pub(crate) delegation_ceiling: super::DelegationCeiling,
    pub(crate) provenance: Option<super::PortalGrantProvenance>,
}

impl From<&GrantBinding> for ConsentBindingPrecondition {
    fn from(binding: &GrantBinding) -> Self {
        Self {
            owner_kind: binding.owner_kind,
            owner_id: binding.owner_id.clone(),
            participant_id: binding.participant_id.clone(),
            revision: binding.revision,
            expires_at: binding.expires_at,
            delegation_ceiling: binding.delegation_ceiling.clone(),
            provenance: binding.provenance.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConsentAuthorityPreconditions {
    pub(crate) policy: Option<super::PortalPolicySnapshot>,
    pub(crate) bindings: Vec<ConsentBindingPrecondition>,
}

#[async_trait]
pub(crate) trait GrantRepository: Send + Sync {
    async fn get_installed_participant_record(
        &self,
        participant_id: String,
        revision: Option<u64>,
    ) -> Result<Option<(u64, ParticipantBindingRecord)>, AuthorizationStateError>;

    async fn is_companion_participant(
        &self,
        participant_id: String,
    ) -> Result<bool, AuthorizationStateError>;

    #[allow(dead_code)]
    async fn get_installed_package_evidence(
        &self,
        evidence_digest: &str,
    ) -> Result<Option<trellis_idl::PackageEvidence>, AuthorizationStateError>;

    /// Return the immutable compiled graph for an exact installed evidence
    /// document, reusing the store's bounded semantic cache.
    async fn compiled_installed_evidence(
        &self,
        evidence_digest: &str,
    ) -> Result<
        std::sync::Arc<super::compiled_evidence::CompiledInstalledEvidence>,
        AuthorizationStateError,
    >;

    /// Return the immutable selected-surface compatibility result for one
    /// consumer selection against one provider, reusing the store's cache.
    async fn compare_installed_selection(
        &self,
        consumer: std::sync::Arc<super::compiled_evidence::CompiledInstalledEvidence>,
        selection: trellis_idl::InteractionSelection,
        provider: std::sync::Arc<super::compiled_evidence::CompiledInstalledEvidence>,
    ) -> Result<std::sync::Arc<trellis_idl::CompatibilityReport>, AuthorizationStateError>;

    /// Read every exact API binding row for one consumer scope.
    async fn get_api_bindings(
        &self,
        participant_id: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, AuthorizationStateError>;

    #[allow(dead_code)]
    async fn get_api_binding(
        &self,
        participant_id: &str,
        api_id: &str,
    ) -> Result<Option<String>, AuthorizationStateError>;

    async fn put_api_binding(
        &self,
        participant_id: &str,
        api_id: &str,
        provider_deployment_id: &str,
    ) -> Result<(), AuthorizationStateError>;

    async fn accept_presented_package(
        &self,
        input: super::evidence::PackageEvidenceInput,
        now: i64,
    ) -> Result<ParticipantBindingRecord, AuthorizationStateError>;

    async fn get_credential_participant_assignment(
        &self,
        identity_key_id: String,
    ) -> Result<Option<String>, AuthorizationStateError>;

    async fn get_grant_binding(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    ) -> Result<Option<GrantBinding>, AuthorizationStateError>;

    async fn consent_resource_actuals(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    ) -> Result<Vec<super::ephemeral::ConsentResourceActualEntry>, AuthorizationStateError>;

    async fn set_grant_binding(
        &self,
        replacement: GrantBindingReplacement,
        idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError>;

    async fn set_portal_grant_binding(
        &self,
        replacement: GrantBindingReplacement,
        policy: super::PortalPolicySnapshot,
        idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError>;

    async fn set_consent_grant_binding(
        &self,
        replacement: GrantBindingReplacement,
        authority: ConsentAuthorityPreconditions,
        idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        if let Some(policy) = authority.policy {
            self.set_portal_grant_binding(replacement, policy, idempotency)
                .await
        } else {
            self.set_grant_binding(replacement, idempotency).await
        }
    }

    async fn revoke_portal_grant_binding(
        &self,
        owner_id: String,
        participant_id: String,
        expected_revision: u64,
        policy: super::PortalPolicySnapshot,
        idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError>;
}
