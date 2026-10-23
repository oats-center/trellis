use super::*;

/// The fixture API with an `editor` group at a revision and member list.
fn grouped(revision: u64, members: &[&str]) -> String {
    let members: String = members
        .iter()
        .map(|member| format!("capability documents.{member}; "))
        .collect();
    SOURCE.replace(
        "\n }\n}\nservice Reader",
        &format!(
            "\n  group editor {{ title \"Editor\"; description \"Edit documents.\"; \
            consequence \"Changes documents.\"; consent_revision {revision}; {members}}}\
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
    accept(
        &f.store,
        &f.admin,
        &grouped(1, &["read", "write"]),
        false,
        NOW,
    );
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
    accept(&f.store, &f.admin, &grouped(1, &["read"]), false, NOW + 1);
    assert!(holds(&f, &user, &f.read));
    assert!(!holds(&f, &user, &f.write));
    // So does adding one, with the consent revision raised.
    accept(
        &f.store,
        &f.admin,
        &grouped(2, &["read", "write"]),
        false,
        NOW + 2,
    );
    assert!(holds(&f, &user, &f.write));
}

#[test]
fn an_app_keeps_the_members_its_user_consented_to_until_asked_again() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(1, &["read"]), false, NOW);
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
    assert_eq!(app_holds(&f, grant.clone(), NOW), [f.read.clone()]);
    accept(
        &f.store,
        &f.admin,
        &grouped(2, &["read", "write"]),
        false,
        NOW + 1,
    );
    // The user holds the grown group; the app keeps what was approved.
    assert!(holds(&f, &user, &f.write));
    assert_eq!(app_holds(&f, grant, NOW + 1), [f.read.clone()]);
    let regranted = approve_editor(&f, &user, NOW + 2);
    let mut both = vec![f.read.clone(), f.write.clone()];
    both.sort();
    assert_eq!(app_holds(&f, regranted, NOW + 2), both);
}

#[test]
fn a_group_cannot_grow_without_a_new_consent_revision() {
    let f = fixture();
    accept(&f.store, &f.admin, &grouped(1, &["read"]), false, NOW);
    let proposed = evidence(&grouped(1, &["read", "write"]));
    let graph = trellis_idl::compile_evidence(proposed.clone()).unwrap();
    let api = graph
        .root_package()
        .apis()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let error = f
        .store
        .review_api(&f.admin, proposed, &api, NOW + 1)
        .unwrap_err();
    assert!(error.to_string().contains("consent_revision must increase"));
}
