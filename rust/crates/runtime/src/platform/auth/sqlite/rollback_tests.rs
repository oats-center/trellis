use serde_json::json;

use super::SqliteAuthorizationStore;
use crate::platform::auth::{
    AccountCreation, AccountRepository, AuthorizationStateError, IdempotencyResultRecord,
    OutboxRepository, PostCommitActionKind, PostCommitActionRecord, PrincipalKind, PrincipalRecord,
    PrincipalState, UserAccountMutation, UserProfileRecord,
};

const NOW: i64 = 1_700_000_000_000;

fn idempotency(
    scope: char,
    purpose: &str,
    request_id: &str,
    digest: char,
) -> IdempotencyResultRecord {
    IdempotencyResultRecord {
        scope_key: scope.to_string().repeat(43),
        purpose: purpose.to_owned(),
        signer_id: "test-signer".to_owned(),
        request_id: request_id.to_owned(),
        request_digest: digest.to_string().repeat(43),
        result: json!({ "ok": true }),
        created_at: NOW,
        expires_at: NOW + 60_000,
    }
}

#[tokio::test]
async fn user_update_rolls_back_when_real_outbox_constraint_fails() {
    let repository = SqliteAuthorizationStore::open_in_memory().expect("open sqlite auth store");
    let principal = PrincipalRecord {
        principal_id: "usr_rollback".to_owned(),
        kind: PrincipalKind::User,
        state: PrincipalState::Active,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
        disabled_at: None,
        revoked_at: None,
    };
    let profile = UserProfileRecord {
        principal_id: principal.principal_id.clone(),
        display_name: Some("Before".to_owned()),
        email: Some("before@example.test".to_owned()),
        image_url: None,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
    };
    repository
        .create_user_account(AccountCreation {
            principal: principal.clone(),
            profile: profile.clone(),
            credential: None,
            identity: None,
            idempotency: idempotency('A', "account.create", "create-user", 'B'),
            actions: Vec::new(),
        })
        .await
        .expect("create user account");
    let actor = crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
        &repository,
        NOW,
    )
    .await
    .expect("install mutation actor");

    let update_idempotency = idempotency('C', "account.update", "update-user", 'D');
    let error = repository
        .update_user_account(UserAccountMutation {
            actor,
            principal: PrincipalRecord {
                updated_at: NOW + 1,
                version: 2,
                ..principal.clone()
            },
            profile: UserProfileRecord {
                display_name: Some("After".to_owned()),
                email: Some("after@example.test".to_owned()),
                updated_at: NOW + 1,
                version: 2,
                ..profile.clone()
            },
            expected_version: 1,
            idempotency: update_idempotency.clone(),
            actions: vec![PostCommitActionRecord {
                predecessor_action_id: None,
                action_id: "bad".to_owned(),
                kind: PostCommitActionKind::Event,
                payload: json!({ "eventType": "User.Updated" }),
                created_at: NOW + 1,
                attempts: 0,
                next_attempt_at: NOW + 1,
                claimed_until: None,
                last_error: None,
            }],
        })
        .await
        .expect_err("invalid action id unexpectedly committed");
    assert_eq!(error, AuthorizationStateError::StorageConflict);

    assert_eq!(
        repository
            .get_user_account(&principal.principal_id)
            .await
            .expect("read user after failed update"),
        Some((principal, profile))
    );
    assert!(repository
        .get_idempotency_result(
            &update_idempotency.purpose,
            &update_idempotency.signer_id,
            &update_idempotency.request_id,
        )
        .await
        .expect("read update idempotency result")
        .is_none());
    assert!(repository
        .list_ready_post_commit_actions(NOW + 10, 10)
        .await
        .expect("list post-commit actions")
        .is_empty());
}
