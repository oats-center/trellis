use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use trellis_protocol::{
    GrantSet, ParticipantResourceKind, PermissionAtom, PermissionTarget, PlatformPrivilege,
};

use super::{
    ephemeral::{
        ConsentCapability, ConsentCompanion, ConsentOwnerKind, ConsentRequest, ConsentResource,
        ConsentResourceActualEntry, ConsentResourceChange,
    },
    ApprovalMode, ApprovedCapability, ApprovedResource, AuthorizationResourceKind,
    AuthorizationStateError, CapabilityGroupRecord, ConsentAuthorityPreconditions,
    ConsentBindingPrecondition, DelegationCeiling, GrantBinding, LoginPortalRecord,
    LoginSettingsRecord, ParticipantBindingRecord, PortalGrantOverrideRecord,
    PortalGrantProvenance, PortalPolicySnapshot, ResourceBindingEvidence, ResourceBindingState,
    ResourceCommitment,
};

pub(crate) fn portal_allows_authenticated_provider(
    portal: &LoginPortalRecord,
    settings: &LoginSettingsRecord,
    provider_id: &str,
) -> bool {
    !portal.disabled
        && !portal.removed
        && portal.provider_ids.iter().any(|id| id == provider_id)
        && (provider_id != "local" || settings.local_login_enabled)
}

pub(crate) fn portal_policy_snapshot(
    portal: &LoginPortalRecord,
    settings: &LoginSettingsRecord,
    participant_id: &str,
    policy: Option<&PortalGrantOverrideRecord>,
    groups: &BTreeMap<String, CapabilityGroupRecord>,
) -> Result<PortalPolicySnapshot, AuthorizationStateError> {
    if settings.portal_id != portal.portal_id
        || policy.is_some_and(|policy| {
            policy.portal_id != portal.portal_id || policy.participant_id != participant_id
        })
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "portal policy snapshot inputs disagree".to_owned(),
        ));
    }
    let mut pending = policy
        .into_iter()
        .flat_map(|policy| {
            policy
                .capability_group_keys
                .iter()
                .chain(
                    policy
                        .role_mappings
                        .iter()
                        .flat_map(|mapping| mapping.capability_group_keys.iter()),
                )
                .cloned()
        })
        .collect::<Vec<_>>();
    let mut versions = BTreeMap::new();
    let mut fingerprints = BTreeMap::new();
    while let Some(group_key) = pending.pop() {
        if versions.contains_key(&group_key) {
            continue;
        }
        let group = groups.get(&group_key).ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(format!(
                "portal policy references missing capability group {group_key}"
            ))
        })?;
        versions.insert(group_key, group.version);
        fingerprints.insert(group.group_key.clone(), policy_record_fingerprint(group)?);
        pending.extend(group.included_groups.iter().cloned());
    }
    Ok(PortalPolicySnapshot {
        portal_id: portal.portal_id.clone(),
        portal_version: portal.version,
        portal_fingerprint: policy_record_fingerprint(portal)?,
        login_settings_version: settings.version,
        login_settings_fingerprint: policy_record_fingerprint(settings)?,
        participant_id: participant_id.to_owned(),
        policy_version: policy.map(|policy| policy.version),
        policy_fingerprint: policy.map(policy_record_fingerprint).transpose()?,
        capability_group_versions: versions.into_iter().collect(),
        capability_group_fingerprints: fingerprints.into_iter().collect(),
    })
}

pub(super) fn policy_record_fingerprint<T: Serialize>(
    record: &T,
) -> Result<String, AuthorizationStateError> {
    let value = serde_json::to_value(record)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    trellis_protocol::digest_json(&value)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
}

