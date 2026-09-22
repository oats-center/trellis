use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};
use trellis_rs::client::SessionAuth;
use trellis_rs::service::{OperationSnapshot, OperationState, RequestContext, Router, ServerError};
use trellis_runtime_apis::apis::trellis_auth_v1::operations::DeviceUserAuthoritiesResolve;
use trellis_runtime_apis::types::{
    AuthDeviceUserAuthoritiesResolveProgress,
    AuthDeviceUserAuthoritiesResolveRequest as AuthDeviceUserAuthoritiesResolveInput,
    AuthDeviceUserAuthoritiesResolveResponse as AuthDeviceUserAuthoritiesResolveOutput,
};

type AuthDeviceUserAuthoritiesResolveOperation =
    trellis_rs::generated::OperationAdapter<DeviceUserAuthoritiesResolve>;

use super::auth::{
    ActivationReviewClaim, ActivationReviewDecision, ApprovalMode, ApprovedCapability,
    ApprovedResource, AuthService, AuthorityEvidenceRepository, ClaimActivationReviewInput,
    DecideActivationReviewInput, DeploymentRepository, DeviceActivationReviewRecord,
    DeviceActivationReviewState, DeviceDelegationRecord, DeviceDelegationState, DeviceReviewMode,
    GrantBindingReplacement, GrantBindingState, GrantOwnerKind, IdempotencyResultRecord,
    PortalGrantProvenance, PortalRepository, PostCommitActionRecord, ProvisioningRepository,
    SessionRecord, SessionState, SqliteAuthorizationStore,
};

async fn companion_consent_authority(
    service: &AuthService<SqliteAuthorizationStore>,
    user_principal_id: &str,
    caller_participant_id: &str,
    child: &super::auth::ParticipantBindingRecord,
) -> Result<super::auth::policy::ConsentAuthority, super::auth::AuthorizationStateError> {
    let target_binding = service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            child.participant_id.clone(),
        )
        .await?;
    if target_binding.as_ref().is_some_and(|binding| {
        binding.provenance.is_none() && binding.state == GrantBindingState::Active
    }) {
        return super::auth::policy::consent_authority(
            super::auth::policy::ConsentAuthoritySource::Explicit {
                target: target_binding.as_ref().expect("checked above"),
            },
            now_ms().map_err(|error| {
                super::auth::AuthorizationStateError::InvalidRecord(error.to_string())
            })?,
        );
    }
    let caller_binding = service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            caller_participant_id.to_owned(),
        )
        .await?
        .filter(|binding| binding.state == GrantBindingState::Active)
        .ok_or_else(|| {
            tracing::warn!(
                user_principal_id,
                caller_participant_id,
                child_participant_id = %child.participant_id,
                "companion consent caller grant is unavailable"
            );
            super::auth::AuthorizationStateError::NotAuthorized
        })?;
    let source = caller_binding.provenance.clone().ok_or_else(|| {
        tracing::warn!(
            user_principal_id,
            caller_participant_id,
            child_participant_id = %child.participant_id,
            "companion consent caller grant has no portal provenance"
        );
        super::auth::AuthorizationStateError::NotAuthorized
    })?;
    let policy = service
        .repository()
        .get_portal_grant_override(&source.portal_id, &child.participant_id)
        .await?
        .ok_or_else(|| {
            tracing::warn!(
                portal_id = %source.portal_id,
                child_participant_id = %child.participant_id,
                "companion consent portal policy is unavailable"
            );
            super::auth::AuthorizationStateError::NotAuthorized
        })?;
    let (portal, settings) = service
        .repository()
        .get_login_portal(&source.portal_id)
        .await?
        .ok_or_else(|| {
            tracing::warn!(
                portal_id = %source.portal_id,
                "companion consent login portal is unavailable"
            );
            super::auth::AuthorizationStateError::NotAuthorized
        })?;
    let groups = service
        .repository()
        .list_capability_groups()
        .await?
        .into_iter()
        .map(|group| (group.group_key.clone(), group))
        .collect();
    let snapshot = super::auth::policy::portal_policy_snapshot(
        &portal,
        &settings,
        &child.participant_id,
        Some(&policy),
        &groups,
    )
    .map_err(|error| {
        tracing::warn!(
            portal_id = %source.portal_id,
            child_participant_id = %child.participant_id,
            ?error,
            "companion portal policy snapshot resolution failed"
        );
        error
    })?;
    let selection = super::auth::policy::resolve_portal_authority_selection(
        &policy,
        &groups,
        child,
        &super::auth::policy::ProviderLoginAttributes {
            provider_id: source.provider_id.clone(),
            roles: Vec::new(),
        },
    )
    .map_err(|error| {
        tracing::warn!(
            portal_id = %source.portal_id,
            provider_id = %source.provider_id,
            child_participant_id = %child.participant_id,
            ?error,
            "companion portal authority selection failed"
        );
        error
    })?;
    let effective_policy_digest = selection.effective_policy_digest.clone();
    super::auth::policy::consent_authority(
        super::auth::policy::ConsentAuthoritySource::Portal(Box::new(
            super::auth::policy::PortalConsentAuthority {
                selection,
                snapshot,
                provenance: PortalGrantProvenance {
                    portal_id: source.portal_id,
                    provider_id: source.provider_id,
                    roles: Vec::new(),
                    effective_policy_digest,
                },
                source: Some(&caller_binding),
                retained_target: target_binding
                    .as_ref()
                    .filter(|binding| binding.provenance.is_some()),
            },
        )),
        now_ms().map_err(|error| {
            super::auth::AuthorizationStateError::InvalidRecord(error.to_string())
        })?,
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompanionClaim {
    participant_id: String,
    kind: trellis_protocol::ParticipantKind,
    installation_public_key: String,
    request_proof: String,
    request_proof_digest: String,
}

fn convert_generated_integers(value: &mut Value, encode: bool) {
    match value {
        Value::Array(values) => {
            for value in values {
                convert_generated_integers(value, encode);
            }
        }
        Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(
                    name.as_str(),
                    "installedRevision"
                        | "expectedGrantRevision"
                        | "desiredMaxObjectBytes"
                        | "desiredMaxTotalBytes"
                        | "desiredMaxValueBytes"
                        | "history"
                        | "ttlMs"
                        | "maxObjectBytes"
                        | "maxTotalBytes"
                        | "maxValueBytes"
                        | "representationVersion"
                ) {
                    if encode {
                        if let Some(number) = value.as_u64() {
                            *value = Value::String(number.to_string());
                        }
                    } else if let Some(text) = value.as_str() {
                        if let Ok(number) = text.parse::<u64>() {
                            *value = Value::Number(number.into());
                        }
                    }
                } else {
                    convert_generated_integers(value, encode);
                }
            }
        }
        _ => {}
    }
}

