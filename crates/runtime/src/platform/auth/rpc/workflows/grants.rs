use serde_json::{json, Value};
use trellis_protocol::{GrantSet, PermissionTarget, PlatformPrivilege};
use trellis_runtime_apis::types::{
    AuthDeploymentsApplyRequest, AuthGrantSet, AuthGrantsGetRequest, AuthGrantsGetRequestOwnerKind,
    AuthGrantsListRequest, AuthGrantsListRequestOwnerId, AuthGrantsListRequestOwnerKind,
    AuthGrantsRevokeRequest, AuthGrantsRevokeRequestOwnerKind, AuthGrantsSetRequest,
    AuthGrantsSetRequestOwnerKind, AuthIssuersRevokeRequest, AuthParticipantsGetRequest,
    AuthParticipantsInstallRequest, AuthParticipantsListRequest,
};

use super::super::{
    mutation_actor, now_millis, require_admin, rpc_idempotency, AuthRpcProcessor, ValidatedRequest,
};
use crate::platform::auth::domain::{
    ApprovedCapability, ApprovedResource, GrantBindingReplacement, ResourceCommitment,
};
use crate::platform::auth::evidence::PackageEvidenceInput;
use crate::platform::auth::{
    participant_resource_commitments, ApprovalMode, AuthorizationResourceKind,
    AuthorizationStateError, DelegationCeiling, GrantBindingState, GrantOwnerKind,
    ParticipantBindingRecord,
};

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    let input: Value = serde_json::from_slice(payload)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let repository = processor.service.repository();
    let now = now_millis()?;
    match subject {
        "rpc.v1.auth.Issuers.Revoke" => {
            require_admin(&caller)?;
            let request: AuthIssuersRevokeRequest = serde_json::from_value(input.clone())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let idempotency = rpc_idempotency(
                "Auth.Issuers.Revoke",
                &caller.principal_id,
                &request.idempotency_key.0,
                &input,
                now,
            )?;
            repository
                .revoke_issuer(
                    mutation_actor(&caller),
                    request.key_id,
                    caller.principal_id,
                    request.reason,
                    idempotency,
                )
                .await
        }
        "rpc.v1.auth.Grants.Get" => {
            let request: AuthGrantsGetRequest = serde_json::from_value(input)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let owner_kind = match request.owner_kind {
                AuthGrantsGetRequestOwnerKind::User => GrantOwnerKind::User,
                AuthGrantsGetRequestOwnerKind::Deployment => GrantOwnerKind::Deployment,
                AuthGrantsGetRequestOwnerKind::Unknown(_) => {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "unknown grant owner kind".to_owned(),
                    ))
                }
            };
            if owner_kind != GrantOwnerKind::User || request.owner_id.0 != caller.principal_id {
                require_admin(&caller)?;
            }
            let binding = repository
                .get_grant_binding(owner_kind, request.owner_id.0, request.participant_id.0)
                .await?;
            Ok(json!({"binding": binding}))
        }
        "rpc.v1.auth.Grants.List" => {
            let mut request: AuthGrantsListRequest = serde_json::from_value(input)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            if !caller
                .platform_privileges
                .contains(&trellis_protocol::PlatformPrivilege::Admin)
            {
                if request
                    .owner_kind
                    .as_ref()
                    .is_some_and(|kind| *kind != AuthGrantsListRequestOwnerKind::User)
                    || request
                        .owner_id
                        .as_ref()
                        .is_some_and(|id| id.0 != caller.principal_id)
                {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "not_authorized".to_owned(),
                    ));
                }
                request.owner_kind = Some(AuthGrantsListRequestOwnerKind::User);
                request.owner_id = Some(AuthGrantsListRequestOwnerId(caller.principal_id));
            }
            repository.list_grant_bindings(request).await
        }
        "rpc.v1.auth.Participants.Get" => {
            let request: AuthParticipantsGetRequest = serde_json::from_value(input)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let revision = request
                .revision
                .map(|revision| u64::try_from(revision.0 .0))
                .transpose()
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            repository
                .get_installed_participant(request.participant_id.0, revision)
                .await
        }
        "rpc.v1.auth.Participants.List" => {
            let request: AuthParticipantsListRequest = serde_json::from_value(input)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            repository.list_installed_participants(request).await
        }
        "rpc.v1.auth.Participants.Install" => {
            require_admin(&caller)?;
            let request: AuthParticipantsInstallRequest = serde_json::from_value(input.clone())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let expected = request.expected_revision.0 .0;
            let idempotency = rpc_idempotency(
                "Auth.Participants.Install",
                &caller.principal_id,
                &request.idempotency_key.0,
                &input,
                now,
            )?;
            let evidence = PackageEvidenceInput::from_generated_wire(
                request.package_evidence,
                request.participant_path,
                request.package_digest,
            )?;
            let root_package = evidence.package_evidence.root_package.clone();
            let (binding, evidence_json) =
                ParticipantBindingRecord::from_package_evidence(&evidence, now)?;
            repository
                .install_participant(
                    mutation_actor(&caller),
                    binding,
                    root_package,
                    evidence_json,
                    request.platform_trust.unwrap_or(false),
                    (expected, idempotency),
                )
                .await
        }
        "rpc.v1.auth.Deployments.Apply" => {
            require_admin(&caller)?;
            let request: AuthDeploymentsApplyRequest = serde_json::from_value(input.clone())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let expected = request.expected_revision.0 .0;
            let idempotency = rpc_idempotency(
                "Auth.Deployments.Apply",
                &caller.principal_id,
                &request.idempotency_key.0,
                &input,
                now,
            )?;
            let evidence = PackageEvidenceInput::from_generated_wire(
                request.package_evidence,
                request.participant_path,
                request.package_digest,
            )?;
            let root_package = evidence.package_evidence.root_package.clone();
            let (binding, evidence_json) =
                ParticipantBindingRecord::from_package_evidence(&evidence, now)?;
            let approval = request.approval;
            let approved_resources = approval
                .as_ref()
                .into_iter()
                .flat_map(|approval| approval.approved_resources.iter().cloned())
                .map(|resource| {
                    let kind = match resource.kind {
                        trellis_runtime_apis::types::ResourceKind::Consumer => {
                            AuthorizationResourceKind::Consumer
                        }
                        trellis_runtime_apis::types::ResourceKind::Job => {
                            AuthorizationResourceKind::Job
                        }
                        trellis_runtime_apis::types::ResourceKind::Kv => {
                            AuthorizationResourceKind::Kv
                        }
                        trellis_runtime_apis::types::ResourceKind::State => {
                            AuthorizationResourceKind::State
                        }
                        trellis_runtime_apis::types::ResourceKind::Store => {
                            AuthorizationResourceKind::Store
                        }
                        trellis_runtime_apis::types::ResourceKind::Unknown(value) => {
                            return Err(AuthorizationStateError::InvalidRecord(format!(
                                "unknown approved resource kind {value}"
                            )));
                        }
                    };
                    Ok(ApprovedResource {
                        kind,
                        name: resource.name,
                        commitment: ResourceCommitment {
                            desired_max_object_bytes: resource
                                .commitment
                                .desired_max_object_bytes
                                .map(|value| value.0 .0),
                            desired_max_total_bytes: resource
                                .commitment
                                .desired_max_total_bytes
                                .map(|value| value.0 .0),
                            desired_max_value_bytes: resource
                                .commitment
                                .desired_max_value_bytes
                                .map(|value| value.0 .0),
                            history: resource.commitment.history.map(|value| value.0 .0),
                            ttl_ms: resource.commitment.ttl_ms.map(|value| value.0),
                        },
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let approved_capabilities = approval
                .as_ref()
                .into_iter()
                .flat_map(|approval| approval.approved_capabilities.iter().cloned())
                .map(|capability| ApprovedCapability {
                    id: capability.id,
                    consent_digest: capability.consent_digest,
                })
                .collect();
            let mode = match approval.as_ref().map(|approval| &approval.mode) {
                Some(trellis_runtime_apis::types::ApprovalMode::Capabilities) => {
                    ApprovalMode::Capabilities
                }
                Some(trellis_runtime_apis::types::ApprovalMode::Exact) => ApprovalMode::Exact,
                Some(trellis_runtime_apis::types::ApprovalMode::Unknown(value)) => {
                    return Err(AuthorizationStateError::InvalidRecord(format!(
                        "unknown approval mode {value}"
                    )));
                }
                None => ApprovalMode::Capabilities,
            };
            let delegation_ceiling = approval
                .as_ref()
                .and_then(|approval| approval.delegation_ceiling.clone())
                .map(|ceiling| {
                    let exact_restrictions = ceiling
                        .exact_restrictions
                        .map(|grants| {
                            serde_json::from_value(serde_json::to_value(grants).map_err(
                                |error| AuthorizationStateError::InvalidRecord(error.to_string()),
                            )?)
                            .map_err(|error| {
                                AuthorizationStateError::InvalidRecord(error.to_string())
                            })
                        })
                        .transpose()?;
                    Ok(crate::platform::auth::ephemeral::ConsentDelegationCeiling {
                        capabilities: ceiling
                            .capabilities
                            .into_iter()
                            .map(|capability| ApprovedCapability {
                                id: capability.id,
                                consent_digest: capability.consent_digest,
                            })
                            .collect(),
                        exact_restrictions,
                    })
                })
                .transpose()?;
            repository
                .apply_deployment(
                    mutation_actor(&caller),
                    request.deployment_id.0,
                    (
                        binding,
                        root_package,
                        evidence_json,
                        approval.map(|approval| {
                            crate::platform::auth::ephemeral::ConsentApproval {
                                mode,
                                installed_revision: approval.installed_revision.0,
                                expected_grant_revision: approval.expected_grant_revision.0,
                                decision_digest: approval.decision_digest,
                                approved_capabilities,
                                approved_resources,
                                companion_approved: approval.companion_approved,
                                delegation_ceiling,
                            }
                        }),
                    ),
                    (expected, idempotency),
                )
                .await
        }
        "rpc.v1.auth.Grants.Set" => {
            require_admin(&caller)?;
            let request: AuthGrantsSetRequest = serde_json::from_value(input.clone())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let expected = request.expected_revision.0 .0;
            let owner_kind = match request.owner_kind {
                AuthGrantsSetRequestOwnerKind::User => GrantOwnerKind::User,
                AuthGrantsSetRequestOwnerKind::Deployment => GrantOwnerKind::Deployment,
                AuthGrantsSetRequestOwnerKind::Unknown(_) => {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "unknown grant owner kind".to_owned(),
                    ))
                }
            };
            let idempotency = rpc_idempotency(
                "Auth.Grants.Set",
                &caller.principal_id,
                &request.idempotency_key.0,
                &input,
                now,
            )?;
            let grants = grant_set_from_generated(request.grants)?;
            let platform_privileges: Vec<PlatformPrivilege> =
                serde_json::from_value(input["platformPrivileges"].clone())
                    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let installed_revision = u64::try_from(request.installed_revision.0 .0)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let participant_id = request.participant_id.0;
            let (_, participant) = repository
                .get_installed_participant_record(participant_id.clone(), Some(installed_revision))
                .await?
                .ok_or(AuthorizationStateError::ParticipantMissing)?;
            let approved_resources = participant_resource_commitments(&participant)?
                .into_iter()
                .filter(|approved| {
                    grants.permissions().iter().any(|permission| {
                        matches!(permission.target().clone(), PermissionTarget::ParticipantResource { participant, resource, name }
                            if participant == participant_id
                                && AuthorizationResourceKind::from(resource) == approved.kind
                                && name == approved.name)
                    })
                })
                .collect();
            let delegation_ceiling = DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(grants.clone()),
                platform_privileges: platform_privileges.clone(),
            };
            let expires_at = match request.expires_at {
                trellis_runtime_apis::__types::Nullable::Null => None,
                trellis_runtime_apis::__types::Nullable::Value(value) => {
                    Some(i64::try_from(value.0 .0).map_err(|error| {
                        AuthorizationStateError::InvalidRecord(error.to_string())
                    })?)
                }
            };
            repository
                .admin_set_grant_binding(
                    mutation_actor(&caller),
                    GrantBindingReplacement {
                        owner_kind,
                        owner_id: request.owner_id.0,
                        participant_id,
                        installed_revision,
                        grants,
                        approval_mode: ApprovalMode::Exact,
                        approved_capabilities: Vec::new(),
                        approved_resources,
                        delegation_ceiling,
                        approval_decision_digest: idempotency.request_digest.clone(),
                        companion_approved: false,
                        platform_privileges,
                        expected_revision: expected,
                        expected_current_installed_revision: None,
                        state: GrantBindingState::Active,
                        expires_at,
                        provenance: None,
                    },
                    idempotency,
                )
                .await
        }
        "rpc.v1.auth.Grants.Revoke" => {
            let request: AuthGrantsRevokeRequest = serde_json::from_value(input.clone())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let expected = request.expected_revision.0 .0;
            let owner_kind = match request.owner_kind {
                AuthGrantsRevokeRequestOwnerKind::User => GrantOwnerKind::User,
                AuthGrantsRevokeRequestOwnerKind::Deployment => GrantOwnerKind::Deployment,
                AuthGrantsRevokeRequestOwnerKind::Unknown(_) => {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "unknown grant owner kind".to_owned(),
                    ))
                }
            };
            if owner_kind != GrantOwnerKind::User || request.owner_id.0 != caller.principal_id {
                require_admin(&caller)?;
            }
            let idempotency = rpc_idempotency(
                "Auth.Grants.Revoke",
                &caller.principal_id,
                &request.idempotency_key.0,
                &input,
                now,
            )?;
            repository
                .revoke_grant_binding(
                    mutation_actor(&caller),
                    owner_kind,
                    request.owner_id.0,
                    request.participant_id.0,
                    expected,
                    idempotency,
                )
                .await
        }
        _ => Err(AuthorizationStateError::InvalidRecord(format!(
            "unknown grants operation: {subject}"
        ))),
    }
}

