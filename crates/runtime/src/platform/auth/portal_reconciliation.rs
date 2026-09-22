use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{Mutex, Notify};

use super::{
    portal_policy_snapshot, resolve_portal_authority_selection, AccountRepository, ApprovalMode,
    AuthService, AuthorizationStateError, CapabilityGroupRecord, GrantBinding,
    GrantBindingReplacement, GrantBindingState, GrantOwnerKind, GrantRepository,
    IdempotencyResultRecord, LoginPortalRecord, LoginSettingsRecord, PortalGrantOverrideRecord,
    PortalGrantProvenance, PortalPolicySnapshot, PortalRepository, ProviderLoginAttributes,
};
use crate::shutdown::StopHandle;

#[derive(Clone)]
pub(crate) struct PortalPolicyReconciliationHandle {
    notify: Arc<Notify>,
    all_pending: Arc<AtomicBool>,
    pending_portals: Arc<Mutex<BTreeSet<String>>>,
}

struct PortalPolicyBatch {
    portal: LoginPortalRecord,
    settings: LoginSettingsRecord,
    policy: Option<PortalGrantOverrideRecord>,
    snapshot: PortalPolicySnapshot,
    groups: Arc<BTreeMap<String, CapabilityGroupRecord>>,
}

impl PortalPolicyReconciliationHandle {
    pub(crate) fn notify_all(&self) {
        self.all_pending.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    pub(crate) async fn notify_portal(&self, portal_id: &str) {
        self.pending_portals
            .lock()
            .await
            .insert(portal_id.to_owned());
        self.notify.notify_one();
    }
}

pub(crate) struct PortalPolicyReconciliationWorker<R> {
    service: AuthService<R>,
    handle: PortalPolicyReconciliationHandle,
}

pub(crate) fn portal_policy_reconciliation<R>(
    service: AuthService<R>,
) -> (
    PortalPolicyReconciliationHandle,
    PortalPolicyReconciliationWorker<R>,
) {
    let notify = Arc::new(Notify::new());
    let handle = PortalPolicyReconciliationHandle {
        notify,
        all_pending: Arc::new(AtomicBool::new(false)),
        pending_portals: Arc::new(Mutex::new(BTreeSet::new())),
    };
    (
        handle.clone(),
        PortalPolicyReconciliationWorker { service, handle },
    )
}

impl<R> PortalPolicyReconciliationWorker<R>
where
    R: AccountRepository + GrantRepository + PortalRepository + Clone + Send + Sync,
{
    pub(crate) async fn run(self, stop: StopHandle) -> Result<(), AuthorizationStateError> {
        loop {
            tokio::select! {
                () = stop.stopped() => return Ok(()),
                () = self.handle.notify.notified() => self.reconcile_pending().await?,
            }
        }
    }

    pub(crate) async fn reconcile_startup(&self) -> Result<(), AuthorizationStateError> {
        self.reconcile_global(true).await
    }

    async fn reconcile_pending(&self) -> Result<(), AuthorizationStateError> {
        loop {
            self.reconcile_pending_portals().await?;
            if self.handle.all_pending.swap(false, Ordering::AcqRel) {
                self.reconcile_global(false).await?;
                continue;
            }
            return Ok(());
        }
    }

    async fn reconcile_pending_portals(&self) -> Result<(), AuthorizationStateError> {
        loop {
            let portal_ids = {
                let mut pending = self.handle.pending_portals.lock().await;
                std::mem::take(&mut *pending)
            };
            if portal_ids.is_empty() {
                return Ok(());
            }
            let bindings = self
                .service
                .repository()
                .list_portal_grant_bindings()
                .await?
                .into_iter()
                .filter(|binding| portal_ids.contains(&binding.portal_id))
                .collect();
            let groups = self.capability_groups().await?;
            self.reconcile_bindings(bindings, false, groups).await?;
        }
    }

    async fn reconcile_global(
        &self,
        materialize_immediately: bool,
    ) -> Result<(), AuthorizationStateError> {
        let groups = self.capability_groups().await?;
        let mut bindings = self
            .service
            .repository()
            .list_portal_grant_bindings()
            .await?
            .into_iter();
        loop {
            self.reconcile_pending_portals().await?;
            let chunk = bindings.by_ref().take(16).collect::<Vec<_>>();
            if chunk.is_empty() {
                return Ok(());
            }
            self.reconcile_bindings(chunk, materialize_immediately, groups.clone())
                .await?;
        }
    }

    async fn capability_groups(
        &self,
    ) -> Result<Arc<BTreeMap<String, CapabilityGroupRecord>>, AuthorizationStateError> {
        Ok(Arc::new(
            self.service
                .repository()
                .list_capability_groups()
                .await?
                .into_iter()
                .map(|group| (group.group_key.clone(), group))
                .collect(),
        ))
    }

    async fn reconcile_bindings(
        &self,
        bindings: Vec<super::PortalGrantBindingRecord>,
        materialize_immediately: bool,
        groups: Arc<BTreeMap<String, CapabilityGroupRecord>>,
    ) -> Result<(), AuthorizationStateError> {
        let mut grouped = BTreeMap::<(String, String), Vec<_>>::new();
        for binding in bindings {
            grouped
                .entry((binding.portal_id.clone(), binding.participant_id.clone()))
                .or_default()
                .push(binding);
        }
        let mut work = Vec::new();
        for ((portal_id, participant_id), bindings) in grouped {
            let portal = self.service.repository().get_login_portal(&portal_id).await;
            let (portal, settings) = match portal {
                Ok(Some(portal)) => portal,
                Ok(None) => {
                    tracing::warn!(
                        portal_id,
                        participant_id,
                        binding_count = bindings.len(),
                        error = "portal-managed authority references a missing portal",
                        "skipping invalid portal authority reconciliation batch"
                    );
                    continue;
                }
                Err(error @ AuthorizationStateError::InvalidRecord(_)) => {
                    tracing::warn!(
                        portal_id,
                        participant_id,
                        binding_count = bindings.len(),
                        error = %error,
                        "skipping invalid portal authority reconciliation batch"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            let policy = match self
                .service
                .repository()
                .get_portal_grant_override(&portal_id, &participant_id)
                .await
            {
                Ok(policy) => policy,
                Err(error @ AuthorizationStateError::InvalidRecord(_)) => {
                    tracing::warn!(
                        portal_id,
                        participant_id,
                        binding_count = bindings.len(),
                        error = %error,
                        "skipping invalid portal authority reconciliation batch"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            let snapshot = match portal_policy_snapshot(
                &portal,
                &settings,
                &participant_id,
                policy.as_ref(),
                &groups,
            ) {
                Ok(snapshot) => snapshot,
                Err(error @ AuthorizationStateError::InvalidRecord(_)) => {
                    tracing::warn!(
                        portal_id,
                        participant_id,
                        binding_count = bindings.len(),
                        error = %error,
                        "skipping invalid portal authority reconciliation batch"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut current_bindings = Vec::new();
            for binding in bindings {
                match self
                    .service
                    .repository()
                    .get_grant_binding(
                        GrantOwnerKind::User,
                        binding.principal_id.clone(),
                        binding.participant_id.clone(),
                    )
                    .await
                {
                    Ok(Some(current)) => current_bindings.push((binding, current)),
                    Ok(None) => {}
                    Err(error @ AuthorizationStateError::InvalidRecord(_)) => {
                        tracing::warn!(
                            principal_id = binding.principal_id,
                            participant_id = binding.participant_id,
                            portal_id = binding.portal_id,
                            error = %error,
                            "skipping invalid portal authority binding reconciliation"
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
            let batch = Arc::new(PortalPolicyBatch {
                portal,
                settings,
                policy,
                snapshot,
                groups: groups.clone(),
            });
            work.extend(
                current_bindings
                    .into_iter()
                    .map(|(binding, current)| (binding, current, batch.clone())),
            );
        }
        let mut reconciliations = stream::iter(work)
            .map(|(binding, current, batch)| {
                let principal_id = binding.principal_id.clone();
                let participant_id = binding.participant_id.clone();
                let portal_id = binding.portal_id.clone();
                async move {
                    (
                        principal_id,
                        participant_id,
                        portal_id,
                        self.reconcile_binding(binding, current, batch, materialize_immediately)
                            .await,
                    )
                }
            })
            .buffer_unordered(16);
        while let Some((principal_id, participant_id, portal_id, result)) =
            reconciliations.next().await
        {
            match result {
                Ok(()) => {}
                Err(
                    error @ (AuthorizationStateError::InvalidRecord(_)
                    | AuthorizationStateError::ParticipantMissing),
                ) => {
                    tracing::warn!(
                        principal_id,
                        participant_id,
                        portal_id,
                        error = %error,
                        "skipping invalid portal authority binding reconciliation"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    async fn reconcile_binding(
        &self,
        binding: super::PortalGrantBindingRecord,
        current: GrantBinding,
        batch: Arc<PortalPolicyBatch>,
        _materialize_immediately: bool,
    ) -> Result<(), AuthorizationStateError> {
        if current.state == GrantBindingState::Revoked {
            return Ok(());
        }
        let Some(current_provenance) = current.provenance.as_ref() else {
            return Ok(());
        };
        if binding.grant_revision != current.revision
            || current_provenance.portal_id != binding.portal_id
            || current_provenance.provider_id != binding.provider_id
            || current_provenance.roles != binding.roles
            || current_provenance.effective_policy_digest != binding.effective_policy_digest
        {
            return Ok(());
        }
        let provider_allowed = super::policy::portal_allows_authenticated_provider(
            &batch.portal,
            &batch.settings,
            &binding.provider_id,
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?
            .as_millis()
            .try_into()
            .map_err(|_| AuthorizationStateError::Storage("current time exceeds i64".to_owned()))?;
        let retry_portal_id = binding.portal_id.clone();
        // With no override, the unchanged ordinary default remains the effective
        // policy: eligibility over the installed vocabulary that the binding was
        // consented under. Companion children stay strict and a binding whose
        // recorded policy no longer applies is revoked.
        let default_authority = if batch.policy.is_none()
            && !self
                .service
                .repository()
                .is_companion_participant(binding.participant_id.clone())
                .await?
        {
            let (_, participant) = self
                .service
                .repository()
                .get_installed_participant_record(
                    binding.participant_id.clone(),
                    Some(current.installed_revision),
                )
                .await?
                .ok_or(AuthorizationStateError::ParticipantMissing)?;
            let selection = super::policy::default_portal_authority_selection(
                &binding.portal_id,
                &participant,
            )?;
            (current_provenance.effective_policy_digest == selection.effective_policy_digest)
                .then_some((participant, selection))
        } else {
            None
        };
        let result = if !provider_allowed || (batch.policy.is_none() && default_authority.is_none())
        {
            let request = json!({
                "principalId": binding.principal_id,
                "participantId": binding.participant_id,
                "expectedRevision": current.revision,
                "state": "revoked",
            });
            let request_digest = trellis_protocol::digest_json(&request)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            self.service
                .repository()
                .revoke_portal_grant_binding(
                    binding.principal_id,
                    binding.participant_id,
                    current.revision,
                    batch.snapshot.clone(),
                    idempotency("portal.policy.revoke", &request_digest, &request, now)?,
                )
                .await
                .map(|_| ())
        } else if let Some((participant, selection)) = default_authority {
            // Reduce existing approvals to what the unchanged default still
            // makes eligible; never add an approval the user did not make.
            let mut delegation_ceiling = selection.ceiling.clone();
            delegation_ceiling.platform_privileges = current.platform_privileges.clone();
            let approved_capabilities = current
                .approved_capabilities
                .iter()
                .filter(|capability| delegation_ceiling.capabilities.contains(capability))
                .cloned()
                .collect::<Vec<_>>();
            let resolved = super::policy::resolve_authority(
                &participant,
                ApprovalMode::Capabilities,
                &approved_capabilities,
                &current.approved_resources,
                &current.platform_privileges,
                &delegation_ceiling,
                (&[], true),
            )?;
            let grants = resolved.exact_grants;
            let platform_privileges = resolved.platform_privileges;
            let provenance = PortalGrantProvenance {
                portal_id: binding.portal_id.clone(),
                provider_id: binding.provider_id.clone(),
                roles: binding.roles.clone(),
                effective_policy_digest: selection.effective_policy_digest.clone(),
            };
            if current.grants == grants
                && current.platform_privileges == platform_privileges
                && current.approved_capabilities == approved_capabilities
                && current.delegation_ceiling == delegation_ceiling
                && current.provenance.as_ref() == Some(&provenance)
            {
                return Ok(());
            }
            let request = json!({
                "principalId": binding.principal_id,
                "participantId": binding.participant_id,
                "expectedRevision": current.revision,
                "effectivePolicyDigest": selection.effective_policy_digest,
            });
            let request_digest = trellis_protocol::digest_json(&request)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            self.service
                .repository()
                .set_portal_grant_binding(
                    GrantBindingReplacement {
                        owner_kind: GrantOwnerKind::User,
                        owner_id: binding.principal_id,
                        participant_id: binding.participant_id,
                        installed_revision: current.installed_revision,
                        grants,
                        approval_mode: ApprovalMode::Capabilities,
                        approved_capabilities,
                        approved_resources: current.approved_resources,
                        delegation_ceiling,
                        approval_decision_digest: request_digest.clone(),
                        companion_approved: current.companion_approved,
                        platform_privileges,
                        state: GrantBindingState::Active,
                        expires_at: current.expires_at,
                        provenance: Some(provenance),
                        expected_revision: current.revision,
                        expected_current_installed_revision: None,
                    },
                    batch.snapshot.clone(),
                    idempotency("portal.policy.replace", &request_digest, &request, now)?,
                )
                .await
                .map(|_| ())
        } else {
            let (_, participant) = self
                .service
                .repository()
                .get_installed_participant_record(
                    binding.participant_id.clone(),
                    Some(current.installed_revision),
                )
                .await?
                .ok_or(AuthorizationStateError::ParticipantMissing)?;
            let Some(policy) = batch.policy.as_ref() else {
                unreachable!("portal policy was checked above")
            };
            let selection = resolve_portal_authority_selection(
                policy,
                &batch.groups,
                &participant,
                &ProviderLoginAttributes {
                    provider_id: binding.provider_id.clone(),
                    roles: binding.roles.clone(),
                },
            )?;
            let provenance = PortalGrantProvenance {
                portal_id: binding.portal_id.clone(),
                provider_id: binding.provider_id.clone(),
                roles: binding.roles.clone(),
                effective_policy_digest: selection.effective_policy_digest.clone(),
            };
            let mut delegation_ceiling =
                super::policy::participant_delegation_ceiling(&participant)?;
            delegation_ceiling
                .capabilities
                .retain(|capability| selection.ceiling.capabilities.contains(capability));
            delegation_ceiling.platform_privileges = selection.ceiling.platform_privileges.clone();
            let approved_capabilities = current
                .approved_capabilities
                .iter()
                .filter(|capability| delegation_ceiling.capabilities.contains(capability))
                .cloned()
                .collect::<Vec<_>>();
            let resolved = super::policy::resolve_authority(
                &participant,
                ApprovalMode::Capabilities,
                &approved_capabilities,
                &current.approved_resources,
                &current.platform_privileges,
                &delegation_ceiling,
                (&[], true),
            )?;
            let grants = resolved.exact_grants;
            let platform_privileges = resolved.platform_privileges;
            if current.grants == grants
                && current.platform_privileges == platform_privileges
                && current.approved_capabilities == approved_capabilities
                && current.delegation_ceiling == delegation_ceiling
                && current.provenance.as_ref() == Some(&provenance)
            {
                return Ok(());
            }
            let request = json!({
                "principalId": binding.principal_id,
                "participantId": binding.participant_id,
                "expectedRevision": current.revision,
                "effectivePolicyDigest": selection.effective_policy_digest,
            });
            let request_digest = trellis_protocol::digest_json(&request)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            self.service
                .repository()
                .set_portal_grant_binding(
                    GrantBindingReplacement {
                        owner_kind: GrantOwnerKind::User,
                        owner_id: binding.principal_id,
                        participant_id: binding.participant_id,
                        installed_revision: current.installed_revision,
                        grants,
                        approval_mode: ApprovalMode::Capabilities,
                        approved_capabilities,
                        approved_resources: current.approved_resources,
                        delegation_ceiling,
                        approval_decision_digest: request_digest.clone(),
                        companion_approved: current.companion_approved,
                        platform_privileges,
                        state: GrantBindingState::Active,
                        expires_at: current.expires_at,
                        provenance: Some(provenance),
                        expected_revision: current.revision,
                        expected_current_installed_revision: None,
                    },
                    batch.snapshot.clone(),
                    idempotency("portal.policy.replace", &request_digest, &request, now)?,
                )
                .await
                .map(|_| ())
        };
        if matches!(
            result,
            Err(AuthorizationStateError::StorageConflict
                | AuthorizationStateError::PortalPolicyChanged
                | AuthorizationStateError::RevisionConflict { .. })
        ) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.handle.notify_portal(&retry_portal_id).await;
            return Ok(());
        }
        if result == Err(AuthorizationStateError::NotAuthorized) {
            // The protected bootstrap administrator cannot be demoted by policy.
            return Ok(());
        }
        result?;
        Ok(())
    }
}

fn idempotency(
    purpose: &str,
    request_digest: &str,
    request: &Value,
    now: i64,
) -> Result<IdempotencyResultRecord, AuthorizationStateError> {
    Ok(IdempotencyResultRecord {
        scope_key: trellis_protocol::digest_json(&json!([purpose, request_digest]))
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        purpose: purpose.to_owned(),
        signer_id: "portal-policy-reconciler".to_owned(),
        request_id: request_digest.to_owned(),
        request_digest: trellis_protocol::digest_json(request)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        result: Value::Null,
        created_at: now,
        expires_at: now.saturating_add(86_400_000),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::auth::application::repository::{
        AccountCreation, LoginPortalMutation, PortalRepository,
    };
    use crate::platform::auth::{
        builtins, policy::resolve_authority, AuthServiceConfig, DelegationCeiling,
        GrantBindingReplacement, LocalCredentialRecord, PrincipalKind, PrincipalRecord,
        PrincipalState, SqliteAuthorizationStore, UserProfileRecord,
    };
    use serde_json::json;
    use trellis_protocol::{GrantSet, PlatformPrivilege};

    #[tokio::test]
    async fn mixed_startup_and_pending_reconciliation_isolate_invalid_binding() {
        const NOW: i64 = 1_700_000_000_000;
        let proof = |purpose: &str| IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!([purpose])).unwrap(),
            purpose: purpose.to_owned(),
            signer_id: "test".to_owned(),
            request_id: purpose.to_owned(),
            request_digest: trellis_protocol::digest_json(&json!({ "purpose": purpose })).unwrap(),
            result: Value::Null,
            created_at: NOW,
            expires_at: NOW + 60_000,
        };
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        store.ensure_admin_capability_group(NOW).await.unwrap();
        let mut participant = builtins::cli_participant_binding(NOW).unwrap();
        participant.projection.optional_capability_definitions = participant
            .projection
            .referenced_apis
            .values()
            .flat_map(|api| api.capabilities.iter())
            .take(2)
            .map(|(id, capability)| (id.clone(), GrantSet::new(capability.allows.clone())))
            .collect();
        let participant_id = participant.participant_id.clone();
        store.put_participant_binding(participant).await.unwrap();
        store
            .create_user_account(AccountCreation {
                principal: PrincipalRecord {
                    principal_id: "usr_reconcile".to_owned(),
                    kind: PrincipalKind::User,
                    state: PrincipalState::Active,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                    disabled_at: None,
                    revoked_at: None,
                },
                profile: UserProfileRecord {
                    principal_id: "usr_reconcile".to_owned(),
                    display_name: None,
                    email: None,
                    image_url: None,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                },
                credential: None::<LocalCredentialRecord>,
                identity: None,
                idempotency: proof("account.create"),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        store
            .create_user_account(AccountCreation {
                principal: PrincipalRecord {
                    principal_id: "usr_invalid_reconcile".to_owned(),
                    kind: PrincipalKind::User,
                    state: PrincipalState::Active,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                    disabled_at: None,
                    revoked_at: None,
                },
                profile: UserProfileRecord {
                    principal_id: "usr_invalid_reconcile".to_owned(),
                    display_name: None,
                    email: None,
                    image_url: None,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                },
                credential: None::<LocalCredentialRecord>,
                identity: None,
                idempotency: proof("invalid.account.create"),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        let portal = LoginPortalRecord {
            portal_id: "portal".to_owned(),
            display_name: "Portal".to_owned(),
            entry_url: None,
            builtin: false,
            disabled: false,
            removed: false,
            local_registration_enabled: false,
            provider_ids: vec!["oidc".to_owned()],
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        let settings = LoginSettingsRecord {
            portal_id: portal.portal_id.clone(),
            default_provider_id: Some("oidc".to_owned()),
            local_login_enabled: false,
            federated_registration_enabled: true,
            provider_selection_enabled: false,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_login_portal(LoginPortalMutation {
                portal: portal.clone(),
                settings: settings.clone(),
                expected_version: None,
                idempotency: proof("portal.create"),
                actions: Vec::new(),
            })
            .await
            .unwrap();

        let (_, installed) = store
            .get_installed_participant_record(participant_id.clone(), Some(1))
            .await
            .unwrap()
            .unwrap();
        let mut optional = installed
            .resolve()
            .unwrap()
            .optional_capability_definitions
            .keys()
            .take(2)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(optional.len(), 2);
        let first = optional.remove(0);
        let second = optional.remove(0);
        let mut policy = PortalGrantOverrideRecord {
            portal_id: portal.portal_id.clone(),
            participant_id: participant_id.clone(),
            direct_capabilities: vec![first.clone()],
            capability_group_keys: vec!["admin".to_owned()],
            role_mappings: Vec::new(),
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_portal_grant_override(policy.clone(), None, proof("policy.1"))
            .await
            .unwrap();
        let groups = store
            .list_capability_groups()
            .await
            .unwrap()
            .into_iter()
            .map(|group| (group.group_key.clone(), group))
            .collect();
        let selection = resolve_portal_authority_selection(
            &policy,
            &groups,
            &installed,
            &ProviderLoginAttributes {
                provider_id: "oidc".to_owned(),
                roles: Vec::new(),
            },
        )
        .unwrap();
        let initial = resolve_authority(
            &installed,
            ApprovalMode::Capabilities,
            &selection.ceiling.capabilities,
            &[],
            &selection.ceiling.platform_privileges,
            &selection.ceiling,
            (&[], true),
        )
        .unwrap();
        let expires_at = Some(NOW + 30_000);
        store
            .set_grant_binding(
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: "usr_reconcile".to_owned(),
                    participant_id: participant_id.clone(),
                    installed_revision: 1,
                    grants: initial.exact_grants.clone(),
                    approval_mode: ApprovalMode::Capabilities,
                    approved_capabilities: selection.ceiling.capabilities.clone(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: selection.ceiling.clone(),
                    approval_decision_digest: "A".repeat(43),
                    companion_approved: false,
                    platform_privileges: initial.platform_privileges.clone(),
                    state: GrantBindingState::Active,
                    expires_at,
                    provenance: Some(PortalGrantProvenance {
                        portal_id: "portal".to_owned(),
                        provider_id: "oidc".to_owned(),
                        roles: Vec::new(),
                        effective_policy_digest: selection.effective_policy_digest.clone(),
                    }),
                    expected_revision: 0,
                    expected_current_installed_revision: None,
                },
                proof("binding.create"),
            )
            .await
            .unwrap();
        store
            .set_grant_binding(
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: "usr_invalid_reconcile".to_owned(),
                    participant_id: participant_id.clone(),
                    installed_revision: 1,
                    grants: initial.exact_grants.clone(),
                    approval_mode: ApprovalMode::Capabilities,
                    approved_capabilities: selection.ceiling.capabilities.clone(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: selection.ceiling.clone(),
                    approval_decision_digest: "A".repeat(43),
                    companion_approved: false,
                    platform_privileges: initial.platform_privileges.clone(),
                    state: GrantBindingState::Active,
                    expires_at,
                    provenance: Some(PortalGrantProvenance {
                        portal_id: "portal".to_owned(),
                        provider_id: "oidc".to_owned(),
                        roles: Vec::new(),
                        effective_policy_digest: selection.effective_policy_digest,
                    }),
                    expected_revision: 0,
                    expected_current_installed_revision: None,
                },
                proof("invalid.binding.create"),
            )
            .await
            .unwrap();
        store
            .run(|connection| {
                connection
                    .execute(
                        "UPDATE auth_grant_bindings SET provenance_json = json_set(provenance_json, '$.portalId', 'missing_portal') WHERE owner_id = 'usr_invalid_reconcile'",
                        [],
                    )
                    .map(|_| ())
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))
            })
            .await
            .unwrap();
        let invalid = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_invalid_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        let service = AuthService::new(store.clone(), AuthServiceConfig::default()).unwrap();
        let (handle, worker) = portal_policy_reconciliation(service);

        policy.direct_capabilities.insert(1, second);
        policy.updated_at += 1;
        policy.version += 1;
        store
            .put_portal_grant_override(policy.clone(), Some(1), proof("policy.2"))
            .await
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        let widened = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        // The widened override adds eligibility but must not add an approval
        // the user never made; the carried Admin privilege remains.
        assert_eq!(widened.approved_capabilities.len(), 1);
        assert_eq!(widened.grants, initial.exact_grants);
        assert_eq!(widened.platform_privileges, [PlatformPrivilege::Admin]);
        assert_eq!(widened.installed_revision, 1);
        assert_eq!(widened.expires_at, expires_at);
        assert_eq!(
            store
                .get_grant_binding(
                    GrantOwnerKind::User,
                    "usr_invalid_reconcile".to_owned(),
                    participant_id.clone(),
                )
                .await
                .unwrap(),
            Some(invalid.clone())
        );

        policy.direct_capabilities.clear();
        policy.updated_at += 1;
        policy.version += 1;
        store
            .put_portal_grant_override(policy.clone(), Some(2), proof("policy.3"))
            .await
            .unwrap();
        handle.notify_portal("portal").await;
        worker.reconcile_pending().await.unwrap();
        let narrowed = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(narrowed.grants.permissions().len() < widened.grants.permissions().len());
        assert_eq!(narrowed.platform_privileges, [PlatformPrivilege::Admin]);
        assert_eq!(narrowed.installed_revision, 1);
        assert_eq!(narrowed.expires_at, expires_at);
        assert_eq!(
            store
                .get_grant_binding(
                    GrantOwnerKind::User,
                    "usr_invalid_reconcile".to_owned(),
                    participant_id.clone(),
                )
                .await
                .unwrap(),
            Some(invalid)
        );

        store
            .run(|connection| {
                connection
                    .execute(
                        "INSERT INTO auth_bootstrap_administrator (singleton, principal_id, created_at) VALUES (1, 'usr_reconcile', ?1)",
                        [NOW],
                    )
                    .map(|_| ())
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))
            })
            .await
            .unwrap();

        policy.direct_capabilities.clear();
        policy.capability_group_keys.clear();
        policy.updated_at += 1;
        policy.version += 1;
        store
            .put_portal_grant_override(policy.clone(), Some(3), proof("policy.4"))
            .await
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        assert_eq!(
            store
                .get_grant_binding(
                    GrantOwnerKind::User,
                    "usr_reconcile".to_owned(),
                    participant_id.clone(),
                )
                .await
                .unwrap(),
            Some(narrowed.clone())
        );
        store
            .run(|connection| {
                connection
                    .execute("DELETE FROM auth_bootstrap_administrator", [])
                    .map(|_| ())
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))
            })
            .await
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        let without_admin = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(without_admin.platform_privileges.is_empty());
        assert_eq!(without_admin.installed_revision, 1);
        assert_eq!(without_admin.expires_at, expires_at);

        store
            .remove_portal_grant_override("portal", &participant_id, 4, proof("policy.remove"))
            .await
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        let policy_revoked = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(policy_revoked.state, GrantBindingState::Revoked);
        assert!(policy_revoked.provenance.is_some());

        policy.direct_capabilities = vec![first.clone()];
        policy.capability_group_keys = vec!["admin".to_owned()];
        policy.created_at += 1;
        policy.updated_at += 1;
        policy.version = 1;
        store
            .put_portal_grant_override(policy.clone(), None, proof("policy.restore"))
            .await
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        assert_eq!(
            store
                .get_grant_binding(
                    GrantOwnerKind::User,
                    "usr_reconcile".to_owned(),
                    participant_id.clone(),
                )
                .await
                .unwrap(),
            Some(policy_revoked.clone())
        );

        store
            .set_grant_binding(
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: "usr_reconcile".to_owned(),
                    participant_id: participant_id.clone(),
                    installed_revision: 1,
                    grants: without_admin.grants.clone(),
                    approval_mode: ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: without_admin.approved_resources.clone(),
                    delegation_ceiling: DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(without_admin.grants.clone()),
                        platform_privileges: Vec::new(),
                    },
                    approval_decision_digest: "A".repeat(43),
                    companion_approved: false,
                    platform_privileges: Vec::new(),
                    state: GrantBindingState::Active,
                    expires_at,
                    provenance: None,
                    expected_revision: policy_revoked.revision,
                    expected_current_installed_revision: None,
                },
                proof("binding.manual"),
            )
            .await
            .unwrap();
        let manual = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        assert_eq!(
            store
                .get_grant_binding(
                    GrantOwnerKind::User,
                    "usr_reconcile".to_owned(),
                    participant_id.clone(),
                )
                .await
                .unwrap(),
            Some(manual.clone())
        );
    }

    #[tokio::test]
    async fn ordinary_default_binding_survives_reconciliation_and_narrows() {
        const NOW: i64 = 1_700_000_000_000;
        let proof = |purpose: &str| IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!([purpose])).unwrap(),
            purpose: purpose.to_owned(),
            signer_id: "test".to_owned(),
            request_id: purpose.to_owned(),
            request_digest: trellis_protocol::digest_json(&json!({ "purpose": purpose })).unwrap(),
            result: Value::Null,
            created_at: NOW,
            expires_at: NOW + 60_000,
        };
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let mut participant = builtins::cli_participant_binding(NOW).unwrap();
        participant.projection.optional_capability_definitions = participant
            .projection
            .referenced_apis
            .values()
            .flat_map(|api| api.capabilities.iter())
            .take(2)
            .map(|(id, capability)| (id.clone(), GrantSet::new(capability.allows.clone())))
            .collect();
        let participant_id = participant.participant_id.clone();
        store.put_participant_binding(participant).await.unwrap();
        store
            .create_user_account(AccountCreation {
                principal: PrincipalRecord {
                    principal_id: "usr_default_reconcile".to_owned(),
                    kind: PrincipalKind::User,
                    state: PrincipalState::Active,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                    disabled_at: None,
                    revoked_at: None,
                },
                profile: UserProfileRecord {
                    principal_id: "usr_default_reconcile".to_owned(),
                    display_name: None,
                    email: None,
                    image_url: None,
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                },
                credential: None::<LocalCredentialRecord>,
                identity: None,
                idempotency: proof("default.account.create"),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        let portal = LoginPortalRecord {
            portal_id: "portal-default".to_owned(),
            display_name: "Portal".to_owned(),
            entry_url: None,
            builtin: false,
            disabled: false,
            removed: false,
            local_registration_enabled: false,
            provider_ids: vec!["local".to_owned()],
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        let settings = LoginSettingsRecord {
            portal_id: portal.portal_id.clone(),
            default_provider_id: Some("local".to_owned()),
            local_login_enabled: true,
            federated_registration_enabled: false,
            provider_selection_enabled: false,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_login_portal(LoginPortalMutation {
                portal: portal.clone(),
                settings,
                expected_version: None,
                idempotency: proof("default.portal.create"),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        let (_, installed) = store
            .get_installed_participant_record(participant_id.clone(), Some(1))
            .await
            .unwrap()
            .unwrap();
        let selection = crate::platform::auth::policy::default_portal_authority_selection(
            &portal.portal_id,
            &installed,
        )
        .unwrap();
        let approved_capabilities = selection
            .ceiling
            .capabilities
            .iter()
            .take(1)
            .cloned()
            .collect::<Vec<_>>();
        let unapproved_capability = selection
            .ceiling
            .capabilities
            .iter()
            .find(|capability| !approved_capabilities.contains(capability))
            .cloned()
            .expect("the fixture selects an unapproved optional capability");
        assert_eq!(approved_capabilities.len(), 1);
        let resolved = resolve_authority(
            &installed,
            ApprovalMode::Capabilities,
            &approved_capabilities,
            &[],
            &[],
            &selection.ceiling,
            (&[], true),
        )
        .unwrap();
        store
            .set_grant_binding(
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: "usr_default_reconcile".to_owned(),
                    participant_id: participant_id.clone(),
                    installed_revision: 1,
                    grants: resolved.exact_grants.clone(),
                    approval_mode: ApprovalMode::Capabilities,
                    approved_capabilities: approved_capabilities.clone(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: selection.ceiling.clone(),
                    approval_decision_digest: "A".repeat(43),
                    companion_approved: false,
                    platform_privileges: Vec::new(),
                    state: GrantBindingState::Active,
                    expires_at: None,
                    provenance: Some(PortalGrantProvenance {
                        portal_id: portal.portal_id.clone(),
                        provider_id: "local".to_owned(),
                        roles: Vec::new(),
                        effective_policy_digest: selection.effective_policy_digest.clone(),
                    }),
                    expected_revision: 0,
                    expected_current_installed_revision: None,
                },
                proof("default.binding.create"),
            )
            .await
            .unwrap();
        let service = AuthService::new(store.clone(), AuthServiceConfig::default()).unwrap();
        let (_, worker) = portal_policy_reconciliation(service);
        worker.reconcile_startup().await.unwrap();
        let survived = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_default_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(survived.state, GrantBindingState::Active);
        assert_eq!(survived.approved_capabilities, approved_capabilities);
        assert_eq!(survived.grants, resolved.exact_grants);

        // An override that makes the second capability eligible must not
        // approve it: eligibility is not a user approval.
        store
            .put_portal_grant_override(
                PortalGrantOverrideRecord {
                    portal_id: portal.portal_id.clone(),
                    participant_id: participant_id.clone(),
                    direct_capabilities: vec![
                        approved_capabilities[0].id.clone(),
                        unapproved_capability.id.clone(),
                    ],
                    capability_group_keys: Vec::new(),
                    role_mappings: Vec::new(),
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                },
                None,
                proof("default.override.ab"),
            )
            .await
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        let widened = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_default_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            widened
                .delegation_ceiling
                .capabilities
                .contains(&unapproved_capability),
            "the override ceiling contains the eligible capability"
        );
        assert_eq!(widened.approved_capabilities, approved_capabilities);
        assert_eq!(widened.grants, survived.grants);

        // A narrower override reduces the approved scope without adding scope.
        store
            .put_portal_grant_override(
                PortalGrantOverrideRecord {
                    portal_id: portal.portal_id.clone(),
                    participant_id: participant_id.clone(),
                    direct_capabilities: Vec::new(),
                    capability_group_keys: Vec::new(),
                    role_mappings: Vec::new(),
                    created_at: NOW,
                    updated_at: NOW,
                    version: 2,
                },
                Some(1),
                proof("default.override.create"),
            )
            .await
            .unwrap();
        worker.reconcile_startup().await.unwrap();
        let narrowed = store
            .get_grant_binding(
                GrantOwnerKind::User,
                "usr_default_reconcile".to_owned(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(narrowed.state, GrantBindingState::Active);
        assert!(narrowed.approved_capabilities.is_empty());
        assert!(
            narrowed.grants.permissions().len() < survived.grants.permissions().len(),
            "narrowing must reduce the granted scope"
        );
    }
}