fn companion_consent_value(
    consent: Option<super::auth::ConsentRequest>,
) -> Result<Value, ServerError> {
    let mut value = serde_json::to_value(consent)?;
    convert_generated_integers(&mut value, true);
    Ok(value)
}

fn decode_companion_approval(
    approval: &trellis_runtime_apis::types::Approval,
) -> Result<(super::auth::ConsentApproval, String), super::auth::AuthorizationStateError> {
    let mut value = serde_json::to_value(approval)
        .map_err(|error| super::auth::AuthorizationStateError::InvalidRecord(error.to_string()))?;
    convert_generated_integers(&mut value, false);
    let approval = serde_json::from_value(value)
        .map_err(|error| super::auth::AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let request_digest =
        trellis_protocol::digest_json(&serde_json::to_value(&approval).map_err(|error| {
            super::auth::AuthorizationStateError::InvalidRecord(error.to_string())
        })?)
        .map_err(|error| super::auth::AuthorizationStateError::InvalidRecord(error.to_string()))?;
    Ok((approval, request_digest))
}

async fn companion_consent(
    service: &AuthService<SqliteAuthorizationStore>,
    review: &DeviceActivationReviewRecord,
    user_principal_id: &str,
    caller_participant_id: &str,
) -> Result<Option<super::auth::ConsentRequest>, super::auth::AuthorizationStateError> {
    let Some(claim) = review.payload.get("companion") else {
        return Ok(None);
    };
    let claim: CompanionClaim = serde_json::from_value(claim.clone())
        .map_err(|error| super::auth::AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let (_, outer) = service
        .repository()
        .get_installed_participant_record(
            review.payload["participantId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            None,
        )
        .await?
        .ok_or(super::auth::AuthorizationStateError::ParticipantMissing)?;
    let outer = outer.resolve()?;
    if outer.companion_participant_id.as_deref() != Some(&claim.participant_id)
        || outer.companion_participant_kind != Some(claim.kind)
    {
        return Err(super::auth::AuthorizationStateError::InvalidRecord(
            "companion claim does not match the device descriptor".to_owned(),
        ));
    }
    let (installed_revision, child) = service
        .repository()
        .get_installed_participant_record(claim.participant_id.clone(), None)
        .await?
        .ok_or(super::auth::AuthorizationStateError::ParticipantMissing)?;
    let current = service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            claim.participant_id.clone(),
        )
        .await?;
    let actuals = service
        .repository()
        .consent_resource_actuals(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            claim.participant_id,
        )
        .await?;
    let ceiling =
        companion_consent_authority(service, user_principal_id, caller_participant_id, &child)
            .await?
            .ceiling;
    super::auth::policy::consent_request(
        &child,
        installed_revision,
        current.as_ref(),
        &ceiling,
        &actuals,
        None,
    )
    .map(Some)
}

async fn apply_companion_approval(
    service: &AuthService<SqliteAuthorizationStore>,
    review: &DeviceActivationReviewRecord,
    user_principal_id: &str,
    caller_participant_id: &str,
    approval: &trellis_runtime_apis::types::Approval,
) -> Result<
    (
        Option<GrantBindingReplacement>,
        String,
        super::auth::ConsentAuthorityPreconditions,
    ),
    super::auth::AuthorizationStateError,
> {
    let consent = companion_consent(service, review, user_principal_id, caller_participant_id)
        .await
        .map_err(|error| {
            tracing::warn!(
                review_id = %review.review_id,
                user_principal_id,
                caller_participant_id,
                ?error,
                "companion consent recomputation during approval failed"
            );
            error
        })?
        .ok_or_else(|| {
            super::auth::AuthorizationStateError::InvalidRecord(
                "companion approval supplied for a device without a companion".to_owned(),
            )
        })?;
    let (mut approval, request_digest) = decode_companion_approval(approval)?;
    let approved_capabilities = approval
        .approved_capabilities
        .iter()
        .collect::<BTreeSet<_>>();
    let approved_resources = approval.approved_resources.iter().collect::<BTreeSet<_>>();
    let rejection = if approval.mode != ApprovalMode::Capabilities {
        Some("approval mode")
    } else if approval.companion_approved {
        Some("companion approval marker")
    } else if approval.decision_digest != consent.decision_digest {
        Some("decision digest")
    } else if approval.installed_revision != consent.installed_revision {
        Some("installed revision")
    } else if approval.expected_grant_revision != consent.expected_grant_revision {
        Some("grant revision")
    } else if approved_capabilities.len() != approval.approved_capabilities.len() {
        Some("duplicate capability")
    } else if approved_resources.len() != approval.approved_resources.len() {
        Some("duplicate resource")
    } else if approval.approved_capabilities.iter().any(|approved| {
        !consent.capabilities.iter().any(|capability| {
            capability.eligible
                && capability.id == approved.id
                && capability.consent_digest == approved.consent_digest
        })
    }) {
        Some("capability eligibility")
    } else if approval.approved_resources.iter().any(|approved| {
        !consent.resources.iter().any(|resource| {
            resource.eligible
                && resource.kind == approved.kind
                && resource.name == approved.name
                && resource.requested_commitment == approved.commitment
        })
    }) {
        Some("resource eligibility")
    } else if consent.capabilities.iter().any(|capability| {
        capability.required
            && capability.eligible
            && !capability.already_approved
            && !approved_capabilities.contains(&ApprovedCapability {
                id: capability.id.clone(),
                consent_digest: capability.consent_digest.clone(),
            })
    }) {
        Some("required capability")
    } else if consent.resources.iter().any(|resource| {
        resource.required
            && resource.eligible
            && !resource.already_approved
            && !approved_resources.contains(&ApprovedResource {
                kind: resource.kind,
                name: resource.name.clone(),
                commitment: resource.requested_commitment.clone(),
            })
    }) {
        Some("required resource")
    } else {
        None
    };
    if let Some(rejection) = rejection {
        tracing::warn!(
            child_participant_id = %consent.participant_id,
            "companion approval did not match current eligible consent: {rejection}"
        );
        return Err(super::auth::AuthorizationStateError::NotAuthorized);
    }
    approval.approved_capabilities.extend(
        consent
            .capabilities
            .iter()
            .filter(|capability| capability.already_approved && capability.eligible)
            .map(|capability| ApprovedCapability {
                id: capability.id.clone(),
                consent_digest: capability.consent_digest.clone(),
            }),
    );
    approval.approved_resources.extend(
        consent
            .resources
            .iter()
            .filter(|resource| resource.already_approved && resource.eligible)
            .map(|resource| ApprovedResource {
                kind: resource.kind,
                name: resource.name.clone(),
                commitment: resource.requested_commitment.clone(),
            }),
    );
    approval.approved_capabilities.sort();
    approval.approved_capabilities.dedup();
    approval.approved_resources.sort();
    approval.approved_resources.dedup();
    let (_, child) = service
        .repository()
        .get_installed_participant_record(
            consent.participant_id.clone(),
            Some(consent.installed_revision),
        )
        .await?
        .ok_or(super::auth::AuthorizationStateError::ParticipantMissing)?;
    let current = service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            consent.participant_id.clone(),
        )
        .await?;
    let super::auth::policy::ConsentAuthority {
        ceiling,
        expires_at,
        provenance,
        preconditions,
    } = companion_consent_authority(service, user_principal_id, caller_participant_id, &child)
        .await?;
    if approval
        .delegation_ceiling
        .as_ref()
        .is_some_and(|submitted| {
            submitted.capabilities != ceiling.capabilities
                || submitted.exact_restrictions != ceiling.exact_restrictions
        })
    {
        tracing::warn!(
            child_participant_id = %consent.participant_id,
            "companion approval submitted delegation ceiling is stale"
        );
        return Err(super::auth::AuthorizationStateError::NotAuthorized);
    }
    let resources = service
        .repository()
        .resource_bindings(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            consent.participant_id.clone(),
            consent.installed_revision,
        )
        .await?;
    tracing::info!(
        event = "trellis.auth.companion_authority.resolve",
        child_participant_id = %consent.participant_id,
        approved_capability_count = approval.approved_capabilities.len(),
        approved_resource_count = approval.approved_resources.len(),
        ceiling_capability_count = ceiling.capabilities.len(),
        has_exact_restrictions = ceiling.exact_restrictions.is_some(),
        "resolving companion authority against portal policy ceiling"
    );
    let authority = super::auth::policy::resolve_authority(
        &child,
        ApprovalMode::Capabilities,
        &approval.approved_capabilities,
        &approval.approved_resources,
        &[],
        &ceiling,
        (&resources, false),
    )
    .map_err(|error| {
        tracing::warn!(
            child_participant_id = %consent.participant_id,
            ?error,
            "companion authority materialization failed"
        );
        error
    })?;
    let replacement = GrantBindingReplacement {
        owner_kind: GrantOwnerKind::User,
        owner_id: user_principal_id.to_owned(),
        participant_id: consent.participant_id,
        installed_revision: consent.installed_revision,
        grants: authority.exact_grants,
        approval_mode: ApprovalMode::Capabilities,
        approved_capabilities: approval.approved_capabilities,
        approved_resources: approval.approved_resources,
        delegation_ceiling: ceiling,
        approval_decision_digest: consent.decision_digest,
        companion_approved: false,
        platform_privileges: Vec::new(),
        expected_revision: consent.expected_grant_revision,
        expected_current_installed_revision: Some(consent.installed_revision),
        state: GrantBindingState::Active,
        expires_at,
        provenance,
    };
    let unchanged = current.as_ref().is_some_and(|binding| {
        binding.installed_revision == replacement.installed_revision
            && binding.grants == replacement.grants
            && binding.approval_mode == replacement.approval_mode
            && binding.approved_capabilities == replacement.approved_capabilities
            && binding.approved_resources == replacement.approved_resources
            && binding.delegation_ceiling == replacement.delegation_ceiling
            && binding.companion_approved == replacement.companion_approved
            && binding.platform_privileges == replacement.platform_privileges
            && binding.state == replacement.state
            && binding.expires_at == replacement.expires_at
            && binding.provenance == replacement.provenance
    });
    Ok((
        (!unchanged).then_some(replacement),
        request_digest,
        preconditions,
    ))
}

pub(crate) async fn companion_activation(
    service: &AuthService<SqliteAuthorizationStore>,
    review: &DeviceActivationReviewRecord,
    user_principal_id: &str,
    now: i64,
) -> Result<
    (Option<DeviceDelegationRecord>, Option<SessionRecord>),
    super::auth::AuthorizationStateError,
> {
    companion_activation_with_replacement(service, review, user_principal_id, now, None).await
}

async fn companion_activation_with_replacement(
    service: &AuthService<SqliteAuthorizationStore>,
    review: &DeviceActivationReviewRecord,
    user_principal_id: &str,
    now: i64,
    replacement: Option<&GrantBindingReplacement>,
) -> Result<
    (Option<DeviceDelegationRecord>, Option<SessionRecord>),
    super::auth::AuthorizationStateError,
> {
    let Some(claim) = review.payload.get("companion") else {
        return Ok((None, None));
    };
    let claim: CompanionClaim = serde_json::from_value(claim.clone())
        .map_err(|error| super::auth::AuthorizationStateError::InvalidRecord(error.to_string()))?;
    super::auth::verify_detached_ed25519_proof(
        &claim.installation_public_key,
        &claim.request_proof_digest,
        &claim.request_proof,
    )?;
    let (_, outer) = service
        .repository()
        .get_installed_participant_record(
            review.payload["participantId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            None,
        )
        .await?
        .ok_or(super::auth::AuthorizationStateError::ParticipantMissing)?;
    let projection = outer.resolve()?;
    if projection.companion_participant_id.as_deref() != Some(&claim.participant_id)
        || projection.companion_participant_kind != Some(claim.kind)
        || !matches!(
            claim.kind,
            trellis_protocol::ParticipantKind::App | trellis_protocol::ParticipantKind::Agent
        )
    {
        return Err(super::auth::AuthorizationStateError::InvalidRecord(
            "companion claim does not match the device descriptor".to_owned(),
        ));
    }
    let device_grant = service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::Deployment,
            review.deployment_id.clone(),
            projection.participant_id.clone(),
        )
        .await?
        .ok_or(super::auth::AuthorizationStateError::NotAuthorized)?;
    let child_grant = service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            claim.participant_id.clone(),
        )
        .await?;
    let (installed_revision, child) = service
        .repository()
        .get_installed_participant_record(claim.participant_id.clone(), None)
        .await?
        .ok_or(super::auth::AuthorizationStateError::ParticipantMissing)?;
    let (
        child_installed_revision,
        approval_mode,
        approved_capabilities,
        approved_resources,
        platform_privileges,
        grants,
        delegation_ceiling,
        expires_at,
        child_grant_revision,
    ) = if let Some(replacement) = replacement {
        if replacement.owner_kind != GrantOwnerKind::User
            || replacement.owner_id != user_principal_id
            || replacement.participant_id != claim.participant_id
            || replacement.state != super::auth::GrantBindingState::Active
        {
            return Err(super::auth::AuthorizationStateError::NotAuthorized);
        }
        (
            replacement.installed_revision,
            replacement.approval_mode,
            &replacement.approved_capabilities,
            &replacement.approved_resources,
            &replacement.platform_privileges,
            &replacement.grants,
            &replacement.delegation_ceiling,
            replacement.expires_at,
            replacement
                .expected_revision
                .checked_add(1)
                .ok_or_else(|| {
                    super::auth::AuthorizationStateError::InvalidRecord(
                        "grant revision overflow".to_owned(),
                    )
                })?,
        )
    } else if let Some(child_grant) = child_grant.as_ref() {
        (
            child_grant.installed_revision,
            child_grant.approval_mode,
            &child_grant.approved_capabilities,
            &child_grant.approved_resources,
            &child_grant.platform_privileges,
            &child_grant.grants,
            &child_grant.delegation_ceiling,
            child_grant.expires_at,
            child_grant.revision,
        )
    } else {
        return if projection.companion_required {
            Err(super::auth::AuthorizationStateError::NotAuthorized)
        } else {
            Ok((None, None))
        };
    };
    if child.participant_kind != claim.kind
        || child_installed_revision != installed_revision
        || approval_mode != super::auth::ApprovalMode::Capabilities
        || expires_at.is_some_and(|expires_at| expires_at <= now)
        || replacement.is_none()
            && child_grant
                .as_ref()
                .is_some_and(|binding| binding.state != super::auth::GrantBindingState::Active)
    {
        return Err(super::auth::AuthorizationStateError::NotAuthorized);
    }
    let resources = service
        .repository()
        .resource_bindings(
            GrantOwnerKind::User,
            user_principal_id.to_owned(),
            claim.participant_id.clone(),
            installed_revision,
        )
        .await?;
    let authority = super::auth::policy::resolve_authority(
        &child,
        approval_mode,
        approved_capabilities,
        approved_resources,
        platform_privileges,
        delegation_ceiling,
        (&resources, true),
    )?;
    if authority.exact_grants != *grants
        || authority
            .missing_required
            .iter()
            .any(|item| !item.starts_with("resource:"))
    {
        return Err(super::auth::AuthorizationStateError::NotAuthorized);
    }
    let session_id = ulid::Ulid::new().to_string();
    let session = SessionRecord {
        session_id: session_id.clone(),
        principal_id: user_principal_id.to_owned(),
        participant_id: claim.participant_id.clone(),
        participant_kind: claim.kind,
        session_key_id: crate::platform::auth::validate_ed25519_public_key(
            "installationPublicKey",
            &claim.installation_public_key,
        )?,
        session_public_key: claim.installation_public_key.clone(),
        state: SessionState::Active,
        created_at: now,
        last_authenticated_at: now,
        expires_at,
        revoked_at: None,
        version: 1,
    };
    Ok((
        Some(DeviceDelegationRecord {
            principal_id: review.principal_id.clone(),
            deployment_id: review.deployment_id.clone(),
            companion_participant_id: Some(claim.participant_id),
            user_login_session_id: Some(session_id),
            installation_public_key: Some(claim.installation_public_key),
            device_grant_revision: Some(device_grant.revision),
            child_grant_revision: Some(child_grant_revision),
            required: projection.companion_required,
            state: DeviceDelegationState::Active,
            expires_at,
        }),
        Some(session),
    ))
}
use crate::shutdown::StopHandle;
use crate::supervisor::RuntimeError;