fn grant_set_from_generated(grants: AuthGrantSet) -> Result<GrantSet, AuthorizationStateError> {
    let permissions = grants
        .permissions
        .into_iter()
        .map(|permission| {
            let target: Value = serde_json::from_slice(&permission.target.0)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            Ok(json!({ "action": permission.action, "target": target }))
        })
        .collect::<Result<Vec<_>, AuthorizationStateError>>()?;
    serde_json::from_value(json!({ "format": grants.format, "permissions": permissions }))
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_grant_set_decodes_opaque_target_bytes() {
        let generated: AuthGrantSet = serde_json::from_value(json!({
            "format": "trellis.grant-set.v1",
            "permissions": [{
                "action": "call",
                "target": "eyJraW5kIjoiYXBpU3VyZmFjZSIsImFwaSI6InRyZWxsaXMuYXV0aCIsInN1cmZhY2UiOiJycGMiLCJuYW1lIjoiR3JhbnRzLkdldCJ9"
            }]
        }))
        .expect("generated grant DTO");

        let grants = grant_set_from_generated(generated).expect("protocol grant set");

        assert_eq!(
            grants.permissions()[0].target().as_api_surface(),
            Some((
                "trellis.auth",
                trellis_protocol::ApiSurfaceKind::Rpc,
                "Grants.Get"
            ))
        );
    }
}
