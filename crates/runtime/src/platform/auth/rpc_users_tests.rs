use super::*;
use crate::platform::auth::tests::{mutation, open, NOW};
use ed25519_dalek::SigningKey;

fn reset_session(
    store: &SqliteAuthorizationStore,
    admin: &str,
    actor: &str,
    username: &str,
    password: &str,
) -> String {
    use crate::platform::auth::lifecycle::ClientRegistration;
    use crate::platform::auth::tests::grant_request;
    use crate::platform::auth::{AdmissionIdentity, Credential};
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let client = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Reset requester".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![],
        metadata: serde_json::json!({}),
        expected_revision: 0,
    };
    store
        .transaction(|connection| {
            store.register_client_in_transaction(
                connection,
                &mutation(admin, &client),
                &client,
                NOW,
            )
        })
        .unwrap();
    let cookie = store.create_portal_session(NOW).unwrap();
    let login = store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"r".repeat(43),
            (username, password),
            &id(),
            NOW,
        )
        .unwrap();
    let mut approval = grant_request(actor, &login.login_session_id, "", "", NOW);
    let manage = Administration::ManagePrincipals.capability_id().to_string();
    approval.selection.required = vec![manage.clone()];
    approval.selection.optional.clear();
    approval.approved = vec![manage];
    approval.administration_confirmed = true;
    approval.remember = false;
    let grant = store
        .approve_grant(&mutation(actor, &approval), &approval, NOW)
        .unwrap();
    // Another administrator approves the delegation.
    let pending: Option<(String, u64)> = store
        .read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT review_id,revision FROM auth_administration_reviews \
                    WHERE principal_id=?1 AND state='pending'",
                    [actor],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })
        .unwrap();
    if let Some((review, revision)) = pending {
        let input: trellis_runtime_apis::types::AuthAdministrationReviewDecideRequest =
            serde_json::from_value(serde_json::json!({
                "reviewId": review,
                "decision": "approved",
                "expectedRevision": revision.to_string(),
                "idempotencyKey": id(),
            }))
            .unwrap();
        crate::platform::auth::administration::decide(store, &mutation(admin, &input), &input, NOW)
            .unwrap();
    }
    let admission = AdmissionIdentity {
        refresh_family_id: Some(crate::platform::auth::tests::oauth_family(
            store,
            &grant,
            NOW,
            NOW + 86400,
        )),
        credential: Credential::OAuthGrant(grant),
        runtime_id: URL_SAFE_NO_PAD.encode([21; 16]),
        session_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&[51; 32]).verifying_key().as_bytes()),
        inbox_prefix: "_INBOX.resetRequester".into(),
    };
    crate::platform::auth::tests::issue_authority(store, &admission, None, NOW)
        .unwrap()
        .authority
        .unsigned
        .authorization_session_id
}

#[test]
fn created_account_authenticates_and_profile_cas_preserves_protected_admin() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[41; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let input = AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "Alice".to_string().into(),
        display_name: "Alice Example".to_string().into(),
        password: Some("initial alice password".to_string().into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&admin, &input);
    let created = create_user(&store, &identity, &input, NOW).unwrap();
    assert_eq!(
        create_user(&store, &identity, &input, NOW + 1).unwrap(),
        created
    );
    let cookie = store.create_portal_session(NOW).unwrap();
    let login = store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"a".repeat(43),
            ("ALICE", "initial alice password"),
            &id(),
            NOW,
        )
        .unwrap();
    assert_eq!(login.principal_id, created.user.principal_id.to_string());
    let update = AuthUserUpdateRequest {
        given_name: None,
        family_name: None,
        email: None,
        clear_email: None,
        user_id: created.user.principal_id,
        expected_revision: AuthRevision(1.into()),
        display_name: Some("Alice Updated".to_string().into()),
        expected_profile_revision: Some(created.user.account.as_ref().unwrap().profile_revision),
        enabled: true,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let updated = update_user(&store, &mutation(&admin, &update), &update, NOW + 1).unwrap();
    assert_eq!(updated.user.display_name, "Alice Updated");
    let stale = AuthUserUpdateRequest {
        given_name: None,
        family_name: None,
        email: None,
        clear_email: None,
        enabled: false,
        display_name: Some("Stale write".to_string().into()),
        idempotency_key: id().into(),
        ..update
    };
    assert!(matches!(
        update_user(&store, &mutation(&admin, &stale), &stale, NOW + 2),
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        store
            .transaction(|connection| read_user(&store, connection, &login.principal_id, true))
            .unwrap(),
        updated.user
    );
    let protected = AuthUserUpdateRequest {
        given_name: None,
        family_name: None,
        email: None,
        clear_email: None,
        user_id: admin.parse().unwrap(),
        expected_revision: AuthRevision(1.into()),
        display_name: None,
        expected_profile_revision: None,
        enabled: false,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        update_user(&store, &mutation(&admin, &protected), &protected, NOW + 2),
        Err(AuthError::Denied)
    ));
    assert!(
        store
            .transaction(|connection| read_user(&store, connection, &admin, true))
            .unwrap()
            .account
            .unwrap()
            .principal
            .enabled
    );
}