const OPERATION: &str = "Auth.DeviceUserAuthorities.Resolve";

pub(crate) struct AuthOperationRuntime {
    client: async_nats::Client,
    router: Router,
    verifier: super::auth::verifier::RuntimeAuthVerifier,
}

impl AuthOperationRuntime {
    pub(crate) async fn new(
        client: async_nats::Client,
        _auth: SessionAuth,
        service: AuthService<SqliteAuthorizationStore>,
        verifier: super::auth::verifier::RuntimeAuthVerifier,
    ) -> Result<Self, RuntimeError> {
        const DEPLOYMENT_ID: &str = "trellis-auth-runtime";
        super::auth::resources::ensure_operation_store(&client, DEPLOYMENT_ID)
            .await
            .map_err(|error| RuntimeError::Nats(error.to_string()))?;
        let store = async_nats::jetstream::new(client.clone())
            .get_key_value(format!("trellis_operations_{DEPLOYMENT_ID}"))
            .await
            .map_err(|error| RuntimeError::Nats(error.to_string()))?;
        let staging = async_nats::jetstream::new(client.clone())
            .get_object_store(format!("trellis_operation_staging_{DEPLOYMENT_ID}"))
            .await
            .map_err(|error| RuntimeError::Nats(error.to_string()))?;
        let mut router = Router::new();
        router.set_provider_deployment_id("dep_trellis_auth_runtime");
        router.register_operation_handler::<AuthDeviceUserAuthoritiesResolveOperation, _, _, _>(
            trellis_rs::service::internal::OperationHandlerRuntime {
                service: "trellis.auth@v1".to_owned(),
                deployment_id: DEPLOYMENT_ID.to_owned(),
                executor_id: ulid::Ulid::new().to_string(),
                connection_id: ulid::Ulid::new().to_string(),
                repository: trellis_rs::service::KvOperationRepository::new(store),
                nats: client.clone(),
                service_session_key: "trellis-auth-runtime".to_owned(),
                staging: trellis_rs::service::internal::BoundStoreResourceClient::new(staging),
                validator: verifier.clone(),
            },
            move |context, input, operation| {
                let service = service.clone();
                async move {
                    let caller = caller_principal_id(&context)?;
                    let caller_participant = caller_participant_id(&context)?;
                    claim_activation(&service, caller, caller_participant, &input).await?;
                    approve_unreviewed_activation(&service, caller, caller_participant, &input)
                        .await?;
                    let snapshot = resolve_snapshot(&service, &context, &input.flow_id).await?;
                    match snapshot.state {
                        OperationState::Completed => {
                            operation
                                .complete(snapshot.output.ok_or_else(|| {
                                    ServerError::Nats(
                                        "completed auth operation has no output".to_owned(),
                                    )
                                })?)
                                .await?;
                        }
                        OperationState::Running => {
                            if let Some(progress) = snapshot.progress {
                                operation.progress(progress).await?;
                            }
                        }
                        _ => {}
                    }
                    Ok(())
                }
            },
        );
        Ok(Self {
            client,
            router,
            verifier,
        })
    }

