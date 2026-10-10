use std::{collections::BTreeMap, path::PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use trellis_idl::{
    canonical_package, compile_project,
    project::{GenerateConfig, PackageManifest, PackageMetadata},
    CanonicalMode, PackageEvidence, PackageSourceEvidence, SourceUnit,
};
use trellis_protocol::{
    verify_session_authority, ApiSurfaceKind, CatalogActionIdentity, PermissionAction,
    SessionAuthorityPurpose, SessionAuthorityVerificationInput, SessionAuthorityVerificationPolicy,
};

use super::sqlite::{digest, id};
use super::*;

const NOW: i64 = 1_800_000_000;
const SOURCE: &str = r#"
model Document { id: string; }
api documents@v1 {
 title "Documents";
 description "Document management.";
 rpc Get { input Document; output Document; }
 rpc Put { input Document; output Document; }
 rpc Export { input Document; output Document; }
 capabilities {
  capability read { title "Read"; description "Read documents."; consequence "Sees contents."; consent_revision 1; allows { rpc Get; } }
  capability write { title "Write"; description "Write documents."; consequence "Changes contents."; consent_revision 1; allows { rpc Put; } }
 }
}
service Reader { use documents { required capability read; capability write; } }
"#;

struct Fixture {
    directory: tempfile::TempDir,
    store: SqliteAuthorizationStore,
    admin: String,
    principal: String,
    api: String,
    read: String,
    write: String,
    grant: String,
    admission: AdmissionIdentity,
}

fn mutation(actor: &str, input: &impl Serialize) -> Mutation {
    Mutation {
        actor: actor.into(),
        request_id: id(),
        request_digest: digest(input).unwrap(),
    }
}

fn evidence(source: &str) -> PackageEvidence {
    let graph = compile_project(
        &PackageManifest {
            package: PackageMetadata {
                name: "phase-two".into(),
                version: "1.0.0".parse().unwrap(),
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
            source: source.into(),
        }],
        BTreeMap::new(),
    )
    .unwrap();
    PackageEvidence {
        root_package: graph.root().to_string(),
        root_digest: graph.root_digest().into(),
        packages: vec![PackageSourceEvidence {
            name: graph.root().to_string(),
            version: graph.root_package().version().clone(),
            digest: graph.root_digest().into(),
            source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap(),
        }],
    }
}

fn open(directory: &tempfile::TempDir, key: SigningKey) -> SqliteAuthorizationStore {
    let mut connection = Connection::open(directory.path().join("platform.db")).unwrap();
    let migration = refinery::Migration::unapplied(
        "V1000__platform_init",
        include_str!("../../storage/sqlite/platform/V1000__platform_init.sql"),
    )
    .unwrap();
    refinery::Runner::new(&[migration])
        .run(&mut connection)
        .unwrap();
    SqliteAuthorizationStore::from_connection(
        connection,
        key,
        AuthSettings {
            instance_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            account: "account-phase-two".into(),
            authority_lifetime_seconds: 60,
            clock_skew_seconds: 5,
            enforcement_page_size: 2,
        },
    )
    .unwrap()
}

fn accept(
    store: &SqliteAuthorizationStore,
    admin: &str,
    source: &str,
    force: bool,
    now: i64,
) -> trellis_protocol::SignedCatalogSnapshot {
    let proposed = evidence(source);
    let graph = trellis_idl::compile_evidence(proposed.clone()).unwrap();
    let api = graph
        .root_package()
        .apis()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let review = store.review_api(admin, proposed, &api, now).unwrap();
    let acceptance = ApiAcceptance {
        review_id: review.review_id,
        reviewed_digest: review.definition_digest,
        force,
        acknowledge_breakage: force,
        confirm_consent_meanings: force,
        materially_changed: review.consent_changes,
        policy_edits: vec![],
    };
    store
        .accept_api(&mutation(admin, &acceptance), &acceptance, now)
        .unwrap()
}

fn policy(store: &SqliteAuthorizationStore, admin: &str, command: PolicyMutation, now: i64) -> u64 {
    store
        .apply_policy(&mutation(admin, &command), &command, now)
        .unwrap()
}

fn approve(
    store: &SqliteAuthorizationStore,
    principal: &str,
    read: &str,
    write: &str,
    now: i64,
) -> String {
    let login = store
        .record_verified_login(
            &VerifiedLogin {
                principal_id: principal.into(),
                provider_id: None,
                upstream_issuer: None,
                upstream_subject: None,
                verified_claims: None,
                expires_at: now + 3600,
            },
            now,
        )
        .unwrap();
    let approval = grant_request(principal, &login, read, write, now);
    store
        .approve_grant(&mutation(principal, &approval), &approval, now)
        .unwrap()
}

fn grant_request(principal: &str, login: &str, read: &str, write: &str, now: i64) -> GrantApproval {
    // RFC 7638 public P-256 thumbprint; private material never enters Auth.
    let jwk = serde_json::json!({"kty":"EC","crv":"P-256","x":"f83OJ3D2xF4tH8Ew7k6N4dQx7mD2GxLyD6uZJq9m7tQ","y":"x_FEzRu9m36HLN1qrnUbcS1v6sV_4rxGO6ZGfStUdSU"});
    let jkt = URL_SAFE_NO_PAD.encode(Sha256::digest(
        super::sqlite::json(&jwk).unwrap().as_bytes(),
    ));
    GrantApproval {
        principal_id: principal.into(),
        login_session_id: login.into(),
        client_id: "native-client".into(),
        binding_kind: "native".into(),
        binding_value: jkt.clone(),
        durable_dpop_jkt: jkt,
        public_jwk: jwk,
        selection: CapabilitySelection {
            required: vec![read.into()],
            optional: vec![write.into()],
        },
        approved: vec![read.into(), write.into()],
        reused_remembered: vec![],
        declined: vec![],
        privileges: vec![],
        remember: true,
        remember_until: None,
        expires_at: now + 3600,
    }
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[42; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .bootstrap_administrator("admin", "a strong bootstrap password", NOW)
        .unwrap();
    let client = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Native client".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![],
        eligible_privileges: vec![],
        metadata: serde_json::json!({}),
        expected_revision: 0,
    };
    store
        .register_client(&mutation(&admin, &client), &client, NOW)
        .unwrap();
    let catalog = accept(&store, &admin, SOURCE, false, NOW);
    let api = catalog.api_id;
    let read = catalog
        .capabilities
        .iter()
        .find(|cap| cap.capability_id.ends_with("::read"))
        .unwrap()
        .capability_id
        .clone();
    let write = catalog
        .capabilities
        .iter()
        .find(|cap| cap.capability_id.ends_with("::write"))
        .unwrap()
        .capability_id
        .clone();
    let principal = store
        .create_principal(&mutation(&admin, &"user"), "user", NOW)
        .unwrap();
    for capability in [&read, &write] {
        policy(
            &store,
            &admin,
            PolicyMutation::CapabilityGrant {
                principal_id: principal.clone(),
                capability_id: capability.clone(),
                expected_revision: 0,
                expires_at: None,
            },
            NOW,
        );
    }
    let grant = approve(&store, &principal, &read, &write, NOW);
    let admission = AdmissionIdentity {
        credential: Credential::OAuthGrant(grant.clone()),
        runtime_id: URL_SAFE_NO_PAD.encode([7; 16]),
        session_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&[9; 32]).verifying_key().as_bytes()),
        inbox_prefix: "_INBOX.phaseTwo".into(),
    };
    Fixture {
        directory,
        store,
        admin,
        principal,
        api,
        read,
        write,
        grant,
        admission,
    }
}

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
        let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
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
            let renewed = f
                .store
                .issue_authority(&f.admission, Some(session), NOW + 2)
                .unwrap();
            assert_eq!(
                renewed.authority.unsigned.authorization_session_id,
                *session
            );
            assert!(renewed.authority.unsigned.expires_at > NOW + 10);
        } else {
            let cutoff = f.store.resolve_revocation(session).unwrap().unwrap();
            assert_eq!(cutoff.effective_cutoff, NOW + 1);
            assert!(matches!(
                f.store
                    .issue_authority(&f.admission, Some(session), NOW + 2),
                Err(AuthError::Retired)
            ));
            let replacement = f
                .store
                .issue_authority(&f.admission, None, NOW + 2)
                .unwrap();
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
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    while f.store.enforce_next_page(NOW).unwrap().is_some() {}
    let login = f
        .store
        .record_verified_login(
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
        f.store
            .issue_authority(&f.admission, Some(session), NOW + 1),
        Err(AuthError::Retired)
    ));
    let replacement = f
        .store
        .issue_authority(&f.admission, None, NOW + 1)
        .unwrap();
    assert!(!replacement
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.write));
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
        .register_client(&mutation(&f.admin, &registration), &registration, NOW)
        .unwrap();
    for privilege in [ApisAccept, ApisForceReplace] {
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::PrivilegeAssign {
                principal_id: f.principal.clone(),
                privilege,
                expected_revision: 0,
            },
            NOW,
        );
    }
    let login = f
        .store
        .record_verified_login(
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
        .unwrap();
    let mut approval = grant_request(&f.principal, &login, &f.read, &f.write, NOW);
    approval.privileges = vec![ApisAccept, ApisForceReplace];
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.credential = Credential::OAuthGrant(grant);
    let issued = f.store.issue_authority(&admission, None, NOW).unwrap();
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
    let replacement = f.store.issue_authority(&admission, None, NOW + 1).unwrap();
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
fn explicit_one_off_approval_overrides_old_choices_but_honors_later_withdrawal() {
    for expired in [false, true] {
        let f = fixture();
        let login = f
            .store
            .record_verified_login(
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
        admission.credential = Credential::OAuthGrant(grant);
        let issued = f.store.issue_authority(&admission, None, now).unwrap();
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
                f.store.issue_authority(&previous, None, now),
                Err(AuthError::RequiredMissing)
            ));
        } else {
            let previous = f.store.issue_authority(&previous, None, now).unwrap();
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
        admission.credential = Credential::OAuthGrant(fresh_grant);
        let fresh = f.store.issue_authority(&admission, None, now).unwrap();
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
        .record_verified_login(
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
    admission.credential = Credential::OAuthGrant(grant);
    let issued = f.store.issue_authority(&admission, None, NOW).unwrap();
    assert_eq!(issued.authority.unsigned.expires_at, NOW + 10);
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    assert!(f
        .store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_none());
    assert!(matches!(
        f.store.issue_authority(
            &admission,
            Some(&issued.authority.unsigned.authorization_session_id),
            NOW + 11
        ),
        Err(AuthError::Retired)
    ));
}

#[test]
fn trusted_native_device_provisioning_issues_authority_and_replays_its_identity() {
    let f = fixture();
    let proposed = evidence(&SOURCE.replace("service Reader", "device Reader"));
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
        participant_id: participant.clone(),
        expected_api_generations: BTreeMap::new(),
        identity_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&[11; 32]).verifying_key().as_bytes()),
        resource_commitments: serde_json::json!({}),
        expires_at: None,
    };
    let command = mutation(&f.admin, &provisioning);
    let provisioned = f
        .store
        .provision_deployment(&command, &provisioning, NOW)
        .unwrap();
    let replayed = f
        .store
        .provision_deployment(&command, &provisioning, NOW)
        .unwrap();
    assert_eq!(replayed.identity_key_id, provisioned.identity_key_id);
    let mut admission = f.admission.clone();
    admission.credential = Credential::ProvisionedIdentity(provisioned.identity_key_id.clone());
    let issued = f.store.issue_authority(&admission, None, NOW).unwrap();
    assert_eq!(
        issued.authority.unsigned.principal_kind,
        trellis_protocol::PrincipalKind::Device
    );
    assert_eq!(
        issued.authority.unsigned.principal_id,
        provisioned.principal_id
    );
    assert_eq!(
        issued.authority.unsigned.binding,
        trellis_protocol::SessionBinding::Device {
            deployment_id: provisioned.deployment_id,
            instance_id: provisioned.instance_id,
            participant_id: participant,
        }
    );
    let scope = EnforcementScope::Identity(provisioned.identity_key_id);
    f.store
        .revoke_root(&mutation(&f.admin, &scope), &scope, NOW + 1)
        .unwrap();
    assert!(matches!(
        f.store.issue_authority(&admission, None, NOW + 1),
        Err(AuthError::Denied)
    ));
}

