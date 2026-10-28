use super::*;

#[test]
fn issuer_revocation_uses_only_issuer_privilege_and_preserves_other_scope_gates() {
    let f = fixture();
    let delegate = create_user(&f.store, &f.admin, NOW);
    administer(
        &f.store,
        &f.admin,
        &delegate,
        &[Administration::ManagePlatform],
        NOW,
    );
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let previous = f.store.issuer().unwrap();
    crate::platform::auth::tests::rotate_issuer(&f.store, &delegate, NOW + 1).unwrap();
    drop(f.store);
    let store = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let grant_scope = EnforcementScope::Grant(f.grant);
    assert!(matches!(
        revoke_root(
            &store,
            &mutation(&delegate, &grant_scope),
            &grant_scope,
            NOW + 2
        ),
        Err(AuthError::Denied)
    ));
    let current_scope = EnforcementScope::Issuer(store.issuer().unwrap().key_id);
    assert!(matches!(
        revoke_root(
            &store,
            &mutation(&delegate, &current_scope),
            &current_scope,
            NOW + 2
        ),
        Err(AuthError::Conflict)
    ));
    let scope = EnforcementScope::Issuer(previous.key_id.clone());
    revoke_root(&store, &mutation(&delegate, &scope), &scope, NOW + 2).unwrap();
    assert_eq!(
        store
            .transaction(|connection| SqliteAuthorizationStore::resolve_issuer_in(
                connection,
                &previous.key_id
            ))
            .unwrap()
            .state,
        trellis_protocol::AuthorityIssuerState::Revoked
    );
    while store.enforce_next_page(NOW + 2).unwrap().is_some() {}
    assert!(store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
    assert!(matches!(
        issue_authority(
            &store,
            &f.admission,
            Some(&issued.authority.unsigned.authorization_session_id),
            NOW + 2
        ),
        Err(AuthError::Retired)
    ));
    assert!(issue_authority(&store, &f.admission, None, NOW + 2).is_ok());
}

#[test]
fn rotation_authenticates_the_new_pin_and_rejects_forgery_and_rollback() {
    let f = fixture();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let previous = f.store.issuer().unwrap();
    let rotation =
        crate::platform::auth::tests::rotate_issuer(&f.store, &f.admin, NOW + 1).unwrap();
    let installed = rotation
        .verify_successor(
            &previous,
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            account_key(),
            0,
            NOW + 1,
        )
        .unwrap();
    let mut forged = rotation.clone();
    forged.next_public_key = f.admission.session_public_key.clone();
    assert!(forged
        .verify_successor(
            &previous,
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            account_key(),
            0,
            NOW + 1
        )
        .is_err());
    assert!(rotation
        .verify_successor(
            &installed,
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            account_key(),
            1,
            NOW + 1
        )
        .is_err());
    assert_eq!(
        issue_authority(&f.store, &f.admission, None, NOW + 1)
            .unwrap()
            .authority
            .unsigned
            .issuer_key_id,
        installed.key_id
    );
    let historical = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::resolve_issuer_in(connection, &previous.key_id)
        })
        .unwrap();
    assert_eq!(
        historical.state,
        trellis_protocol::AuthorityIssuerState::Retired
    );
    drop(f.store);
    let restarted = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let renewed = issue_authority(
        &restarted,
        &f.admission,
        Some(&issued.authority.unsigned.authorization_session_id),
        NOW + 1,
    )
    .unwrap();
    assert_eq!(renewed.authority.unsigned.issuer_key_id, installed.key_id);
    assert_eq!(
        resolve_authority(&restarted, &issued.digest).unwrap(),
        issued.authority
    );
}

#[test]
fn issuer_rotation_replay_does_not_rotate_again_and_stale_requests_do_not_change_custody() {
    let f = fixture();
    let original = f.store.issuer().unwrap();
    let input = trellis_runtime_apis::types::AuthIssuerRotateRequest {
        expected_current_key_id: original.key_id.clone().into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &input);
    let rotation = f.store.rotate_issuer(&identity, &input, NOW).unwrap();
    let replay = f.store.rotate_issuer(&identity, &input, NOW + 1).unwrap();
    assert_eq!(
        serde_json::to_value(&rotation).unwrap(),
        serde_json::to_value(&replay).unwrap()
    );
    assert!(matches!(
        f.store
            .rotate_issuer(&mutation(&f.admin, &input), &input, NOW + 1),
        Err(AuthError::Conflict)
    ));
    let issued = issue_authority(&f.store, &f.admission, None, NOW + 1).unwrap();
    let successor = rotation
        .verify_successor(
            &original,
            &f.store.settings.instance_id,
            &f.store.settings.account,
            0,
            NOW,
        )
        .unwrap();
    assert_eq!(issued.authority.unsigned.issuer_key_id, successor.key_id);
    assert!(matches!(
        f.store
            .rotate_issuer(&mutation(&f.principal, &input), &input, NOW + 1),
        Err(AuthError::Denied)
    ));
}

#[cfg(unix)]
#[test]
fn unsafe_issuer_custody_refuses_rotation_without_retiring_the_current_signer() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let original = f.store.issuer().unwrap();
    std::fs::create_dir(&f.store.custody_directory).unwrap();
    std::fs::set_permissions(
        &f.store.custody_directory,
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(
        rotate_issuer(&f.store, &f.admin, NOW),
        Err(AuthError::Unavailable)
    ));
    let issued = issue_authority(&f.store, &f.admission, None, NOW + 1).unwrap();
    assert_eq!(issued.authority.unsigned.issuer_key_id, original.key_id);
    std::fs::set_permissions(
        &f.store.custody_directory,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let rotation = rotate_issuer(&f.store, &f.admin, NOW + 1).unwrap();
    assert_eq!(
        issue_authority(&f.store, &f.admission, None, NOW + 2)
            .unwrap()
            .authority
            .unsigned
            .issuer_key_id,
        rotation.next_key_id
    );
}