    pub(crate) async fn run(self, stop: StopHandle) -> Result<(), RuntimeError> {
        self.router
            .recover_operations()
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        tokio::select! {
            result = trellis_rs::service::internal::run_builtin_authenticated_router(
                self.client,
                "trellis.auth@v1",
                &[
                    "operations.v1.Auth.DeviceUserAuthorities.Resolve",
                    "operations.v1.Auth.DeviceUserAuthorities.Resolve.>",
                ],
                self.router,
                self.verifier,
            ) => result.map_err(|error| RuntimeError::Platform(error.to_string())),
            () = stop.stopped() => Ok(()),
        }
    }
}

async fn claim_activation(
    service: &AuthService<SqliteAuthorizationStore>,
    caller: &str,
    caller_participant_id: &str,
    input: &AuthDeviceUserAuthoritiesResolveInput,
) -> Result<(), ServerError> {
    let now = now_ms()?;
    service
        .expire_due_activation_reviews(now)
        .await
        .map_err(server_error)?;
    let review = service
        .repository()
        .get_activation_review(&input.flow_id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| ServerError::Nats("activation review not found".to_owned()))?;
    let expected_confirmation_code = review
        .payload
        .get("confirmationCode")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ServerError::Nats("activation review is missing confirmation evidence".to_owned())
        })?;
    if !expected_confirmation_code.eq_ignore_ascii_case(&input.confirmation_code) {
        return Err(ServerError::Nats(
            "activation confirmation code is invalid".to_owned(),
        ));
    }
    if let Some(activated_by) = review.activated_by_user_principal_id.as_deref() {
        if activated_by != caller {
            Err(ServerError::Nats(
                "activation review belongs to another user".to_owned(),
            ))?;
        }
        if review.payload.get("companion").is_none() || input.companion_approval.is_none() {
            return Ok(());
        }
    }
    if !matches!(
        review.state,
        DeviceActivationReviewState::Pending | DeviceActivationReviewState::Approved
    ) {
        return Ok(());
    }
    let profile = service
        .repository()
        .get_deployment_profile(&review.deployment_id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| ServerError::Nats("activation deployment not found".to_owned()))?;
    let companion = review.payload.get("companion").is_some();
    if review.state == DeviceActivationReviewState::Pending && companion {
        return Ok(());
    }
    if review.state == DeviceActivationReviewState::Approved && companion {
        let approval = input
            .companion_approval
            .as_ref()
            .ok_or_else(|| ServerError::Nats("companion approval is required".to_owned()))?;
        let (_, request_digest) = decode_companion_approval(approval).map_err(server_error)?;
        let (replacement, authority, delegation, companion_session) = if review
            .activated_by_user_principal_id
            .is_some()
        {
            (None, None, None, None)
        } else {
            let (replacement, _, authority) =
                apply_companion_approval(service, &review, caller, caller_participant_id, approval)
                    .await
                    .map_err(server_error)?;
            let (delegation, session) = companion_activation_with_replacement(
                service,
                &review,
                caller,
                now,
                replacement.as_ref(),
            )
            .await
            .map_err(server_error)?;
            (replacement, Some(authority), delegation, session)
        };
        let mut requested = requested_event(&review, caller, now)?;
        requested.predecessor_action_id = Some(
            crate::platform::auth::activation_review_event_action_id(&review.review_id, "approved")
                .map_err(server_error)?,
        );
        let mut actions = vec![requested];
        if delegation.is_some() {
            actions.push(resolved_event(&review, now, "active")?);
        }
        service
            .repository()
            .apply_companion_activation_claim(
                replacement,
                authority,
                ActivationReviewClaim {
                    review_id: review.review_id.clone(),
                    expected_version: review.version,
                    activated_by_user_principal_id: caller.to_owned(),
                    now,
                    delegation,
                    companion_session,
                    idempotency: companion_activation_idempotency(
                        caller,
                        &review,
                        request_digest,
                        now,
                    )?,
                    actions,
                },
            )
            .await
            .map_err(server_error)?;
        return Ok(());
    }
    let (delegation, companion_session) = if review.state == DeviceActivationReviewState::Approved {
        companion_activation(service, &review, caller, now)
            .await
            .map_err(server_error)?
    } else {
        (None, None)
    };
    let mut requested = requested_event(&review, caller, now)?;
    let requested_predecessor = match (review.state, profile.review_mode) {
        (DeviceActivationReviewState::Approved, _) => Some("approved"),
        (DeviceActivationReviewState::Pending, Some(DeviceReviewMode::Required)) => {
            Some("review-requested")
        }
        (DeviceActivationReviewState::Pending, Some(DeviceReviewMode::None)) => None,
        _ => {
            return Err(ServerError::Nats(
                "device activation review policy is invalid".to_owned(),
            ));
        }
    };
    if let Some(event) = requested_predecessor {
        requested.predecessor_action_id = Some(
            crate::platform::auth::activation_review_event_action_id(&review.review_id, event)
                .map_err(server_error)?,
        );
    }
    let mut actions = vec![requested];
    if delegation.is_some() {
        actions.push(resolved_event(&review, now, "active")?);
    }
    service
        .claim_activation_review(ClaimActivationReviewInput {
            review_id: review.review_id.clone(),
            expected_version: review.version,
            activated_by_user_principal_id: caller.to_owned(),
            now,
            delegation,
            companion_session,
            idempotency: IdempotencyResultRecord {
                scope_key: resolve_scope_key(
                    "device.user-authority.resolve.claim",
                    caller,
                    &review.review_id,
                )?,
                purpose: "device.user-authority.resolve.claim".to_owned(),
                signer_id: caller.to_owned(),
                request_id: review.review_id.clone(),
                request_digest: review.request_digest.clone(),
                result: Value::Null,
                created_at: now,
                expires_at: now
                    .checked_add(86_400_000)
                    .ok_or_else(|| ServerError::Nats("idempotency expiry overflow".to_owned()))?,
            },
            actions,
        })
        .await
        .map_err(server_error)?;
    Ok(())
}

