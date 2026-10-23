//! Bootstrap grants are explicit policy, not wildcard or future authority.

use super::*;
use crate::platform::auth::initialization::{
    cli_registration, console_registration, CLI_CLIENT_ID,
};

#[test]
fn administrator_without_a_password_cannot_log_in_or_be_seeded_with_a_password_later() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[42; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator("admin", None, None, None, NOW)
        .unwrap()
        .principal_id;
    let guest = store.create_portal_session(NOW).unwrap();
    for password in ["", "admin", "a strong bootstrap password"] {
        assert!(matches!(
            store.portal_local_login(
                &guest.cookie,
                &guest.csrf,
                &guest.cookie,
                ("admin", password),
                &id(),
                NOW
            ),
            Err(AuthError::Denied)
        ));
    }
    assert!(matches!(
        store.initialize_administrator(
            "admin",
            Some("a strong bootstrap password"),
            None,
            None,
            NOW
        ),
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        store
            .initialize_administrator("admin", None, None, None, NOW)
            .unwrap()
            .principal_id,
        admin
    );
}

#[test]
fn fresh_bootstrap_rolls_back_all_seeds_and_grants_then_issues_only_consented_subset() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[42; 32]));
    store.initialize_issuer(NOW).unwrap();
    let mut invalid = cli_registration();
    invalid.implied_capabilities.push("unknown::read".into());
    assert!(store
        .initialize_administrator(
            "admin",
            Some("a strong bootstrap password"),
            Some(&[
                console_registration("https://console.example").unwrap(),
                invalid
            ]),
            None,
            NOW
        )
        .is_err());
    // Failure must leave no principal, catalog, grant, or client
    // from this attempted aggregate, including its earlier valid Console.
    store
        .transaction(|connection| {
            for table in [
                "auth_principals",
                "auth_accepted_apis",
                "auth_direct_capabilities",
                "auth_oauth_clients",
            ] {
                assert_eq!(
                    connection
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                            .get::<_, u64>(0))?,
                    0
                );
            }
            Ok(())
        })
        .unwrap();
    let admin = store
        .seed_administrator_and_console(
            "https://console.example",
            "admin",
            "a strong bootstrap password",
            NOW + 1,
        )
        .unwrap();
    let login = store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: admin.clone(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: NOW + 3600,
                },
                NOW + 2,
            )
        })
        .unwrap();
    let read = "trellis.managePrincipals";
    let write = "trellis.auth@v1::readAccount";
    let mut approval = grant_request(&admin, &login, read, write, NOW + 2);
    approval.client_id = CLI_CLIENT_ID.into();
    approval.approved = vec![read.into()];
    approval.declined = vec![write.into()];
    approval.administration_confirmed = true;
    let grant = store
        .approve_grant(&mutation(&admin, &approval), &approval, NOW + 2)
        .unwrap();
    let admission = AdmissionIdentity {
        refresh_family_id: Some(oauth_family(&store, &grant, NOW + 2, NOW + 3600)),
        credential: Credential::OAuthGrant(grant),
        runtime_id: URL_SAFE_NO_PAD.encode([7; 16]),
        session_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&[3; 32]).verifying_key().as_bytes()),
        inbox_prefix: "_INBOX.bootstrap".into(),
    };
    let issued =
        crate::platform::auth::tests::issue_authority(&store, &admission, None, NOW + 3).unwrap();
    assert!(issued
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == read));
    assert!(!issued
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == write));
}

#[test]
fn bootstrap_does_not_entitle_admin_to_an_accepted_domain_api() {
    let f = fixture();
    let login = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: f.admin.clone(),
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
    let approval = grant_request(&f.admin, &login, &f.read, &f.write, NOW);
    assert!(matches!(
        f.store
            .approve_grant(&mutation(&f.admin, &approval), &approval, NOW),
        Err(AuthError::RequiredMissing)
    ));
}

#[test]
fn bootstrap_replay_does_not_grant_new_builtin_capabilities() {
    use trellis_rs::generated::ParticipantDescriptor;
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[42; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator(
            "admin",
            Some("a strong bootstrap password"),
            None,
            None,
            NOW,
        )
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let current: PackageEvidence = serde_json::from_value(
        serde_json::to_value(
            trellis_runtime_apis::participants::trellis_platform::Participant::package_evidence(),
        )
        .unwrap(),
    )
    .unwrap();
    let source = current
        .packages
        .iter()
        .find(|package| package.name == current.root_package)
        .unwrap()
        .source
        .replace(
            "capability rolesRead {",
            "capability futureRead { title \"Future\"; description \"Future \
            optional right.\"; consequence \"Reads future metadata.\"; \
            consent_revision 1; allows { rpc Roles.List; } }\ncapability rolesRead \
            {",
        );
    let graph = compile_project(
        &PackageManifest {
            package: PackageMetadata {
                name: current.root_package.clone(),
                version: "0.100.0".parse().unwrap(),
            },
            sources: BTreeMap::from([("main".into(), "main.trellis".into())]),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        },
        vec![SourceUnit {
            alias: "main".into(),
            path: PathBuf::from("main.trellis"),
            source,
        }],
        BTreeMap::new(),
    )
    .unwrap();
    let proposed = PackageEvidence {
        root_package: graph.root().to_string(),
        root_digest: graph.root_digest().into(),
        packages: vec![PackageSourceEvidence {
            name: graph.root().to_string(),
            version: graph.root_package().version().clone(),
            digest: graph.root_digest().into(),
            source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap(),
        }],
    };
    let review = store
        .review_api(&admin, proposed, "trellis.auth@v1", NOW + 1)
        .unwrap();
    let acceptance = ApiAcceptance {
        review_id: review.review_id,
        reviewed_digest: review.definition_digest,
        force: false,
        acknowledge_breakage: false,
        confirm_consent_meanings: false,
        materially_changed: vec![],
        policy_edits: vec![],
    };
    store
        .accept_api(&mutation(&admin, &acceptance), &acceptance, NOW + 1)
        .unwrap();
    assert_eq!(
        store
            .initialize_administrator(
                "admin",
                Some("a strong bootstrap password"),
                None,
                None,
                NOW + 2
            )
            .map(|initialized| initialized.principal_id)
            .unwrap(),
        admin
    );
    let login = store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: admin.clone(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: NOW + 3600,
                },
                NOW + 2,
            )
        })
        .unwrap();
    let mut approval = grant_request(
        &admin,
        &login,
        "trellis.manageRoles",
        "trellis.auth@v1::futureRead",
        NOW + 2,
    );
    let cli = cli_registration();
    store
        .transaction(|connection| {
            store.register_client_in_transaction(connection, &mutation(&admin, &cli), &cli, NOW + 2)
        })
        .unwrap();
    approval.client_id = CLI_CLIENT_ID.into();
    approval.administration_confirmed = true;
    let grant = store
        .approve_grant(&mutation(&admin, &approval), &approval, NOW + 2)
        .unwrap();
    let admission = AdmissionIdentity {
        refresh_family_id: Some(oauth_family(&store, &grant, NOW + 2, NOW + 3600)),
        credential: Credential::OAuthGrant(grant),
        runtime_id: URL_SAFE_NO_PAD.encode([7; 16]),
        session_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&[3; 32]).verifying_key().as_bytes()),
        inbox_prefix: "_INBOX.replay".into(),
    };
    let issued =
        crate::platform::auth::tests::issue_authority(&store, &admission, None, NOW + 3).unwrap();
    assert!(issued
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == "trellis.manageRoles"));
    assert!(!issued
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == "trellis.auth@v1::futureRead"));
}