pub(crate) fn consent_request(
    binding: &ParticipantBindingRecord,
    installed_revision: u64,
    current: Option<&GrantBinding>,
    ceiling: &DelegationCeiling,
    actuals: &[ConsentResourceActualEntry],
    companion_binding: Option<(
        &ParticipantBindingRecord,
        Option<&GrantBinding>,
        &[ConsentResourceActualEntry],
        &DelegationCeiling,
    )>,
) -> Result<ConsentRequest, AuthorizationStateError> {
    let resolved = binding.resolve()?;
    let ceiling_capabilities = ceiling.capabilities.iter().collect::<BTreeSet<_>>();
    let approved_capabilities = current
        .into_iter()
        .flat_map(|current| current.approved_capabilities.iter())
        .collect::<BTreeSet<_>>();
    let mut capabilities = resolved
        .referenced_apis
        .values()
        .flat_map(|api| api.capabilities.iter())
        .filter(|(id, capability)| {
            selected_permissions(resolved).any(|permission| capability.allows.contains(permission))
                || resolved.required_capabilities.contains(id)
                || resolved.optional_capability_definitions.contains_key(*id)
        })
        .map(|(id, capability)| {
            let fingerprint = ApprovedCapability {
                id: id.clone(),
                consent_digest: capability.consent_digest.clone(),
            };
            ConsentCapability {
                id: id.clone(),
                title: capability.display_name.clone(),
                description: capability.description.clone(),
                consequence: capability.consequence.clone(),
                consent_digest: capability.consent_digest.clone(),
                required: !resolved.optional_capability_definitions.contains_key(id),
                eligible: capability.public || ceiling_capabilities.contains(&fingerprint),
                already_approved: approved_capabilities.contains(&fingerprint),
            }
        })
        .collect::<Vec<_>>();
    capabilities.sort_by(|left, right| left.id.cmp(&right.id));
    let mut resources = resolved
        .resources
        .iter()
        .map(|(name, resource)| {
            let requested_commitment = resource_commitment(resource);
            let approved = current.and_then(|current| {
                current
                    .approved_resources
                    .iter()
                    .find(|approved| approved.name == *name)
            });
            let kind = AuthorizationResourceKind::from(resource.kind);
            let already_approved = approved.is_some_and(|approved| {
                approved.kind == kind
                    && hard_commitment_eq(&approved.commitment, &requested_commitment)
            });
            let change = classify_resource_change(approved, kind, &requested_commitment);
            ConsentResource {
                kind,
                name: name.clone(),
                title: resource.title.clone(),
                description: resource.description.clone(),
                required: !resource.optional,
                requested_commitment,
                change,
                actual: actuals
                    .iter()
                    .find(|actual| actual.kind == kind && actual.name == *name)
                    .map(|actual| actual.actual.clone()),
                eligible: true,
                already_approved,
            }
        })
        .collect::<Vec<_>>();
    if let Some(current) = current {
        resources.extend(
            current
                .approved_resources
                .iter()
                .filter(|approved| !resolved.resources.contains_key(&approved.name))
                .map(|approved| ConsentResource {
                    kind: approved.kind,
                    name: approved.name.clone(),
                    title: approved.name.clone(),
                    description: String::new(),
                    required: false,
                    requested_commitment: approved.commitment.clone(),
                    actual: None,
                    change: ConsentResourceChange::Detached,
                    eligible: false,
                    already_approved: true,
                }),
        );
    }
    resources.sort_by(|left, right| (left.kind, &left.name).cmp(&(right.kind, &right.name)));
    let companion = if let Some(participant_id) = &resolved.companion_participant_id {
        let (child, current, child_actuals, child_ceiling) =
            companion_binding.ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "companion participant definition is missing".to_owned(),
                )
            })?;
        if child.participant_id != *participant_id {
            return Err(AuthorizationStateError::InvalidRecord(
                "companion participant definition does not match".to_owned(),
            ));
        }
        let child_consent = consent_request(
            child,
            installed_revision,
            current,
            child_ceiling,
            child_actuals,
            None,
        )?;
        Some(ConsentCompanion {
            participant_id: participant_id.clone(),
            kind: resolved
                .companion_participant_kind
                .map(ConsentOwnerKind::from)
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(
                        "companion participant kind is missing".to_owned(),
                    )
                })?,
            required: resolved.companion_required,
            capabilities: child_consent.capabilities,
            resources: child_consent.resources,
        })
    } else {
        None
    };
    let mut consent = ConsentRequest {
        participant_id: binding.participant_id.clone(),
        package_digest: binding.package_digest.clone(),
        installed_revision,
        expected_grant_revision: current.map_or(0, |current| current.revision),
        capabilities,
        resources,
        companion,
        decision_digest: String::new(),
    };
    consent.decision_digest = consent.computed_decision_digest()?;
    consent.validate()?;
    Ok(consent)
}

fn classify_resource_change(
    current: Option<&ApprovedResource>,
    requested_kind: AuthorizationResourceKind,
    requested: &ResourceCommitment,
) -> ConsentResourceChange {
    let Some(current) = current else {
        return ConsentResourceChange::New;
    };
    if current.kind != requested_kind {
        ConsentResourceChange::Incompatible
    } else if hard_commitment_eq(&current.commitment, requested) {
        ConsentResourceChange::Unchanged
    } else if commitment_is_reduced(&current.commitment, requested) {
        ConsentResourceChange::Reduced
    } else {
        ConsentResourceChange::Expanded
    }
}

fn commitment_is_reduced(current: &ResourceCommitment, requested: &ResourceCommitment) -> bool {
    let limit = |current: Option<u64>, requested: Option<u64>| match (current, requested) {
        (Some(current), Some(requested)) => requested <= current,
        (None, None) => true,
        _ => false,
    };
    limit(current.history, requested.history) && limit(current.ttl_ms, requested.ttl_ms)
}

fn hard_commitment_eq(current: &ResourceCommitment, requested: &ResourceCommitment) -> bool {
    current.history == requested.history && current.ttl_ms == requested.ttl_ms
}

fn selected_permissions(
    participant: &super::evidence::ParticipantRuntimeProjection,
) -> impl Iterator<Item = &PermissionAtom> {
    participant.required_grants.permissions().iter().chain(
        participant
            .optional_grant_bundles
            .values()
            .flat_map(|grants| grants.permissions()),
    )
}

pub(crate) fn participant_delegation_ceiling(
    binding: &ParticipantBindingRecord,
) -> Result<DelegationCeiling, AuthorizationStateError> {
    let participant = binding.resolve()?;
    let mut capabilities = participant
        .referenced_apis
        .values()
        .flat_map(|api| api.capabilities.iter())
        .filter(|(_, capability)| {
            selected_permissions(participant)
                .any(|permission| capability.allows.contains(permission))
        })
        .map(|(id, capability)| ApprovedCapability {
            id: id.clone(),
            consent_digest: capability.consent_digest.clone(),
        })
        .collect::<Vec<_>>();
    capabilities.sort();
    capabilities.dedup();
    Ok(DelegationCeiling {
        capabilities,
        exact_restrictions: None,
        platform_privileges: Vec::new(),
    })
}