#[test]
fn issuer_revocation_uses_only_issuer_privilege_and_preserves_other_scope_gates() {
    let f = fixture();
    let delegate = f
        .store
        .create_principal(&mutation(&f.admin, &"user"), "user", NOW)
        .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::PrivilegeAssign {
            principal_id: delegate.clone(),
            privilege: trellis_protocol::PlatformPrivilege::PrivilegesManage,
            expected_revision: 0,
        },
        NOW,
    );
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let previous = f.store.issuer();
    let next = SigningKey::from_bytes(&[99; 32]);
    let public = URL_SAFE_NO_PAD.encode(next.verifying_key().as_bytes());
    f.store
        .rotate_issuer(&mutation(&delegate, &public.as_str()), &public, NOW + 1)
        .unwrap();
    drop(f.store);
    let store = open(&f.directory, next);
    let grant_scope = EnforcementScope::Grant(f.grant);
    assert!(matches!(
        store.revoke_root(&mutation(&delegate, &grant_scope), &grant_scope, NOW + 2),
        Err(AuthError::Denied)
    ));
    let current_scope = EnforcementScope::Issuer(store.issuer().key_id);
    assert!(matches!(
        store.revoke_root(
            &mutation(&delegate, &current_scope),
            &current_scope,
            NOW + 2
        ),
        Err(AuthError::Conflict)
    ));
    let scope = EnforcementScope::Issuer(previous.key_id.clone());
    store
        .revoke_root(&mutation(&delegate, &scope), &scope, NOW + 2)
        .unwrap();
    assert_eq!(
        store.resolve_issuer(&previous.key_id).unwrap().state,
        trellis_protocol::AuthorityIssuerState::Revoked
    );
    while store.enforce_next_page(NOW + 2).unwrap().is_some() {}
    assert!(store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
    assert!(matches!(
        store.issue_authority(
            &f.admission,
            Some(&issued.authority.unsigned.authorization_session_id),
            NOW + 2
        ),
        Err(AuthError::Retired)
    ));
    assert!(store.issue_authority(&f.admission, None, NOW + 2).is_ok());
}

