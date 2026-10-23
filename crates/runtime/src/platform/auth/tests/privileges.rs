use super::*;

#[test]
fn registered_development_privileges_require_eligibility_user_authority_and_consent() {
    use trellis_protocol::PlatformPrivilege::*;
    let f = fixture();
    let privileges = vec![
        PrincipalsManage,
        RolesManage,
        ApisAccept,
        ApisForceReplace,
        ClientsManage,
        PrivilegesManage,
    ];
    let mut client = ClientRegistration {
        client_id: "development-console".into(),
        kind: "browser".into(),
        display_name: "Development Console".into(),
        redirect_uris: vec!["http://localhost:5173/callback".into()],
        development: true,
        implied_capabilities: vec![],
        eligible_privileges: privileges.clone(),
        metadata: serde_json::json!({}),
        expected_revision: 0,
    };
    assert!(matches!(
        f.store
            .transaction(|connection| f.store.register_client_in_transaction(
                connection,
                &mutation(&f.principal, &client),
                &client,
                NOW
            )),
        Err(AuthError::Denied)
    ));
    let client_manager = create_user(&f.store, &f.admin, NOW);
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::PrivilegeAssign {
            principal_id: client_manager.clone(),
            privileges: vec![ClientsManage],
            expected_revision: 0,
        },
        NOW,
    );
    // Client management alone cannot assign administrative eligibility.
    assert!(matches!(
        f.store
            .transaction(|connection| f.store.register_client_in_transaction(
                connection,
                &mutation(&client_manager, &client),
                &client,
                NOW
            )),
        Err(AuthError::Denied)
    ));
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &client),
                &client,
                NOW,
            )
        })
        .unwrap();
    client.client_id = "ordinary-development-client".into();
    client.eligible_privileges.clear();
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&client_manager, &client),
                &client,
                NOW,
            )
        })
        .unwrap();

    for (principal, client_id, consent, allowed) in [
        (&f.admin, "development-console", true, true),
        (&f.admin, "development-console", false, true),
        (&f.admin, "ordinary-development-client", true, false),
        (&f.admin, "ordinary-development-client", false, true),
        (&f.principal, "development-console", true, false),
    ] {
        let login = f
            .store
            .transaction(|connection| {
                SqliteAuthorizationStore::record_verified_login_in(
                    connection,
                    &VerifiedLogin {
                        principal_id: principal.clone(),
                        provider_id: None,
                        upstream_issuer: None,
                        upstream_subject: None,
                        verified_claims: None,
                        expires_at: NOW + 3600,
                    },
                    NOW,
                )
            })
            .unwrap();
        let mut approval = grant_request(principal, &login, &f.read, &f.write, NOW);
        approval.client_id = client_id.into();
        approval.binding_kind = "browser".into();
        approval.binding_value = "http://localhost:5173".into();
        approval.selection.required.clear();
        approval.selection.optional.clear();
        approval.approved.clear();
        approval.remember = false;
        approval.privileges = if consent { privileges.clone() } else { vec![] };
        let result = f
            .store
            .approve_grant(&mutation(principal, &approval), &approval, NOW);
        if !allowed {
            assert!(
                matches!(result, Err(AuthError::Denied)),
                "{client_id}, principal={principal}"
            );
            continue;
        }
        let mut admission = f.admission.clone();
        admission.runtime_id = URL_SAFE_NO_PAD.encode(ulid::Ulid::new().to_bytes());
        let grant = result.unwrap();
        admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
        admission.credential = Credential::OAuthGrant(grant);
        let issued = issue_authority(&f.store, &admission, None, NOW).unwrap();
        assert_eq!(
            issued
                .authority
                .unsigned
                .platform_privileges
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            approval
                .privileges
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
    }
}