pub(crate) fn participant_resource_commitments(
    participant: &ParticipantBindingRecord,
) -> Result<Vec<ApprovedResource>, AuthorizationStateError> {
    Ok(participant
        .resolve()?
        .resources
        .iter()
        .map(|(name, resource)| ApprovedResource {
            kind: resource.kind.into(),
            name: name.clone(),
            commitment: resource_commitment(resource),
        })
        .collect())
}

fn resource_commitment(
    resource: &super::evidence::ResourceRuntimeProjection,
) -> ResourceCommitment {
    ResourceCommitment {
        desired_max_object_bytes: resource.desired_max_object,
        desired_max_total_bytes: resource.desired_max_total,
        desired_max_value_bytes: resource.desired_max_value,
        history: resource.history,
        ttl_ms: resource.ttl_ms,
    }
}

/// Why an approved capability or resource is currently unavailable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AvailabilityReason {
    NotApproved,
    NotDelegable,
    ProviderUnavailable,
    ProviderIncompatible,
    ResourceUnavailable,
    CompanionUnavailable,
    NotSelected,
}

/// Current availability of one approved surface.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Availability {
    pub available: bool,
    pub reason: Option<AvailabilityReason>,
}

/// Pure result used by consent, reconciliation, and issuance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedAuthority {
    pub exact_grants: GrantSet,
    pub platform_privileges: Vec<PlatformPrivilege>,
    pub capabilities: BTreeMap<String, Availability>,
    pub resources: BTreeMap<String, Availability>,
    pub apis: BTreeMap<String, Availability>,
    pub companion: Option<Availability>,
    pub readiness: bool,
    pub missing_required: Vec<String>,
}