#[test]
fn password_change_keeps_current_login_revokes_siblings_and_replays_without_old_password() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[42; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let cookie = store.create_portal_session(NOW).unwrap();
    let current = store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"a".repeat(43),
            ("admin", "administrator password"),
            &id(),
            NOW,
        )
        .unwrap();
    let current_csrf = cookie.csrf.clone();
    let cookie = store.create_portal_session(NOW).unwrap();
    store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"b".repeat(43),
            ("admin", "administrator password"),
            &id(),
            NOW,
        )
        .unwrap();
    let input = AuthPasswordChangeRequest {
        current_password: "administrator password".to_string().into(),
        new_password: "replacement password".to_string().into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&admin, &input);
    let changed = change_password(
        &store,
        &identity,
        &input,
        &current.login_session_id,
        NOW + 1,
    )
    .unwrap();
    assert_eq!(
        change_password(
            &store,
            &identity,
            &input,
            &current.login_session_id,
            NOW + 2
        )
        .unwrap(),
        changed
    );
    assert!(store
        .portal_login(&"a".repeat(43), &current_csrf, NOW + 2)
        .is_ok());
    // Use each cookie's own CSRF value: sibling failure must be revocation, not
    // an unrelated CSRF mismatch.
    assert!(matches!(
        store.portal_login(&"b".repeat(43), &cookie.csrf, NOW + 2),
        Err(AuthError::Denied)
    ));
    let cookie = store.create_portal_session(NOW + 2).unwrap();
    assert!(matches!(
        store.portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"c".repeat(43),
            ("admin", "administrator password"),
            &id(),
            NOW + 2
        ),
        Err(AuthError::Denied)
    ));
    assert!(store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"c".repeat(43),
            ("admin", "replacement password"),
            &id(),
            NOW + 2
        )
        .is_ok());
    let audits: Vec<String> = store
        .transaction(|connection| {
            let mut statement = connection.prepare("SELECT data_json FROM auth_security_audit")?;
            let rows = statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    assert!(audits
        .iter()
        .all(|audit| !audit.contains("administrator password")
            && !audit.contains("replacement password")
            && !audit.contains("argon2")));
}

#[test]
fn provider_disable_blocks_verified_login_and_stale_delete_cannot_remove_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[43; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let provider = id();
    let registration = crate::platform::auth::lifecycle::OidcProviderRegistration {
        provider_id: provider.clone(),
        issuer: "https://identity.example".into(),
        display_name: "Company".into(),
        client_id: "trellis".into(),
        sealed_client_secret: None,
        config: serde_json::json!({"scopes": ["openid"], "roleClaim": "/roles"}),
        claim_freshness_seconds: 3600,
        expected_revision: 0,
    };
    store
        .transaction(|connection| {
            store.register_oidc_provider_in_transaction(
                connection,
                &mutation(&admin, &registration),
                &registration,
                NOW,
            )
        })
        .unwrap();
    store
        .transaction(|connection| {
            SqliteAuthorizationStore::link_oidc_identity_in(
                connection, &admin, &provider, "subject", NOW,
            )
        })
        .unwrap();
    let login = crate::platform::auth::VerifiedLogin {
        principal_id: admin.clone(),
        provider_id: Some(provider.clone()),
        upstream_issuer: Some(registration.issuer),
        upstream_subject: Some("subject".into()),
        verified_claims: Some(serde_json::json!({"roles": []})),
        expires_at: NOW + 300,
    };
    assert!(store
        .transaction(
            |connection| SqliteAuthorizationStore::record_verified_login_in(
                connection, &login, NOW
            )
        )
        .is_ok());
    let input = AuthOIDCProviderMutationRequest {
        provider_id: provider.parse().unwrap(),
        expected_revision: AuthRevision(1.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    disable_provider(&store, &mutation(&admin, &input), &input, NOW + 1).unwrap();
    assert!(matches!(
        store.transaction(
            |connection| SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &login,
                NOW + 2
            )
        ),
        Err(AuthError::Denied)
    ));
    let stale = AuthOIDCProviderMutationRequest {
        idempotency_key: id().into(),
        ..input
    };
    assert!(matches!(
        delete_provider(&store, &mutation(&admin, &stale), &stale, NOW + 2),
        Err(AuthError::Conflict)
    ));
    let listed = store
        .transaction(|connection| {
            read_providers(
                connection,
                &AuthPageRequest {
                    page: None,
                    search: Some("Company".to_string().into()),
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert!(!listed.items[0].enabled);
}

#[test]
fn passwordless_account_reserves_username_without_creating_a_sign_in_credential() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[44; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let input = AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "Reserved".to_string().into(),
        display_name: "Pending account".to_string().into(),
        password: None,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let created = create_user(&store, &mutation(&admin, &input), &input, NOW).unwrap();
    let account = created.user.account.unwrap();
    assert_eq!(account.username, "reserved");
    assert!(!account.has_local_password);
    let duplicate = AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "RESERVED".to_string().into(),
        password: Some("a usable password".to_string().into()),
        idempotency_key: id().into(),
        ..input
    };
    assert!(matches!(
        create_user(&store, &mutation(&admin, &duplicate), &duplicate, NOW + 1),
        Err(AuthError::Conflict)
    ));
    let cookie = store.create_portal_session(NOW).unwrap();
    assert!(matches!(
        store.portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"d".repeat(43),
            ("reserved", "a usable password"),
            &id(),
            NOW
        ),
        Err(AuthError::Denied)
    ));
}

#[test]
fn racing_password_changes_cannot_both_replace_the_same_credential() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[45; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let cookie = store.create_portal_session(NOW).unwrap();
    let login = store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"e".repeat(43),
            ("admin", "administrator password"),
            &id(),
            NOW,
        )
        .unwrap()
        .login_session_id;
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let threads = ["first replacement password", "second replacement password"].map(|password| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let admin = admin.clone();
        let login = login.clone();
        std::thread::spawn(move || {
            let input = AuthPasswordChangeRequest {
                current_password: "administrator password".to_string().into(),
                new_password: password.to_string().into(),
                idempotency_key: id().into(),
                extra: Default::default(),
            };
            let identity = mutation(&admin, &input);
            barrier.wait();
            (
                password,
                change_password(&store, &identity, &input, &login, NOW + 1),
            )
        })
    });
    let outcomes = threads.map(|thread| thread.join().unwrap());
    assert_eq!(
        outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    let winner = outcomes
        .iter()
        .find(|(_, result)| result.is_ok())
        .unwrap()
        .0;
    let loser = outcomes.iter().find(|(_, result)| result.is_err()).unwrap();
    assert!(matches!(
        loser.1,
        Err(AuthError::Conflict | AuthError::Denied)
    ));
    let cookie = store.create_portal_session(NOW + 2).unwrap();
    assert!(store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"f".repeat(43),
            ("admin", winner),
            &id(),
            NOW + 2
        )
        .is_ok());
}

#[test]
fn provider_put_replays_generated_identity_and_cas_keeps_secret_on_metadata_update() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[46; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[19; 32]).unwrap();
    let input = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "https://identity.example".into(),
        client_id: "trellis".into(),
        title: "Company".to_string().into(),
        scopes: vec!["openid".into(), "email".into()],
        role_claim: "/roles".into(),
        claim_freshness_seconds: 300.into(),
        client_secret: Some("upstream confidential credential".into()),
        expected_revision: 0.into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&admin, &input);
    let created = put_provider(&storage, &identity, &input, NOW).unwrap();
    assert_eq!(
        put_provider(&storage, &identity, &input, NOW + 1).unwrap(),
        created
    );
    assert!(created.provider.has_client_secret);
    let update = AuthOIDCProviderPutRequest {
        provider_id: Some(created.provider.provider_id),
        title: "Updated company".to_string().into(),
        expected_revision: 1.into(),
        client_secret: None,
        idempotency_key: id().into(),
        ..input
    };
    let updated = put_provider(&storage, &mutation(&admin, &update), &update, NOW + 1).unwrap();
    assert!(updated.provider.has_client_secret);
    assert_eq!(updated.provider.title, "Updated company");
    let stale = AuthOIDCProviderPutRequest {
        title: "Stale company".to_string().into(),
        idempotency_key: id().into(),
        ..update
    };
    assert!(matches!(
        put_provider(&storage, &mutation(&admin, &stale), &stale, NOW + 2),
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        store
            .transaction(|connection| read_provider(
                connection,
                &created.provider.provider_id.to_string()
            ))
            .unwrap(),
        updated.provider
    );
    let exposed: bool = store
        .transaction(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_security_audit WHERE instr(data_json,?1)>0)",
                ["upstream confidential credential"],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!(!exposed);
}

#[test]
fn identity_unlink_uses_persistent_owner_scoped_id_and_preserves_last_sign_in() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[47; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[20; 32]).unwrap();
    let provider = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "https://identity.example".into(),
        client_id: "trellis".into(),
        title: "Company".to_string().into(),
        scopes: vec!["openid".into()],
        role_claim: "/roles".into(),
        claim_freshness_seconds: 300.into(),
        client_secret: None,
        expected_revision: 0.into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let provider = put_provider(&storage, &mutation(&admin, &provider), &provider, NOW)
        .unwrap()
        .provider
        .provider_id
        .to_string();
    let account = AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "alice".to_string().into(),
        display_name: "Alice".to_string().into(),
        password: None,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let alice = create_user(&store, &mutation(&admin, &account), &account, NOW)
        .unwrap()
        .user
        .principal_id
        .to_string();
    for (principal, subject) in [(&alice, "alice-at-idp"), (&admin, "admin-at-idp")] {
        store
            .transaction(|connection| {
                SqliteAuthorizationStore::link_oidc_identity_in(
                    connection, principal, &provider, subject, NOW,
                )
            })
            .unwrap();
    }
    let page = AuthPageRequest {
        page: None,
        search: None,
        extra: Default::default(),
    };
    let listed = store
        .transaction(|connection| read_identities(connection, &alice, &page))
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    let unlink = AuthUserIdentityUnlinkRequest {
        identity_id: listed.items[0].identity_id,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        unlink_identity(&store, &mutation(&admin, &unlink), &unlink, NOW + 1),
        Err(AuthError::NotFound)
    ));
    assert!(matches!(
        unlink_identity(&store, &mutation(&alice, &unlink), &unlink, NOW + 1),
        Err(AuthError::Denied)
    ));
    let second = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "https://other-identity.example".into(),
        client_id: "trellis".into(),
        title: "Other company".to_string().into(),
        scopes: vec!["openid".into()],
        role_claim: "/roles".into(),
        claim_freshness_seconds: 300.into(),
        client_secret: None,
        expected_revision: 0.into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let second = put_provider(&storage, &mutation(&admin, &second), &second, NOW)
        .unwrap()
        .provider
        .provider_id
        .to_string();
    store
        .transaction(|connection| {
            SqliteAuthorizationStore::link_oidc_identity_in(
                connection,
                &alice,
                &second,
                "alice-other",
                NOW,
            )
        })
        .unwrap();
    unlink_identity(&store, &mutation(&alice, &unlink), &unlink, NOW + 1).unwrap();
    let remaining = store
        .transaction(|connection| read_identities(connection, &alice, &page))
        .unwrap();
    assert_eq!(remaining.items.len(), 1);
    assert_eq!(remaining.items[0].provider_id.to_string(), second);
    let admin_identity = store
        .transaction(|connection| read_identities(connection, &admin, &page))
        .unwrap()
        .items
        .remove(0);
    let protected = AuthUserIdentityUnlinkRequest {
        identity_id: admin_identity.identity_id,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        unlink_identity(&store, &mutation(&admin, &protected), &protected, NOW + 2),
        Err(AuthError::Denied)
    ));
}

