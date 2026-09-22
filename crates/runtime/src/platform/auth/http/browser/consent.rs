use super::super::*;
use super::local::{portal_flow_response, PortalFlowResponse};
use crate::platform::auth::policy::{
    default_portal_authority_selection, portal_allows_authenticated_provider,
};
use crate::platform::auth::{
    ApprovalMode, ApprovedCapability, ApprovedResource, GrantBinding, PortalGrantProvenance,
};

async fn consent_ceiling<R, E>(
    state: &AuthHttpState<R, E>,
    flow: &AuthBrowserFlow,
    participant: &ParticipantBindingRecord,
    current: Option<&GrantBinding>,
    attributes: &ProviderLoginAttributes,
    now: i64,
) -> Result<super::super::super::policy::ConsentAuthority, HttpError>
where
    R: PortalRepository + Clone,
{
    let (portal, settings) = state
        .service
        .repository()
        .get_login_portal(&flow.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    if portal.removed {
        return Err(HttpError::gone("portal_unavailable"));
    }
    if !portal_allows_authenticated_provider(&portal, &settings, &attributes.provider_id) {
        return Err(HttpError::forbidden(if portal.disabled {
            "portal_disabled"
        } else if attributes.provider_id == "local" && !settings.local_login_enabled {
            "local_login_disabled"
        } else {
            "provider_not_allowed"
        }));
    }
    let groups = state
        .service
        .repository()
        .list_capability_groups()
        .await?
        .into_iter()
        .map(|group| (group.group_key.clone(), group))
        .collect();
    let policy = state
        .service
        .repository()
        .get_portal_grant_override(&flow.portal_id, &participant.participant_id)
        .await?;
    let selection = match policy.as_ref() {
        Some(policy) => {
            resolve_portal_authority_selection(policy, &groups, participant, attributes)?
        }
        None => default_portal_authority_selection(&flow.portal_id, participant)?,
    };
    let effective_policy_digest = selection.effective_policy_digest.clone();
    let snapshot = portal_policy_snapshot(
        &portal,
        &settings,
        &participant.participant_id,
        policy.as_ref(),
        &groups,
    )?;
    // A current active binding is the carried source of authority and lifetime
    // regardless of whether it already has portal provenance; the portal policy
    // still bounds capability eligibility.
    let source = consent_source(current, now);
    super::super::super::policy::consent_authority(
        super::super::super::policy::ConsentAuthoritySource::Portal(Box::new(
            super::super::super::policy::PortalConsentAuthority {
                selection,
                snapshot,
                provenance: PortalGrantProvenance {
                    portal_id: flow.portal_id.clone(),
                    provider_id: attributes.provider_id.clone(),
                    roles: attributes.roles.clone(),
                    effective_policy_digest,
                },
                source,
                retained_target: None,
            },
        )),
        now,
    )
    .map_err(HttpError::from)
}

pub(crate) async fn decide_approval<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(flow_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ConsentDecision>,
) -> Result<Json<PortalFlowResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let mut flow = load_flow(&state.ephemeral, &flow_id).await?;
    let (portal, _) = state
        .service
        .repository()
        .get_login_portal(&flow.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    require_selected_portal_origin(&headers, &portal, &state.public_origin)?;
    require_portal_binding(&flow, &headers)?;
    let request_value =
        serde_json::to_value(&request).map_err(|_| HttpError::bad_request("invalid_approval"))?;
    let request_digest = trellis_protocol::digest_json(&request_value)
        .map_err(|_| HttpError::bad_request("invalid_approval"))?;
    if matches!(
        flow.state,
        AuthBrowserFlowState::Approved | AuthBrowserFlowState::Consumed
    ) {
        let signer_id = super::super::super::domain::validate_ed25519_public_key(
            "sessionPublicKey",
            &flow.session_public_key,
        )?;
        let recorded = state
            .service
            .repository()
            .get_idempotency_result("browser.grant.accept", &signer_id, &flow_id)
            .await?;
        if request.decision != ConsentDecisionKind::Approve
            || request
                .approval
                .as_ref()
                .map(|approval| &approval.decision_digest)
                != Some(&flow.consent.decision_digest)
            || recorded.as_ref().map(|record| &record.request_digest) != Some(&request_digest)
        {
            return Err(HttpError::conflict("approval_replay_mismatch"));
        }
        return Ok(Json(portal_flow_response(&state, flow).await?));
    }
    if flow.state != AuthBrowserFlowState::ApprovalRequired {
        return Err(HttpError::conflict("flow_not_awaiting_approval"));
    }
    let now = now_ms()?;
    let (_, binding) = state
        .service
        .repository()
        .get_installed_participant_record(
            flow.participant_id.clone(),
            Some(flow.installed_revision),
        )
        .await?
        .ok_or_else(|| HttpError::internal("participant_binding_missing"))?;
    let current = state
        .service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::User,
            flow.principal_id
                .clone()
                .ok_or_else(|| HttpError::conflict("flow_has_no_principal"))?,
            flow.participant_id.clone(),
        )
        .await?;
    let attributes = ProviderLoginAttributes {
        provider_id: flow
            .authenticated_provider_id
            .clone()
            .ok_or_else(|| HttpError::conflict("flow_has_no_provider"))?,
        roles: flow.authenticated_roles.clone(),
    };
    let mut authority =
        consent_ceiling(&state, &flow, &binding, current.as_ref(), &attributes, now).await?;
    authority.ceiling.platform_privileges = current
        .as_ref()
        .filter(|binding| {
            binding.state == GrantBindingState::Active
                && binding.expires_at.is_none_or(|expires_at| expires_at > now)
        })
        .map_or_else(Vec::new, |binding| binding.platform_privileges.clone());
    let actuals = state
        .service
        .repository()
        .consent_resource_actuals(
            GrantOwnerKind::User,
            flow.principal_id
                .clone()
                .ok_or_else(|| HttpError::conflict("flow_has_no_principal"))?,
            flow.participant_id.clone(),
        )
        .await?;
    let companion_id = binding.resolve()?.companion_participant_id.clone();
    let companion = if let Some(participant_id) = companion_id {
        let (_, child) = state
            .service
            .repository()
            .get_installed_participant_record(participant_id.clone(), None)
            .await?
            .ok_or_else(|| HttpError::internal("companion_binding_missing"))?;
        let child_current = state
            .service
            .repository()
            .get_grant_binding(
                GrantOwnerKind::User,
                flow.principal_id
                    .clone()
                    .ok_or_else(|| HttpError::conflict("flow_has_no_principal"))?,
                participant_id.clone(),
            )
            .await?;
        let child_actuals = state
            .service
            .repository()
            .consent_resource_actuals(
                GrantOwnerKind::User,
                flow.principal_id
                    .clone()
                    .ok_or_else(|| HttpError::conflict("flow_has_no_principal"))?,
                participant_id,
            )
            .await?;
        let child_authority = consent_ceiling(
            &state,
            &flow,
            &child,
            child_current.as_ref(),
            &attributes,
            now,
        )
        .await?;
        Some((child, child_current, child_actuals, child_authority))
    } else {
        None
    };
    let current_consent = super::super::super::policy::consent_request(
        &binding,
        flow.installed_revision,
        current.as_ref(),
        &authority.ceiling,
        &actuals,
        companion
            .as_ref()
            .map(|(child, current, actuals, authority)| {
                (
                    child,
                    current.as_ref(),
                    actuals.as_slice(),
                    &authority.ceiling,
                )
            }),
    )?;
    if current_consent != flow.consent {
        return Err(HttpError::conflict("consent_view_changed"));
    }
    if request.decision == ConsentDecisionKind::Reject {
        if request.approval.is_some() {
            return Err(HttpError::bad_request("invalid_consent_decision"));
        }
        let expected = flow.version;
        flow.state = AuthBrowserFlowState::ApprovalDenied;
        flow.completed_at = Some(now);
        flow.version += 1;
        state
            .ephemeral
            .replace_browser_flow(expected, flow.clone())
            .await?;
        return Ok(Json(portal_flow_response(&state, flow).await?));
    }
    let principal_id = flow
        .principal_id
        .clone()
        .ok_or_else(|| HttpError::conflict("flow_has_no_principal"))?;
    let approval = request
        .approval
        .as_ref()
        .ok_or_else(|| HttpError::bad_request("invalid_consent_decision"))?;
    validate_consent_decision(&flow.consent, approval, &authority.ceiling)?;
    let platform_privileges = current
        .as_ref()
        .filter(|binding| {
            binding.state == GrantBindingState::Active
                && binding.expires_at.is_none_or(|expires_at| expires_at > now)
        })
        .map_or_else(Vec::new, |binding| binding.platform_privileges.clone());
    let resolved = super::super::super::policy::resolve_authority(
        &binding,
        ApprovalMode::Capabilities,
        &approval.approved_capabilities,
        &approval.approved_resources,
        &platform_privileges,
        &authority.ceiling,
        (&[], approval.companion_approved),
    )?;
    let signer_id = super::super::super::domain::validate_ed25519_public_key(
        "sessionPublicKey",
        &flow.session_public_key,
    )?;
    let replacement = GrantBindingReplacement {
        owner_kind: GrantOwnerKind::User,
        owner_id: principal_id.clone(),
        participant_id: flow.participant_id.clone(),
        installed_revision: flow.installed_revision,
        grants: resolved.exact_grants,
        approval_mode: ApprovalMode::Capabilities,
        approved_capabilities: approval.approved_capabilities.clone(),
        approved_resources: approval.approved_resources.clone(),
        delegation_ceiling: authority.ceiling.clone(),
        approval_decision_digest: approval.decision_digest.clone(),
        companion_approved: approval.companion_approved,
        platform_privileges,
        expected_revision: flow.target_grant_revision,
        expected_current_installed_revision: Some(flow.installed_revision),
        state: GrantBindingState::Active,
        expires_at: authority.expires_at,
        provenance: authority.provenance.clone(),
    };
    let idempotency = idempotency(
        &flow_id,
        "browser.grant.accept",
        &signer_id,
        &flow_id,
        &request_digest,
        now,
    )?;
    let durable = state
        .service
        .repository()
        .set_consent_grant_binding(replacement, authority.preconditions, idempotency)
        .await
        .map_err(|error| match error {
            AuthorizationStateError::StorageConflict => HttpError::conflict("authority_changed"),
            AuthorizationStateError::InvalidRecord(message)
                if message == "grant binding does not match resolved authority" =>
            {
                HttpError::conflict("authority_changed")
            }
            error => error.into(),
        })?;
    let durable_result_digest = trellis_protocol::digest_json(&durable)
        .map_err(|_| HttpError::internal("authority_digest"))?;
    let expected = flow.version;
    flow.state = AuthBrowserFlowState::Approved;
    flow.durable_result_digest = Some(durable_result_digest);
    flow.completed_at = Some(now);
    flow.version += 1;
    if let Err(error) = state
        .ephemeral
        .replace_browser_flow(expected, flow.clone())
        .await
    {
        if error != AuthorizationStateError::StorageConflict {
            return Err(error.into());
        }
        let current = load_flow(&state.ephemeral, &flow.flow_id).await?;
        if !matches!(
            current.state,
            AuthBrowserFlowState::Approved | AuthBrowserFlowState::Consumed
        ) || current.durable_result_digest != flow.durable_result_digest
        {
            return Err(HttpError::conflict("approval_completion_conflict"));
        }
        flow = current;
    }
    Ok(Json(portal_flow_response(&state, flow).await?))
}

