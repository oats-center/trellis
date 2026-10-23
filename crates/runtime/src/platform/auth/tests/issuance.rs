use super::*;

#[test]
fn latest_verified_oidc_claims_and_freshness_bound_issuance_without_removing_direct_rights() {
    let f = fixture();
    let provider = super::super::lifecycle::OidcProviderRegistration {
        provider_id: id(),
        issuer: "https://identity.example".into(),
        display_name: "Identity".into(),
        client_id: "trellis".into(),
        sealed_client_secret: None,
        config: serde_json::json!({}),
        claim_freshness_seconds: 5,
        expected_revision: 0,
    };
    f.store
        .transaction(|connection| {
            f.store.register_oidc_provider_in_transaction(
                connection,
                &mutation(&f.admin, &provider),
                &provider,
                NOW,
            )
        })
        .unwrap();
    f.store
        .transaction(|connection| {
            SqliteAuthorizationStore::link_oidc_identity_in(
                connection,
                &f.principal,
                &provider.provider_id,
                "upstream-user",
                NOW,
            )
        })
        .unwrap();
    let role = id();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RolePut {
            role_id: role.clone(),
            expected_revision: 0,
            title: "OIDC reader".into(),
            description: String::new(),
            capabilities: vec![f.read.clone()],
        },
        NOW,
    );
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::OidcMappingPut {
            mapping_id: id(),
            provider_id: provider.provider_id.clone(),
            claim_name: "/groups".into(),
            claim_value: "readers".into(),
            role_ids: vec![role],
            expected_revision: 0,
        },
        NOW,
    );
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityRevoke {
            principal_id: f.principal.clone(),
            capability_id: f.read.clone(),
            expected_revision: 1,
        },
        NOW,
    );
    let mut login = VerifiedLogin {
        principal_id: f.principal.clone(),
        provider_id: Some(provider.provider_id.clone()),
        upstream_issuer: Some(provider.issuer.clone()),
        upstream_subject: Some("upstream-user".into()),
        verified_claims: Some(serde_json::json!({"groups":["readers"]})),
        expires_at: NOW + 3600,
    };
    let login_id = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(connection, &login, NOW)
        })
        .unwrap();
    let request = grant_request(&f.principal, &login_id, &f.read, &f.write, NOW);
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &request), &request, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    let first = issue_authority(&f.store, &admission, None, NOW).unwrap();
    assert_eq!(first.authority.unsigned.expires_at, NOW + 5);
    // A second linked identity at the same issuer must not replace the selected
    // identity's verified claims, even when its login is newer.
    f.store
        .transaction(|connection| {
            SqliteAuthorizationStore::link_oidc_identity_in(
                connection,
                &f.principal,
                &provider.provider_id,
                "another-upstream-user",
                NOW,
            )
        })
        .unwrap();
    let mut other_login = login.clone();
    other_login.upstream_subject = Some("another-upstream-user".into());
    other_login.verified_claims = Some(serde_json::json!({"groups":[]}));
    f.store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(connection, &other_login, NOW)
        })
        .unwrap();
    let unchanged = issue_authority(
        &f.store,
        &admission,
        Some(&first.authority.unsigned.authorization_session_id),
        NOW,
    )
    .expect("another linked subject must not remove this grant's required role");
    assert!(unchanged
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.read));
    login.verified_claims = Some(serde_json::json!({"groups":[]}));
    f.store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(connection, &login, NOW + 1)
        })
        .unwrap();
    assert!(matches!(
        issue_authority(
            &f.store,
            &admission,
            Some(&first.authority.unsigned.authorization_session_id),
            NOW + 1
        ),
        Err(AuthError::Retired)
    ));
    other_login.verified_claims = Some(serde_json::json!({"groups":["readers"]}));
    f.store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(connection, &other_login, NOW + 1)
        })
        .unwrap();
    assert!(
        matches!(
            issue_authority(&f.store, &admission, None, NOW + 1),
            Err(AuthError::RequiredMissing)
        ),
        "another linked subject must not supply this grant's missing role"
    );
    login.verified_claims = Some(serde_json::json!({"groups":["readers"]}));
    f.store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(connection, &login, NOW + 2)
        })
        .unwrap();
    let second = issue_authority(&f.store, &admission, None, NOW + 2).unwrap();
    assert_ne!(
        second.authority.unsigned.authorization_session_id,
        first.authority.unsigned.authorization_session_id
    );
    assert!(matches!(
        issue_authority(&f.store, &admission, None, NOW + 8),
        Err(AuthError::RequiredMissing)
    ));
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: f.principal.clone(),
            capability_id: f.read.clone(),
            expected_revision: 0,
            expires_at: None,
        },
        NOW + 8,
    );
    let direct = issue_authority(&f.store, &admission, None, NOW + 8).unwrap();
    assert_eq!(direct.authority.unsigned.expires_at, NOW + 68);
    assert!(direct
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.read));
}