async fn approve_unreviewed_activation(
    service: &AuthService<SqliteAuthorizationStore>,
    caller: &str,
    caller_participant_id: &str,
    input: &AuthDeviceUserAuthoritiesResolveInput,
) -> Result<(), ServerError> {
    let review = service
        .repository()
        .get_activation_review(&input.flow_id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| ServerError::Nats("activation review not found".to_owned()))?;
    if review.state != DeviceActivationReviewState::Pending {
        if review.state == DeviceActivationReviewState::Approved
            && review.payload.get("companion").is_some()
            && review.activated_by_user_principal_id.as_deref() == Some(caller)
        {
            let approval = input
                .companion_approval
                .as_ref()
                .ok_or_else(|| ServerError::Nats("companion approval is required".to_owned()))?;
            let (_, request_digest) = decode_companion_approval(approval).map_err(server_error)?;
            let now = now_ms()?;
            service
                .repository()
                .apply_companion_activation_claim(
                    None,
                    None,
                    ActivationReviewClaim {
                        review_id: review.review_id.clone(),
                        expected_version: review.version,
                        activated_by_user_principal_id: caller.to_owned(),
                        now,
                        delegation: None,
                        companion_session: None,
                        idempotency: companion_activation_idempotency(
                            caller,
                            &review,
                            request_digest,
                            now,
                        )?,
                        actions: Vec::new(),
                    },
                )
                .await
                .map_err(server_error)?;
        }
        return Ok(());
    }
    let profile = service
        .repository()
        .get_deployment_profile(&review.deployment_id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| ServerError::Nats("activation deployment not found".to_owned()))?;
    if profile.review_mode == Some(DeviceReviewMode::Required) {
        return Ok(());
    }
    if profile.review_mode != Some(DeviceReviewMode::None) {
        return Err(ServerError::Nats(
            "device activation review policy is invalid".to_owned(),
        ));
    }
    let companion = review.payload.get("companion").is_some();
    if companion && input.companion_approval.is_none() {
        return Ok(());
    }
    let now = now_ms()?;
    let mut approved = approved_event(&review, caller, now)?;
    approved.predecessor_action_id = Some(
        crate::platform::auth::activation_review_event_action_id(&review.review_id, "requested")
            .map_err(server_error)?,
    );
    let mut resolved = resolved_event(&review, now, "active")?;
    resolved.predecessor_action_id = Some(
        crate::platform::auth::activation_review_event_action_id(&review.review_id, "approved")
            .map_err(server_error)?,
    );
    if companion {
        let approval = input
            .companion_approval
            .as_ref()
            .ok_or_else(|| ServerError::Nats("companion approval is required".to_owned()))?;
        let (replacement, request_digest, policy) =
            apply_companion_approval(
                service,
                &review,
                caller,
                caller_participant_id,
                approval,
            )
                .await
                .map_err(|error| {
                    tracing::warn!(?error, review_id = %review.review_id, "companion approval resolution failed");
                    server_error(error)
                })?;
        let (delegation, companion_session) = companion_activation_with_replacement(
            service,
            &review,
            caller,
            now,
            replacement.as_ref(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(?error, review_id = %review.review_id, "companion activation construction failed");
            server_error(error)
        })?;
        service
            .repository()
            .apply_companion_activation_decision(
                replacement,
                Some(policy),
                ActivationReviewDecision {
                    review_id: review.review_id.clone(),
                    expected_version: review.version,
                    state: DeviceActivationReviewState::Approved,
                    decided_at: now,
                    decided_by: caller.to_owned(),
                    reason: None,
                    delegation,
                    companion_session,
                    activate_device: true,
                    idempotency: companion_activation_idempotency(
                        caller,
                        &review,
                        request_digest,
                        now,
                    )?,
                    actions: vec![approved, resolved],
                },
            )
            .await
            .map_err(|error| {
                tracing::warn!(?error, review_id = %review.review_id, "companion activation commit failed");
                server_error(error)
            })?;
        tracing::info!(
            event = "trellis.auth.companion_activation.committed",
            review_id = %review.review_id,
            "committed companion grant, delegation, session, and device activation"
        );
        return Ok(());
    }
    let (delegation, companion_session) = if profile.requires_device_delegation {
        companion_activation(service, &review, caller, now)
            .await
            .map_err(server_error)?
    } else {
        (None, None)
    };
    service
        .decide_activation_review(DecideActivationReviewInput {
            review_id: review.review_id.clone(),
            expected_version: review.version,
            state: DeviceActivationReviewState::Approved,
            decided_at: now,
            decided_by: caller.to_owned(),
            reason: None,
            delegation,
            companion_session,
            activate_device: true,
            idempotency: IdempotencyResultRecord {
                scope_key: resolve_scope_key(
                    "device.user-authority.resolve.approve",
                    caller,
                    &review.review_id,
                )?,
                purpose: "device.user-authority.resolve.approve".to_owned(),
                signer_id: caller.to_owned(),
                request_id: review.review_id.clone(),
                request_digest: review.request_digest.clone(),
                result: Value::Null,
                created_at: now,
                expires_at: now
                    .checked_add(86_400_000)
                    .ok_or_else(|| ServerError::Nats("idempotency expiry overflow".to_owned()))?,
            },
            actions: vec![approved, resolved],
        })
        .await
        .map_err(server_error)?;
    Ok(())
}

