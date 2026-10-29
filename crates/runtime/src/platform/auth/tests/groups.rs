use super::*;

/// The fixture API with an `editor` group of the given members.
fn grouped(members: &[&str]) -> String {
    let members: String = members
        .iter()
        .map(|member| format!("capability documents.{member}; "))
        .collect();
    SOURCE.replace(
        "\n }\n}\nservice Reader",
        &format!(
            "\n  group editor {{ title \"Editor\"; description \"Edit documents.\"; \
            grants \"Changes documents.\"; {members}}}\
            \n }}\n}}\nservice Reader"
        ),
    )
}

fn editor(f: &Fixture) -> String {
    f.read.replace(":documents.read", ":editor")
}

fn holds(f: &Fixture, principal: &str, capability: &str) -> bool {
    f.store
        .read(|connection| super::super::policy::entitlements(connection, principal, None, NOW))
        .unwrap()
        .contains_key(capability)
}

fn approve_editor(f: &Fixture, principal: &str, now: i64) -> String {
    let login = f
        .store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
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
        })
        .unwrap();
    let mut approval = grant_request(principal, &login, &f.read, &f.write, now);
    approval.selection.required.clear();
    approval.selection.optional = vec![editor(f)];
    approval.approved = vec![editor(f)];
    approval.remember = false;
    f.store
        .approve_grant(&mutation(principal, &approval), &approval, now)
        .unwrap()
}

fn app_holds(f: &Fixture, grant: String, now: i64) -> Vec<String> {
    let mut admission = f.admission.clone();
    admission.runtime_id = URL_SAFE_NO_PAD.encode(ulid::Ulid::new().to_bytes());
    admission.refresh_family_id = Some(oauth_family(&f.store, &grant, now, now + 86400));
    admission.credential = Credential::OAuthGrant(grant);
    issue_authority(&f.store, &admission, None, now)
        .unwrap()
        .authority
        .unsigned
        .capabilities
        .into_iter()
        .map(|held| held.capability_id)
        .collect()
}

#[test]
fn holding_a_group_follows_its_membership_live() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read", "write"]), false, NOW);
    let user = create_user(&f.store, &f.admin, NOW);
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: user.clone(),
            capability_id: editor(&f),
            expected_revision: 0,
            expires_at: None,
        },
        NOW,
    );
    assert!(holds(&f, &user, &f.read) && holds(&f, &user, &f.write));
    // Removing a member reaches holders at once.
    accept(&f.store, &f.admin, &grouped(&["read"]), false, NOW + 1);
    assert!(holds(&f, &user, &f.read));
    assert!(!holds(&f, &user, &f.write));
    // So does adding one.
    accept(
        &f.store,
        &f.admin,
        &grouped(&["read", "write"]),
        false,
        NOW + 2,
    );
    assert!(holds(&f, &user, &f.write));
}

/// Grant `editor` to a new user and approve an app for it.
fn approved_editor(f: &Fixture) -> (String, String) {
    let user = create_user(&f.store, &f.admin, NOW);
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: user.clone(),
            capability_id: editor(f),
            expected_revision: 0,
            expires_at: None,
        },
        NOW,
    );
    let grant = approve_editor(f, &user, NOW);
    (user, grant)
}

#[test]
fn an_approved_app_gets_a_group_s_new_members_while_its_grants_hold() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read"]), false, NOW);
    let (_, grant) = approved_editor(&f);
    assert_eq!(
        app_holds(&f, grant.clone(), NOW),
        std::slice::from_ref(&f.read)
    );
    accept(
        &f.store,
        &f.admin,
        &grouped(&["read", "write"]),
        false,
        NOW + 1,
    );
    let mut both = vec![f.read.clone(), f.write.clone()];
    both.sort();
    assert_eq!(app_holds(&f, grant, NOW + 1), both);
}

#[test]
fn changing_a_group_s_grants_needs_the_user_to_approve_again() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read"]), false, NOW);
    let (user, grant) = approved_editor(&f);
    let regranted = grouped(&["read"]).replace("Changes documents.", "Publishes documents.");
    accept(&f.store, &f.admin, &regranted, false, NOW + 1);
    // The user still holds the group; the app's approval no longer covers it.
    assert!(holds(&f, &user, &f.read));
    assert!(app_holds(&f, grant, NOW + 1).is_empty());
    let renewed = approve_editor(&f, &user, NOW + 2);
    assert_eq!(
        app_holds(&f, renewed, NOW + 2),
        std::slice::from_ref(&f.read)
    );
}

#[test]
fn a_member_s_new_grants_do_not_reopen_an_approved_group() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read"]), false, NOW);
    let (_, grant) = approved_editor(&f);
    let reworded = grouped(&["read"]).replace("Sees contents.", "Sees confidential contents.");
    accept(&f.store, &f.admin, &reworded, false, NOW + 1);
    assert_eq!(app_holds(&f, grant, NOW + 1), std::slice::from_ref(&f.read));
}

#[test]
fn effective_access_traces_a_group_s_members_to_the_group_s_grant() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read", "write"]), false, NOW);
    let user = create_user(&f.store, &f.admin, NOW);
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: user.clone(),
            capability_id: editor(&f),
            expected_revision: 0,
            expires_at: None,
        },
        NOW,
    );
    let access = f
        .store
        .transaction(|connection| {
            super::super::rpc_policy::read_effective_access(connection, &user, NOW)
        })
        .unwrap();
    let grant = |capability: &str| {
        access
            .capabilities
            .iter()
            .find(|held| held.definition.identity.capability_id.as_ref() == capability)
            .and_then(|held| held.sources[0].grant_id)
            .unwrap()
    };
    assert_eq!(grant(&f.read), grant(&editor(&f)));
    assert_eq!(grant(&f.write), grant(&editor(&f)));
}