#[test]
fn issues_verifiable_authority_and_retains_it_after_restart_and_expiry() {
    let f = fixture();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let issuer = f.store.issuer().unwrap();
    let policy = SessionAuthorityVerificationPolicy {
        now_unix_seconds: NOW,
        allowed_clock_skew_seconds: 5,
        maximum_authority_lifetime_seconds: 60,
        maximum_authority_bytes: 65536,
        maximum_entries: 256,
    };
    let verified = verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &issuer,
        trellis_instance_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        audience_nats_account: account_key(),
        authority: &issued.authority,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
    })
    .unwrap();
    assert_eq!(verified.digest(), issued.digest);
    assert_eq!(issued.authority.unsigned.capabilities.len(), 2);
    drop(f.store);
    let restarted = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    restarted.prune_expired_artifacts(NOW + 10000).unwrap();
    assert_eq!(
        resolve_authority(&restarted, &issued.digest).unwrap(),
        issued.authority
    );
}

#[test]
fn union_survives_one_entitlement_path_and_required_loss_retires_the_session() {
    let f = fixture();
    let role_id = id();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RolePut {
            role_id: role_id.clone(),
            expected_revision: 0,
            title: "Reader".into(),
            description: String::new(),
            capabilities: vec![f.read.clone()],
        },
        NOW,
    );
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RoleAssign {
            principal_id: f.principal.clone(),
            role_id: role_id.clone(),
            expected_revision: 0,
            expires_at: None,
        },
        NOW,
    );
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityRevoke {
            principal_id: f.principal.clone(),
            capability_id: f.read.clone(),
            expected_revision: 1,
        },
        NOW + 1,
    );
    drain(&f.store, NOW + 1);
    assert!(f
        .store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_none());
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RoleRevoke {
            principal_id: f.principal.clone(),
            role_id,
            expected_revision: 1,
        },
        NOW + 2,
    );
    drain(&f.store, NOW + 2);
    assert_eq!(
        f.store
            .authorization_session_status(
                issued
                    .authority
                    .unsigned
                    .authorization_session_id
                    .parse()
                    .unwrap()
            )
            .unwrap()
            .state,
        trellis_runtime_apis::types::AuthAuthorizationSessionState::Superseded,
    );
    assert!(f
        .store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
    assert!(matches!(
        issue_authority(&f.store, &f.admission, None, NOW + 2),
        Err(AuthError::RequiredMissing)
    ));
}

#[test]
fn optional_reduction_requires_new_logical_id_and_old_admission_stays_closed() {
    let mut f = fixture();
    f.admission.inbox_prefix =
        trellis_protocol::admission_inbox_prefix(&f.admission.session_public_key).unwrap();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let session = &issued.authority.unsigned.authorization_session_id;
    let nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
        .unwrap()
        .public_key();
    let (attachment, _) = f
        .store
        .admit_connection(
            &fixture_admission(&f, &nkey, NOW),
            &nkey,
            &nkey,
            "broker",
            1,
            NOW,
        )
        .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityRevoke {
            principal_id: f.principal.clone(),
            capability_id: f.write.clone(),
            expected_revision: 1,
        },
        NOW + 1,
    );
    // The admitted socket cannot renew old authority before enforcement scans it.
    let subject = trellis_protocol::admission_get_subject(&f.admission.session_public_key).unwrap();
    let reply = format!("{}.renewal", f.admission.inbox_prefix);
    let request = trellis_protocol::sign_admission_get(
        trellis_protocol::AdmissionGet {
            trellis_instance_id: f.store.settings.instance_id.clone(),
            runtime_id: f.admission.runtime_id.clone(),
            session_public_key: f.admission.session_public_key.clone(),
            attachment_id: attachment.attachment_id,
            request_nonce: URL_SAFE_NO_PAD.encode([23; 16]),
            issued_at: NOW + 1,
        },
        &subject,
        &reply,
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    assert!(matches!(
        f.store.own_context(
            &serde_json::to_vec(&request).unwrap(),
            &subject,
            &reply,
            NOW + 1
        ),
        Err(AuthError::Retired)
    ));
    assert!(matches!(
        issue_authority(&f.store, &f.admission, Some(session), NOW + 1),
        Err(AuthError::Retired)
    ));
    let replacement = issue_authority(&f.store, &f.admission, None, NOW + 1).unwrap();
    assert_ne!(
        replacement.authority.unsigned.authorization_session_id,
        *session
    );
    assert_eq!(
        replacement
            .authority
            .unsigned
            .capabilities
            .iter()
            .map(|cap| cap.capability_id.as_str())
            .collect::<Vec<_>>(),
        vec![f.read.as_str()]
    );
    let replacement_session = &replacement.authority.unsigned.authorization_session_id;
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: f.principal.clone(),
            capability_id: f.write.clone(),
            expected_revision: 0,
            expires_at: Some(NOW + 10),
        },
        NOW + 2,
    );
    while f.store.enforce_next_page(NOW + 2).unwrap().is_some() {}
    assert!(f
        .store
        .resolve_revocation(replacement_session)
        .unwrap()
        .is_none());
    let expanded =
        issue_authority(&f.store, &f.admission, Some(replacement_session), NOW + 3).unwrap();
    assert_eq!(
        expanded.authority.unsigned.authorization_session_id,
        *replacement_session
    );
    assert!(expanded
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.write));
    assert_eq!(expanded.authority.unsigned.expires_at, NOW + 10);
    // The short-lived write context is no longer usable, but the earlier read
    // context still is. A spent permission deadline cannot become a root limit.
    let read_only =
        issue_authority(&f.store, &f.admission, Some(replacement_session), NOW + 16).unwrap();
    assert_eq!(
        read_only.authority.unsigned.authorization_session_id,
        *replacement_session
    );
    assert!(!read_only
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.write));
    assert!(read_only.authority.unsigned.expires_at > replacement.authority.unsigned.expires_at);
    assert!(matches!(
        issue_authority(&f.store, &f.admission, Some(session), NOW + 16),
        Err(AuthError::Retired)
    ));
}

