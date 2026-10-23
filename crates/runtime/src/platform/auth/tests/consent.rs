use super::*;

#[test]
fn shortened_deadline_retires_outstanding_context_unless_another_path_preserves_it() {
    for alternative in [false, true] {
        let f = fixture();
        if alternative {
            let role_id = id();
            policy(
                &f.store,
                &f.admin,
                PolicyMutation::RolePut {
                    role_id: role_id.clone(),
                    expected_revision: 0,
                    title: "Readers".into(),
                    description: "Independent read entitlement".into(),
                    capabilities: vec![f.read.clone()],
                },
                NOW,
            );
            policy(
                &f.store,
                &f.admin,
                PolicyMutation::RoleAssign {
                    principal_id: f.principal.clone(),
                    role_id,
                    expected_revision: 0,
                    expires_at: None,
                },
                NOW,
            );
        }
        while f.store.enforce_next_page(NOW).unwrap().is_some() {}
        let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
        let session = &issued.authority.unsigned.authorization_session_id;
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::CapabilityGrant {
                principal_id: f.principal.clone(),
                capability_id: f.read.clone(),
                expected_revision: 1,
                expires_at: Some(NOW + 10),
            },
            NOW + 1,
        );
        while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
        if alternative {
            assert!(f.store.resolve_revocation(session).unwrap().is_none());
            let renewed = issue_authority(&f.store, &f.admission, Some(session), NOW + 2).unwrap();
            assert_eq!(
                renewed.authority.unsigned.authorization_session_id,
                *session
            );
            assert!(renewed.authority.unsigned.expires_at > NOW + 10);
        } else {
            let cutoff = f.store.resolve_revocation(session).unwrap().unwrap();
            assert_eq!(cutoff.effective_cutoff, NOW + 1);
            assert!(matches!(
                issue_authority(&f.store, &f.admission, Some(session), NOW + 2),
                Err(AuthError::Retired)
            ));
            let replacement = issue_authority(&f.store, &f.admission, None, NOW + 2).unwrap();
            assert_ne!(
                replacement.authority.unsigned.authorization_session_id,
                *session
            );
            assert_eq!(replacement.authority.unsigned.expires_at, NOW + 10);
        }
    }
}

#[test]
fn remembered_decline_enforces_existing_sessions_without_a_renewal() {
    let f = fixture();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    while f.store.enforce_next_page(NOW).unwrap().is_some() {}
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
                NOW + 1,
            )
        })
        .unwrap();
    let mut approval = grant_request(&f.principal, &login, &f.read, &f.write, NOW + 1);
    approval.approved = vec![f.read.clone()];
    approval.declined = vec![f.write.clone()];
    f.store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW + 1)
        .unwrap();
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    let session = &issued.authority.unsigned.authorization_session_id;
    assert!(f.store.resolve_revocation(session).unwrap().is_some());
    assert!(matches!(
        issue_authority(&f.store, &f.admission, Some(session), NOW + 1),
        Err(AuthError::Retired)
    ));
    let replacement = issue_authority(&f.store, &f.admission, None, NOW + 1).unwrap();
    assert!(!replacement
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.write));
}

#[test]
fn explicit_one_off_approval_overrides_old_choices_but_honors_later_withdrawal() {
    for expired in [false, true] {
        let f = fixture();
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
        let mut remembered = grant_request(&f.principal, &login, &f.read, &f.write, NOW);
        if expired {
            remembered.remember_until = Some(NOW + 2);
        } else {
            remembered.approved = vec![f.read.clone()];
            remembered.declined = vec![f.write.clone()];
        }
        f.store
            .approve_grant(&mutation(&f.principal, &remembered), &remembered, NOW + 1)
            .unwrap();
        // The decline case shares a timestamp with the fresh approval: consent
        // provenance must not depend on wall-clock ordering within a second.
        let now = if expired { NOW + 3 } else { NOW + 1 };
        let mut explicit = grant_request(&f.principal, &login, &f.read, &f.write, now);
        explicit.selection.required.push(f.write.clone());
        explicit.selection.optional.clear();
        explicit.remember = false;
        let mut invalid_reuse = explicit.clone();
        invalid_reuse.reused_remembered = vec![f.write.clone()];
        assert!(matches!(
            f.store
                .approve_grant(&mutation(&f.principal, &invalid_reuse), &invalid_reuse, now),
            Err(AuthError::Denied)
        ));
        let grant = f
            .store
            .approve_grant(&mutation(&f.principal, &explicit), &explicit, now)
            .unwrap();
        let mut admission = f.admission.clone();
        admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
        admission.credential = Credential::OAuthGrant(grant);
        let issued = issue_authority(&f.store, &admission, None, now).unwrap();
        assert!(issued
            .authority
            .unsigned
            .capabilities
            .iter()
            .any(|cap| cap.capability_id == f.write));
        let mut previous = f.admission.clone();
        previous.runtime_id = URL_SAFE_NO_PAD.encode([8; 16]);
        if expired {
            assert!(matches!(
                issue_authority(&f.store, &previous, None, now),
                Err(AuthError::RequiredMissing)
            ));
        } else {
            let previous = issue_authority(&f.store, &previous, None, now).unwrap();
            assert!(!previous
                .authority
                .unsigned
                .capabilities
                .iter()
                .any(|cap| cap.capability_id == f.write));
        }
        while f.store.enforce_next_page(now).unwrap().is_some() {}
        assert!(f
            .store
            .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
            .unwrap()
            .is_none());
        remembered.approved = vec![f.read.clone()];
        remembered.declined = vec![f.write.clone()];
        remembered.remember_until = None;
        f.store
            .approve_grant(&mutation(&f.principal, &remembered), &remembered, now)
            .unwrap();
        while f.store.enforce_next_page(now).unwrap().is_some() {}
        assert!(f
            .store
            .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
            .unwrap()
            .is_some());
        let fresh_grant = f
            .store
            .approve_grant(&mutation(&f.principal, &explicit), &explicit, now)
            .unwrap();
        admission.refresh_family_id = Some(oauth_family(&f.store, &fresh_grant, NOW, NOW + 86400));
        admission.credential = Credential::OAuthGrant(fresh_grant);
        let fresh = issue_authority(&f.store, &admission, None, now).unwrap();
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::ConsentRevoke {
                principal_id: f.principal.clone(),
                client_id: explicit.client_id,
                binding_kind: explicit.binding_kind,
                binding_value: explicit.binding_value,
                capability_id: Some(f.write.clone()),
            },
            now,
        );
        while f.store.enforce_next_page(now).unwrap().is_some() {}
        assert!(f
            .store
            .resolve_revocation(&fresh.authority.unsigned.authorization_session_id)
            .unwrap()
            .is_some());
    }
}

