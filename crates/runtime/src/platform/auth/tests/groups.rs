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

#[test]
fn an_app_keeps_the_members_its_user_consented_to_until_asked_again() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read"]), false, NOW);
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
    let grant = approve_editor(&f, &user, NOW);
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
    // The user holds the grown group; the app keeps what was approved.
    assert!(holds(&f, &user, &f.write));
    assert_eq!(app_holds(&f, grant, NOW + 1), std::slice::from_ref(&f.read));
    let regranted = approve_editor(&f, &user, NOW + 2);
    let mut both = vec![f.read.clone(), f.write.clone()];
    both.sort();
    assert_eq!(app_holds(&f, regranted, NOW + 2), both);
}

#[test]
fn an_app_loses_a_group_member_that_now_allows_more_until_asked_again() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read"]), false, NOW);
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
    let grant = approve_editor(&f, &user, NOW);
    assert_eq!(
        app_holds(&f, grant.clone(), NOW),
        std::slice::from_ref(&f.read)
    );
    // Reading now also exports: the user approved less than that.
    let widened =
        grouped(&["read"]).replace("allows { rpc Get; }", "allows { rpc Get; rpc Export; }");
    accept(&f.store, &f.admin, &widened, false, NOW + 1);
    assert!(holds(&f, &user, &f.read));
    assert!(app_holds(&f, grant, NOW + 1).is_empty());
    let regranted = approve_editor(&f, &user, NOW + 2);
    assert_eq!(
        app_holds(&f, regranted, NOW + 2),
        std::slice::from_ref(&f.read)
    );
}

#[test]
fn rewording_a_group_member_keeps_the_app_approval() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(&["read"]), false, NOW);
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
    let grant = approve_editor(&f, &user, NOW);
    let reworded = grouped(&["read"]).replace("Sees contents.", "Sees document contents.");
    accept(&f.store, &f.admin, &reworded, false, NOW + 1);
    assert_eq!(app_holds(&f, grant, NOW + 1), std::slice::from_ref(&f.read));
}