async fn resolve_snapshot(
    service: &AuthService<SqliteAuthorizationStore>,
    context: &RequestContext,
    flow_id: &str,
) -> Result<
    OperationSnapshot<
        AuthDeviceUserAuthoritiesResolveProgress,
        AuthDeviceUserAuthoritiesResolveOutput,
    >,
    ServerError,
> {
    let review = service
        .repository()
        .get_activation_review(flow_id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| ServerError::Nats("activation review not found".to_owned()))?;
    let caller = caller_principal_id(context)?;
    if review
        .activated_by_user_principal_id
        .as_deref()
        .is_some_and(|activated_by| activated_by != caller)
    {
        return Err(ServerError::Nats(
            "activation review belongs to another user".to_owned(),
        ));
    }
    match review.state {
        DeviceActivationReviewState::Pending => Ok(snapshot(
            &review,
            OperationState::Running,
            Some(serde_json::from_value(json!({
                    "state": "review_pending",
                    "retryAfterMs": "1000",
                    "companionConsent": companion_consent_value(
                        companion_consent(
                            service,
                            &review,
                            caller,
                            caller_participant_id(context)?,
                        )
                            .await
                            .map_err(server_error)?,
                    )?,
            }))?),
            None,
        )),
        DeviceActivationReviewState::Approved => {
            let profile = service
                .repository()
                .get_deployment_profile(&review.deployment_id)
                .await
                .map_err(server_error)?
                .ok_or_else(|| ServerError::Nats("activation deployment not found".to_owned()))?;
            let device = service
                .repository()
                .get_device(&review.principal_id, &review.deployment_id)
                .await
                .map_err(server_error)?
                .ok_or_else(|| ServerError::Nats("activation device not found".to_owned()))?;
            let delegation = service
                .repository()
                .get_device_delegation(&review.principal_id, &review.deployment_id)
                .await
                .map_err(server_error)?;
            if review.payload.get("companion").is_some() && delegation.is_none() {
                return Ok(snapshot(
                    &review,
                    OperationState::Running,
                    Some(serde_json::from_value(json!({
                        "state": "delegation_pending",
                        "retryAfterMs": "1000",
                        "companionConsent": companion_consent_value(
                            companion_consent(
                                service,
                                &review,
                                caller,
                                caller_participant_id(context)?,
                            )
                                .await
                                .map_err(server_error)?,
                        )?,
                    }))?),
                    None,
                ));
            }
            if profile.requires_device_delegation && review.activated_by_user_principal_id.is_none()
            {
                return Ok(snapshot(
                    &review,
                    OperationState::Running,
                    Some(serde_json::from_value(
                        json!({"state": "review_pending", "retryAfterMs": "1000"}),
                    )?),
                    None,
                ));
            }
            if profile.requires_device_delegation
                && delegation
                    .as_ref()
                    .is_none_or(|delegation| delegation.state != DeviceDelegationState::Active)
            {
                return Err(ServerError::Nats(
                    "approved activation is missing its required delegation".to_owned(),
                ));
            }
            let participant_id = service
                .repository()
                .get_deployment_evidence(&review.deployment_id)
                .await
                .map_err(server_error)?
                .map(|deployment| deployment.participant_id);
            let output = serde_json::from_value(json!({
                "device": {
                    "instanceId": review.instance_id,
                    "deploymentId": review.deployment_id,
                    "principalId": review.principal_id,
                    "identityPublicKey": null,
                    "identityKeyId": null,
                    "participantId": participant_id,
                    "state": "active",
                    "administrativeApproval": "approved",
                    "delegationRequired": profile.requires_device_delegation,
                    "delegationState": "active",
                    "delegationExpiresAt": delegation.and_then(|delegation| delegation.expires_at).map(|value| value.to_string()),
                    "createdAt": device.created_at.to_string(),
                    "updatedAt": device.updated_at.to_string(),
                    "version": device.version.to_string(),
                },
                "review": {
                    "reviewId": review.review_id,
                    "deploymentId": review.deployment_id,
                    "instanceId": review.instance_id,
                    "devicePrincipalId": review.principal_id,
                    "activatedByUserPrincipalId": review.activated_by_user_principal_id,
                    "state": "approved",
                    "requestedAt": review.requested_at.to_string(),
                    "expiresAt": review.expires_at.to_string(),
                    "decidedAt": review.decided_at.map(|value| value.to_string()),
                    "decidedBy": review.decided_by,
                    "reason": review.reason,
                    "version": review.version.to_string(),
                },
            }))?;
            Ok(snapshot(
                &review,
                OperationState::Completed,
                None,
                Some(output),
            ))
        }
        DeviceActivationReviewState::Rejected => {
            let device = service
                .repository()
                .get_device(&review.principal_id, &review.deployment_id)
                .await
                .map_err(server_error)?
                .ok_or_else(|| ServerError::Nats("activation device not found".to_owned()))?;
            let profile = service
                .repository()
                .get_deployment_profile(&review.deployment_id)
                .await
                .map_err(server_error)?
                .ok_or_else(|| ServerError::Nats("activation deployment not found".to_owned()))?;
            let output = serde_json::from_value(json!({
                "device": {
                    "instanceId": review.instance_id,
                    "deploymentId": review.deployment_id,
                    "principalId": review.principal_id,
                    "identityPublicKey": null,
                    "identityKeyId": null,
                    "participantId": profile.participant_id,
                    "state": device.state,
                    "administrativeApproval": "rejected",
                    "delegationRequired": profile.requires_device_delegation,
                    "delegationState": "missing",
                    "delegationExpiresAt": null,
                    "createdAt": device.created_at.to_string(),
                    "updatedAt": device.updated_at.to_string(),
                    "version": device.version.to_string(),
                },
                "review": {
                    "reviewId": review.review_id,
                    "deploymentId": review.deployment_id,
                    "instanceId": review.instance_id,
                    "devicePrincipalId": review.principal_id,
                    "activatedByUserPrincipalId": review.activated_by_user_principal_id,
                    "state": "rejected",
                    "requestedAt": review.requested_at.to_string(),
                    "expiresAt": review.expires_at.to_string(),
                    "decidedAt": review.decided_at.map(|value| value.to_string()),
                    "decidedBy": review.decided_by,
                    "reason": review.reason,
                    "version": review.version.to_string(),
                },
            }))?;
            Ok(snapshot(
                &review,
                OperationState::Completed,
                None,
                Some(output),
            ))
        }
        DeviceActivationReviewState::Expired => {
            let device = service
                .repository()
                .get_device(&review.principal_id, &review.deployment_id)
                .await
                .map_err(server_error)?
                .ok_or_else(|| ServerError::Nats("activation device not found".to_owned()))?;
            let profile = service
                .repository()
                .get_deployment_profile(&review.deployment_id)
                .await
                .map_err(server_error)?
                .ok_or_else(|| ServerError::Nats("activation deployment not found".to_owned()))?;
            let output = serde_json::from_value(json!({
                "device": {
                    "instanceId": review.instance_id,
                    "deploymentId": review.deployment_id,
                    "principalId": review.principal_id,
                    "identityPublicKey": null,
                    "identityKeyId": null,
                    "participantId": profile.participant_id,
                    "state": device.state,
                    "administrativeApproval": if review.decided_at.is_some() { "approved" } else { "pending" },
                    "delegationRequired": profile.requires_device_delegation,
                    "delegationState": "missing",
                    "delegationExpiresAt": null,
                    "createdAt": device.created_at.to_string(),
                    "updatedAt": device.updated_at.to_string(),
                    "version": device.version.to_string(),
                },
                "review": {
                    "reviewId": review.review_id,
                    "deploymentId": review.deployment_id,
                    "instanceId": review.instance_id,
                    "devicePrincipalId": review.principal_id,
                    "activatedByUserPrincipalId": review.activated_by_user_principal_id,
                    "state": "expired",
                    "requestedAt": review.requested_at.to_string(),
                    "expiresAt": review.expires_at.to_string(),
                    "decidedAt": review.decided_at.map(|value| value.to_string()),
                    "decidedBy": review.decided_by,
                    "reason": review.reason,
                    "version": review.version.to_string(),
                },
            }))?;
            Ok(snapshot(
                &review,
                OperationState::Completed,
                None,
                Some(output),
            ))
        }
    }
}