#[test]
fn latest_verified_oidc_claims_and_freshness_bound_issuance_without_removing_direct_rights() {
    let f = fixture();
    let provider = super::lifecycle::OidcProviderRegistration {
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
        .register_oidc_provider(&mutation(&f.admin, &provider), &provider, NOW)
        .unwrap();
    f.store
        .link_oidc_identity(
            &mutation(
                &f.admin,
                &(
                    f.principal.as_str(),
                    provider.provider_id.as_str(),
                    "upstream-user",
                ),
            ),
            &f.principal,
            &provider.provider_id,
            "upstream-user",
            NOW,
        )
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
            role_id: role,
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
    let login_id = f.store.record_verified_login(&login, NOW).unwrap();
    let request = grant_request(&f.principal, &login_id, &f.read, &f.write, NOW);
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &request), &request, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.credential = Credential::OAuthGrant(grant);
    let first = f.store.issue_authority(&admission, None, NOW).unwrap();
    assert_eq!(first.authority.unsigned.expires_at, NOW + 5);
    login.verified_claims = Some(serde_json::json!({"groups":[]}));
    f.store.record_verified_login(&login, NOW + 1).unwrap();
    assert!(matches!(
        f.store.issue_authority(
            &admission,
            Some(&first.authority.unsigned.authorization_session_id),
            NOW + 1
        ),
        Err(AuthError::Retired)
    ));
    login.verified_claims = Some(serde_json::json!({"groups":["readers"]}));
    f.store.record_verified_login(&login, NOW + 2).unwrap();
    let second = f.store.issue_authority(&admission, None, NOW + 2).unwrap();
    assert_ne!(
        second.authority.unsigned.authorization_session_id,
        first.authority.unsigned.authorization_session_id
    );
    assert!(matches!(
        f.store.issue_authority(&admission, None, NOW + 8),
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
    let direct = f.store.issue_authority(&admission, None, NOW + 8).unwrap();
    assert_eq!(direct.authority.unsigned.expires_at, NOW + 68);
    assert!(direct
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|cap| cap.capability_id == f.read));
}

#[test]
fn declined_and_unknown_optional_capabilities_do_not_expand_at_renewal() {
    let f = fixture();
    let original = f.store.issue_authority(&f.admission, None, NOW).unwrap();
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
    let unknown = "phase-two/absent@v1::optional".to_owned();
    request.selection.optional.push(unknown.clone());
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &request), &request, NOW)
        .unwrap();
    let mut admission = f.admission.clone();
    admission.credential = Credential::OAuthGrant(grant);
    admission.runtime_id = URL_SAFE_NO_PAD.encode([8; 16]);
    let first = f.store.issue_authority(&admission, None, NOW).unwrap();
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
    assert!(first.optional_unavailable.contains(&f.write));
    assert!(first.optional_unavailable.contains(&unknown));
    let renewed = f
        .store
        .issue_authority(
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
    let initial = f.store.issue_authority(&provider, None, NOW).unwrap();
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
            &f.store.issuer(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "account-phase-two",
            NOW,
        )
        .unwrap();
    let expansion = SOURCE.replace(
        "rpc Export",
        "rpc Archive { input Document; output Document; } rpc Export",
    );
    accept(&f.store, &f.admin, &expansion, false, NOW + 1);
    let compatible = f
        .store
        .issue_authority(
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
                    f.store.issue_authority(
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
        f.store.issue_authority(&provider, None, NOW + 2),
        Err(AuthError::Denied)
    ));
    let user = f
        .store
        .issue_authority(&f.admission, None, NOW + 2)
        .unwrap();
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
    assert!(user.optional_unavailable.contains(&f.write));
    assert_eq!(user.authority.unsigned.apis[0].generation.get(), 2);
}