fn consent_source(current: Option<&GrantBinding>, now: i64) -> Option<&GrantBinding> {
    current.filter(|binding| {
        binding.state == GrantBindingState::Active
            && binding.expires_at.is_none_or(|expiry| expiry > now)
    })
}

fn validate_consent_decision(
    consent: &ConsentRequest,
    approval: &ConsentApproval,
    ceiling: &DelegationCeiling,
) -> Result<(), HttpError> {
    let approved_capabilities = approval
        .approved_capabilities
        .iter()
        .collect::<BTreeSet<_>>();
    let approved_resources = approval.approved_resources.iter().collect::<BTreeSet<_>>();
    if consent.resources.iter().any(|resource| {
        resource.required
            && resource.eligible
            && !approved_resources.contains(&ApprovedResource {
                kind: resource.kind,
                name: resource.name.clone(),
                commitment: resource.requested_commitment.clone(),
            })
    }) {
        return Err(HttpError::bad_request("required_resource_not_approved"));
    }
    if consent
        .companion
        .as_ref()
        .is_some_and(|companion| companion.required && !approval.companion_approved)
        || (approval.companion_approved && consent.companion.is_none())
    {
        return Err(HttpError::bad_request("invalid_companion_approval"));
    }
    if approval.decision_digest != consent.decision_digest
        || approval.installed_revision != consent.installed_revision
        || approval.expected_grant_revision != consent.expected_grant_revision
    {
        return Err(HttpError::conflict("consent_decision_stale"));
    }
    if approval.approved_capabilities.iter().any(|approved| {
        !consent.capabilities.iter().any(|capability| {
            capability.eligible
                && capability.id == approved.id
                && capability.consent_digest == approved.consent_digest
        })
    }) || approval.approved_resources.iter().any(|approved| {
        !consent.resources.iter().any(|resource| {
            resource.eligible
                && resource.kind == approved.kind
                && resource.name == approved.name
                && resource.requested_commitment == approved.commitment
        })
    }) {
        return Err(HttpError::conflict("consent_selection_stale"));
    }
    if consent.capabilities.iter().any(|capability| {
        capability.required
            && capability.eligible
            && !approved_capabilities.contains(&ApprovedCapability {
                id: capability.id.clone(),
                consent_digest: capability.consent_digest.clone(),
            })
    }) {
        return Err(HttpError::bad_request("required_capability_not_approved"));
    }
    if approval.mode != ApprovalMode::Capabilities {
        return Err(HttpError::bad_request("invalid_approval_mode"));
    }
    if approval
        .delegation_ceiling
        .as_ref()
        .is_some_and(|submitted| {
            submitted.capabilities != ceiling.capabilities
                || submitted.exact_restrictions != ceiling.exact_restrictions
        })
    {
        return Err(HttpError::conflict("delegation_ceiling_changed"));
    }
    if approved_capabilities.len() != approval.approved_capabilities.len()
        || approved_resources.len() != approval.approved_resources.len()
    {
        return Err(HttpError::bad_request("duplicate_consent_selection"));
    }
    Ok(())
}

