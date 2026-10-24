use super::*;

#[test]
fn force_replacement_fences_old_providers_but_only_changed_meanings_need_new_consent() {
    let f = fixture();
    let source = SOURCE
        .replace("service Reader", "service Provider")
        .replace("{ use documents", "{ implements documents; use documents");
    let proposed = evidence(&source);
    let graph = trellis_idl::compile_evidence(proposed.clone()).unwrap();
    let participant = graph
        .root_package()
        .participants()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let provisioning = DeploymentProvisioning {
        evidence: proposed,
        participant_id: participant,
        expected_api_generations: BTreeMap::from([(f.api.clone(), 1)]),
        identity_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&[11; 32]).verifying_key().as_bytes()),
        resource_commitments: serde_json::json!({}),
        expires_at: None,
    };
    let provisioned = f
        .store
        .provision_deployment(&mutation(&f.admin, &provisioning), &provisioning, NOW)
        .unwrap();
    let mut provider = f.admission.clone();
    provider.credential = Credential::ProvisionedIdentity(provisioned.identity_key_id);
    provider.refresh_family_id = None;
    let initial = issue_authority(&f.store, &provider, None, NOW).unwrap();
    let certificate: trellis_protocol::SignedProviderCertificate = serde_json::from_slice(
        &f.store
            .resolve_verification_material(
                "provider",
                &initial.authority.unsigned.provider_bindings[0].provider_certificate_digest,
            )
            .unwrap(),
    )
    .unwrap();
    certificate
        .verify(
            &f.store.issuer().unwrap(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            account_key(),
            NOW,
        )
        .unwrap();
    let expansion = SOURCE.replace(
        "rpc Export",
        "rpc Archive { input Document; output Document; } rpc Export",
    );
    accept(&f.store, &f.admin, &expansion, false, NOW + 1);
    let compatible = issue_authority(
        &f.store,
        &provider,
        Some(&initial.authority.unsigned.authorization_session_id),
        NOW + 1,
    )
    .unwrap();
    let certificate: trellis_protocol::SignedProviderCertificate = serde_json::from_slice(
        &f.store
            .resolve_verification_material(
                "provider",
                &compatible.authority.unsigned.provider_bindings[0].provider_certificate_digest,
            )
            .unwrap(),
    )
    .unwrap();
    assert!(!certificate
        .implemented_actions
        .iter()
        .any(|action| action.name == "Archive"));
    let changed = expansion.replace(
        "consent_revision 1; allows { rpc Put; }",
        "consent_revision 2; allows { rpc Put; }",
    );
    let start = std::sync::Barrier::new(2);
    let concurrent = std::thread::scope(|threads| {
        let issuer = threads.spawn(|| {
            start.wait();
            (0..32)
                .map(|_| {
                    issue_authority(
                        &f.store,
                        &provider,
                        Some(&initial.authority.unsigned.authorization_session_id),
                        NOW + 2,
                    )
                })
                .collect::<Vec<_>>()
        });
        start.wait();
        accept(&f.store, &f.admin, &changed, true, NOW + 2);
        issuer.join().unwrap()
    });
    for issuance in concurrent {
        match issuance {
            Ok(issued) => assert_eq!(issued.authority.unsigned.apis[0].generation.get(), 1),
            Err(AuthError::Denied | AuthError::Retired) => {}
            Err(error) => panic!("unexpected concurrent provider issuance failure: {error}"),
        }
    }
    assert!(matches!(
        issue_authority(&f.store, &provider, None, NOW + 2),
        Err(AuthError::Denied)
    ));
    let user = issue_authority(&f.store, &f.admission, None, NOW + 2).unwrap();
    assert!(user
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.read));
    assert!(!user
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.write));
    assert_eq!(user.authority.unsigned.apis[0].generation.get(), 2);
}

#[test]
fn generated_review_request_preserves_canonical_evidence_for_breakage_review() {
    let f = fixture();
    let proposed = evidence(&SOURCE.replace(
        "model Document { id: string; }",
        "model Document { id: string; requiredValue: string; }",
    ));
    let request: trellis_runtime_apis::types::AuthApiReviewRequest =
        serde_json::from_value(serde_json::json!({
            "apiId": f.api,
            "packageEvidence": proposed,
        }))
        .unwrap();
    let decoded: trellis_runtime_apis::types::AuthApiReviewRequest =
        serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
    let review = f
        .store
        .review_api(
            &f.admin,
            serde_json::from_value(serde_json::to_value(decoded.package_evidence).unwrap())
                .unwrap(),
            &decoded.api_id,
            NOW + 1,
        )
        .unwrap();
    assert!(!review.compatible);
    assert!(!review.breaking_changes.is_empty());
}