#[test]
fn rotation_authenticates_the_new_pin_and_rejects_forgery_and_rollback() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let previous = f.store.issuer();
    let next = SigningKey::from_bytes(&[99; 32]);
    let public = URL_SAFE_NO_PAD.encode(next.verifying_key().as_bytes());
    let rotation = f
        .store
        .rotate_issuer(&mutation(&f.admin, &public.as_str()), &public, NOW + 1)
        .unwrap();
    let installed = rotation
        .verify_successor(
            &previous,
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "account-phase-two",
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
            "account-phase-two",
            0,
            NOW + 1
        )
        .is_err());
    assert!(rotation
        .verify_successor(
            &installed,
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "account-phase-two",
            1,
            NOW + 1
        )
        .is_err());
    assert!(matches!(
        f.store.issue_authority(&f.admission, None, NOW + 1),
        Err(AuthError::Denied)
    ));
    let historical = f.store.resolve_issuer(&previous.key_id).unwrap();
    assert_eq!(
        historical.state,
        trellis_protocol::AuthorityIssuerState::Retired
    );
    drop(f.store);
    let restarted = open(&f.directory, next);
    let renewed = restarted
        .issue_authority(
            &f.admission,
            Some(&issued.authority.unsigned.authorization_session_id),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(renewed.authority.unsigned.issuer_key_id, installed.key_id);
    assert_eq!(
        restarted.resolve_authority(&issued.digest).unwrap(),
        issued.authority
    );
}