#[test]
fn account_reset_is_one_use_expires_and_revokes_existing_real_local_logins() {
    use crate::platform::auth::oauth::account::{complete_reset, create_reset};
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[48; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[21; 32]).unwrap();
    let session = reset_session(&store, &admin, &admin, "admin", "administrator password");
    let account = AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "alice".to_string().into(),
        display_name: "Alice".to_string().into(),
        password: Some("initial alice password".to_string().into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let alice = create_user(&store, &mutation(&admin, &account), &account, NOW)
        .unwrap()
        .user
        .principal_id;
    let cookie = store.create_portal_session(NOW).unwrap();
    store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"g".repeat(43),
            ("alice", "initial alice password"),
            &id(),
            NOW,
        )
        .unwrap();
    let request = AuthPasswordResetCreateRequest {
        user_id: alice,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&admin, &request);
    let reset = create_reset(&storage, &identity, &request, &session, NOW).unwrap();
    assert_eq!(
        create_reset(&storage, &identity, &request, &session, NOW + 1).unwrap(),
        reset
    );
    let guest = store.create_portal_session(NOW + 1).unwrap();
    let replacement = hash_password("replacement alice password").unwrap();
    assert!(matches!(
        complete_reset(
            &storage,
            &reset.token,
            &guest.cookie,
            "wrong csrf",
            &replacement,
            NOW + 1
        ),
        Err(AuthError::Denied)
    ));
    complete_reset(
        &storage,
        &reset.token,
        &guest.cookie,
        &guest.csrf,
        &replacement,
        NOW + 1,
    )
    .unwrap();
    assert!(matches!(
        complete_reset(
            &storage,
            &reset.token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            NOW + 2
        ),
        Err(AuthError::Denied)
    ));
    assert!(store
        .portal_login(&"g".repeat(43), &cookie.csrf, NOW + 2)
        .is_err());
    let guest = store.create_portal_session(NOW + 2).unwrap();
    assert!(store
        .portal_local_login(
            &guest.cookie,
            &guest.csrf,
            &"h".repeat(43),
            ("alice", "replacement alice password"),
            &id(),
            NOW + 2
        )
        .is_ok());
    let request = AuthPasswordResetCreateRequest {
        idempotency_key: id().into(),
        ..request
    };
    let expired = create_reset(
        &storage,
        &mutation(&admin, &request),
        &request,
        &session,
        NOW + 2,
    )
    .unwrap();
    let guest = store.create_portal_session(NOW + 303).unwrap();
    assert!(matches!(
        complete_reset(
            &storage,
            &expired.token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            NOW + 303
        ),
        Err(AuthError::Denied)
    ));
    let protected = AuthPasswordResetCreateRequest {
        user_id: admin.parse().unwrap(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        create_reset(
            &storage,
            &mutation(&admin, &protected),
            &protected,
            &session,
            NOW
        ),
        Err(AuthError::Denied)
    ));
}