#[test]
fn reused_remembered_approval_keeps_its_deadline_and_cannot_be_renewed_after_expiry() {
    let f = fixture();
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
    approval.remember_until = Some(NOW + 10);
    f.store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW)
        .unwrap();
    approval.remember = false;
    approval.remember_until = None;
    approval.reused_remembered = vec![f.write.clone()];
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    let issued = issue_authority(&f.store, &admission, None, NOW).unwrap();
    assert_eq!(issued.authority.unsigned.expires_at, NOW + 10);
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    assert!(f
        .store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_none());
    assert!(matches!(
        issue_authority(
            &f.store,
            &admission,
            Some(&issued.authority.unsigned.authorization_session_id),
            NOW + 11
        ),
        Err(AuthError::Retired)
    ));
}

#[test]
fn declined_and_unknown_optional_capabilities_do_not_expand_at_renewal() {
    let f = fixture();
    let original = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let mut request = grant_request(
        &f.principal,
        original
            .authority
            .unsigned
            .login_session_id
            .as_deref()
            .unwrap(),
        &f.read,
        &f.write,
        NOW,
    );
    request.approved = vec![f.read.clone()];
    request.declined = vec![f.write.clone()];
    request
        .selection
        .optional
        .push("phase-two.absent.v1:absent.optional".into());
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &request), &request, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    admission.runtime_id = URL_SAFE_NO_PAD.encode([8; 16]);
    let first = issue_authority(&f.store, &admission, None, NOW).unwrap();
    assert_eq!(
        first
            .authority
            .unsigned
            .capabilities
            .iter()
            .map(|cap| cap.capability_id.as_str())
            .collect::<Vec<_>>(),
        vec![f.read.as_str()]
    );
    let renewed = issue_authority(
        &f.store,
        &admission,
        Some(&first.authority.unsigned.authorization_session_id),
        NOW + 1,
    )
    .unwrap();
    assert_eq!(
        renewed.authority.unsigned.capabilities,
        first.authority.unsigned.capabilities
    );
}

#[test]
fn trusted_browser_preapproval_is_subset_bound_and_native_clients_need_explicit_choices() {
    let f = fixture();
    let login = issue_authority(&f.store, &f.admission, None, NOW)
        .unwrap()
        .authority
        .unsigned
        .login_session_id
        .unwrap();
    let mut client = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Registered native client".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![f.read.clone()],
        metadata: serde_json::json!({}),
        expected_revision: 1,
    };
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
    let mut request = grant_request(&f.principal, &login, &f.read, &f.write, NOW);
    request.approved.clear();
    assert!(
        matches!(
            f.store
                .approve_grant(&mutation(&f.principal, &request), &request, NOW),
            Err(AuthError::RequiredMissing)
        ),
        "a registered native client cannot substitute preapproval for user consent"
    );

    client.client_id = "trusted-browser".into();
    client.kind = "browser".into();
    client.redirect_uris = vec!["https://trusted.example/callback".into()];
    client.expected_revision = 0;
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
    request.client_id = client.client_id;
    request.binding_kind = "browser".into();
    request.binding_value = "https://trusted.example".into();
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &request), &request, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    let issued = issue_authority(&f.store, &admission, None, NOW).unwrap();
    assert!(issued
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.read));
    assert!(
        !issued
            .authority
            .unsigned
            .capabilities
            .iter()
            .any(|cap| cap.capability_id == f.write),
        "trust in the client cannot approve capabilities outside its configured subset"
    );
}