#[tokio::test]
async fn real_broker_publishes_signed_cutoff_before_closing_only_the_matching_attachment() {
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let attachment = id();
    let marker = format!("{}{attachment}", super::broker::ATTACHMENT_MARKER_PREFIX);
    let directory = tempfile::tempdir().unwrap();
    let listeners =
        std::array::from_fn::<_, 3, _>(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap());
    let [port, http, websocket] = listeners
        .each_ref()
        .map(|socket| socket.local_addr().unwrap().port());
    let config = directory.path().join("nats.conf");
    std::fs::write(&config,format!("port: {port}\nhttp_port: {http}\nwebsocket {{ port: {websocket}, no_tls: true }}\njetstream {{ store_dir: '{}' }}\naccounts {{ SYS {{ users: [{{user: sys,password: pw}}] }}, APP {{ jetstream: enabled, users: [{{user: '{marker}',password: pw}},{{user: control,password: pw}}] }} }}\nsystem_account: SYS\n",directory.path().join("jetstream").display())).unwrap();
    drop(listeners);
    // CI supplies its producer-resolved executable. There is no rebuilt runtime,
    // fake broker, test-support feature, readiness bypass, or hidden skip.
    let binary =
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
    let mut server = tokio::process::Command::new(binary)
        .arg("-c")
        .arg(&config)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("set TRELLIS_NATS_SERVER_BIN to the producer-resolved NATS executable");
    let mut output = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = output.next_line().await.unwrap() {
            eprintln!("{line}");
            if line.contains("Server is ready") {
                return;
            }
        }
        panic!("NATS exited before readiness");
    })
    .await
    .expect("NATS readiness deadline");
    let url = format!("nats://127.0.0.1:{port}");
    let system = async_nats::ConnectOptions::with_user_and_password("sys".into(), "pw".into())
        .connect(&url)
        .await
        .unwrap();
    let closed = std::sync::Arc::new(tokio::sync::Notify::new());
    let notified = closed.clone();
    let app = async_nats::ConnectOptions::with_user_and_password(marker.clone(), "pw".into())
        .max_reconnects(0)
        .event_callback(move |event| {
            let notified = notified.clone();
            async move {
                if matches!(event, async_nats::Event::Disconnected) {
                    notified.notify_one();
                }
            }
        })
        .connect(&url)
        .await
        .unwrap();
    let publisher =
        async_nats::ConnectOptions::with_user_and_password("control".into(), "pw".into())
            .connect(&url)
            .await
            .unwrap();
    let jetstream = async_nats::jetstream::new(publisher.clone());
    let material = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: format!("material_{}", id()),
            ..Default::default()
        })
        .await
        .unwrap();
    let revocations = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: format!("revocations_{}", id()),
            limit_markers: Some(Duration::from_secs(10)),
            ..Default::default()
        })
        .await
        .unwrap();
    let server_id = app.server_info().server_id;
    let inventory = system
        .request(
            format!("$SYS.REQ.SERVER.{server_id}.CONNZ"),
            serde_json::to_vec(&serde_json::json!({"auth":true}))
                .unwrap()
                .into(),
        )
        .await
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.payload).unwrap();
    let cid = inventory["data"]["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connection| connection["authorized_user"] == marker)
        .unwrap()["cid"]
        .as_u64()
        .unwrap();
    super::broker::close_attachment(&system, &server_id, cid, &id())
        .await
        .unwrap();
    app.flush().await.unwrap(); // A reused CID is not authority to kick this user.
    assert!(
        super::broker::close_attachment(&system, "UNKNOWN_SERVER", cid, &attachment)
            .await
            .is_err()
    );
    let admission = Attachment {
        attachment_id: attachment.clone(),
        server_id: server_id.clone(),
        broker_client_id: cid,
        authorization_session_id: issued.authority.unsigned.authorization_session_id.clone(),
        ephemeral_nkey: nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
            .unwrap()
            .public_key(),
        transport_policy_digest: digest(&issued.authority).unwrap(),
    };
    let mut impostor = admission.clone();
    impostor.ephemeral_nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [11; 32])
        .unwrap()
        .public_key();
    assert!(matches!(
        f.store.admit_attachment(&impostor, NOW),
        Err(AuthError::Denied)
    ));
    f.store.admit_attachment(&admission, NOW).unwrap();
    drain(&f.store, NOW);
    f.store
        .process_effects(&material, &revocations, &system, NOW)
        .await
        .unwrap();
    let published = material
        .get(format!("authority.{}", issued.digest))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        trellis_protocol::parse_session_authority(&published, 65536).unwrap(),
        issued.authority
    );
    let session = issued.authority.unsigned.authorization_session_id.clone();
    let mut watcher = revocations.watch_with_history(&session).await.unwrap();
    let scope = EnforcementScope::Session(session.clone());
    f.store
        .revoke_root(&mutation(&f.admin, &scope), &scope, NOW + 1)
        .unwrap();
    f.store
        .process_effects(&material, &revocations, &system, NOW + 1)
        .await
        .unwrap();
    use futures_util::StreamExt;
    let update = tokio::time::timeout(Duration::from_secs(5), watcher.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let signed: trellis_protocol::SignedSessionRevocation =
        serde_json::from_slice(&update.value).unwrap();
    signed
        .verify(
            &f.store.issuer(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "account-phase-two",
        )
        .unwrap();
    assert_eq!(signed.effective_cutoff, NOW + 1);
    let mut cache = super::revocation::RevocationCache::new(
        "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        "account-phase-two".into(),
        1,
    )
    .unwrap();
    let mut forged = signed.clone();
    forged.effective_cutoff = NOW;
    assert!(cache.observe(forged, &f.store.issuer(), NOW + 1).is_err());
    let issuer = f.store.issuer();
    let policy = SessionAuthorityVerificationPolicy {
        now_unix_seconds: NOW + 1,
        allowed_clock_skew_seconds: 5,
        maximum_authority_lifetime_seconds: 60,
        maximum_authority_bytes: 65536,
        maximum_entries: 256,
    };
    let verify = |cutoff| {
        verify_session_authority(SessionAuthorityVerificationInput {
            issuer: &issuer,
            trellis_instance_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            audience_nats_account: "account-phase-two",
            authority: &issued.authority,
            policy: &policy,
            purpose: SessionAuthorityPurpose::Live,
            revocation_cutoff: cutoff,
        })
    };
    assert!(verify(cache.cutoff(&session, NOW + 1)).is_ok());
    cache.observe(signed, &issuer, NOW + 1).unwrap();
    assert!(verify(cache.cutoff(&session, NOW + 1)).is_err());
    app.flush().await.unwrap(); // Publication has completed, kick is still pending.
    f.store
        .process_effects(&material, &revocations, &system, NOW + 1)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), closed.notified())
        .await
        .unwrap();
    let inventory = system
        .request(
            format!("$SYS.REQ.SERVER.{server_id}.CONNZ"),
            serde_json::to_vec(&serde_json::json!({"auth":true,"cid":cid}))
                .unwrap()
                .into(),
        )
        .await
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.payload).unwrap();
    assert!(inventory["data"]["connections"]
        .as_array()
        .unwrap()
        .is_empty());
    publisher.flush().await.unwrap();
    assert_eq!(
        revocations.get(&session).await.unwrap().unwrap().as_ref(),
        update.value.as_ref()
    );
    revocations.purge(&session).await.unwrap();
    assert!(revocations.get(&session).await.unwrap().is_none());
    assert!(verify(cache.cutoff(&session, NOW + 1)).is_err());
    assert!(
        super::broker::close_attachment(&system, &server_id, cid, &attachment)
            .await
            .is_ok()
    );
    server.kill().await.unwrap();
}