#[test]
fn reset_redemption_rechecks_requester_privileges_and_conflicts_roll_back_consumption() {
    use crate::platform::auth::oauth::account::{complete_reset, create_reset};
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[49; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[22; 32]).unwrap();
    let mut principals = vec![];
    for username in ["operator", "alice"] {
        let input = AuthUserCreateRequest {
            given_name: None,
            family_name: None,
            email: None,
            username: username.to_string().into(),
            display_name: username.to_string().into(),
            password: Some("initial account password".to_string().into()),
            idempotency_key: id().into(),
            extra: Default::default(),
        };
        principals.push(
            create_user(&store, &mutation(&admin, &input), &input, NOW)
                .unwrap()
                .user
                .principal_id
                .to_string(),
        );
    }
    let operator = &principals[0];
    let alice = &principals[1];
    let grant = PolicyMutation::CapabilityGrant {
        principal_id: operator.clone(),
        capability_id: Administration::ManagePrincipals.capability_id().into(),
        expected_revision: 0,
        expires_at: None,
    };
    crate::platform::auth::tests::apply_policy(&store, &mutation(&admin, &grant), &grant, NOW)
        .unwrap();
    let session = reset_session(
        &store,
        &admin,
        operator,
        "operator",
        "initial account password",
    );
    let request = AuthPasswordResetCreateRequest {
        user_id: alice.parse().unwrap(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let reset = create_reset(
        &storage,
        &mutation(operator, &request),
        &request,
        &session,
        NOW,
    )
    .unwrap();
    let revoke = PolicyMutation::CapabilityRevoke {
        principal_id: operator.clone(),
        capability_id: Administration::ManagePrincipals.capability_id().into(),
        expected_revision: 1,
    };
    crate::platform::auth::tests::apply_policy(
        &store,
        &mutation(&admin, &revoke),
        &revoke,
        NOW + 1,
    )
    .unwrap();
    let guest = store.create_portal_session(NOW + 1).unwrap();
    let replacement = hash_password("replacement account password").unwrap();
    assert!(matches!(
        complete_reset(
            &storage,
            &reset.token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            NOW + 1
        ),
        Err(AuthError::Denied)
    ));
    let login_cookie = store.create_portal_session(NOW + 1).unwrap();
    assert!(store
        .portal_local_login(
            &login_cookie.cookie,
            &login_cookie.csrf,
            &"i".repeat(43),
            ("alice", "initial account password"),
            &id(),
            NOW + 1
        )
        .is_ok());
    crate::platform::auth::tests::apply_policy(&store, &mutation(&admin, &grant), &grant, NOW + 2)
        .unwrap();
    complete_reset(
        &storage,
        &reset.token,
        &guest.cookie,
        &guest.csrf,
        &replacement,
        NOW + 2,
    )
    .unwrap();
    let request = AuthPasswordResetCreateRequest {
        idempotency_key: id().into(),
        ..request
    };
    let older = create_reset(
        &storage,
        &mutation(operator, &request),
        &request,
        &session,
        NOW + 3,
    )
    .unwrap();
    let newer_request = AuthPasswordResetCreateRequest {
        idempotency_key: id().into(),
        ..request
    };
    let newer = create_reset(
        &storage,
        &mutation(operator, &newer_request),
        &newer_request,
        &session,
        NOW + 3,
    )
    .unwrap();
    complete_reset(
        &storage,
        &newer.token,
        &guest.cookie,
        &guest.csrf,
        &replacement,
        NOW + 4,
    )
    .unwrap();
    assert!(matches!(
        complete_reset(
            &storage,
            &older.token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            NOW + 4
        ),
        Err(AuthError::Conflict)
    ));
    let revoked_request = AuthPasswordResetCreateRequest {
        user_id: alice.parse().unwrap(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let revoked = create_reset(
        &storage,
        &mutation(operator, &revoked_request),
        &revoked_request,
        &session,
        NOW + 4,
    )
    .unwrap();
    let grant: String = store
        .transaction(|connection| {
            Ok(connection.query_row(
                "SELECT oauth_grant_id \
                FROM auth_authorization_sessions \
                WHERE authorization_session_id=?1",
                [&session],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let root = EnforcementScope::Grant(grant);
    crate::platform::auth::tests::revoke_root(&store, &mutation(&admin, &root), &root, NOW + 5)
        .unwrap();
    assert!(matches!(
        complete_reset(
            &storage,
            &revoked.token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            NOW + 5
        ),
        Err(AuthError::Denied)
    ));
}

#[test]
fn competing_reset_redemptions_commit_exactly_one_password() {
    use crate::platform::auth::oauth::account::{complete_reset, create_reset};
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[50; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage =
        Arc::new(crate::platform::auth::OAuthStorage::new(store.clone(), &[23; 32]).unwrap());
    let session = reset_session(&store, &admin, &admin, "admin", "administrator password");
    let account = AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "alice".to_string().into(),
        display_name: "Alice".to_string().into(),
        password: None,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let alice = create_user(&store, &mutation(&admin, &account), &account, NOW)
        .unwrap()
        .user
        .principal_id;
    let request = AuthPasswordResetCreateRequest {
        user_id: alice,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let reset = create_reset(
        &storage,
        &mutation(&admin, &request),
        &request,
        &session,
        NOW,
    )
    .unwrap();
    let guest = store.create_portal_session(NOW).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let threads = ["first reset password", "second reset password"].map(|password| {
        let storage = Arc::clone(&storage);
        let barrier = Arc::clone(&barrier);
        let token = reset.token.clone();
        let cookie = guest.cookie.clone();
        let csrf = guest.csrf.clone();
        std::thread::spawn(move || {
            let replacement = hash_password(password).unwrap();
            barrier.wait();
            (
                password,
                complete_reset(&storage, &token, &cookie, &csrf, &replacement, NOW + 1),
            )
        })
    });
    let results = threads.map(|thread| thread.join().unwrap());
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    let winner = results.iter().find(|(_, result)| result.is_ok()).unwrap().0;
    let cookie = store.create_portal_session(NOW + 2).unwrap();
    assert!(store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"j".repeat(43),
            ("alice", winner),
            &id(),
            NOW + 2
        )
        .is_ok());
}