#[test]
fn sdk_dispatch_checks_signed_bytes_new_membership_and_session_retirement() {
    use trellis_protocol::{sign_session_request, SessionRequest};
    use trellis_rs::client::{AuthorizationVerificationCore, RequestVerificationInput};

    let f = fixture();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let updated = SOURCE.replace("allows { rpc Get; }", "allows { rpc Get; rpc Export; }");
    let catalog = accept(&f.store, &f.admin, &updated, false, NOW + 1);
    let renewed = issue_authority(
        &f.store,
        &f.admission,
        Some(&issued.authority.unsigned.authorization_session_id),
        NOW + 1,
    )
    .unwrap();
    let issuer = f.store.issuer().unwrap();
    let catalog = trellis_protocol::AuthenticatedCatalog::authenticate(
        catalog,
        &issuer,
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        account_key(),
    )
    .unwrap();
    let policy = SessionAuthorityVerificationPolicy {
        now_unix_seconds: NOW + 1,
        allowed_clock_skew_seconds: 5,
        maximum_authority_lifetime_seconds: 60,
        maximum_authority_bytes: 65536,
        maximum_entries: 256,
    };
    let verifier = AuthorizationVerificationCore::new();
    let action = CatalogActionIdentity {
        kind: ApiSurfaceKind::Rpc,
        name: "Export".into(),
        direction: PermissionAction::Call,
    };
    for (candidate, allowed) in [(&issued, false), (&renewed, true)] {
        let authority = Arc::new(
            verify_session_authority(SessionAuthorityVerificationInput {
                issuer: &issuer,
                trellis_instance_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                audience_nats_account: account_key(),
                authority: &candidate.authority,
                policy: &policy,
                purpose: SessionAuthorityPurpose::Live,
                revocation_cutoff: None,
            })
            .unwrap(),
        );
        let api = &candidate.authority.unsigned.apis[0];
        let request = SessionRequest {
            authority_digest: candidate.digest.clone(),
            api_id: api.api_id.clone(),
            api_generation: api.generation,
            accepted_revision: api.accepted_revision,
            action: "Export".into(),
            subject: "test.Export".into(),
            reply_subject: Some(format!(
                "{}.reply",
                candidate.authority.unsigned.inbox_prefix
            )),
            request_id: id(),
            issued_at: NOW + 1,
        };
        let payload = br#"{"id":"sample"}"#;
        let proof =
            sign_session_request(&request, payload, &SigningKey::from_bytes(&[9; 32])).unwrap();
        let verify = |bytes: &[u8], revoked| {
            verifier.verify_request(RequestVerificationInput {
                authority: authority.clone(),
                request: &request,
                payload: bytes,
                proof: &proof,
                policy: &policy,
                known_revoked: revoked,
                catalog: &catalog,
                catalog_issuer: &issuer,
                action: &action,
            })
        };
        assert_eq!(verify(payload, false).is_ok(), allowed);
        assert!(verify(br#"{"id":"substituted"}"#, false).is_err());
        assert!(verify(payload, true).is_err());
        if allowed {
            let verified = verify(payload, false).unwrap();
            assert_eq!(verified.caller().principal_id, f.principal);
            assert_eq!(verified.request().request_id, request.request_id);
        }
    }
}