fn drain(store: &SqliteAuthorizationStore, now: i64) {
    while store.enforce_next_page(now).unwrap().is_some() {}
}

#[test]
fn bounded_revocation_cache_never_evicts_live_cutoffs_and_reclaims_only_expired_entries() {
    let f = fixture();
    let mut admission = f.admission.clone();
    let first = f.store.issue_authority(&admission, None, NOW).unwrap();
    admission.runtime_id = URL_SAFE_NO_PAD.encode([8; 16]);
    let second = f.store.issue_authority(&admission, None, NOW).unwrap();
    for issued in [&first, &second] {
        let scope =
            EnforcementScope::Session(issued.authority.unsigned.authorization_session_id.clone());
        f.store
            .revoke_root(&mutation(&f.admin, &scope), &scope, NOW + 1)
            .unwrap();
    }
    let first_id = &first.authority.unsigned.authorization_session_id;
    let second_id = &second.authority.unsigned.authorization_session_id;
    let mut cache = super::revocation::RevocationCache::new(
        "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        "account-phase-two".into(),
        1,
    )
    .unwrap();
    let issuer = f.store.issuer();
    cache
        .observe(
            f.store.resolve_revocation(first_id).unwrap().unwrap(),
            &issuer,
            NOW + 2,
        )
        .unwrap();
    assert!(matches!(
        cache.observe(
            f.store.resolve_revocation(second_id).unwrap().unwrap(),
            &issuer,
            NOW + 2
        ),
        Err(AuthError::Unavailable)
    ));
    assert_eq!(cache.cutoff(first_id, NOW + 2), Some(NOW + 1));
    admission.runtime_id = URL_SAFE_NO_PAD.encode([9; 16]);
    let third = f.store.issue_authority(&admission, None, NOW + 60).unwrap();
    let third_id = &third.authority.unsigned.authorization_session_id;
    let scope = EnforcementScope::Session(third_id.clone());
    f.store
        .revoke_root(&mutation(&f.admin, &scope), &scope, NOW + 61)
        .unwrap();
    cache
        .observe(
            f.store.resolve_revocation(third_id).unwrap().unwrap(),
            &issuer,
            NOW + 66,
        )
        .unwrap();
    assert_eq!(cache.cutoff(third_id, NOW + 66), Some(NOW + 61));
    // Cache expiry does not destroy durable historical verification evidence.
    f.store
        .resolve_revocation(first_id)
        .unwrap()
        .unwrap()
        .verify(&issuer, "01ARZ3NDEKTSV4RRFFQ69G5FAV", "account-phase-two")
        .unwrap();
}