fn companion_activation_idempotency(
    caller: &str,
    review: &DeviceActivationReviewRecord,
    request_digest: String,
    now: i64,
) -> Result<IdempotencyResultRecord, ServerError> {
    const PURPOSE: &str = "device.companion-activation.approve";
    Ok(IdempotencyResultRecord {
        scope_key: resolve_scope_key(PURPOSE, caller, &review.review_id)?,
        purpose: PURPOSE.to_owned(),
        signer_id: caller.to_owned(),
        request_id: review.review_id.clone(),
        request_digest,
        result: Value::Null,
        created_at: now,
        expires_at: now
            .checked_add(86_400_000)
            .ok_or_else(|| ServerError::Nats("idempotency expiry overflow".to_owned()))?,
    })
}

fn resolve_scope_key(purpose: &str, caller: &str, review_id: &str) -> Result<String, ServerError> {
    trellis_protocol::digest_json(&json!({
        "purpose": purpose,
        "signerId": caller,
        "requestId": review_id,
    }))
    .map_err(|error| ServerError::Nats(error.to_string()))
}

fn snapshot(
    review: &DeviceActivationReviewRecord,
    state: OperationState,
    progress: Option<AuthDeviceUserAuthoritiesResolveProgress>,
    output: Option<AuthDeviceUserAuthoritiesResolveOutput>,
) -> OperationSnapshot<
    AuthDeviceUserAuthoritiesResolveProgress,
    AuthDeviceUserAuthoritiesResolveOutput,