pub(crate) fn resolve_authority(
    participant: &ParticipantBindingRecord,
    approval_mode: ApprovalMode,
    approved_capabilities: &[ApprovedCapability],
    approved_resources: &[ApprovedResource],
    approved_platform_privileges: &[PlatformPrivilege],
    ceiling: &DelegationCeiling,
    availability: (&[ResourceBindingEvidence], bool),
) -> Result<ResolvedAuthority, AuthorizationStateError> {
    let (usable_resources, companion_available) = availability;
    let resolved = participant.resolve()?;
    let selected = selected_permissions(resolved).cloned().collect::<Vec<_>>();
    let capabilities = resolved
        .referenced_apis
        .values()
        .flat_map(|api| api.capabilities.iter())
        .collect::<BTreeMap<_, _>>();
    let implicated = capabilities
        .iter()
        .filter(|(_, capability)| {
            capability
                .allows
                .iter()
                .any(|permission| selected.contains(permission))
        })
        .collect::<BTreeMap<_, _>>();
    let ceiling_capabilities = ceiling.capabilities.iter().collect::<BTreeSet<_>>();
    let approved_capabilities = approved_capabilities.iter().collect::<BTreeSet<_>>();
    let exact_restrictions = ceiling
        .exact_restrictions
        .as_ref()
        .map(|grants| grants.permissions().to_vec());
    if approved_capabilities
        .iter()
        .chain(&ceiling_capabilities)
        .any(|approved| {
            capabilities
                .get(&approved.id)
                .is_none_or(|capability| capability.consent_digest != approved.consent_digest)
        })
        || approved_resources.iter().any(|approved| {
            resolved
                .resources
                .get(&approved.name)
                .is_none_or(|resource| {
                    AuthorizationResourceKind::from(resource.kind) != approved.kind
                        || resource_commitment(resource) != approved.commitment
                })
        })
        || exact_restrictions
            .as_ref()
            .is_some_and(|restrictions| restrictions.iter().any(|atom| !selected.contains(atom)))
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "approval or delegation ceiling is outside the installed participant definitions"
                .to_owned(),
        ));
    }
    let mut capability_availability = BTreeMap::new();
    let mut allowed = if approval_mode == ApprovalMode::Exact {
        exact_restrictions
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|permission| selected.contains(permission))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut missing_required = Vec::new();
    for (id, capability) in implicated {
        let fingerprint = ApprovedCapability {
            id: (*id).clone(),
            consent_digest: capability.consent_digest.clone(),
        };
        let approved = capability.public || approved_capabilities.contains(&fingerprint);
        let delegable = capability.public || ceiling_capabilities.contains(&fingerprint);
        let available = if approval_mode == ApprovalMode::Exact {
            capability
                .allows
                .iter()
                .filter(|permission| selected.contains(*permission))
                .all(|permission| {
                    exact_restrictions
                        .as_ref()
                        .is_some_and(|restrictions| restrictions.contains(permission))
                })
        } else {
            approved && delegable
        };
        capability_availability.insert(
            (*id).clone(),
            Availability {
                available,
                reason: (!available).then_some(if approved {
                    AvailabilityReason::NotDelegable
                } else {
                    AvailabilityReason::NotApproved
                }),
            },
        );
        if available {
            allowed.extend(
                capability
                    .allows
                    .iter()
                    .filter(|permission| selected.contains(*permission))
                    .cloned(),
            );
        } else if approval_mode == ApprovalMode::Capabilities
            && !capability.public
            && !resolved.optional_capability_definitions.contains_key(*id)
        {
            missing_required.push(format!("capability:{id}"));
        }
    }
    if approval_mode == ApprovalMode::Capabilities {
        for permission in &selected {
            if !capabilities
                .values()
                .any(|capability| capability.allows.contains(permission))
                && !allowed.contains(permission)
            {
                allowed.push(permission.clone());
            }
        }
    }
    if let Some(restrictions) = &exact_restrictions {
        allowed.retain(|permission| restrictions.contains(permission));
    }
    allowed.retain(|permission| {
        !matches!(
            permission.target(),
            PermissionTarget::ParticipantResource { .. }
        )
    });

    let mut resource_availability = BTreeMap::new();
    for (name, declaration) in &resolved.resources {
        let commitment = resource_commitment(declaration);
        let approval = approved_resources.iter().find(|approval| {
            approval.name == *name
                && approval.kind == declaration.kind.into()
                && approval.commitment == commitment
        });
        let usable = usable_resources.iter().find(|resource| {
            resource.local_name == *name
                && resource.resource_kind == resource_kind_name(declaration.kind)
                && resource.owner_participant_id == resolved.participant_id
                && resource.state == ResourceBindingState::Available
        });
        let available = approval.is_some() && usable.is_some();
        resource_availability.insert(
            name.clone(),
            Availability {
                available,
                reason: (!available).then_some(if approval.is_none() {
                    AvailabilityReason::NotApproved
                } else {
                    AvailabilityReason::ResourceUnavailable
                }),
            },
        );
        if available {
            allowed.extend(selected.iter().filter(|permission| matches!(permission.target(), PermissionTarget::ParticipantResource { participant, resource, name: resource_name } if participant == &resolved.participant_id && *resource == declaration.kind && resource_name == name)).cloned());
        }
        if !available && !declaration.optional {
            missing_required.push(format!(
                "resource:{}:{name}",
                resource_kind_name(declaration.kind)
            ));
        }
    }
    if resolved
        .required_grants
        .permissions()
        .iter()
        .any(|permission| {
            !matches!(
                permission.target(),
                PermissionTarget::ParticipantResource { .. }
            ) && !allowed.contains(permission)
        })
    {
        missing_required.push("authority".to_owned());
    }
    let mut platform_privileges = approved_platform_privileges
        .iter()
        .filter(|privilege| ceiling.platform_privileges.contains(privilege))
        .copied()
        .collect::<Vec<_>>();
    platform_privileges.sort_unstable();
    platform_privileges.dedup();
    let companion = resolved
        .companion_participant_id
        .as_ref()
        .map(|_| Availability {
            available: companion_available,
            reason: (!companion_available).then_some(AvailabilityReason::CompanionUnavailable),
        });
    if resolved.companion_required && !companion_available {
        missing_required.push("companion".to_owned());
    }
    missing_required.sort();
    missing_required.dedup();
    let apis = resolved
        .referenced_apis
        .keys()
        .map(|id| {
            let permissions = selected
                .iter()
                .filter(|permission| match permission.target() {
                    PermissionTarget::ApiSurface { api, .. }
                    | PermissionTarget::OperationSignal { api, .. } => api == id,
                    PermissionTarget::ParticipantResource { .. } => false,
                });
            let (count, available) =
                permissions.fold((0, true), |(count, available), permission| {
                    (count + 1, available && allowed.contains(permission))
                });
            (
                id.clone(),
                Availability {
                    available: count > 0 && available,
                    reason: (count == 0)
                        .then_some(AvailabilityReason::NotSelected)
                        .or_else(|| (!available).then_some(AvailabilityReason::NotApproved)),
                },
            )
        })
        .collect();
    Ok(ResolvedAuthority {
        exact_grants: GrantSet::new(allowed),
        platform_privileges,
        capabilities: capability_availability,
        resources: resource_availability,
        apis,
        companion,
        readiness: missing_required.is_empty(),
        missing_required,
    })
}