#[test]
fn issues_verifiable_authority_and_retains_it_after_restart_and_expiry() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let issuer = f.store.issuer();
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
        audience_nats_account: "account-phase-two",
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
        restarted.resolve_authority(&issued.digest).unwrap(),
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
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
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
    assert!(f
        .store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
    assert!(matches!(
        f.store.issue_authority(&f.admission, None, NOW + 2),
        Err(AuthError::RequiredMissing)
    ));
}

#[test]
fn optional_reduction_requires_new_logical_id_and_old_admission_stays_closed() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let session = &issued.authority.unsigned.authorization_session_id;
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
    // Admission checks current SQL before the durable scan has run.
    let attachment = Attachment {
        attachment_id: id(),
        server_id: "server".into(),
        broker_client_id: 1,
        authorization_session_id: session.clone(),
        ephemeral_nkey: nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
            .unwrap()
            .public_key(),
        transport_policy_digest: issued.digest.clone(),
    };
    assert!(matches!(
        f.store.admit_attachment(&attachment, NOW + 1),
        Err(AuthError::Retired)
    ));
    assert!(matches!(
        f.store
            .issue_authority(&f.admission, Some(session), NOW + 1),
        Err(AuthError::Retired)
    ));
    let replacement = f
        .store
        .issue_authority(&f.admission, None, NOW + 1)
        .unwrap();
    assert_ne!(
        replacement.authority.unsigned.authorization_session_id,
        *session
    );
    assert_eq!(replacement.authority.unsigned.capabilities.len(), 1);
    assert_eq!(replacement.optional_unavailable, vec![f.write]);
}

