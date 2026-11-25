use super::*;
use crate::platform::auth::oauth::oidc::IssuerPin;
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
    let manage = Administration::ManageAccounts.capability_id().to_string();
    approval.selection.required = vec![manage.clone()];
    approval.selection.optional.clear();
    approval.approved = vec![manage];
    approval.administration_confirmed = true;
    approval.remember = false;
    let grant = store
        .approve_grant(&mutation(actor, &approval), &approval, NOW)
        .unwrap();
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
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: "Alice".to_string().into(),
        display_name: "Alice Example".to_string().into(),
        password: Some("initial alice password".to_string().into()),
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        ..update
    };
    // The refusal says what changed, so the caller knows to reload.
    assert!(matches!(
        update_user(&store, &mutation(&admin, &stale), &stale, NOW + 2),
        Err(AuthError::Conflict(message))
            if message.contains("changed since revision")
                && message.ends_with("reload and try again")
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        issuer_certificate: None,
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        ..input
    };
    assert!(matches!(
        delete_provider(&store, &mutation(&admin, &stale), &stale, NOW + 2),
        Err(AuthError::Conflict(_))
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
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: "Reserved".to_string().into(),
        display_name: "Pending account".to_string().into(),
        password: None,
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let created = create_user(&store, &mutation(&admin, &input), &input, NOW).unwrap();
    let account = created.user.account.unwrap();
    assert_eq!(account.username, "reserved");
    assert!(!account.has_local_password);
    let duplicate = AuthUserCreateRequest {
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: "RESERVED".to_string().into(),
        password: Some("a usable password".to_string().into()),
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        ..input
    };
    assert!(matches!(
        create_user(&store, &mutation(&admin, &duplicate), &duplicate, NOW + 1),
        Err(AuthError::Conflict(message)) if message == "username reserved is already taken"
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
                idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        Err(AuthError::Conflict(_) | AuthError::Denied)
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
        certificate_fingerprint: None,
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 0.into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let identity = mutation(&admin, &input);
    let created = put_provider(&storage, &identity, &input, IssuerPin::Keep, NOW).unwrap();
    assert_eq!(
        put_provider(&storage, &identity, &input, IssuerPin::Keep, NOW + 1).unwrap(),
        created
    );
    assert!(created.provider.has_client_secret);
    let update = AuthOIDCProviderPutRequest {
        provider_id: Some(created.provider.provider_id),
        title: "Updated company".to_string().into(),
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 1.into(),
        client_secret: None,
        certificate_fingerprint: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        ..input
    };
    let updated = put_provider(
        &storage,
        &mutation(&admin, &update),
        &update,
        IssuerPin::Keep,
        NOW + 1,
    )
    .unwrap();
    assert!(updated.provider.has_client_secret);
    assert_eq!(updated.provider.title, "Updated company");
    let stale = AuthOIDCProviderPutRequest {
        title: "Stale company".to_string().into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        ..update
    };
    assert!(matches!(
        put_provider(
            &storage,
            &mutation(&admin, &stale),
            &stale,
            IssuerPin::Keep,
            NOW + 2
        ),
        Err(AuthError::Conflict(_))
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

#[tokio::test]
async fn provider_put_names_the_issuer_when_it_is_not_an_https_url() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[49; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[19; 32]).unwrap();
    let bare = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "identity.example".into(),
        client_id: "trellis".into(),
        title: "Company".to_string().into(),
        scopes: vec!["openid".into()],
        role_claim: String::new(),
        claim_freshness_seconds: 300.into(),
        client_secret: None,
        certificate_fingerprint: None,
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 0.into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    // Through the whole Put, so the issuer's certificate check cannot replace
    // the explanation with a vaguer one.
    match put_oidc_provider(storage.clone(), mutation(&admin, &bare), bare.clone(), NOW).await {
        Err(AuthError::Invalid(detail)) => assert!(detail.contains("https URL"), "{detail}"),
        other => panic!("expected an issuer validation error, got {other:?}"),
    }
    let fixed = AuthOIDCProviderPutRequest {
        issuer: "https://identity.example/".into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        ..bare
    };
    let created = put_provider(
        &storage,
        &mutation(&admin, &fixed),
        &fixed,
        IssuerPin::Keep,
        NOW,
    )
    .unwrap();
    assert_eq!(created.provider.issuer, "https://identity.example/");
}

/// A custom authorization parameter must not replace one Auth sets itself:
/// a second `redirect_uri` or `state` could send the code elsewhere or defeat
/// the CSRF binding, depending on which copy the provider reads. A
/// `response_mode` of `form_post` or `fragment` would deliver the code where
/// the GET-only callback cannot read it.
#[test]
fn provider_put_refuses_custom_authorization_parameters_trellis_sets_itself() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[52; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[19; 32]).unwrap();
    let put = |parameters: &[(&str, &str)]| {
        let input = AuthOIDCProviderPutRequest {
            provider_id: None,
            issuer: "https://identity.example/".into(),
            client_id: "trellis".into(),
            title: "Company".to_string().into(),
            scopes: vec!["openid".into()],
            role_claim: String::new(),
            claim_freshness_seconds: 300.into(),
            client_secret: None,
            certificate_fingerprint: None,
            prompt_after_sign_out: None,
            authorization_parameters: Some(
                parameters
                    .iter()
                    .map(|(name, value)| {
                        trellis_runtime_apis::__types::trellis::AuthOIDCAuthorizationParameter {
                            name: (*name).into(),
                            value: (*value).into(),
                            extra: Default::default(),
                        }
                    })
                    .collect(),
            ),
            expected_revision: 0.into(),
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
            extra: Default::default(),
        };
        put_provider(
            &storage,
            &mutation(&admin, &input),
            &input,
            IssuerPin::Keep,
            NOW,
        )
    };
    for (parameters, expected) in [
        (
            vec![("redirect_uri", "https://attacker.example/")],
            "set by Trellis",
        ),
        (vec![("state", "fixed")], "set by Trellis"),
        (vec![("response_mode", "form_post")], "callback query"),
        (
            vec![("organization", "org_1"), ("organization", "org_2")],
            "more than once",
        ),
        (vec![("org id", "org_1")], "letters, digits"),
    ] {
        match put(&parameters) {
            Err(AuthError::Invalid(detail)) => assert!(detail.contains(expected), "{detail}"),
            other => panic!("{parameters:?} was not refused: {other:?}"),
        }
    }
    let created = put(&[("organization", "org_123")]).unwrap();
    assert_eq!(created.provider.authorization_parameters.len(), 1);
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
        certificate_fingerprint: None,
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 0.into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let provider = put_provider(
        &storage,
        &mutation(&admin, &provider),
        &provider,
        IssuerPin::Keep,
        NOW,
    )
    .unwrap()
    .provider
    .provider_id
    .to_string();
    let account = AuthUserCreateRequest {
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: "alice".to_string().into(),
        display_name: "Alice".to_string().into(),
        password: None,
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        unlink_identity(&store, &mutation(&admin, &unlink), &unlink, NOW + 1),
        Err(AuthError::NotFound(_))
    ));
    // Her only way to sign in: refused, saying so.
    match unlink_identity(&store, &mutation(&alice, &unlink), &unlink, NOW + 1) {
        Err(AuthError::Invalid(detail)) => {
            assert!(detail.contains("only way to sign in"), "{detail}")
        }
        other => panic!("expected the last sign-in to be kept, got {other:?}"),
    }
    let second = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "https://other-identity.example".into(),
        client_id: "trellis".into(),
        title: "Other company".to_string().into(),
        scopes: vec!["openid".into()],
        role_claim: "/roles".into(),
        claim_freshness_seconds: 300.into(),
        client_secret: None,
        certificate_fingerprint: None,
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 0.into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let second = put_provider(
        &storage,
        &mutation(&admin, &second),
        &second,
        IssuerPin::Keep,
        NOW,
    )
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    match unlink_identity(&store, &mutation(&admin, &protected), &protected, NOW + 2) {
        Err(AuthError::Invalid(detail)) => {
            assert!(detail.contains("default administrator"), "{detail}")
        }
        other => panic!("expected the default administrator's sign-in to be kept, got {other:?}"),
    }
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
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: "alice".to_string().into(),
        display_name: "Alice".to_string().into(),
        password: Some("initial alice password".to_string().into()),
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let identity = mutation(&admin, &request);
    let reset = create_reset(&storage, &identity, &request, &session, NOW).unwrap();
    assert_eq!(
        create_reset(&storage, &identity, &request, &session, NOW + 1).unwrap(),
        reset
    );
    let guest = store.create_portal_session(NOW + 1).unwrap();
    let replacement = hash_password("replacement alice password", 12).unwrap();
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
    // The link lasts a day.
    let guest = store.create_portal_session(NOW + 2 + 86_400).unwrap();
    assert!(matches!(
        complete_reset(
            &storage,
            &expired.token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            NOW + 2 + 86_400
        ),
        Err(AuthError::Denied)
    ));
    let protected = AuthPasswordResetCreateRequest {
        user_id: admin.parse().unwrap(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
            user_id: None,
            given_name: None,
            family_name: None,
            email: None,
            username: username.to_string().into(),
            display_name: username.to_string().into(),
            password: Some("initial account password".to_string().into()),
            identities: None,
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        capability_id: Administration::ManageAccounts.capability_id().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        capability_id: Administration::ManageAccounts.capability_id().into(),
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
    let replacement = hash_password("replacement account password", 12).unwrap();
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        Err(AuthError::Conflict(_))
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
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: "alice".to_string().into(),
        display_name: "Alice".to_string().into(),
        password: None,
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let alice = create_user(&store, &mutation(&admin, &account), &account, NOW)
        .unwrap()
        .user
        .principal_id;
    let request = AuthPasswordResetCreateRequest {
        user_id: alice,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
            let replacement = hash_password(password, 12).unwrap();
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

fn sign_in_settings(
    store: &SqliteAuthorizationStore,
    admin: &str,
    revision: u64,
    local_users: bool,
    password_min_length: u32,
) -> Result<AuthSignInSettingsGetResponse, AuthError> {
    let input = AuthSignInSettingsUpdateRequest {
        local_users,
        local_sign_up: local_users,
        password_min_length: AuthPasswordMinLength(password_min_length),
        expected_revision: AuthRevision(revision.into()),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    crate::platform::auth::sign_in::update(store, &mutation(admin, &input), &input, NOW)
}

#[test]
fn the_sign_in_settings_minimum_governs_created_changed_and_signed_up_passwords() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[45; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("an administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    // 19 and 20 bytes, either side of the minimum set below.
    let (short, exact) = ("nineteen characters", "twenty characters ok");
    let account = |username: &str, password: &str| AuthUserCreateRequest {
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: username.to_string().into(),
        display_name: "Configured".to_string().into(),
        password: Some(password.to_string().into()),
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    // The default minimum of 12 lets the shorter password in.
    let before = account("before", short);
    let creation = mutation(&admin, &before);
    let original = create_user(&store, &creation, &before, NOW).unwrap();
    let saved = sign_in_settings(&store, &admin, 1, true, 20).unwrap();
    assert_eq!(saved.settings.password_min_length.0, 20);
    // A retry of the completed creation, say after a lost response, gets its
    // recorded result even though the password no longer meets the minimum.
    let retried = create_user(&store, &creation, &before, NOW + 1).unwrap();
    assert_eq!(retried.user.principal_id, original.user.principal_id);

    let refused = account("short", short);
    assert!(matches!(
        create_user(&store, &mutation(&admin, &refused), &refused, NOW),
        Err(AuthError::Invalid(_))
    ));
    let created = account("exact", exact);
    let exact_id = create_user(&store, &mutation(&admin, &created), &created, NOW)
        .unwrap()
        .user
        .principal_id
        .to_string();

    // Existing passwords still sign in; only new ones must meet the minimum.
    let sign_in = |username: &str, password: &str| {
        let cookie = store.create_portal_session(NOW).unwrap();
        store.portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &format!("{}{}", id(), "e".repeat(17)),
            (username, password),
            &id(),
            NOW,
        )
    };
    sign_in("before", short).unwrap();
    let login = sign_in("exact", exact).unwrap();
    let change = |new_password: &str| {
        let input = AuthPasswordChangeRequest {
            current_password: exact.to_string().into(),
            new_password: new_password.to_string().into(),
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
            extra: Default::default(),
        };
        change_password(
            &store,
            &mutation(&exact_id, &input),
            &input,
            &login.login_session_id,
            NOW,
        )
    };
    assert!(matches!(change(short), Err(AuthError::Invalid(_))));
    change("another twenty chars").unwrap();

    let sign_up = |username: &str, password: &str| {
        let guest = store.create_portal_session(NOW).unwrap();
        store.portal_local_sign_up(
            &guest.cookie,
            &guest.csrf,
            &crate::platform::auth::oauth::LocalSignUp {
                username: username.into(),
                password: password.into(),
                name: "Newcomer".into(),
                email: String::new(),
            },
            &ulid::Ulid::generate().to_string(),
            NOW,
        )
    };
    assert!(matches!(
        sign_up("newcomer", short),
        Err(AuthError::Invalid(_))
    ));
    sign_up("newcomer", exact).unwrap();
    sign_in("newcomer", exact).unwrap();
}

#[test]
fn the_password_minimum_counts_characters_not_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[47; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("an administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    sign_in_settings(&store, &admin, 1, true, 8).unwrap();
    // Eight characters in 16 bytes meet the minimum; seven in 28 bytes do not.
    let enough = "é".repeat(8);
    let short = "🔑".repeat(7);
    let account = |username: &str, password: &str| AuthUserCreateRequest {
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: username.to_string().into(),
        display_name: "Multibyte".to_string().into(),
        password: Some(password.to_string().into()),
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let refused = account("short", &short);
    assert!(matches!(
        create_user(&store, &mutation(&admin, &refused), &refused, NOW),
        Err(AuthError::Invalid(_))
    ));
    let created = account("created", &enough);
    create_user(&store, &mutation(&admin, &created), &created, NOW).unwrap();

    let sign_up = |username: &str, password: &str| {
        let guest = store.create_portal_session(NOW).unwrap();
        store.portal_local_sign_up(
            &guest.cookie,
            &guest.csrf,
            &crate::platform::auth::oauth::LocalSignUp {
                username: username.into(),
                password: password.into(),
                name: "Newcomer".into(),
                email: String::new(),
            },
            &ulid::Ulid::generate().to_string(),
            NOW,
        )
    };
    assert!(matches!(
        sign_up("newcomer", &short),
        Err(AuthError::Invalid(_))
    ));
    sign_up("newcomer", &enough).unwrap();

    // Both accounts sign in with the password they were given.
    for username in ["created", "newcomer"] {
        let cookie = store.create_portal_session(NOW).unwrap();
        store
            .portal_local_login(
                &cookie.cookie,
                &cookie.csrf,
                &format!("{}{}", id(), "e".repeat(17)),
                (username, &enough),
                &id(),
                NOW,
            )
            .unwrap();
    }
}

#[test]
fn sign_in_settings_and_providers_always_leave_a_way_to_sign_in() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[46; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("an administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let sign_in = || {
        let cookie = store.create_portal_session(NOW).unwrap();
        store.portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &format!("{}{}", id(), "e".repeat(17)),
            ("admin", "an administrator password"),
            &id(),
            NOW,
        )
    };
    // With no provider, username and password is the only way in.
    assert!(matches!(
        sign_in_settings(&store, &admin, 1, false, 12),
        Err(AuthError::Invalid(_))
    ));
    sign_in().unwrap();

    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[21; 32]).unwrap();
    let company = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "https://identity.example".into(),
        client_id: "trellis".into(),
        title: "Company".to_string().into(),
        scopes: vec!["openid".into()],
        role_claim: "/groups".into(),
        claim_freshness_seconds: 300.into(),
        client_secret: Some("upstream confidential credential".into()),
        certificate_fingerprint: None,
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 0.into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let company = put_provider(
        &storage,
        &mutation(&admin, &company),
        &company,
        IssuerPin::Keep,
        NOW,
    )
    .unwrap()
    .provider;
    let change = |revision: AuthRevision| AuthOIDCProviderMutationRequest {
        provider_id: company.provider_id,
        expected_revision: revision,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    // A provider that is off is no way in either.
    let off = change(company.revision);
    let off = disable_provider(&store, &mutation(&admin, &off), &off, NOW).unwrap();
    assert!(matches!(
        sign_in_settings(&store, &admin, 1, false, 12),
        Err(AuthError::Invalid(_))
    ));
    let on = change(off.revision);
    let on = enable_provider(&store, &mutation(&admin, &on), &on, NOW).unwrap();
    // With a provider on, turning local users off applies to the next
    // sign-in, with no restart.
    sign_in_settings(&store, &admin, 1, false, 12).unwrap();
    assert!(matches!(sign_in(), Err(AuthError::Denied)));
    // The last provider that is on can then neither be turned off nor removed.
    let last = change(on.revision);
    assert!(matches!(
        disable_provider(&store, &mutation(&admin, &last), &last, NOW),
        Err(AuthError::Invalid(_))
    ));
    let last = change(on.revision);
    assert!(matches!(
        delete_provider(&store, &mutation(&admin, &last), &last, NOW),
        Err(AuthError::Invalid(_))
    ));
    // Once local users are back, it can go.
    sign_in_settings(&store, &admin, 2, true, 12).unwrap();
    sign_in().unwrap();
    let last = change(on.revision);
    disable_provider(&store, &mutation(&admin, &last), &last, NOW).unwrap();
}

#[test]
fn account_managers_find_reset_disable_and_delete_any_account_but_the_default_administrator() {
    use crate::platform::auth::oauth::account::create_reset;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[53; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[23; 32]).unwrap();
    let create = |actor: &str, username: &str| {
        let input = AuthUserCreateRequest {
            user_id: None,
            given_name: None,
            family_name: None,
            email: None,
            username: username.to_string().into(),
            display_name: username.to_string().into(),
            password: Some("initial account password".to_string().into()),
            identities: None,
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
            extra: Default::default(),
        };
        create_user(&store, &mutation(actor, &input), &input, NOW).map(|created| created.user)
    };
    let manager = create(&admin, "manager").unwrap().principal_id.to_string();
    let boss = create(&admin, "boss").unwrap().principal_id.to_string();
    for (principal, administration) in [
        (&manager, Administration::ManageAccounts),
        (&boss, Administration::ManageRoles),
    ] {
        let grant = PolicyMutation::CapabilityGrant {
            principal_id: principal.clone(),
            capability_id: administration.capability_id().into(),
            expected_revision: 0,
            expires_at: None,
        };
        crate::platform::auth::tests::apply_policy(&store, &mutation(&admin, &grant), &grant, NOW)
            .unwrap();
    }
    let provider = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "https://identity.example".into(),
        client_id: "trellis".into(),
        title: "Company".to_string().into(),
        scopes: vec!["openid".into()],
        role_claim: "/roles".into(),
        claim_freshness_seconds: 300.into(),
        client_secret: None,
        certificate_fingerprint: None,
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 0.into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let provider = put_provider(
        &storage,
        &mutation(&admin, &provider),
        &provider,
        IssuerPin::Keep,
        NOW,
    )
    .unwrap()
    .provider
    .provider_id;
    for (principal, subject) in [(&boss, "auth0|boss"), (&admin, "auth0|admin")] {
        store
            .transaction(|connection| {
                SqliteAuthorizationStore::link_oidc_identity_in(
                    connection,
                    principal,
                    &provider.to_string(),
                    subject,
                    NOW,
                )
            })
            .unwrap();
    }
    let find = |user_id: Option<&str>, subject: Option<&str>| {
        let input = AuthUserFindRequest {
            user_id: user_id.map(|user| user.parse().unwrap()),
            identity: subject.map(|subject| AuthUserFindIdentity {
                provider_id: provider,
                subject: subject.into(),
                extra: Default::default(),
            }),
            extra: Default::default(),
        };
        store.transaction(|connection| find_user(&store, connection, &input))
    };
    let account = |user: &AuthUser| user.account.as_ref().unwrap().principal.revision;

    // The boss holds roles.manage, which the manager lacks. The manager still
    // finds the boss by sign-in, disables, resets and deletes the account.
    let found = find(None, Some("auth0|boss")).unwrap().found.unwrap();
    assert_eq!(found.user.principal_id.to_string(), boss);
    assert_eq!(found.identities.len(), 1);
    assert_eq!(found.identities[0].provider_id, provider);
    assert_eq!(found.identities[0].subject, "auth0|boss");
    for miss in ["AUTH0|BOSS", "auth0|bos", "boss"] {
        assert!(find(None, Some(miss)).unwrap().found.is_none(), "{miss}");
    }
    assert!(matches!(
        find(Some(&boss), Some("auth0|boss")),
        Err(AuthError::Invalid(_))
    ));
    assert!(matches!(find(None, None), Err(AuthError::Invalid(_))));
    let disable = |target: &AuthUser| AuthUserUpdateRequest {
        given_name: None,
        family_name: None,
        email: None,
        clear_email: None,
        user_id: target.principal_id,
        expected_revision: account(target),
        display_name: None,
        expected_profile_revision: None,
        enabled: false,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let input = disable(&found.user);
    update_user(&store, &mutation(&manager, &input), &input, NOW + 1).unwrap();
    let disabled = find(Some(&boss), None).unwrap().found.unwrap().user;
    let details = disabled.account.as_ref().unwrap();
    assert!(!details.principal.enabled);
    assert!(details.principal.revision > account(&found.user));
    let session = reset_session(
        &store,
        &admin,
        &manager,
        "manager",
        "initial account password",
    );
    let reset = |target: &AuthUser| {
        let request = AuthPasswordResetCreateRequest {
            user_id: target.principal_id,
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
            extra: Default::default(),
        };
        create_reset(
            &storage,
            &mutation(&manager, &request),
            &request,
            &session,
            NOW + 1,
        )
    };
    let alice = create(&manager, "alice").unwrap();
    assert!(reset(&alice).is_ok());
    let delete = |target: &AuthUser| AuthUserDeleteRequest {
        user_id: target.principal_id,
        expected_revision: account(target),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let input = delete(&disabled);
    delete_user(&store, &mutation(&manager, &input), &input, NOW + 2).unwrap();
    assert!(find(None, Some("auth0|boss")).unwrap().found.is_none());
    assert!(find(Some(&boss), None).unwrap().found.is_none());
    // A retry after a lost reply is a new request for an account that is gone.
    let retry = delete(&disabled);
    assert!(matches!(
        delete_user(&store, &mutation(&manager, &retry), &retry, NOW + 3),
        Err(AuthError::NotFound(_))
    ));
    // The sign-in is free: the person's next OIDC sign-in links a new account.
    let fresh = create(&admin, "boss-again")
        .unwrap()
        .principal_id
        .to_string();
    store
        .transaction(|connection| {
            SqliteAuthorizationStore::link_oidc_identity_in(
                connection,
                &fresh,
                &provider.to_string(),
                "auth0|boss",
                NOW + 3,
            )
        })
        .unwrap();
    let relinked = find(None, Some("auth0|boss")).unwrap().found.unwrap();
    assert_eq!(relinked.user.principal_id.to_string(), fresh);

    // Nobody but the default administrator acts on the default administrator.
    let admin_user = find(None, Some("auth0|admin")).unwrap().found.unwrap().user;
    let input = disable(&admin_user);
    assert!(matches!(
        update_user(&store, &mutation(&manager, &input), &input, NOW + 3),
        Err(AuthError::Denied)
    ));
    assert!(matches!(reset(&admin_user), Err(AuthError::Denied)));
    let input = delete(&admin_user);
    assert!(matches!(
        delete_user(&store, &mutation(&manager, &input), &input, NOW + 3),
        Err(AuthError::Denied)
    ));

    // A deleted local account can't sign in, and its username is free.
    let input = delete(&alice);
    delete_user(&store, &mutation(&manager, &input), &input, NOW + 4).unwrap();
    let cookie = store.create_portal_session(NOW + 4).unwrap();
    assert!(store
        .portal_local_login(
            &cookie.cookie,
            &cookie.csrf,
            &"c".repeat(43),
            ("alice", "initial account password"),
            &id(),
            NOW + 4,
        )
        .is_err());
    let again = create(&manager, "alice").unwrap();
    assert_ne!(again.principal_id, alice.principal_id);
}

#[test]
fn link_providers_lists_active_providers_by_name_and_drops_a_disabled_one() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[50; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[19; 32]).unwrap();
    let put = |issuer: &str, title: &str| {
        let input = AuthOIDCProviderPutRequest {
            provider_id: None,
            issuer: issuer.into(),
            client_id: "trellis".into(),
            title: title.to_string().into(),
            scopes: vec!["openid".into()],
            role_claim: String::new(),
            claim_freshness_seconds: 300.into(),
            client_secret: None,
            certificate_fingerprint: None,
            prompt_after_sign_out: None,
            authorization_parameters: None,
            expected_revision: 0.into(),
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
            extra: Default::default(),
        };
        put_provider(
            &storage,
            &mutation(&admin, &input),
            &input,
            IssuerPin::Keep,
            NOW,
        )
        .unwrap()
        .provider
    };
    let company = put("https://company.example/", "Company");
    let retired = put("https://retired.example/", "Retired");
    let disable = AuthOIDCProviderMutationRequest {
        provider_id: retired.provider_id,
        expected_revision: retired.revision,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    disable_provider(&store, &mutation(&admin, &disable), &disable, NOW + 1).unwrap();
    let listed = store.read(read_link_providers).unwrap().providers;
    assert_eq!(
        listed
            .iter()
            .map(|item| (item.provider_id, item.title.as_str()))
            .collect::<Vec<_>>(),
        [(company.provider_id, "Company")]
    );
}

/// Choosing which OIDC sign-ins open someone's account is account management.
/// A caller without accounts.manage, even one whose session was allowed the
/// action, is refused inside the transaction and nothing changes.
#[test]
fn linking_and_unlinking_another_persons_sign_ins_needs_accounts_manage() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[57; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let storage = crate::platform::auth::OAuthStorage::new(store.clone(), &[29; 32]).unwrap();
    let create = |username: &str| {
        let input = AuthUserCreateRequest {
            user_id: None,
            given_name: None,
            family_name: None,
            email: None,
            username: username.to_string().into(),
            display_name: username.to_string().into(),
            password: Some("initial account password".to_string().into()),
            identities: None,
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
            extra: Default::default(),
        };
        create_user(&store, &mutation(&admin, &input), &input, NOW)
            .unwrap()
            .user
    };
    let member = create("member").principal_id.to_string();
    let target = create("target");
    let provider = AuthOIDCProviderPutRequest {
        provider_id: None,
        issuer: "https://identity.example".into(),
        client_id: "trellis".into(),
        title: "Company".to_string().into(),
        scopes: vec!["openid".into()],
        role_claim: "/roles".into(),
        claim_freshness_seconds: 300.into(),
        client_secret: None,
        certificate_fingerprint: None,
        prompt_after_sign_out: None,
        authorization_parameters: None,
        expected_revision: 0.into(),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let provider_id = put_provider(
        &storage,
        &mutation(&admin, &provider),
        &provider,
        IssuerPin::Keep,
        NOW,
    )
    .unwrap()
    .provider
    .provider_id;
    let identity = AuthUserFindIdentity {
        provider_id,
        subject: "auth0|target".into(),
        extra: Default::default(),
    };
    let revision = target.account.as_ref().unwrap().principal.revision;
    let link = AuthUserIdentitiesLinkRequest {
        user_id: target.principal_id,
        identity: identity.clone(),
        expected_revision: revision,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let subjects = || {
        let input = AuthUserFindRequest {
            user_id: Some(target.principal_id),
            identity: None,
            extra: Default::default(),
        };
        store
            .transaction(|connection| find_user(&store, connection, &input))
            .unwrap()
            .found
            .unwrap()
            .identities
            .into_iter()
            .map(|identity| identity.subject)
            .collect::<Vec<_>>()
    };
    assert!(matches!(
        link_user_identity(&store, &mutation(&member, &link), &link, NOW + 1),
        Err(AuthError::Denied)
    ));
    assert!(subjects().is_empty());
    let linked = link_user_identity(&store, &mutation(&admin, &link), &link, NOW + 1).unwrap();
    let unlink = AuthUserIdentitiesUnlinkRequest {
        user_id: target.principal_id,
        identity,
        expected_revision: linked.user.account.unwrap().principal.revision,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        unlink_user_identity(&store, &mutation(&member, &unlink), &unlink, NOW + 2),
        Err(AuthError::Denied)
    ));
    assert_eq!(subjects(), ["auth0|target"]);
    unlink_user_identity(&store, &mutation(&admin, &unlink), &unlink, NOW + 2).unwrap();
    assert!(subjects().is_empty());
}

/// A creation recorded before Users.Create took `identities` digested only the
/// request. Retrying it with the same key and request, say after a lost
/// response across an upgrade, gets the original account; the same key with
/// sign-ins added is a different request and conflicts.
#[test]
fn a_creation_recorded_before_linked_sign_ins_replays_its_result() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[58; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", Some("administrator password"), None, None, NOW)
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let input = AuthUserCreateRequest {
        user_id: None,
        given_name: None,
        family_name: None,
        email: None,
        username: "earlier".to_string().into(),
        display_name: "Earlier Account".to_string().into(),
        password: None,
        identities: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let identity = mutation(&admin, &input);
    // Record the creation the way the release before linked sign-ins did.
    let earlier = serde_json::json!({"requestDigest": identity.request_digest});
    let recorded = Mutation {
        request_digest: digest(&earlier).unwrap(),
        ..identity.clone()
    };
    let original = store
        .mutate(&recorded, "rpc.users.create", &earlier, NOW, |connection| {
            let principal = id();
            insert_user(connection, &principal, NOW)?;
            insert_local_account(
                connection,
                &principal,
                "earlier",
                None,
                &AuthUserProfileUpdateRequest {
                    display_name: Some(input.display_name.clone()),
                    given_name: None,
                    family_name: None,
                    email: None,
                    clear_email: None,
                    picture_provider_id: None,
                    use_gravatar: None,
                    expected_revision: AuthRevision(1u64.into()),
                    idempotency_key: input.idempotency_key.clone(),
                    extra: Default::default(),
                },
                NOW,
            )?;
            Ok(AuthUserGetResponse {
                user: read_user(&store, connection, &principal, true)?,
                extra: Default::default(),
            })
        })
        .unwrap();

    assert_eq!(
        create_user(&store, &identity, &input, NOW + 1).unwrap(),
        original
    );
    let linked = AuthUserCreateRequest {
        user_id: None,
        identities: Some(vec![AuthUserFindIdentity {
            provider_id: id().parse().unwrap(),
            subject: "auth0|earlier".into(),
            extra: Default::default(),
        }]),
        ..input
    };
    let changed = Mutation {
        request_digest: digest(&linked).unwrap(),
        ..identity
    };
    assert!(matches!(
        create_user(&store, &changed, &linked, NOW + 1),
        Err(AuthError::Conflict(message))
            if message.contains("was already used for a different request")
    ));
}