fn resource_kind_name(kind: ParticipantResourceKind) -> &'static str {
    match kind {
        ParticipantResourceKind::Kv => "kv",
        ParticipantResourceKind::Store => "store",
        ParticipantResourceKind::JobQueue => "jobQueue",
        ParticipantResourceKind::EventConsumer => "eventConsumer",
        ParticipantResourceKind::State => "state",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderLoginAttributes {
    pub provider_id: String,
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PortalAuthoritySelection {
    pub ceiling: DelegationCeiling,
    pub effective_policy_digest: String,
}

pub(crate) struct ConsentAuthority {
    pub(crate) ceiling: DelegationCeiling,
    pub(crate) expires_at: Option<i64>,
    pub(crate) provenance: Option<PortalGrantProvenance>,
    pub(crate) preconditions: ConsentAuthorityPreconditions,
}

pub(crate) struct PortalConsentAuthority<'a> {
    pub(crate) selection: PortalAuthoritySelection,
    pub(crate) snapshot: PortalPolicySnapshot,
    pub(crate) provenance: PortalGrantProvenance,
    pub(crate) source: Option<&'a GrantBinding>,
    pub(crate) retained_target: Option<&'a GrantBinding>,
}

pub(crate) enum ConsentAuthoritySource<'a> {
    Explicit { target: &'a GrantBinding },
    Portal(Box<PortalConsentAuthority<'a>>),
}

pub(crate) fn consent_authority(
    source: ConsentAuthoritySource<'_>,
    now: i64,
) -> Result<ConsentAuthority, AuthorizationStateError> {
    match source {
        ConsentAuthoritySource::Explicit { target }
            if target.provenance.is_none()
                && target.state == super::GrantBindingState::Active
                && target.expires_at.is_none_or(|expiry| expiry > now) =>
        {
            Ok(ConsentAuthority {
                ceiling: target.delegation_ceiling.clone(),
                expires_at: target.expires_at,
                provenance: None,
                preconditions: ConsentAuthorityPreconditions {
                    policy: None,
                    bindings: vec![ConsentBindingPrecondition::from(target)],
                },
            })
        }
        ConsentAuthoritySource::Explicit { .. } => Err(AuthorizationStateError::NotAuthorized),
        ConsentAuthoritySource::Portal(portal) => {
            let PortalConsentAuthority {
                mut selection,
                snapshot,
                provenance,
                source,
                retained_target,
            } = *portal;
            for binding in source.into_iter().chain(retained_target) {
                if binding.state != super::GrantBindingState::Active
                    || binding.expires_at.is_some_and(|expiry| expiry <= now)
                {
                    return Err(AuthorizationStateError::NotAuthorized);
                }
            }
            if let Some(target) = retained_target {
                selection
                    .ceiling
                    .capabilities
                    .retain(|candidate| target.delegation_ceiling.capabilities.contains(candidate));
                selection.ceiling.platform_privileges.retain(|candidate| {
                    target
                        .delegation_ceiling
                        .platform_privileges
                        .contains(candidate)
                });
                selection.ceiling.exact_restrictions =
                    target.delegation_ceiling.exact_restrictions.clone();
            }
            let expires_at = source
                .and_then(|binding| binding.expires_at)
                .into_iter()
                .chain(retained_target.and_then(|binding| binding.expires_at))
                .min();
            let mut bindings = source
                .into_iter()
                .chain(retained_target)
                .map(ConsentBindingPrecondition::from)
                .collect::<Vec<_>>();
            bindings.dedup_by(|left, right| {
                left.owner_kind == right.owner_kind
                    && left.owner_id == right.owner_id
                    && left.participant_id == right.participant_id
                    && left.revision == right.revision
            });
            Ok(ConsentAuthority {
                ceiling: selection.ceiling,
                expires_at,
                provenance: Some(provenance),
                preconditions: ConsentAuthorityPreconditions {
                    policy: Some(snapshot),
                    bindings,
                },
            })
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EffectivePortalAuthority<'a> {
    format: &'static str,
    portal_id: &'a str,
    participant_id: &'a str,
    participant_digest: &'a str,
    participant_needs_digest: &'a str,
    capabilities: &'a [ApprovedCapability],
    platform_privileges: &'a [PlatformPrivilege],
}

/// Format marker for the ordinary no-override portal policy selection: the
/// participant's installed capability vocabulary is eligible, and the default
/// selects no platform privileges. The same interpretation is used by consent
/// presentation, the consent decision, and portal-policy reconciliation.
const DEFAULT_PORTAL_AUTHORITY_FORMAT: &str = "trellis.portal-default-eligibility.v1";

/// Resolve the effective portal authority for an ordinary participant when the
/// portal has no override: installed-vocabulary eligibility with its own
/// deterministic digest, so an unchanged default binding stays valid while a
/// later explicit override still replaces it.
pub(crate) fn default_portal_authority_selection(
    portal_id: &str,
    participant: &ParticipantBindingRecord,
) -> Result<PortalAuthoritySelection, AuthorizationStateError> {
    let ceiling = participant_delegation_ceiling(participant)?;
    let platform_privileges: Vec<PlatformPrivilege> = Vec::new();
    let digest_value = serde_json::to_value(EffectivePortalAuthority {
        format: DEFAULT_PORTAL_AUTHORITY_FORMAT,
        portal_id,
        participant_id: &participant.participant_id,
        participant_digest: &participant.participant_digest,
        participant_needs_digest: &participant.needs_digest,
        capabilities: &ceiling.capabilities,
        platform_privileges: &platform_privileges,
    })
    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let effective_policy_digest = trellis_protocol::digest_json(&digest_value)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    Ok(PortalAuthoritySelection {
        ceiling,
        effective_policy_digest,
    })
}

pub(crate) fn resolve_portal_authority_selection(
    policy: &PortalGrantOverrideRecord,
    groups: &BTreeMap<String, CapabilityGroupRecord>,
    participant: &ParticipantBindingRecord,
    attributes: &ProviderLoginAttributes,
) -> Result<PortalAuthoritySelection, AuthorizationStateError> {
    if policy.participant_id != participant.participant_id {
        return Err(AuthorizationStateError::InvalidRecord(
            "portal policy participant does not match consent proposal".to_owned(),
        ));
    }
    let mut selected = policy
        .direct_capabilities
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let roles = attributes
        .roles
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    expand_groups(&policy.capability_group_keys, groups, &mut selected)?;
    for mapping in &policy.role_mappings {
        if mapping.provider_id == attributes.provider_id && roles.contains(mapping.role.as_str()) {
            selected.extend(mapping.direct_capabilities.iter().cloned());
            expand_groups(&mapping.capability_group_keys, groups, &mut selected)?;
        }
    }

    let resolved = participant.resolve()?;
    let mut capabilities = Vec::new();
    // The admin marker is platform classification, not participant permission
    // evidence, so it bypasses proposal bounding; only admins can write the
    // policy that selects it.
    let admin_marker_selected = selected.contains("trellis.auth::admin");
    for capability in selected {
        if let Some(definition) = resolved
            .referenced_apis
            .values()
            .find_map(|api| api.capabilities.get(&capability))
        {
            capabilities.push(ApprovedCapability {
                id: capability,
                consent_digest: definition.consent_digest.clone(),
            });
        }
    }
    capabilities.sort();
    capabilities.dedup();
    let platform_privileges = admin_marker_selected
        .then_some(PlatformPrivilege::Admin)
        .into_iter()
        .collect::<Vec<_>>();
    let digest_value = serde_json::to_value(EffectivePortalAuthority {
        format: "trellis.portal-effective-authority.v1",
        portal_id: &policy.portal_id,
        participant_id: &policy.participant_id,
        participant_digest: &participant.participant_digest,
        participant_needs_digest: &participant.needs_digest,
        capabilities: &capabilities,
        platform_privileges: &platform_privileges,
    })
    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let effective_policy_digest = trellis_protocol::digest_json(&digest_value)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;

    Ok(PortalAuthoritySelection {
        ceiling: DelegationCeiling {
            capabilities,
            exact_restrictions: None,
            platform_privileges,
        },
        effective_policy_digest,
    })
}

fn expand_groups(
    group_keys: &[String],
    groups: &BTreeMap<String, CapabilityGroupRecord>,
    capabilities: &mut BTreeSet<String>,
) -> Result<(), AuthorizationStateError> {
    let mut pending = group_keys.to_vec();
    let mut visited = BTreeSet::new();
    while let Some(key) = pending.pop() {
        if !visited.insert(key.clone()) {
            continue;
        }
        let group = groups.get(&key).ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(format!("capability group '{key}' is missing"))
        })?;
        capabilities.extend(group.capabilities.iter().cloned());
        pending.extend(group.included_groups.iter().cloned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::auth::evidence::{
        ApiRuntimeProjection, CapabilityRuntimeProjection, ParticipantRuntimeProjection,
    };
    use trellis_protocol::{ApiSurfaceKind, PermissionAction, PermissionAtom, PermissionTarget};

    fn atom(name: &str) -> PermissionAtom {
        PermissionAtom::new(
            PermissionTarget::api_surface("app@v1", ApiSurfaceKind::Rpc, name).unwrap(),
            PermissionAction::Call,
        )
        .unwrap()
    }

    fn participant() -> ParticipantBindingRecord {
        let read = atom("Read");
        ParticipantBindingRecord {
            participant_id: "example.app".to_owned(),
            participant_kind: trellis_protocol::ParticipantKind::App,
            participant_digest: "A".repeat(43),
            needs_digest: "B".repeat(43),
            package_digest: "A".repeat(43),
            evidence_digest: "E".repeat(43),
            participant_path: "app".to_owned(),
            projection: ParticipantRuntimeProjection {
                participant_id: "example.app".to_owned(),
                participant_kind: trellis_protocol::ParticipantKind::App,
                display_name: "Example".to_owned(),
                implemented_apis: BTreeMap::new(),
                referenced_apis: BTreeMap::from([(
                    "app@v1".to_owned(),
                    ApiRuntimeProjection {
                        digest: "A".repeat(43),
                        major: 1,
                        actions: BTreeMap::new(),
                        capabilities: BTreeMap::from([
                            (
                                "app::first".to_owned(),
                                CapabilityRuntimeProjection {
                                    display_name: "First".to_owned(),
                                    description: String::new(),
                                    consequence: String::new(),
                                    consent_digest: "A".repeat(43),
                                    public: false,
                                    allows: vec![read.clone()],
                                },
                            ),
                            (
                                "app::overlap".to_owned(),
                                CapabilityRuntimeProjection {
                                    display_name: "Overlap".to_owned(),
                                    description: String::new(),
                                    consequence: String::new(),
                                    consent_digest: "A".repeat(43),
                                    public: false,
                                    allows: vec![read.clone()],
                                },
                            ),
                        ]),
                    },
                )]),
                resources: BTreeMap::new(),
                required_grants: GrantSet::new(vec![read]),
                optional_grant_bundles: BTreeMap::new(),
                required_capabilities: vec!["app::first".to_owned(), "app::overlap".to_owned()],
                optional_capability_definitions: BTreeMap::new(),
                companion_participant_id: None,
                companion_participant_kind: None,
                companion_required: false,
            },
            resolved_at: 1,
            state: super::super::ParticipantBindingState::Resolved,
            error: None,
        }
    }

    #[test]
    fn overlap_grants_once_but_all_required_capabilities_gate_readiness() {
        let participant = participant();
        let approved = ApprovedCapability {
            id: "app::first".to_owned(),
            consent_digest: "A".repeat(43),
        };
        let ceiling = DelegationCeiling {
            capabilities: vec![approved.clone()],
            exact_restrictions: None,
            platform_privileges: Vec::new(),
        };
        let resolved = resolve_authority(
            &participant,
            ApprovalMode::Capabilities,
            &[approved],
            &[],
            &[],
            &ceiling,
            (&[], true),
        )
        .unwrap();
        assert_eq!(resolved.exact_grants, GrantSet::new(vec![atom("Read")]));
        assert!(!resolved.readiness);
        assert_eq!(resolved.missing_required, ["capability:app::overlap"]);
    }

    #[test]
    fn request_inventory_is_not_a_consent_ceiling() {
        let mut participant = participant();
        participant
            .projection
            .referenced_apis
            .get_mut("app@v1")
            .unwrap()
            .capabilities
            .get_mut("app::first")
            .unwrap()
            .public = true;
        let ceiling = DelegationCeiling {
            capabilities: Vec::new(),
            exact_restrictions: None,
            platform_privileges: Vec::new(),
        };
        let consent = consent_request(&participant, 1, None, &ceiling, &[], None).unwrap();
        assert!(
            consent
                .capabilities
                .iter()
                .find(|capability| capability.id == "app::first")
                .unwrap()
                .eligible
        );
        assert!(
            !consent
                .capabilities
                .iter()
                .find(|capability| capability.id == "app::overlap")
                .unwrap()
                .eligible
        );
        let resolved = resolve_authority(
            &participant,
            ApprovalMode::Capabilities,
            &[],
            &[],
            &[],
            &ceiling,
            (&[], true),
        )
        .unwrap();
        assert_eq!(resolved.exact_grants, GrantSet::new(vec![atom("Read")]));
        assert!(!resolved.readiness);
        assert_eq!(resolved.missing_required, ["capability:app::overlap"]);
    }

    #[test]
    fn exact_mode_never_auto_expands_beyond_the_explicit_atom_set() {
        let participant = participant();
        let resolved = resolve_authority(
            &participant,
            ApprovalMode::Exact,
            &[],
            &[],
            &[],
            &DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(GrantSet::new(Vec::new())),
                platform_privileges: Vec::new(),
            },
            (&[], true),
        )
        .unwrap();
        assert!(resolved.exact_grants.permissions().is_empty());
        assert!(!resolved.readiness);
    }

    #[test]
    fn consent_digest_binds_revisions_and_resource_change_classification() {
        let participant = participant();
        let ceiling = participant_delegation_ceiling(&participant).unwrap();
        let first = consent_request(&participant, 1, None, &ceiling, &[], None).unwrap();
        let second = consent_request(&participant, 2, None, &ceiling, &[], None).unwrap();
        assert_ne!(first.decision_digest, second.decision_digest);
        assert_eq!(
            classify_resource_change(
                Some(&ApprovedResource {
                    kind: AuthorizationResourceKind::Kv,
                    name: "cache".to_owned(),
                    commitment: ResourceCommitment {
                        desired_max_value_bytes: Some(100),
                        history: Some(10),
                        ttl_ms: Some(100),
                        ..ResourceCommitment::default()
                    },
                }),
                AuthorizationResourceKind::Kv,
                &ResourceCommitment {
                    desired_max_value_bytes: Some(50),
                    history: Some(5),
                    ttl_ms: Some(50),
                    ..ResourceCommitment::default()
                },
            ),
            ConsentResourceChange::Reduced
        );
        assert_eq!(
            classify_resource_change(
                Some(&ApprovedResource {
                    kind: AuthorizationResourceKind::Kv,
                    name: "cache".to_owned(),
                    commitment: ResourceCommitment {
                        desired_max_value_bytes: Some(100),
                        ..ResourceCommitment::default()
                    },
                }),
                AuthorizationResourceKind::Kv,
                &ResourceCommitment {
                    desired_max_value_bytes: Some(50),
                    ..ResourceCommitment::default()
                },
            ),
            ConsentResourceChange::Unchanged
        );
    }

    fn consent_binding(
        approval_mode: ApprovalMode,
        ceiling: DelegationCeiling,
        expires_at: Option<i64>,
        provenance: Option<PortalGrantProvenance>,
    ) -> GrantBinding {
        let participant = participant();
        GrantBinding {
            owner_kind: super::super::GrantOwnerKind::User,
            owner_id: "user-1".to_owned(),
            participant_id: participant.participant_id.clone(),
            installed_revision: 1,
            grants: participant.projection.required_grants.clone(),
            approval_mode,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: ceiling,
            approval_decision_digest: "A".repeat(43),
            approval_expected_grant_revision: 0,
            companion_approved: false,
            platform_privileges: Vec::new(),
            revision: 3,
            state: super::super::GrantBindingState::Active,
            expires_at,
            provenance,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn portal_snapshot() -> PortalPolicySnapshot {
        PortalPolicySnapshot {
            portal_id: "portal".to_owned(),
            portal_version: 1,
            portal_fingerprint: "B".repeat(43),
            login_settings_version: 1,
            login_settings_fingerprint: "B".repeat(43),
            participant_id: "example.app".to_owned(),
            policy_version: Some(1),
            policy_fingerprint: Some("B".repeat(43)),
            capability_group_versions: Vec::new(),
            capability_group_fingerprints: Vec::new(),
        }
    }

    fn provenance(digest: &str) -> PortalGrantProvenance {
        PortalGrantProvenance {
            portal_id: "portal".to_owned(),
            provider_id: "local".to_owned(),
            roles: Vec::new(),
            effective_policy_digest: digest.to_owned(),
        }
    }

    #[test]
    fn explicit_target_entitlement_keeps_its_ceiling_and_finite_expiry() {
        let ceiling = DelegationCeiling {
            capabilities: vec![ApprovedCapability {
                id: "app::first".to_owned(),
                consent_digest: "A".repeat(43),
            }],
            exact_restrictions: Some(GrantSet::new(vec![atom("Read")])),
            platform_privileges: Vec::new(),
        };
        let target = consent_binding(ApprovalMode::Exact, ceiling.clone(), Some(1_000), None);
        let authority =
            consent_authority(ConsentAuthoritySource::Explicit { target: &target }, 1).unwrap();
        assert_eq!(authority.ceiling, ceiling);
        assert_eq!(authority.expires_at, Some(1_000));
        assert!(authority.provenance.is_none());
        assert_eq!(authority.preconditions.bindings.len(), 1);
        assert_eq!(authority.preconditions.bindings[0].revision, 3);
        assert!(authority.preconditions.policy.is_none());

        let expired = consent_binding(ApprovalMode::Exact, ceiling.clone(), Some(1), None);
        assert!(matches!(
            consent_authority(ConsentAuthoritySource::Explicit { target: &expired }, 1),
            Err(AuthorizationStateError::NotAuthorized)
        ));
        let policy_managed = consent_binding(
            ApprovalMode::Exact,
            ceiling.clone(),
            Some(1_000),
            Some(provenance(&"C".repeat(43))),
        );
        assert!(matches!(
            consent_authority(
                ConsentAuthoritySource::Explicit {
                    target: &policy_managed
                },
                1
            ),
            Err(AuthorizationStateError::NotAuthorized)
        ));
    }

    #[test]
    fn capabilities_mode_target_keeps_explicit_entitlement_after_first_consent() {
        let ceiling = DelegationCeiling {
            capabilities: vec![ApprovedCapability {
                id: "app::first".to_owned(),
                consent_digest: "A".repeat(43),
            }],
            exact_restrictions: Some(GrantSet::new(vec![atom("Read")])),
            platform_privileges: Vec::new(),
        };
        let first = consent_binding(ApprovalMode::Exact, ceiling.clone(), Some(1_000), None);
        let first_authority =
            consent_authority(ConsentAuthoritySource::Explicit { target: &first }, 1).unwrap();
        assert_eq!(first_authority.ceiling, ceiling);

        // The first approval persists the user's selection as a capability-mode
        // binding with the same server-owned ceiling and no portal provenance.
        let second = consent_binding(
            ApprovalMode::Capabilities,
            ceiling.clone(),
            Some(1_000),
            None,
        );
        let second_authority =
            consent_authority(ConsentAuthoritySource::Explicit { target: &second }, 1).unwrap();
        assert_eq!(second_authority.ceiling, ceiling);
        assert_eq!(second_authority.expires_at, Some(1_000));
        assert!(second_authority.provenance.is_none());
        assert_eq!(second_authority.preconditions.bindings.len(), 1);
        assert_eq!(second_authority.preconditions.bindings[0].revision, 3);

        let expired = consent_binding(ApprovalMode::Capabilities, ceiling, Some(1), None);
        assert!(matches!(
            consent_authority(ConsentAuthoritySource::Explicit { target: &expired }, 1),
            Err(AuthorizationStateError::NotAuthorized)
        ));
    }

    #[test]
    fn portal_authority_never_widens_a_narrower_retained_child() {
        let retained = consent_binding(
            ApprovalMode::Capabilities,
            DelegationCeiling {
                capabilities: vec![ApprovedCapability {
                    id: "app::first".to_owned(),
                    consent_digest: "A".repeat(43),
                }],
                exact_restrictions: None,
                platform_privileges: Vec::new(),
            },
            Some(1_000),
            Some(provenance(&"C".repeat(43))),
        );
        let selection = PortalAuthoritySelection {
            ceiling: DelegationCeiling {
                capabilities: vec![
                    ApprovedCapability {
                        id: "app::first".to_owned(),
                        consent_digest: "A".repeat(43),
                    },
                    ApprovedCapability {
                        id: "app::overlap".to_owned(),
                        consent_digest: "A".repeat(43),
                    },
                ],
                exact_restrictions: None,
                platform_privileges: Vec::new(),
            },
            effective_policy_digest: "D".repeat(43),
        };
        let authority = consent_authority(
            ConsentAuthoritySource::Portal(Box::new(PortalConsentAuthority {
                selection,
                snapshot: portal_snapshot(),
                provenance: provenance(&"D".repeat(43)),
                source: None,
                retained_target: Some(&retained),
            })),
            1,
        )
        .unwrap();
        assert_eq!(
            authority.ceiling.capabilities,
            vec![ApprovedCapability {
                id: "app::first".to_owned(),
                consent_digest: "A".repeat(43),
            }]
        );
        assert_eq!(authority.expires_at, Some(1_000));
        assert!(authority.provenance.is_some());
        assert!(authority.preconditions.policy.is_some());
        assert_eq!(authority.preconditions.bindings.len(), 1);
    }
}