#[test]
fn concurrent_acceptance_preserves_coherent_authority_and_old_membership_limits() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let updated = SOURCE.replace("allows { rpc Get; }", "allows { rpc Get; rpc Export; }");
    let start = std::sync::Barrier::new(2);
    let (catalog, concurrent) = std::thread::scope(|threads| {
        let issuer = threads.spawn(|| {
            start.wait();
            (0..32)
                .map(|_| {
                    f.store
                        .issue_authority(
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
            &f.store.issuer(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "account-phase-two",
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
                &f.store.issuer(),
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "account-phase-two",
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
    let renewed = f
        .store
        .issue_authority(
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
    let manager = f
        .store
        .create_principal(&mutation(&f.admin, &"user"), "user", NOW)
        .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::PrivilegeAssign {
            principal_id: manager.clone(),
            privilege: trellis_protocol::PlatformPrivilege::ApisAccept,
            expected_revision: 0,
        },
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
fn hard_grant_revocation_blocks_offline_reissue_and_preserves_signed_cutoff() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let scope = EnforcementScope::Grant(f.grant.clone());
    let identity = mutation(&f.admin, &scope);
    let work = f.store.revoke_root(&identity, &scope, NOW + 1).unwrap();
    assert_eq!(
        f.store.revoke_root(&identity, &scope, NOW + 2).unwrap(),
        work
    );
    assert!(matches!(
        f.store.issue_authority(&f.admission, None, NOW + 1),
        Err(AuthError::Denied)
    ));
    drop(f.store);
    let restarted = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    drain(&restarted, NOW + 1);
    let revocation = restarted
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .unwrap();
    revocation
        .verify(
            &restarted.issuer(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "account-phase-two",
        )
        .unwrap();
    assert_eq!(revocation.effective_cutoff, NOW + 1);
    restarted.prune_expired_artifacts(NOW + 10000).unwrap();
    assert!(restarted
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
}

#[test]
fn outbox_orders_revocation_before_kicks_and_fences_expired_claims() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    f.store
        .admit_attachment(
            &Attachment {
                attachment_id: id(),
                server_id: "offline-broker".into(),
                broker_client_id: 8,
                authorization_session_id: issued
                    .authority
                    .unsigned
                    .authorization_session_id
                    .clone(),
                ephemeral_nkey: nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
                    .unwrap()
                    .public_key(),
                transport_policy_digest: issued.digest.clone(),
            },
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
    drain(&f.store, NOW + 1);
    let effects = f.store.claim_effects(256, NOW + 1, 2).unwrap();
    assert!(!effects.iter().any(|effect| effect.kind == "kick"));
    let revoke = effects
        .iter()
        .find(|effect| effect.kind == "session_revoke")
        .unwrap();
    assert!(matches!(
        f.store.complete_effect(revoke, NOW + 3),
        Err(AuthError::Conflict)
    ));
    drop(f.store);
    let restarted = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let effects = restarted.claim_effects(256, NOW + 3, 10).unwrap();
    let revoke = effects
        .iter()
        .find(|effect| effect.kind == "session_revoke")
        .unwrap();
    restarted.complete_effect(revoke, NOW + 3).unwrap();
    let effects = restarted.claim_effects(256, NOW + 3, 10).unwrap();
    let kick = effects.iter().find(|effect| effect.kind == "kick").unwrap();
    restarted.retry_effect(kick, NOW + 3).unwrap();
    assert!(restarted
        .claim_effects(256, NOW + 3, 10)
        .unwrap()
        .is_empty());
    let retried = restarted.claim_effects(256, NOW + 4, 10).unwrap();
    restarted
        .complete_effect(
            retried.iter().find(|effect| effect.kind == "kick").unwrap(),
            NOW + 4,
        )
        .unwrap();
}
