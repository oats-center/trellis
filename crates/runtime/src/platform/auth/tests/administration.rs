use super::*;

fn login(f: &Fixture, principal: &str) -> String {
    f.store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: principal.into(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: NOW + 3600,
                },
                NOW,
            )
        })
        .unwrap()
}

/// A confirmed request from `principal` for just `trellis.auth.v1:roles.manage`.
fn request_roles(f: &Fixture, principal: &str) -> GrantApproval {
    let login = login(f, principal);
    let mut approval = grant_request(principal, &login, &f.read, &f.write, NOW);
    let roles = Administration::ManageRoles.capability_id().to_string();
    approval.selection.required.clear();
    approval.selection.optional = vec![roles.clone()];
    approval.approved = vec![roles];
    approval.administration_confirmed = true;
    approval.remember = false;
    approval
}

fn authority_holds(f: &Fixture, grant: String, capability: &str) -> bool {
    let mut admission = f.admission.clone();
    admission.runtime_id = URL_SAFE_NO_PAD.encode(ulid::Ulid::new().to_bytes());
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, NOW, NOW + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    issue_authority(&f.store, &admission, None, NOW)
        .unwrap()
        .authority
        .unsigned
        .capabilities
        .iter()
        .any(|held| held.capability_id == capability)
}

#[test]
fn the_default_administrator_holds_administration_without_grants_and_cannot_lose_it() {
    let f = fixture();
    let held = f
        .store
        .read(|connection| super::super::policy::entitlements(connection, &f.admin, None, NOW))
        .unwrap();
    for administration in Administration::ALL {
        assert_eq!(
            held[administration.capability_id()].sources,
            ["administrator"]
        );
    }
    // A direct grant adds a source; revoking it leaves the inherent one.
    administer(
        &f.store,
        &f.admin,
        &f.admin,
        &[Administration::ManageRoles],
        NOW,
    );
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityRevoke {
            principal_id: f.admin.clone(),
            capability_id: Administration::ManageRoles.capability_id().into(),
            expected_revision: 1,
        },
        NOW,
    );
    let held = f
        .store
        .read(|connection| super::super::policy::entitlements(connection, &f.admin, None, NOW))
        .unwrap();
    assert!(held.contains_key(Administration::ManageRoles.capability_id()));
}

#[test]
fn only_platform_managers_change_who_holds_administration() {
    let f = fixture();
    let manager = create_user(&f.store, &f.admin, NOW);
    administer(
        &f.store,
        &f.admin,
        &manager,
        &[Administration::ManageRoles],
        NOW,
    );
    let command = PolicyMutation::CapabilityGrant {
        principal_id: f.principal.clone(),
        capability_id: Administration::ManageRoles.capability_id().into(),
        expected_revision: 0,
        expires_at: None,
    };
    assert!(matches!(
        apply_policy(&f.store, &mutation(&manager, &command), &command, NOW),
        Err(AuthError::Denied)
    ));
    administer(
        &f.store,
        &f.admin,
        &manager,
        &[Administration::ManagePlatform],
        NOW,
    );
    apply_policy(&f.store, &mutation(&manager, &command), &command, NOW).unwrap();
}

#[test]
fn delegating_administration_requires_confirmation_and_a_registered_client() {
    let f = fixture();
    let mut approval = request_roles(&f, &f.admin);
    approval.administration_confirmed = false;
    assert!(matches!(
        f.store
            .approve_grant(&mutation(&f.admin, &approval), &approval, NOW),
        Err(AuthError::Denied)
    ));
    f.store
        .transaction(|connection| {
            connection.execute(
                "INSERT INTO auth_cimd_clients VALUES('https://app.example/client.json',\
                'native','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','{}',?1,?2)",
                rusqlite::params![NOW, NOW + 3600],
            )?;
            Ok(())
        })
        .unwrap();
    let mut approval = request_roles(&f, &f.admin);
    approval.client_id = "https://app.example/client.json".into();
    assert!(matches!(
        f.store
            .approve_grant(&mutation(&f.admin, &approval), &approval, NOW),
        Err(AuthError::Denied)
    ));
    let approval = request_roles(&f, &f.admin);
    let grant = f
        .store
        .approve_grant(&mutation(&f.admin, &approval), &approval, NOW)
        .unwrap();
    assert!(authority_holds(
        &f,
        grant,
        Administration::ManageRoles.capability_id()
    ));
}

#[test]
fn an_administrators_confirmed_delegation_reaches_the_app_at_once() {
    let f = fixture();
    administer(
        &f.store,
        &f.admin,
        &f.principal,
        &[Administration::ManageRoles],
        NOW,
    );
    let roles = Administration::ManageRoles.capability_id();
    let approval = request_roles(&f, &f.principal);
    let grant = f
        .store
        .approve_grant(&mutation(&f.principal, &approval), &approval, NOW)
        .unwrap();
    assert!(authority_holds(&f, grant, roles));
}