#[test]
fn concurrent_acceptance_preserves_coherent_authority_and_old_membership_limits() {
    let f = fixture();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let updated = SOURCE.replace("allows { rpc Get; }", "allows { rpc Get; rpc Export; }");
    let start = std::sync::Barrier::new(2);
    let (catalog, concurrent) = std::thread::scope(|threads| {
        let issuer = threads.spawn(|| {
            start.wait();
            (0..32)
                .map(|_| {
                    issue_authority(
                        &f.store,
                        &f.admission,
                        Some(&issued.authority.unsigned.authorization_session_id),
                        NOW + 1,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>()
        });
        start.wait();
        let catalog = accept(&f.store, &f.admin, &updated, false, NOW + 1);
        (catalog, issuer.join().unwrap())
    });
    let action = CatalogActionIdentity {
        kind: ApiSurfaceKind::Rpc,
        name: "Export".into(),
        direction: PermissionAction::Call,
    };
    catalog
        .verify(
            &f.store.issuer().unwrap(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            account_key(),
        )
        .unwrap();
    assert!(!catalog.authorizes(&issued.authority, &action));
    for candidate in concurrent {
        let snapshot = &candidate.authority.unsigned.apis[0];
        let bytes = f
            .store
            .resolve_verification_material("catalog", &snapshot.catalog_snapshot_digest)
            .unwrap();
        let committed: trellis_protocol::SignedCatalogSnapshot =
            serde_json::from_slice(&bytes).unwrap();
        committed
            .verify(
                &f.store.issuer().unwrap(),
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                account_key(),
            )
            .unwrap();
        // Current-catalog dispatch must agree with the immutable catalog
        // actually committed in this context, on either side of acceptance.
        assert_eq!(
            catalog.authorizes(&candidate.authority, &action),
            committed.authorizes(&candidate.authority, &action)
        );
        assert_eq!(snapshot.accepted_revision, committed.accepted_revision);
    }
    let renewed = issue_authority(
        &f.store,
        &f.admission,
        Some(&issued.authority.unsigned.authorization_session_id),
        NOW + 1,
    )
    .unwrap();
    assert_eq!(
        renewed.authority.unsigned.authorization_session_id,
        issued.authority.unsigned.authorization_session_id
    );
    assert!(catalog.authorizes(&renewed.authority, &action));
}

#[test]
fn stale_review_and_force_without_privilege_are_atomic_failures() {
    let f = fixture();
    let changed = SOURCE.replace("id: string;", "id: string; mandatory: string;");
    let review = f
        .store
        .review_api(&f.admin, evidence(&changed), &f.api, NOW)
        .unwrap();
    assert!(!review.compatible);
    let acceptance = ApiAcceptance {
        review_id: review.review_id.clone(),
        reviewed_digest: review.definition_digest.clone(),
        force: false,
        acknowledge_breakage: false,
        confirm_consent_meanings: false,
        materially_changed: vec![],
        policy_edits: vec![],
    };
    assert!(matches!(
        f.store
            .accept_api(&mutation(&f.admin, &acceptance), &acceptance, NOW),
        Err(AuthError::Denied)
    ));
    accept(&f.store, &f.admin, SOURCE, false, NOW + 1);
    let forced = ApiAcceptance {
        force: true,
        acknowledge_breakage: true,
        confirm_consent_meanings: true,
        ..acceptance
    };
    assert!(matches!(
        f.store
            .accept_api(&mutation(&f.admin, &forced), &forced, NOW + 2),
        Err(AuthError::Conflict)
    ));
    assert_eq!(f.store.current_catalog(&f.api).unwrap().generation.get(), 1);
    let manager = create_user(&f.store, &f.admin, NOW);
    administer(
        &f.store,
        &f.admin,
        &manager,
        &[Administration::AcceptApis],
        NOW,
    );
    let review = f
        .store
        .review_api(&manager, evidence(&changed), &f.api, NOW + 2)
        .unwrap();
    let forced = ApiAcceptance {
        review_id: review.review_id,
        reviewed_digest: review.definition_digest,
        ..forced
    };
    assert!(matches!(
        f.store
            .accept_api(&mutation(&manager, &forced), &forced, NOW + 2),
        Err(AuthError::Denied)
    ));
    assert_eq!(f.store.current_catalog(&f.api).unwrap().generation.get(), 1);
}

#[test]
fn acceptance_role_edits_need_platform_management_to_bundle_administration() {
    let f = fixture();
    let manager = create_user(&f.store, &f.admin, NOW);
    administer(
        &f.store,
        &f.admin,
        &manager,
        &[Administration::AcceptApis, Administration::ManageRoles],
        NOW,
    );
    let expansion = SOURCE.replace(
        "rpc Export",
        "rpc Archive { input Document; output Document; } rpc Export",
    );
    let role_id = ulid::Ulid::new().to_string();
    let accept_with = |actor: &str, capability: &str, now: i64| {
        let review = f
            .store
            .review_api(actor, evidence(&expansion), &f.api, now)
            .unwrap();
        let acceptance = ApiAcceptance {
            review_id: review.review_id,
            reviewed_digest: review.definition_digest,
            force: false,
            acknowledge_breakage: false,
            confirm_consent_meanings: false,
            materially_changed: review.consent_changes,
            policy_edits: vec![PolicyMutation::RolePut {
                role_id: role_id.clone(),
                expected_revision: 0,
                title: "Bundled".into(),
                description: String::new(),
                capabilities: vec![capability.into()],
            }],
        };
        f.store
            .accept_api(&mutation(actor, &acceptance), &acceptance, now)
    };
    let role_exists = || {
        f.store
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_roles WHERE role_id=?1)",
                    [&role_id],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .unwrap()
    };
    let before = f.store.current_catalog(&f.api).unwrap().accepted_revision;

    assert!(matches!(
        accept_with(
            &manager,
            Administration::ManagePlatform.capability_id(),
            NOW + 1
        ),
        Err(AuthError::Denied)
    ));
    assert_eq!(
        f.store.current_catalog(&f.api).unwrap().accepted_revision,
        before
    );
    assert!(!role_exists());

    accept_with(&manager, &f.read, NOW + 2).unwrap();
    assert!(role_exists());
}
