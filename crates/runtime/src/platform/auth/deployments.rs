use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::VerifyingKey;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trellis_idl::{
    api_digest, compile_evidence, participant_digest, ActionKind, ActionSelection,
    InteractionDirection, PackageEvidence, ParticipantKind,
};

use super::authorization_sessions::ProviderImplementation;
use super::catalog::{action_identity, resolve_api};
use super::policy::capability;
use super::sqlite::{decode, digest, id, json, AuthError, Mutation, SqliteAuthorizationStore};

/// Deployment authoring comes from native source evidence, never reconstructed
/// action schemas. The provisioner supplies resolved runtime resource commitments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DeploymentProvisioning {
    pub(crate) evidence: PackageEvidence,
    pub(crate) participant_id: String,
    pub(crate) expected_api_generations: BTreeMap<String, u64>,
    pub(crate) identity_public_key: String,
    pub(crate) resource_commitments: serde_json::Value,
    pub(crate) expires_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProvisionedDeployment {
    pub(crate) principal_id: String,
    pub(crate) deployment_id: String,
    pub(crate) instance_id: String,
    pub(crate) identity_key_id: String,
}

impl SqliteAuthorizationStore {
    /// Trusted provisioning and Runtime startup use the same native participant
    /// installation and compiler as every other service/device identity.
    pub(crate) fn provision_deployment(
        &self,
        mutation: &Mutation,
        input: &DeploymentProvisioning,
        now: i64,
    ) -> Result<ProvisionedDeployment, AuthError> {
        let graph = compile_evidence(input.evidence.clone())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let participant = graph
            .root_package()
            .participants()
            .values()
            .find(|participant| participant.identity().as_str() == input.participant_id)
            .ok_or(AuthError::NotFound)?;
        let needs = graph
            .participant_needs(participant.identity())
            .ok_or(AuthError::NotFound)?;
        let participant_digest = participant_digest(&graph, participant.identity())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let required: Vec<String> = needs
            .required_capabilities()
            .iter()
            .map(ToString::to_string)
            .collect();
        let optional: Vec<String> = needs
            .optional_capabilities()
            .iter()
            .map(ToString::to_string)
            .collect();
        let bytes = URL_SAFE_NO_PAD
            .decode(&input.identity_public_key)
            .map_err(|_| AuthError::Denied)?;
        let raw: [u8; 32] = bytes.try_into().map_err(|_| AuthError::Denied)?;
        if VerifyingKey::from_bytes(&raw)
            .map_err(|_| AuthError::Denied)?
            .is_weak()
            || URL_SAFE_NO_PAD.encode(raw) != input.identity_public_key
            || input.expires_at.is_some_and(|expiry| expiry <= now)
            || !input.resource_commitments.is_object()
        {
            return Err(AuthError::Denied);
        }
        let kind = match participant.kind() {
            ParticipantKind::Service => "service",
            ParticipantKind::Device => "device",
        };
        self.mutate(mutation, "deployment.provision", input, now, |connection| {
            Self::require_privilege(connection, &mutation.actor, "principals.manage")?;
            let mut provided = Vec::new();
            for api in participant.implements() {
                let (generation, accepted): (u64, String) = connection.query_row(
                    "SELECT generation,definition_json FROM auth_accepted_apis WHERE api_id=?1",
                    [api.as_str()], |row| Ok((row.get(0)?,row.get(1)?)),
                ).optional()?.ok_or(AuthError::Denied)?;
                if input.expected_api_generations.get(api.as_str()) != Some(&generation) {return Err(AuthError::Conflict);}
                let accepted = compile_evidence(decode(&accepted)?).map_err(|error| AuthError::Invalid(error.to_string()))?;
                let implementation = graph.api(api).ok_or(AuthError::NotFound)?;
                let accepted = resolve_api(&accepted, api.as_str())?;
                if !trellis_idl::compare_implementation(implementation, accepted).compatible {return Err(AuthError::Denied);}
                let implemented_actions = implementation.definition().actions().keys().flat_map(|identity| {
                    action_identity(&ActionSelection {action: identity.clone(),direction: match identity.kind {
                        ActionKind::Rpc => InteractionDirection::Call,
                        ActionKind::Operation => InteractionDirection::Invoke,
                        ActionKind::Event => InteractionDirection::Publish,
                        ActionKind::Live => InteractionDirection::Subscribe,
                    }})
                }).collect();
                provided.push(ProviderImplementation {
                    api_id: api.to_string(), generation,
                    implementation_digest: api_digest(&graph, api).map_err(|error| AuthError::Invalid(error.to_string()))?,
                    implemented_actions,
                });
            }
            if input.expected_api_generations.len() != provided.len() {return Err(AuthError::Conflict);}
            for requested in required.iter().chain(&optional) {capability(connection, requested)?;}
            for package in &input.evidence.packages {
                connection.execute("INSERT OR IGNORE INTO auth_package_evidence(package_digest,accepted_at) VALUES(?1,?2)",params![package.digest,now])?;
            }
            let evidence_digest = digest(&input.evidence)?;
            connection.execute("INSERT OR IGNORE INTO auth_package_evidence_documents VALUES(?1,?2,?3,?4)",params![evidence_digest,input.evidence.root_digest,json(&input.evidence)?,now])?;
            let revision: u64 = connection.query_row("SELECT coalesce(max(revision),0)+1 FROM auth_installed_participants WHERE participant_id=?1",[&input.participant_id],|row|row.get(0))?;
            connection.execute("INSERT INTO auth_installed_participants(participant_id,revision,participant_kind,participant_digest,needs_digest,package_digest,evidence_digest,participant_path,companion_required,projection_json,installed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?1,0,?8,?9)",params![input.participant_id,revision,kind,participant_digest,needs.digest(),input.evidence.root_digest,evidence_digest,json(&input.evidence)?,now])?;
            let result = ProvisionedDeployment {principal_id:id(),deployment_id:id(),instance_id:id(),identity_key_id:URL_SAFE_NO_PAD.encode(Sha256::digest(raw))};
            connection.execute("INSERT INTO auth_principals(principal_id,kind,state,created_at,updated_at,version) VALUES(?1,?2,'active',?3,?3,1)",params![result.principal_id,kind,now])?;
            connection.execute("INSERT INTO auth_deployments VALUES(?1,?2,?3,'active',?4)",params![result.deployment_id,input.participant_id,kind,input.expires_at])?;
            if participant.kind() == ParticipantKind::Device {
                connection.execute("INSERT INTO auth_devices(principal_id,deployment_id,state,created_at,updated_at,version) VALUES(?1,?2,'active',?3,?3,1)",params![result.principal_id,result.deployment_id,now])?;
            }
            connection.execute("INSERT INTO auth_instances VALUES(?1,?2,?3,?4,'active',?5,?5,1)",params![result.instance_id,result.deployment_id,result.principal_id,revision,now])?;
            connection.execute("INSERT INTO auth_deployment_bindings(deployment_id,installed_revision,implementation_digest,provided_apis_json,required_capabilities_json,optional_capabilities_json,resource_commitments_json,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,1)",params![result.deployment_id,revision,participant_digest,json(&provided)?,json(&required)?,json(&optional)?,json(&input.resource_commitments)?])?;
            connection.execute("INSERT INTO auth_provisioned_identities(identity_key_id,principal_id,deployment_id,instance_id,participant_id,kind,identity_public_key,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8)",params![result.identity_key_id,result.principal_id,result.deployment_id,result.instance_id,input.participant_id,kind,input.identity_public_key,now])?;
            // Trusted deployment approval establishes entitlement; selection is
            // independently bounded by the installed native participant above.
            for requested in required.iter().chain(&optional) {
                let cap = capability(connection, requested)?;
                connection.execute("INSERT INTO auth_direct_capabilities VALUES(?1,?2,?3,1,?4,?5)",params![result.principal_id,requested,cap.identity_generation.get(),input.expires_at,now])?;
            }
            Ok(result)
        })
    }
}