> {
    OperationSnapshot {
        id: Some(review.review_id.clone()),
        service: Some("trellis.auth@v1".to_owned()),
        operation: Some(OPERATION.to_owned()),
        revision: review.version,
        state,
        created_at: None,
        updated_at: None,
        completed_at: None,
        progress,
        transfer: None,
        output,
        error: None,
    }
}

fn caller_principal_id(context: &RequestContext) -> Result<&str, ServerError> {
    let caller = context
        .caller
        .as_ref()
        .ok_or_else(|| ServerError::Nats("authenticated user principal is missing".to_owned()))?;
    if caller.principal_kind != trellis_protocol::AuthorizationPrincipalKind::User {
        return Err(ServerError::Nats(
            "device activation requires a user principal".to_owned(),
        ));
    }
    if caller.principal_id.is_empty() {
        return Err(ServerError::Nats(
            "authenticated user principal is missing".to_owned(),
        ));
    }
    Ok(&caller.principal_id)
}

fn caller_participant_id(context: &RequestContext) -> Result<&str, ServerError> {
    context
        .caller
        .as_ref()
        .map(|caller| caller.participant_id.as_str())
        .filter(|participant_id| !participant_id.is_empty())
        .ok_or_else(|| ServerError::Nats("authenticated participant is missing".to_owned()))
}

fn requested_event(
    review: &DeviceActivationReviewRecord,
    caller: &str,
    now: i64,
) -> Result<PostCommitActionRecord, ServerError> {
    activation_event::<
        trellis_runtime_apis::apis::trellis_auth_v1::events::DeviceUserAuthoritiesRequested,
    >(
        review,
        "requested",
        "Auth.DeviceUserAuthorities.Requested",
        now,
        json!({
            "userPrincipalId": caller,
            "requestedAt": now,
        }),
    )
}

fn approved_event(
    review: &DeviceActivationReviewRecord,
    caller: &str,
    now: i64,
) -> Result<PostCommitActionRecord, ServerError> {
    activation_event::<
        trellis_runtime_apis::apis::trellis_auth_v1::events::DeviceUserAuthoritiesApproved,
    >(
        review,
        "approved",
        "Auth.DeviceUserAuthorities.Approved",
        now,
        json!({
            "approvedBy": caller,
            "approvedAt": now,
        }),
    )
}

fn resolved_event(
    review: &DeviceActivationReviewRecord,
    now: i64,
    state: &str,
) -> Result<PostCommitActionRecord, ServerError> {
    activation_event::<
        trellis_runtime_apis::apis::trellis_auth_v1::events::DeviceUserAuthoritiesResolved,
    >(
        review,
        "resolved",
        "Auth.DeviceUserAuthorities.Resolved",
        now,
        json!({ "state": state }),
    )
}

fn activation_event<D: trellis_rs::client::EventDescriptor>(
    review: &DeviceActivationReviewRecord,
    suffix: &str,
    event_type: &str,
    now: i64,
    fields: Value,
) -> Result<PostCommitActionRecord, ServerError> {
    crate::platform::auth::activation_review_event::<D>(review, suffix, event_type, now, fields)
        .map_err(server_error)
}

fn now_ms() -> Result<i64, ServerError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(server_error)?
            .as_millis(),
    )
    .map_err(|_| ServerError::Nats("current time exceeds i64 milliseconds".to_owned()))
}

fn server_error(error: impl std::fmt::Display) -> ServerError {
    tracing::warn!(%error, "auth operation failed");
    ServerError::Nats("auth_operation_failed".to_owned())
}

#[cfg(test)]
mod tests {
    use super::server_error;

    #[test]
    fn operation_errors_never_expose_internal_causes() {
        let secret = "postgres://admin:secret@internal/auth";
        let encoded = format!("{:?}", server_error(secret));
        assert!(!encoded.contains(secret));
        assert!(encoded.contains("auth_operation_failed"));
    }
}