pub(super) async fn apply_trusted_portal_authority<R, E>(
    state: &AuthHttpState<R, E>,
    mut flow: AuthBrowserFlow,
    attributes: ProviderLoginAttributes,
    now: i64,
) -> Result<Option<AuthBrowserFlow>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let (_, binding) = state
        .service
        .repository()
        .get_installed_participant_record(
            flow.participant_id.clone(),
            Some(flow.installed_revision),
        )
        .await?
        .ok_or_else(|| HttpError::internal("participant_binding_missing"))?;
    let principal_id = flow
        .principal_id
        .clone()
        .ok_or_else(|| HttpError::conflict("flow_has_no_principal"))?;
    let current = state
        .service
        .repository()
        .get_grant_binding(
            GrantOwnerKind::User,
            principal_id.clone(),
            flow.participant_id.clone(),
        )
        .await?;
    let signer_id = super::super::super::domain::validate_ed25519_public_key(
        "sessionPublicKey",
        &flow.session_public_key,
    )?;
    let durable = 'policy: {
        for attempt in 0..3 {
            let Some((portal, settings)) = state
                .service
                .repository()
                .get_login_portal(&flow.portal_id)
                .await?
            else {
                return Err(HttpError::gone("portal_unavailable"));
            };
            if portal.removed {
                return Err(HttpError::gone("portal_unavailable"));
            }
            if !portal_allows_authenticated_provider(&portal, &settings, &attributes.provider_id) {
                return Err(HttpError::forbidden(if portal.disabled {
                    "portal_disabled"
                } else if attributes.provider_id == "local" && !settings.local_login_enabled {
                    "local_login_disabled"
                } else {
                    "provider_not_allowed"
                }));
            }
            if attributes.provider_id != "local"
                && !state.oidc_providers.contains_key(&attributes.provider_id)
            {
                return Err(HttpError::not_found("provider_not_found"));
            }
            let Some(policy) = state
                .service
                .repository()
                .get_portal_grant_override(&flow.portal_id, &flow.participant_id)
                .await?
            else {
                return Ok(None);
            };
            let groups = state
                .service
                .repository()
                .list_capability_groups()
                .await?
                .into_iter()
                .map(|group| (group.group_key.clone(), group))
                .collect();
            let snapshot = portal_policy_snapshot(
                &portal,
                &settings,
                &flow.participant_id,
                Some(&policy),
                &groups,
            )?;
            let selection =
                resolve_portal_authority_selection(&policy, &groups, &binding, &attributes)?;
            let mut ceiling =
                super::super::super::policy::participant_delegation_ceiling(&binding)?;
            ceiling
                .capabilities
                .retain(|capability| selection.ceiling.capabilities.contains(capability));
            ceiling.platform_privileges = selection.ceiling.platform_privileges.clone();
            let approved_capabilities = ceiling.capabilities.clone();
            let approved_resources = current
                .as_ref()
                .map_or_else(Vec::new, |binding| binding.approved_resources.clone());
            let resolved = super::super::super::policy::resolve_authority(
                &binding,
                ApprovalMode::Capabilities,
                &approved_capabilities,
                &approved_resources,
                &selection.ceiling.platform_privileges,
                &ceiling,
                (&[], true),
            )?;
            let request_digest = trellis_protocol::digest_json(&json!({
                "flowId": flow.flow_id,
                "portalId": flow.portal_id,
                "portalVersion": snapshot.portal_version,
                "loginSettingsVersion": snapshot.login_settings_version,
                "participantId": flow.participant_id,
                "policyVersion": snapshot.policy_version,
                "capabilityGroupVersions": snapshot.capability_group_versions,
                "providerId": attributes.provider_id,
                "roles": attributes.roles,
                "effectivePolicyDigest": selection.effective_policy_digest,
            }))
            .map_err(|_| HttpError::internal("portal_policy_digest"))?;
            let result = state
                .service
                .repository()
                .set_portal_grant_binding(
                    GrantBindingReplacement {
                        owner_kind: GrantOwnerKind::User,
                        owner_id: principal_id.clone(),
                        participant_id: flow.participant_id.clone(),
                        installed_revision: flow.installed_revision,
                        grants: resolved.exact_grants,
                        approval_mode: ApprovalMode::Capabilities,
                        approved_capabilities,
                        approved_resources,
                        delegation_ceiling: ceiling,
                        approval_decision_digest: request_digest.clone(),
                        companion_approved: false,
                        platform_privileges: resolved.platform_privileges,
                        expected_revision: flow.target_grant_revision,
                        expected_current_installed_revision: Some(flow.installed_revision),
                        state: GrantBindingState::Active,
                        expires_at: None,
                        provenance: Some(PortalGrantProvenance {
                            portal_id: flow.portal_id.clone(),
                            provider_id: attributes.provider_id.clone(),
                            roles: attributes.roles.clone(),
                            effective_policy_digest: selection.effective_policy_digest.clone(),
                        }),
                    },
                    snapshot,
                    idempotency(
                        &flow.flow_id,
                        "portal.grant.accept",
                        &signer_id,
                        &flow.flow_id,
                        &request_digest,
                        now,
                    )?,
                )
                .await;
            match result {
                Ok(durable) => break 'policy durable,
                Err(AuthorizationStateError::PortalPolicyChanged) if attempt < 2 => continue,
                Err(AuthorizationStateError::PortalPolicyChanged) => {
                    return Err(HttpError::conflict("portal_policy_changed"));
                }
                Err(AuthorizationStateError::StorageConflict) if attempt < 2 => {
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!("bounded portal policy retries return or break")
    };
    flow.state = AuthBrowserFlowState::Approved;
    flow.durable_result_digest = Some(
        trellis_protocol::digest_json(&durable)
            .map_err(|_| HttpError::internal("authority_digest"))?,
    );
    flow.completed_at = Some(now);
    Ok(Some(flow))
}

pub(super) async fn complete_authenticated_flow<R, E>(
    state: &AuthHttpState<R, E>,
    mut flow: AuthBrowserFlow,
    principal_id: String,
    attributes: ProviderLoginAttributes,
    portal_binding_digest: String,
    require_explicit_approval: bool,
    now: i64,
) -> Result<AuthBrowserFlow, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    if flow.state == AuthBrowserFlowState::ChooseProvider {
        let current = state
            .service
            .repository()
            .get_grant_binding(
                GrantOwnerKind::User,
                principal_id.clone(),
                flow.participant_id.clone(),
            )
            .await?;
        flow.target_grant_revision = current.as_ref().map_or(0, |binding| binding.revision);
        let (_, participant) = state
            .service
            .repository()
            .get_installed_participant_record(
                flow.participant_id.clone(),
                Some(flow.installed_revision),
            )
            .await?
            .ok_or_else(|| HttpError::internal("participant_binding_missing"))?;
        let mut authority = consent_ceiling(
            state,
            &flow,
            &participant,
            current.as_ref(),
            &attributes,
            now,
        )
        .await?;
        authority.ceiling.platform_privileges = current
            .as_ref()
            .filter(|binding| {
                binding.state == GrantBindingState::Active
                    && binding.expires_at.is_none_or(|expires_at| expires_at > now)
            })
            .map_or_else(Vec::new, |binding| binding.platform_privileges.clone());
        let actuals = state
            .service
            .repository()
            .consent_resource_actuals(
                GrantOwnerKind::User,
                principal_id.clone(),
                flow.participant_id.clone(),
            )
            .await?;
        let companion_id = participant.resolve()?.companion_participant_id.clone();
        let companion = if let Some(participant_id) = companion_id {
            let (_, child) = state
                .service
                .repository()
                .get_installed_participant_record(participant_id.clone(), None)
                .await?
                .ok_or_else(|| HttpError::internal("companion_binding_missing"))?;
            let child_current = state
                .service
                .repository()
                .get_grant_binding(
                    GrantOwnerKind::User,
                    principal_id.clone(),
                    participant_id.clone(),
                )
                .await?;
            let child_actuals = state
                .service
                .repository()
                .consent_resource_actuals(
                    GrantOwnerKind::User,
                    principal_id.clone(),
                    participant_id,
                )
                .await?;
            let child_authority = consent_ceiling(
                state,
                &flow,
                &child,
                child_current.as_ref(),
                &attributes,
                now,
            )
            .await?;
            Some((child, child_current, child_actuals, child_authority))
        } else {
            None
        };
        flow.consent = super::super::super::policy::consent_request(
            &participant,
            flow.installed_revision,
            current.as_ref(),
            &authority.ceiling,
            &actuals,
            companion
                .as_ref()
                .map(|(child, current, actuals, authority)| {
                    (
                        child,
                        current.as_ref(),
                        actuals.as_slice(),
                        &authority.ceiling,
                    )
                }),
        )?;
        let expected = flow.version;
        flow.state = AuthBrowserFlowState::Authenticated;
        flow.principal_id = Some(principal_id.clone());
        flow.authenticated_provider_id = Some(attributes.provider_id.clone());
        flow.authenticated_roles = attributes.roles.clone();
        flow.portal_binding_digest = Some(portal_binding_digest.clone());
        flow.version += 1;
        match state
            .ephemeral
            .replace_browser_flow(expected, flow.clone())
            .await
        {
            Ok(()) => {}
            Err(AuthorizationStateError::StorageConflict) => {
                flow = load_flow(&state.ephemeral, &flow.flow_id).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if flow.principal_id.as_deref() != Some(&principal_id)
        || flow.authenticated_provider_id.as_deref() != Some(&attributes.provider_id)
        || flow.authenticated_roles != attributes.roles
        || flow.portal_binding_digest.as_deref() != Some(&portal_binding_digest)
    {
        return Err(HttpError::conflict("flow_authentication_conflict"));
    }
    if matches!(
        flow.state,
        AuthBrowserFlowState::ApprovalRequired
            | AuthBrowserFlowState::Approved
            | AuthBrowserFlowState::Consumed
    ) {
        return Ok(flow);
    }
    if flow.state != AuthBrowserFlowState::Authenticated {
        return Err(HttpError::conflict("flow_not_pending"));
    }
    let expected = flow.version;
    let allow_automatic_approval = automatic_approval_allowed(require_explicit_approval);
    // A portal policy governs this login even when an accepted authority
    // exists, so reconcile through policy instead of the fast path.
    let policy_governs = allow_automatic_approval
        && state
            .service
            .repository()
            .get_portal_grant_override(&flow.portal_id, &flow.participant_id)
            .await?
            .is_some();
    let existing_binding = if !allow_automatic_approval || policy_governs {
        None
    } else {
        state
            .service
            .repository()
            .get_grant_binding(
                GrantOwnerKind::User,
                principal_id.clone(),
                flow.participant_id.clone(),
            )
            .await?
            .filter(|binding| {
                binding.state == GrantBindingState::Active
                    && binding.expires_at.is_none_or(|expires_at| expires_at > now)
            })
    };
    let mut completed = if let Some(binding) = existing_binding {
        flow.state = AuthBrowserFlowState::Approved;
        flow.durable_result_digest = Some(
            trellis_protocol::digest_json(
                &serde_json::to_value(binding)
                    .map_err(|_| HttpError::internal("binding_encode"))?,
            )
            .map_err(|_| HttpError::internal("authority_digest"))?,
        );
        flow.completed_at = Some(now);
        flow
    } else if !allow_automatic_approval {
        flow.state = AuthBrowserFlowState::ApprovalRequired;
        flow
    } else if let Some(approved) =
        apply_trusted_portal_authority(state, flow.clone(), attributes, now).await?
    {
        approved
    } else {
        flow.state = AuthBrowserFlowState::ApprovalRequired;
        flow
    };
    completed.version += 1;
    match state
        .ephemeral
        .replace_browser_flow(expected, completed.clone())
        .await
    {
        Ok(()) => Ok(completed),
        Err(AuthorizationStateError::StorageConflict) => {
            let current = load_flow(&state.ephemeral, &completed.flow_id).await?;
            let converged = current.principal_id == completed.principal_id
                && (current.state == completed.state
                    || matches!(
                        (completed.state, current.state),
                        (
                            AuthBrowserFlowState::ApprovalRequired,
                            AuthBrowserFlowState::Approved | AuthBrowserFlowState::Consumed
                        ) | (
                            AuthBrowserFlowState::Approved,
                            AuthBrowserFlowState::Consumed
                        )
                    ))
                && completed
                    .durable_result_digest
                    .as_ref()
                    .is_none_or(|digest| current.durable_result_digest.as_ref() == Some(digest));
            if converged {
                Ok(current)
            } else {
                Err(HttpError::conflict("flow_completion_conflict"))
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn automatic_approval_allowed(require_explicit_approval: bool) -> bool {
    !require_explicit_approval
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::auth::{builtins, sqlite::SqliteAuthorizationStore, DelegationCeiling};
    use trellis_protocol::PlatformPrivilege;

    #[test]
    fn administrator_account_continuation_requires_explicit_approval() {
        assert!(!super::automatic_approval_allowed(true));
        assert!(super::automatic_approval_allowed(false));
    }

    #[tokio::test]
    async fn carried_source_keeps_provenance_bearing_lifetime(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let store = SqliteAuthorizationStore::open_in_memory()?;
        let now = 1_700_000_000_000;
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, now,
            )
            .await?;
        let participant = builtins::console_participant_binding(now)?;
        let participant_id = participant.participant_id.clone();
        store.put_participant_binding(participant.clone()).await?;
        let grants = participant.projection.required_grants.clone();
        let expires_at = now + 60_000;
        store
            .set_grant_binding(
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: actor.principal_id.clone(),
                    participant_id: participant_id.clone(),
                    installed_revision: 1,
                    grants,
                    approval_mode: ApprovalMode::Capabilities,
                    approved_capabilities: Vec::new(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: None,
                        platform_privileges: vec![PlatformPrivilege::Admin],
                    },
                    approval_decision_digest: digest_parts(&["carried-source"]),
                    companion_approved: false,
                    platform_privileges: vec![PlatformPrivilege::Admin],
                    state: GrantBindingState::Active,
                    expires_at: Some(expires_at),
                    provenance: Some(PortalGrantProvenance {
                        portal_id: "portal".to_owned(),
                        provider_id: "local".to_owned(),
                        roles: Vec::new(),
                        effective_policy_digest: digest_parts(&["carried-policy"]),
                    }),
                    expected_revision: 0,
                    expected_current_installed_revision: Some(1),
                },
                idempotency(
                    "carried-source",
                    "browser.grant.accept",
                    "browser-signer",
                    "carried-source",
                    &digest_parts(&["carried-source"]),
                    now,
                )
                .expect("test idempotency is valid"),
            )
            .await?;
        let binding = store
            .get_grant_binding(
                GrantOwnerKind::User,
                actor.principal_id.clone(),
                participant_id,
            )
            .await?
            .unwrap();
        // A provenance-bearing, active, unexpired binding is still the carried
        // source of authority and lifetime for the next explicit consent.
        let selected = consent_source(Some(&binding), now).expect("active source is carried");
        assert_eq!(selected.expires_at, Some(expires_at));
        assert!(consent_source(Some(&binding), expires_at).is_none());
        Ok(())
    }

    #[tokio::test]
    async fn approval_is_fenced_by_installed_and_grant_revisions(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let store = SqliteAuthorizationStore::open_in_memory()?;
        let now = 1_700_000_000_000;
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, now,
            )
            .await?;
        let principal_id = actor.principal_id.clone();
        let participant_v1 = builtins::console_participant_binding(now)?;
        let participant_id = participant_v1.participant_id.clone();
        assert_eq!(
            store
                .put_participant_binding(participant_v1.clone())
                .await?,
            1
        );
        let grants = participant_v1.projection.required_grants.clone();
        let approval = GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: grants.clone(),
            approval_mode: ApprovalMode::Exact,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(grants.clone()),
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: digest_parts(&["approval-r1"]),
            companion_approved: false,
            platform_privileges: Vec::new(),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
            expected_revision: 0,
            expected_current_installed_revision: Some(1),
        };
        let approval_idempotency = idempotency(
            "flow-r1",
            "browser.grant.accept",
            "browser-signer",
            "flow-r1",
            &digest_parts(&["approval-r1"]),
            now,
        )
        .expect("test idempotency is valid");
        let approved = store
            .set_grant_binding(approval.clone(), approval_idempotency.clone())
            .await?;
        assert_eq!(
            store
                .set_grant_binding(approval, approval_idempotency)
                .await?,
            approved
        );
        assert_eq!(
            store.list_ready_post_commit_actions(now, 10).await?.len(),
            1
        );

        let mut participant_v2 = participant_v1;
        participant_v2.projection.display_name = "Trellis Console R2".to_owned();
        participant_v2.needs_digest =
            trellis_protocol::digest_json(&serde_json::to_value(&participant_v2.projection)?)?;
        participant_v2.resolved_at = now + 1;
        assert_eq!(store.put_participant_binding(participant_v2).await?, 2);

        let stale_approval = GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: grants.clone(),
            approval_mode: ApprovalMode::Exact,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(grants.clone()),
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: digest_parts(&["approval-stale-install"]),
            companion_approved: false,
            platform_privileges: Vec::new(),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
            expected_revision: 1,
            expected_current_installed_revision: Some(1),
        };
        assert_eq!(
            store
                .set_grant_binding(
                    stale_approval,
                    idempotency(
                        "flow-stale-install",
                        "browser.grant.accept",
                        "browser-signer",
                        "flow-stale-install",
                        &digest_parts(&["approval-stale-install"]),
                        now + 2,
                    )
                    .expect("test idempotency is valid"),
                )
                .await,
            Err(AuthorizationStateError::RevisionConflict {
                expected: 1,
                current: 2,
            })
        );
        let unchanged = store
            .get_grant_binding(
                GrantOwnerKind::User,
                principal_id.clone(),
                participant_id.clone(),
            )
            .await?
            .unwrap();
        assert_eq!(unchanged.revision, 1);
        assert_eq!(unchanged.installed_revision, 1);
        assert_eq!(unchanged.grants, grants);
        assert_eq!(
            store
                .list_ready_post_commit_actions(now + 2, 10)
                .await?
                .len(),
            1
        );

        let fresh_approval = GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 2,
            grants: grants.clone(),
            approval_mode: ApprovalMode::Exact,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(grants.clone()),
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: digest_parts(&["approval-r2"]),
            companion_approved: false,
            platform_privileges: Vec::new(),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
            expected_revision: 1,
            expected_current_installed_revision: Some(2),
        };
        store
            .set_grant_binding(
                fresh_approval,
                idempotency(
                    "flow-r2",
                    "browser.grant.accept",
                    "browser-signer",
                    "flow-r2",
                    &digest_parts(&["approval-r2"]),
                    now + 3,
                )
                .expect("test idempotency is valid"),
            )
            .await?;

        store
            .admin_set_grant_binding(
                actor,
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: principal_id.clone(),
                    participant_id: participant_id.clone(),
                    installed_revision: 2,
                    grants: grants.clone(),
                    approval_mode: ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(grants.clone()),
                        platform_privileges: vec![PlatformPrivilege::Admin],
                    },
                    approval_decision_digest: digest_parts(&["admin-replacement"]),
                    companion_approved: false,
                    platform_privileges: vec![PlatformPrivilege::Admin],
                    state: GrantBindingState::Active,
                    expires_at: None,
                    provenance: None,
                    expected_revision: 2,
                    expected_current_installed_revision: None,
                },
                idempotency(
                    "admin-replacement",
                    "Auth.Grants.Set",
                    "admin-signer",
                    "admin-replacement",
                    &digest_parts(&["admin-replacement"]),
                    now + 4,
                )
                .expect("test idempotency is valid"),
            )
            .await?;
        assert!(matches!(
            store
                .set_grant_binding(
                    GrantBindingReplacement {
                        owner_kind: GrantOwnerKind::User,
                        owner_id: principal_id,
                        participant_id,
                        installed_revision: 2,
                        grants: grants.clone(),
                        approval_mode: ApprovalMode::Exact,
                        approved_capabilities: Vec::new(),
                        approved_resources: Vec::new(),
                        delegation_ceiling: DelegationCeiling {
                            capabilities: Vec::new(),
                            exact_restrictions: Some(grants),
                            platform_privileges: Vec::new(),
                        },
                        approval_decision_digest: digest_parts(&["approval-stale-binding"]),
                        companion_approved: false,
                        platform_privileges: Vec::new(),
                        state: GrantBindingState::Active,
                        expires_at: None,
                        provenance: None,
                        expected_revision: 2,
                        expected_current_installed_revision: Some(2),
                    },
                    idempotency(
                        "flow-stale-binding",
                        "browser.grant.accept",
                        "browser-signer",
                        "flow-stale-binding",
                        &digest_parts(&["approval-stale-binding"]),
                        now + 5,
                    )
                    .expect("test idempotency is valid"),
                )
                .await,
            Err(AuthorizationStateError::RevisionConflict {
                expected: 2,
                current: 3,
            })
        ));
        assert_eq!(
            store
                .list_ready_post_commit_actions(now + 5, 10)
                .await?
                .len(),
            3
        );
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BindRequest {
    request_id: String,
    #[serde(rename = "issuedAt")]
    _issued_at: i64,
    proof: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserSessionBundle {
    server_now: i64,
    session: BrowserLoginSession,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserLoginSession {
    session_id: String,
    principal_id: String,
    participant_id: String,
    session_key: String,
    expires_at: Option<i64>,
}

pub(crate) async fn bind_flow<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(flow_id): Path<String>,
    headers: HeaderMap,
    Json(raw): Json<Value>,
) -> Result<Json<BrowserSessionBundle>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let flow = load_flow(&state.ephemeral, &flow_id).await?;
    let redirect_target = flow
        .redirect_target
        .as_deref()
        .ok_or_else(|| HttpError::bad_request("missing_redirect_target"))?;
    require_portal_origin(&headers, redirect_target)?;
    if !matches!(
        flow.state,
        AuthBrowserFlowState::Approved | AuthBrowserFlowState::Consumed
    ) {
        return Err(HttpError::conflict("flow_not_approved"));
    }
    let mut unsigned_request = raw.clone();
    unsigned_request
        .as_object_mut()
        .ok_or_else(|| HttpError::bad_request("invalid_bind_request"))?
        .remove("proof");
    let request: BindRequest =
        serde_json::from_value(raw).map_err(|_| HttpError::bad_request("invalid_bind_request"))?;
    if ulid::Ulid::from_string(&request.request_id)
        .map(|request_id| request_id.to_string() != request.request_id)
        .unwrap_or(true)
    {
        return Err(HttpError::bad_request("invalid_bind_request_id"));
    }
    let input =
        SessionProofInput::user_auth_bind(trellis_protocol::UserAuthBindSessionProofInput {
            origin: state.public_origin.clone(),
            flow_id,
            session_public_key: flow.session_public_key.clone(),
            unsigned_request,
        })
        .map_err(|error| {
            tracing::warn!(%error, "bind proof input rejected");
            HttpError::unauthorized("invalid_proof")
        })?;
    verify_session_proof(
        &input,
        &parse_session_proof(&request.proof).map_err(|error| {
            tracing::warn!(%error, "bind proof envelope rejected");
            HttpError::unauthorized("invalid_proof")
        })?,
        &flow.session_public_key,
        now_ms()?,
        state.proof_policy,
    )
    .map_err(|error| {
        tracing::warn!(%error, "bind proof verification rejected");
        HttpError::unauthorized("invalid_proof")
    })?;
    let flow = complete_flow(&state, flow, now_ms()?).await?;
    Ok(Json(session_bundle(&state, &flow).await?))
}

async fn complete_flow<R, E>(
    state: &AuthHttpState<R, E>,
    mut flow: AuthBrowserFlow,
    now: i64,
) -> Result<AuthBrowserFlow, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    if flow.state == AuthBrowserFlowState::Consumed {
        let session_id = flow
            .claim_owner
            .as_deref()
            .ok_or_else(|| HttpError::internal("flow_session_missing"))?;
        let session = state
            .service
            .repository()
            .get_session(session_id)
            .await?
            .ok_or_else(|| HttpError::internal("flow_session_missing"))?;
        if session.principal_id != flow.principal_id.as_deref().unwrap_or_default()
            || session.participant_id != flow.participant_id
            || session.session_public_key != flow.session_public_key
        {
            return Err(HttpError::internal("flow_session_mismatch"));
        }
        return Ok(flow);
    }
    if flow.state != AuthBrowserFlowState::Approved {
        return Err(HttpError::conflict("flow_not_approved"));
    }
    let (_, binding) = state
        .service
        .repository()
        .get_installed_participant_record(
            flow.participant_id.clone(),
            Some(flow.installed_revision),
        )
        .await?
        .ok_or_else(|| HttpError::conflict("participant_unavailable"))?;
    let principal_id = flow
        .principal_id
        .clone()
        .ok_or_else(|| HttpError::conflict("flow_has_no_principal"))?;
    let signer_id = super::super::super::domain::validate_ed25519_public_key(
        "sessionPublicKey",
        &flow.session_public_key,
    )?;
    let digest = digest_parts(&["browser.session.complete", &flow.flow_id]);
    let outcome = state
        .service
        .create_session(CreateSessionInput {
            principal_id,
            participant_id: flow.participant_id.clone(),
            participant_kind: binding.participant_kind,
            session_public_key: flow.session_public_key.clone(),
            created_at: now,
            idempotency: idempotency(
                &flow.flow_id,
                "browser.session.complete",
                &signer_id,
                &flow.flow_id,
                &digest,
                now,
            )?,
            actions: Vec::new(),
        })
        .await?;
    let session = match outcome {
        IdempotentOutcome::Applied(session) => session,
        IdempotentOutcome::Replayed(value) => {
            let session_id = value
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| HttpError::internal("invalid_session_replay"))?;
            state
                .service
                .repository()
                .get_session(session_id)
                .await?
                .ok_or_else(|| HttpError::internal("session_missing"))?
        }
    };
    let expected = flow.version;
    flow.state = AuthBrowserFlowState::Consumed;
    flow.claim_owner = Some(session.session_id.clone());
    flow.claimed_at = Some(now);
    flow.version += 1;
    match state
        .ephemeral
        .replace_browser_flow(expected, flow.clone())
        .await
    {
        Ok(()) => Ok(flow),
        Err(AuthorizationStateError::StorageConflict) => {
            let current = load_flow(&state.ephemeral, &flow.flow_id).await?;
            if current.state == AuthBrowserFlowState::Consumed
                && current.claim_owner.as_deref() == Some(session.session_id.as_str())
            {
                Ok(current)
            } else {
                Err(HttpError::conflict("flow_completion_conflict"))
            }
        }
        Err(error) => Err(error.into()),
    }
}

async fn session_bundle<R, E>(
    state: &AuthHttpState<R, E>,
    flow: &AuthBrowserFlow,
) -> Result<BrowserSessionBundle, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    if flow.state != AuthBrowserFlowState::Consumed {
        return Err(HttpError::conflict("flow_not_consumed"));
    }
    let now = now_ms()?;
    let session = state
        .service
        .repository()
        .get_session(
            flow.claim_owner
                .as_deref()
                .ok_or_else(|| HttpError::internal("flow_session_missing"))?,
        )
        .await?
        .ok_or_else(|| HttpError::internal("flow_session_missing"))?;
    if session.state != crate::platform::auth::SessionState::Active
        || session
            .expires_at
            .is_some_and(|expires_at| expires_at <= now)
    {
        return Err(HttpError::unauthorized("session_inactive"));
    }
    Ok(BrowserSessionBundle {
        server_now: now,
        session: BrowserLoginSession {
            session_id: session.session_id,
            principal_id: session.principal_id,
            participant_id: session.participant_id,
            session_key: session.session_public_key,
            expires_at: session.expires_at,
        },
    })
}
