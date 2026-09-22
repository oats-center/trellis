use serde_json::json;

use super::fixtures::{digest, profile_for, NOW};
use crate::platform::auth::application::repository::{
    AccountCreation, AccountRepository, DeploymentProfileCreation, DeploymentProfileMutation,
    DeploymentRepository, IdempotentOutcome, OutboxRepository, UserAccountMutation,
};
use crate::platform::auth::{
    AuthorizationStateError, DeploymentProfileRecord, DeploymentProfileState,
    GrantBindingReplacement, GrantBindingState, GrantOwnerKind, IdempotencyResultRecord,
    LocalCredentialRecord, PostCommitActionKind, PostCommitActionRecord, PrincipalKind,
    PrincipalRecord, PrincipalState, ProviderIdentityLink, SqliteAuthorizationStore,
    UserProfileRecord,
};
use trellis_protocol::{GrantSet, PlatformPrivilege};

pub(super) async fn exercise_accounts(
    store: SqliteAuthorizationStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let proof = |byte: u8, purpose: &str| IdempotencyResultRecord {
        scope_key: digest(byte),
        purpose: purpose.to_owned(),
        signer_id: "signer_companion".to_owned(),
        request_id: format!("request_{byte}"),
        request_digest: digest(byte + 1),
        result: json!({ "request": byte }),
        created_at: NOW,
        expires_at: NOW + 1_000,
    };
    let action = |byte: u8, event: &str| PostCommitActionRecord {
        predecessor_action_id: None,
        action_id: digest(byte),
        kind: PostCommitActionKind::Event,
        payload: json!({ "event": event }),
        created_at: NOW,
        attempts: 0,
        next_attempt_at: NOW,
        claimed_until: None,
        last_error: None,
    };
    let deployment_principal = PrincipalRecord {
        principal_id: "dep_profile".to_owned(),
        kind: PrincipalKind::Service,
        state: PrincipalState::Active,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
        disabled_at: None,
        revoked_at: None,
    };
    let mut deployment_profile = DeploymentProfileRecord {
        deployment_id: deployment_principal.principal_id.clone(),
        kind: PrincipalKind::Service,
        display_name: "Profile Service".to_owned(),
        participant_id: None,
        portal_id: None,
        requires_device_delegation: false,
        review_mode: None,
        expires_at: None,
        state: DeploymentProfileState::Active,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
    };
    assert!(matches!(
        store
            .create_deployment_profile(DeploymentProfileCreation {
                principal: deployment_principal,
                profile: deployment_profile.clone(),
                idempotency: proof(90, "deployment.create"),
                actions: Vec::new(),
            })
            .await?,
        IdempotentOutcome::Applied(_)
    ));
    assert_eq!(
        store.list_deployment_profiles().await?,
        vec![deployment_profile.clone()]
    );
    deployment_profile.state = DeploymentProfileState::Disabled;
    deployment_profile.updated_at += 1;
    deployment_profile.version += 1;
    let mut disable_proof = proof(92, "deployment.disable");
    disable_proof.result = serde_json::to_value(&deployment_profile)?;
    let disable = DeploymentProfileMutation {
        profile: deployment_profile.clone(),
        expected_version: 1,
        idempotency: disable_proof,
        actions: Vec::new(),
    };
    assert_eq!(
        store.put_deployment_profile(disable.clone()).await?,
        IdempotentOutcome::Applied(deployment_profile.clone())
    );
    assert_eq!(
        store.put_deployment_profile(disable.clone()).await?,
        IdempotentOutcome::Replayed(serde_json::to_value(&deployment_profile)?)
    );
    assert_eq!(
        store
            .get_deployment_profile("dep_profile")
            .await?
            .map(|value| value.state),
        Some(DeploymentProfileState::Disabled)
    );
    let mut enabled_profile = deployment_profile.clone();
    enabled_profile.state = DeploymentProfileState::Active;
    enabled_profile.updated_at += 1;
    enabled_profile.version += 1;
    enabled_profile.display_name = "Updated Service".to_owned();
    assert_eq!(
        store
            .put_deployment_profile(DeploymentProfileMutation {
                profile: enabled_profile.clone(),
                expected_version: deployment_profile.version,
                idempotency: proof(93, "deployment.enable"),
                actions: Vec::new(),
            })
            .await?,
        IdempotentOutcome::Applied(enabled_profile.clone())
    );
    assert_eq!(
        store.put_deployment_profile(disable).await?,
        IdempotentOutcome::Replayed(serde_json::to_value(&deployment_profile)?)
    );
    assert_eq!(
        store.get_deployment_profile("dep_profile").await?,
        Some(enabled_profile)
    );
    let user = PrincipalRecord {
        principal_id: "usr_companion".to_owned(),
        kind: PrincipalKind::User,
        state: PrincipalState::Active,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
        disabled_at: None,
        revoked_at: None,
    };
    let profile = UserProfileRecord {
        principal_id: user.principal_id.clone(),
        display_name: Some("Companion User".to_owned()),
        email: Some("user@example.com".to_owned()),
        image_url: None,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
    };
    let credential = LocalCredentialRecord {
        principal_id: user.principal_id.clone(),
        normalized_username: "companion".to_owned(),
        password_hash: "argon2id-hash".to_owned(),
        hash_profile: 1,
        failed_attempts: 0,
        locked_until: None,
        password_changed_at: NOW,
        updated_at: NOW,
        version: 1,
    };
    let identity = ProviderIdentityLink {
        provider: "local".to_owned(),
        provider_subject: "companion".to_owned(),
        principal_id: user.principal_id.clone(),
        linked_at: NOW,
        last_seen_at: NOW,
    };
    let account_action = action(20, "account.created");
    let account_creation = AccountCreation {
        principal: user.clone(),
        profile: profile.clone(),
        credential: Some(credential.clone()),
        identity: Some(identity.clone()),
        idempotency: proof(20, "account.create"),
        actions: vec![account_action.clone()],
    };
    assert_eq!(
        store.create_user_account(account_creation.clone()).await?,
        IdempotentOutcome::Applied(profile.clone())
    );
    let mut replayed_account = account_creation.clone();
    replayed_account.profile.version = 99;
    replayed_account.actions[0].payload = json!({ "different": true });
    assert_eq!(
        store.create_user_account(replayed_account).await?,
        IdempotentOutcome::Replayed(account_creation.idempotency.result.clone())
    );
    let mut mismatched_account = account_creation.clone();
    mismatched_account.idempotency.request_digest = digest(99);
    assert_eq!(
        store.create_user_account(mismatched_account).await,
        Err(AuthorizationStateError::StorageConflict)
    );
    assert_eq!(
        store.get_user_profile(&user.principal_id).await?,
        Some(profile.clone())
    );
    assert_eq!(
        store.get_local_credential(&user.principal_id).await?,
        Some(credential.clone())
    );
    assert_eq!(
        store
            .get_provider_identity(&identity.provider, &identity.provider_subject)
            .await?,
        Some(identity)
    );

    let mut conflicting_user = user.clone();
    conflicting_user.principal_id = "usr_rolled_back".to_owned();
    let mut conflicting_profile = profile_for(&conflicting_user.principal_id);
    conflicting_profile.display_name = Some("Rolled Back".to_owned());
    let mut conflicting_credential = credential.clone();
    conflicting_credential.principal_id = conflicting_user.principal_id.clone();
    conflicting_credential.normalized_username = "rolled-back".to_owned();
    let conflicting_identity = ProviderIdentityLink {
        provider: "local".to_owned(),
        provider_subject: "rolled-back".to_owned(),
        principal_id: conflicting_user.principal_id.clone(),
        linked_at: NOW,
        last_seen_at: NOW,
    };
    let account_rollback_proof = proof(22, "account.create");
    assert_eq!(
        store
            .create_user_account(AccountCreation {
                principal: conflicting_user.clone(),
                profile: conflicting_profile,
                credential: Some(conflicting_credential),
                identity: Some(conflicting_identity),
                idempotency: account_rollback_proof.clone(),
                actions: vec![PostCommitActionRecord {
                    payload: json!({ "different": true }),
                    ..account_action.clone()
                }],
            })
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );
    assert_eq!(
        store.get_principal(&conflicting_user.principal_id).await?,
        None
    );
    assert!(store
        .get_idempotency_result(
            &account_rollback_proof.purpose,
            &account_rollback_proof.signer_id,
            &account_rollback_proof.request_id,
        )
        .await?
        .is_none());

    let managed_user = PrincipalRecord {
        principal_id: "usr_account_a".to_owned(),
        ..user.clone()
    };
    let managed_profile = UserProfileRecord {
        principal_id: managed_user.principal_id.clone(),
        display_name: None,
        email: None,
        image_url: None,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
    };
    assert_eq!(
        store
            .create_user_account(AccountCreation {
                principal: managed_user.clone(),
                profile: managed_profile.clone(),
                credential: None,
                identity: None,
                idempotency: proof(100, "account.create-managed"),
                actions: Vec::new(),
            })
            .await?,
        IdempotentOutcome::Applied(managed_profile.clone())
    );
    assert_eq!(
        store.get_user_account(&managed_user.principal_id).await?,
        Some((managed_user.clone(), managed_profile.clone()))
    );
    assert_eq!(
        store
            .get_local_credential(&managed_user.principal_id)
            .await?,
        None
    );
    let managed_user_b = PrincipalRecord {
        principal_id: "usr_account_b".to_owned(),
        ..user.clone()
    };
    let managed_profile_b = UserProfileRecord {
        principal_id: managed_user_b.principal_id.clone(),
        ..managed_profile.clone()
    };
    store
        .create_user_account(AccountCreation {
            principal: managed_user_b.clone(),
            profile: managed_profile_b.clone(),
            credential: None,
            identity: None,
            idempotency: proof(102, "account.create-managed"),
            actions: Vec::new(),
        })
        .await?;
    for index in 0..52 {
        let principal = PrincipalRecord {
            principal_id: format!("usr_noise_{index:03}"),
            ..user.clone()
        };
        let profile = UserProfileRecord {
            principal_id: principal.principal_id.clone(),
            display_name: Some("Equal sort key".to_owned()),
            ..managed_profile.clone()
        };
        store
            .create_user_account(AccountCreation {
                principal,
                profile,
                credential: None,
                identity: None,
                idempotency: proof(120 + index as u8, "account.create-noise"),
                actions: Vec::new(),
            })
            .await?;
    }
    let mut matched_accounts = Vec::new();
    for index in 0..3 {
        let principal = PrincipalRecord {
            principal_id: format!("usr_target_{index}"),
            ..user.clone()
        };
        let profile = UserProfileRecord {
            principal_id: principal.principal_id.clone(),
            display_name: Some(format!("Needle Account {index}")),
            ..managed_profile.clone()
        };
        store
            .create_user_account(AccountCreation {
                principal: principal.clone(),
                profile: profile.clone(),
                credential: None,
                identity: None,
                idempotency: proof(180 + index as u8, "account.create-match"),
                actions: Vec::new(),
            })
            .await?;
        matched_accounts.push((principal, profile));
    }
    assert_eq!(
        store
            .list_user_accounts(None, Some("active"), Some("needle"), 3)
            .await?,
        matched_accounts
    );
    let first_equal_page = store
        .list_user_accounts(None, None, Some("equal sort key"), 10)
        .await?;
    assert_eq!(
        first_equal_page
            .iter()
            .map(|account| account.0.principal_id.clone())
            .collect::<Vec<_>>(),
        (0..10)
            .map(|index| format!("usr_noise_{index:03}"))
            .collect::<Vec<_>>()
    );
    let first_equal_cursor = (
        first_equal_page[9].0.created_at,
        first_equal_page[9].0.principal_id.clone(),
    );
    let second_equal_page = store
        .list_user_accounts(Some(&first_equal_cursor), None, Some("equal sort key"), 10)
        .await?;
    assert_eq!(
        second_equal_page
            .iter()
            .map(|account| account.0.principal_id.clone())
            .collect::<Vec<_>>(),
        (10..20)
            .map(|index| format!("usr_noise_{index:03}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        store.list_user_accounts(None, None, None, 2).await?,
        vec![
            (managed_user.clone(), managed_profile.clone()),
            (managed_user_b.clone(), managed_profile_b.clone()),
        ]
    );
    assert_eq!(
        store
            .list_user_accounts(
                Some(&(managed_user.created_at, managed_user.principal_id.clone())),
                None,
                None,
                2,
            )
            .await?,
        vec![
            (managed_user_b, managed_profile_b),
            (user.clone(), profile.clone()),
        ]
    );

    let mutation_actor = super::fixtures::install_login_mutation_actor(&store, NOW).await?;
    store
        .set_grant_binding(
            GrantBindingReplacement {
                owner_kind: GrantOwnerKind::User,
                owner_id: managed_user.principal_id.clone(),
                participant_id: mutation_actor.participant_id.clone(),
                installed_revision: 1,
                grants: GrantSet::new(Vec::new()),
                approval_mode: super::super::super::ApprovalMode::Exact,
                approved_capabilities: Vec::new(),
                approved_resources: Vec::new(),
                delegation_ceiling: super::super::super::DelegationCeiling {
                    capabilities: Vec::new(),
                    exact_restrictions: Some(GrantSet::new(Vec::new())),
                    platform_privileges: vec![PlatformPrivilege::Admin],
                },
                approval_decision_digest: "A".repeat(43),
                companion_approved: false,
                platform_privileges: vec![PlatformPrivilege::Admin],
                state: GrantBindingState::Active,
                expires_at: None,
                provenance: None,
                expected_revision: 0,
                expected_current_installed_revision: None,
            },
            proof(103, "target.admin"),
        )
        .await?;
    let account_update_action = action(100, "account.updated");
    let mut disabled_user = managed_user.clone();
    disabled_user.state = PrincipalState::Disabled;
    disabled_user.updated_at = NOW + 5;
    disabled_user.version = 2;
    let mut updated_profile = managed_profile.clone();
    updated_profile.email = Some("managed@example.com".to_owned());
    updated_profile.updated_at = NOW + 5;
    updated_profile.version = 2;
    let account_update = UserAccountMutation {
        actor: mutation_actor.clone(),
        principal: disabled_user,
        profile: updated_profile,
        expected_version: 1,
        idempotency: proof(104, "account.update"),
        actions: vec![account_update_action.clone()],
    };
    let (mut disabled_user, updated_profile) =
        match store.update_user_account(account_update.clone()).await? {
            IdempotentOutcome::Applied(account) => account,
            IdempotentOutcome::Replayed(_) => unreachable!(),
        };
    assert_eq!(disabled_user.disabled_at, Some(NOW + 5));
    let persisted_disabled_account = (disabled_user.clone(), updated_profile.clone());
    let mut malformed_replay = account_update.clone();
    malformed_replay.principal.version = 99;
    malformed_replay.actions[0].payload = json!({ "different": true });
    assert_eq!(
        store.update_user_account(malformed_replay).await?,
        IdempotentOutcome::Replayed(account_update.idempotency.result.clone())
    );

    disabled_user.state = PrincipalState::Active;
    disabled_user.updated_at = NOW + 6;
    disabled_user.version = 3;
    let mut rollback_profile = updated_profile.clone();
    rollback_profile.display_name = Some("Should Roll Back".to_owned());
    rollback_profile.updated_at = NOW + 6;
    rollback_profile.version = 3;
    let account_update_rollback_proof = proof(106, "account.update");
    assert_eq!(
        store
            .update_user_account(UserAccountMutation {
                actor: mutation_actor.clone(),
                principal: disabled_user,
                profile: rollback_profile,
                expected_version: 2,
                idempotency: account_update_rollback_proof.clone(),
                actions: vec![PostCommitActionRecord {
                    payload: json!({ "different": true }),
                    ..account_update_action
                }],
            })
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );
    assert_eq!(
        store.get_user_account(&managed_user.principal_id).await?,
        Some(persisted_disabled_account)
    );
    assert!(store
        .get_idempotency_result(
            &account_update_rollback_proof.purpose,
            &account_update_rollback_proof.signer_id,
            &account_update_rollback_proof.request_id,
        )
        .await?
        .is_none());
    store
        .revoke_grant_binding(
            mutation_actor.clone(),
            GrantOwnerKind::User,
            mutation_actor.principal_id.clone(),
            mutation_actor.participant_id.clone(),
            1,
            proof(107, "actor.revoke"),
        )
        .await?;
    assert_eq!(
        store.update_user_account(account_update).await,
        Err(AuthorizationStateError::NotAuthorized)
    );
    Ok(())
}