#[test]
fn replaced_platform_delegation_enforces_existing_sessions_without_a_renewal() {
    use trellis_protocol::PlatformPrivilege::{ApisAccept, ApisForceReplace};
    let f = fixture();
    let registration = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Native client".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![],
        eligible_privileges: vec![ApisAccept, ApisForceReplace],
        metadata: serde_json::json!({}),
        expected_revision: 1,
    };
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &registration),
                &registration,
                NOW,
            )
        })
        .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::PrivilegeAssign {
            principal_id: f.principal.clone(),
            privileges: vec![ApisAccept, ApisForceReplace],
            expected_revision: 0,
        },
        NOW,
    );
    let login = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: f.principal.clone(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: NOW + 3600,
                },
                NOW,
            )
        })
        .unwrap();
    let mut approval = grant_request(&f.principal, &login, &f.read, &f.write, NOW);
    approval.privileges = vec![ApisAccept, ApisForceReplace];
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    let issued = issue_authority(&f.store, &admission, None, NOW).unwrap();
    assert!(issued
        .authority
        .unsigned
        .platform_privileges
        .contains(&ApisForceReplace));
    while f.store.enforce_next_page(NOW).unwrap().is_some() {}
    approval.privileges = vec![ApisAccept];
    f.store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW + 1)
        .unwrap();
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    let session = &issued.authority.unsigned.authorization_session_id;
    assert!(f.store.resolve_revocation(session).unwrap().is_some());
    let replacement = issue_authority(&f.store, &admission, None, NOW + 1).unwrap();
    assert!(replacement
        .authority
        .unsigned
        .platform_privileges
        .contains(&ApisAccept));
    assert!(!replacement
        .authority
        .unsigned
        .platform_privileges
        .contains(&ApisForceReplace));
}

#[test]
fn gained_platform_privilege_deadline_does_not_shorten_unprivileged_contexts() {
    use trellis_protocol::PlatformPrivilege::ApisAccept;
    let f = fixture();
    let mut registration = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Native client".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![],
        eligible_privileges: vec![ApisAccept],
        metadata: serde_json::json!({}),
        expected_revision: 1,
    };
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &registration),
                &registration,
                NOW,
            )
        })
        .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::PrivilegeAssign {
            principal_id: f.principal.clone(),
            privileges: vec![ApisAccept],
            expected_revision: 0,
        },
        NOW,
    );
    let login = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: f.principal.clone(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: NOW + 3600,
                },
                NOW,
            )
        })
        .unwrap();
    let mut approval = grant_request(&f.principal, &login, &f.read, &f.write, NOW);
    approval.privileges = vec![ApisAccept];
    approval.remember = false;
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    registration.expected_revision = 2;
    registration.eligible_privileges.clear();
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &registration),
                &registration,
                NOW,
            )
        })
        .unwrap();
    let unprivileged = issue_authority(&f.store, &admission, None, NOW).unwrap();
    let session = &unprivileged.authority.unsigned.authorization_session_id;
    assert!(unprivileged
        .authority
        .unsigned
        .platform_privileges
        .is_empty());
    while f.store.enforce_next_page(NOW).unwrap().is_some() {}
    registration.expected_revision = 3;
    registration.eligible_privileges = vec![ApisAccept];
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &registration),
                &registration,
                NOW + 1,
            )
        })
        .unwrap();
    approval.remember_until = Some(NOW + 10);
    f.store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW + 1)
        .unwrap();
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    assert!(f.store.resolve_revocation(session).unwrap().is_none());
    let privileged = issue_authority(&f.store, &admission, Some(session), NOW + 2).unwrap();
    assert_eq!(
        privileged.authority.unsigned.authorization_session_id,
        *session
    );
    assert!(privileged
        .authority
        .unsigned
        .platform_privileges
        .contains(&ApisAccept));
    assert_eq!(privileged.authority.unsigned.expires_at, NOW + 10);
    let renewed = issue_authority(&f.store, &admission, Some(session), NOW + 16).unwrap();
    assert_eq!(
        renewed.authority.unsigned.authorization_session_id,
        *session
    );
    assert!(renewed.authority.unsigned.platform_privileges.is_empty());
    assert!(renewed.authority.unsigned.expires_at > unprivileged.authority.unsigned.expires_at);
    approval.remember_until = Some(NOW + 100);
    f.store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW + 16)
        .unwrap();
    let longer = issue_authority(&f.store, &admission, Some(session), NOW + 17).unwrap();
    assert!(longer
        .authority
        .unsigned
        .platform_privileges
        .contains(&ApisAccept));
    assert!(longer.authority.unsigned.expires_at > NOW + 25);
    approval.remember_until = Some(NOW + 25);
    f.store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW + 18)
        .unwrap();
    while f.store.enforce_next_page(NOW + 18).unwrap().is_some() {}
    assert!(f.store.resolve_revocation(session).unwrap().is_some());
    assert!(matches!(
        issue_authority(&f.store, &admission, Some(session), NOW + 18),
        Err(AuthError::Retired)
    ));
}
