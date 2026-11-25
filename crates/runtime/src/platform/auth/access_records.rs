//! Roles, role members, direct grants and app consent, written so that
//! another installation can recreate them.
//!
//! A service bundle carries those that name a capability of an API its
//! deployment provides; an installation export carries every role and grant.
//! Users are named by their Trellis user ID and their sign-ins. An import
//! finds each user by ID, or by the same OIDC issuer and subject, and
//! recreates what it can: a user it cannot find, a user someone else here is
//! in the way of, or a role that exists with another definition, is skipped
//! and reported, and the rest still applies. See
//! `design/auth/service-export.md` and `design/auth/installation-export.md`.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::policy::{apply_policy_in_transaction, PolicyMutation};
use super::sqlite::{decode, json, Connection};
use super::{AuthError, Mutation, SqliteAuthorizationStore};

/// A user named by access records.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UserRef {
    /// The user's Trellis user ID in the exporting installation.
    pub(super) user_id: String,
    pub(super) identities: Vec<SignIn>,
}

/// One way a user signs in: `local` with their username, or an OIDC
/// provider with the provider's subject for them.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SignIn {
    /// `local`, or the OIDC provider's ID.
    pub(super) provider: String,
    /// The OIDC provider's issuer, which finds the same provider in an
    /// installation where it has another ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) issuer: Option<String>,
    pub(super) subject: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RoleRecord {
    pub(super) role_id: String,
    pub(super) title: String,
    pub(super) description: String,
    /// Whether the role is given to every user created afterwards.
    pub(super) new_users: bool,
    /// Every capability the role gives, not only those of the bundle's APIs.
    pub(super) capabilities: Vec<String>,
    pub(super) members: Vec<Member>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Member {
    pub(super) user_id: String,
    pub(super) expires_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GrantRecord {
    pub(super) user_id: String,
    pub(super) capability: String,
    pub(super) expires_at: Option<i64>,
}

/// Capabilities an app may use without asking each person, which an
/// administrator set as the app's implied consent.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AppConsentRecord {
    pub(super) client_id: String,
    pub(super) capabilities: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AccessDocument {
    pub(super) users: Vec<UserRef>,
    pub(super) roles: Vec<RoleRecord>,
    pub(super) grants: Vec<GrantRecord>,
    pub(super) apps: Vec<AppConsentRecord>,
}

/// Read the access to the capabilities of `apis`, or with `None` every role
/// and direct grant; app consent is read only for `apis`. Users that are
/// deleted, and principals other than users, are left out.
pub(super) fn read_access(
    connection: &Connection,
    apis: Option<&[String]>,
) -> Result<AccessDocument, AuthError> {
    let apis_json = apis.map(|apis| json(&apis)).transpose()?;
    let mut statement = connection.prepare(
        "SELECT r.role_id,r.identity_generation,r.title,r.description,r.assign_to_new_users \
        FROM auth_roles r WHERE r.state='active' AND (?1 IS NULL OR EXISTS(SELECT 1 \
        FROM auth_role_capabilities rc JOIN auth_capability_identities ci \
        ON ci.capability_id=rc.capability_id AND ci.identity_generation=rc.capability_generation \
        WHERE rc.role_id=r.role_id AND rc.role_generation=r.identity_generation \
        AND ci.api_id IN (SELECT value FROM json_each(?1)))) ORDER BY r.role_id",
    )?;
    let roles = statement
        .query_map([&apis_json], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut document = AccessDocument::default();
    let mut users = BTreeSet::new();
    for (role_id, generation, title, description, new_users) in roles {
        let capabilities = connection
            .prepare(
                "SELECT capability_id FROM auth_role_capabilities \
                WHERE role_id=?1 AND role_generation=?2 ORDER BY capability_id",
            )?
            .query_map(params![role_id, generation], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let members = connection
            .prepare(
                "SELECT a.principal_id,a.expires_at FROM auth_role_assignments a \
                JOIN auth_principals p ON p.principal_id=a.principal_id \
                WHERE a.role_id=?1 AND a.role_generation=?2 \
                AND p.kind='user' AND p.state!='revoked' ORDER BY a.principal_id",
            )?
            .query_map(params![role_id, generation], |row| {
                Ok(Member {
                    user_id: row.get(0)?,
                    expires_at: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        users.extend(members.iter().map(|member| member.user_id.clone()));
        document.roles.push(RoleRecord {
            role_id,
            title,
            description,
            new_users,
            capabilities,
            members,
        });
    }
    document.grants = connection
        .prepare(
            "SELECT d.principal_id,d.capability_id,d.expires_at FROM auth_direct_capabilities d \
            JOIN auth_capability_identities ci ON ci.capability_id=d.capability_id \
            AND ci.identity_generation=d.capability_generation \
            JOIN auth_principals p ON p.principal_id=d.principal_id \
            WHERE p.kind='user' AND p.state!='revoked' \
            AND (?1 IS NULL OR ci.api_id IN (SELECT value FROM json_each(?1))) \
            ORDER BY d.principal_id,d.capability_id",
        )?
        .query_map([&apis_json], |row| {
            Ok(GrantRecord {
                user_id: row.get(0)?,
                capability: row.get(1)?,
                expires_at: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    users.extend(document.grants.iter().map(|grant| grant.user_id.clone()));
    if apis_json.is_some() {
        let clients = connection
            .prepare(
                "SELECT client_id,implied_capabilities_json FROM auth_oauth_clients \
                WHERE state!='deleted' ORDER BY client_id",
            )?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (client_id, implied) in clients {
            let mut capabilities = Vec::new();
            for capability in decode::<Vec<String>>(&implied)? {
                let ours: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_capability_identities \
                    WHERE capability_id=?1 AND state='active' \
                    AND api_id IN (SELECT value FROM json_each(?2)))",
                    params![capability, apis_json],
                    |row| row.get(0),
                )?;
                if ours {
                    capabilities.push(capability);
                }
            }
            if !capabilities.is_empty() {
                document.apps.push(AppConsentRecord {
                    client_id,
                    capabilities,
                });
            }
        }
    }
    for user_id in users {
        let identities = sign_ins(connection, &user_id)?;
        document.users.push(UserRef {
            user_id,
            identities,
        });
    }
    Ok(document)
}

/// The sign-ins of `principal`, OIDC ones with their provider's issuer.
pub(super) fn sign_ins(connection: &Connection, principal: &str) -> Result<Vec<SignIn>, AuthError> {
    Ok(connection
        .prepare(
            "SELECT i.provider,o.issuer,i.provider_subject FROM auth_provider_identities i \
            LEFT JOIN auth_oidc_providers o ON o.provider_id=i.provider \
            WHERE i.principal_id=?1 ORDER BY i.provider,i.provider_subject",
        )?
        .query_map([principal], |row| {
            Ok(SignIn {
                provider: row.get(0)?,
                issuer: row.get(1)?,
                subject: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Who a user named by access records is in this installation.
#[derive(Clone, Debug)]
pub(super) enum Match {
    /// The same person: the user with that ID whose sign-ins agree, or the
    /// one user with one of its OIDC sign-ins. `by` says which.
    Found {
        principal: String,
        by: &'static str,
    },
    Missing,
    /// Someone else here is in the way: why the user, and everything that
    /// names them, is skipped.
    Conflict(String),
}

/// The live user that signs in with `sign_in` here. An OIDC sign-in is the
/// same subject at a provider with the same issuer, whatever its ID here.
fn owner(connection: &Connection, sign_in: &SignIn) -> Result<Option<String>, AuthError> {
    if sign_in.provider == "local" {
        return Ok(connection
            .query_row(
                "SELECT principal_id FROM auth_principals WHERE kind='user' \
                AND state!='revoked' AND principal_id IN (\
                SELECT principal_id FROM auth_provider_identities \
                WHERE provider='local' AND provider_subject=?1 \
                UNION SELECT principal_id FROM auth_local_credentials \
                WHERE normalized_username=?1) LIMIT 1",
                [&sign_in.subject],
                |row| row.get(0),
            )
            .optional()?);
    }
    let Some(issuer) = &sign_in.issuer else {
        return Ok(None);
    };
    Ok(connection
        .query_row(
            "SELECT i.principal_id FROM auth_provider_identities i \
            JOIN auth_oidc_providers o ON o.provider_id=i.provider \
            JOIN auth_principals p ON p.principal_id=i.principal_id \
            WHERE i.provider_subject=?1 AND o.issuer=?2 \
            AND p.kind='user' AND p.state!='revoked' LIMIT 1",
            params![sign_in.subject, issuer],
            |row| row.get(0),
        )
        .optional()?)
}

/// Find `user` here. The user with the same ID is the same person when its
/// sign-ins agree: it has the same username or none, and no sign-in of the
/// record belongs to anyone else. With no user under that ID, the one user
/// with the same OIDC issuer and subject is the same person. Anyone else in
/// the way, such as another user with the same username, is a conflict.
pub(super) fn find_user(connection: &Connection, user: &UserRef) -> Result<Match, AuthError> {
    let by_id: Option<(String, String)> = connection
        .query_row(
            "SELECT kind,state FROM auth_principals WHERE principal_id=?1",
            [&user.user_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((kind, state)) = by_id {
        if kind != "user" || state == "revoked" {
            return Ok(Match::Conflict(format!(
                "user ID {} belongs to a {} here",
                user.user_id,
                if kind == "user" {
                    "deleted user"
                } else {
                    &kind
                }
            )));
        }
        let username: Option<String> = connection
            .query_row(
                "SELECT provider_subject FROM auth_provider_identities \
                WHERE principal_id=?1 AND provider='local' \
                UNION ALL SELECT normalized_username FROM auth_local_credentials \
                WHERE principal_id=?1 LIMIT 1",
                [&user.user_id],
                |row| row.get(0),
            )
            .optional()?;
        for sign_in in &user.identities {
            if let Some(here) = username.as_ref().filter(|_| sign_in.provider == "local") {
                if *here != sign_in.subject {
                    return Ok(Match::Conflict(format!(
                        "user {} here has the username {here}, not {}",
                        user.user_id, sign_in.subject
                    )));
                }
            }
            if let Some(other) = owner(connection, sign_in)?.filter(|other| *other != user.user_id)
            {
                return Ok(Match::Conflict(format!(
                    "its sign-in {} belongs to user {other} here",
                    sign_in.subject
                )));
            }
        }
        return Ok(Match::Found {
            principal: user.user_id.clone(),
            by: "id",
        });
    }
    let mut owners = BTreeSet::new();
    for sign_in in &user.identities {
        let Some(found) = owner(connection, sign_in)? else {
            continue;
        };
        // A username names an account, not a person: another user with it
        // is someone else.
        if sign_in.provider == "local" {
            return Ok(Match::Conflict(format!(
                "username {} belongs to user {found} here, under another user ID",
                sign_in.subject
            )));
        }
        owners.insert(found);
    }
    Ok(match owners.len() {
        0 => Match::Missing,
        1 => Match::Found {
            principal: owners.into_iter().next().unwrap_or_default(),
            by: "signIn",
        },
        _ => Match::Conflict(format!(
            "its sign-ins belong to {} users here: {}",
            owners.len(),
            owners.into_iter().collect::<Vec<_>>().join(", ")
        )),
    })
}

/// What an import recreated and what it skipped, and why.
#[derive(Debug, Default, Serialize)]
pub(super) struct AccessOutcome {
    pub(super) recreated: Vec<Value>,
    pub(super) skipped: Vec<Value>,
}

/// Run `step` inside a savepoint. A refusal rolls back what the step wrote
/// and is returned for the report; a storage failure stops the import.
fn attempt<T>(
    connection: &Connection,
    step: impl FnOnce() -> Result<T, AuthError>,
) -> Result<Result<T, String>, AuthError> {
    connection.execute_batch("SAVEPOINT access_record")?;
    match step() {
        Ok(value) => {
            connection.execute_batch("RELEASE access_record")?;
            Ok(Ok(value))
        }
        Err(error) => {
            connection.execute_batch("ROLLBACK TO access_record; RELEASE access_record")?;
            match error {
                AuthError::NotFound(_)
                | AuthError::Hidden(..)
                | AuthError::Conflict(_)
                | AuthError::InUse(_)
                | AuthError::IdentityTaken
                | AuthError::Denied
                | AuthError::DeniedNeeding(_)
                | AuthError::Invalid(_)
                | AuthError::ResourceLimit => Ok(Err(error.to_string())),
                error => Err(error),
            }
        }
    }
}

fn capability_exists(connection: &Connection, capability: &str) -> Result<bool, AuthError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_capability_identities \
        WHERE capability_id=?1 AND state='active')",
        [capability],
        |row| row.get(0),
    )?)
}

/// Recreate `access` here, as `identity`'s actor. Each role, member, grant
/// and app consent is recreated, found already present, or skipped with its
/// reason; a skipped one changes nothing. `known` says who some of the
/// record's users are here, as the caller decided; [`find_user`] finds the
/// others.
pub(super) fn recreate(
    connection: &Connection,
    identity: &Mutation,
    access: &AccessDocument,
    known: &BTreeMap<String, Match>,
    now: i64,
) -> Result<AccessOutcome, AuthError> {
    let mut outcome = AccessOutcome::default();
    let mut matches = known.clone();
    for user in &access.users {
        if !matches.contains_key(&user.user_id) {
            matches.insert(user.user_id.clone(), find_user(connection, user)?);
        }
    }
    // Who a record's user is here, or why the record is skipped.
    let target = |user_id: &str| match matches.get(user_id) {
        Some(Match::Found { principal, by }) => Ok((principal.clone(), *by)),
        Some(Match::Conflict(reason)) => Err(format!("its user was skipped: {reason}")),
        _ => Err(format!(
            "no user here has ID {user_id} or any of its sign-ins"
        )),
    };
    for role in &access.roles {
        let item = json!({ "kind": "role", "roleId": role.role_id, "role": role.title });
        let existing: Option<(u64, String, String)> = connection
            .query_row(
                "SELECT identity_generation,title,description FROM auth_roles \
                WHERE role_id=?1 AND state='active'",
                [&role.role_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let mut available = BTreeSet::new();
        for capability in &role.capabilities {
            if capability_exists(connection, capability)? {
                available.insert(capability.clone());
            } else {
                outcome.skipped.push(json!({
                    "kind": "roleCapability",
                    "roleId": role.role_id,
                    "role": role.title,
                    "capability": capability,
                    "reason": "the capability is not in this installation's catalog",
                }));
            }
        }
        let recreated = match existing {
            // The same role, found again: the same ID and name, and nothing
            // the record lacks. It gets what it is missing.
            Some((generation, title, description)) => {
                let held: BTreeSet<String> = connection
                    .prepare(
                        "SELECT capability_id FROM auth_role_capabilities \
                        WHERE role_id=?1 AND role_generation=?2",
                    )?
                    .query_map(params![role.role_id, generation], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let recorded: BTreeSet<&String> = role.capabilities.iter().collect();
                if title != role.title
                    || description != role.description
                    || held.iter().any(|capability| !recorded.contains(capability))
                {
                    Err(format!(
                        "role {} exists here as {title:?} with another definition; \
                        it was left as it is",
                        role.role_id
                    ))
                } else if available.is_subset(&held) {
                    Ok(false)
                } else {
                    let revision: u64 = connection.query_row(
                        "SELECT revision FROM auth_roles WHERE role_id=?1 AND state='active'",
                        [&role.role_id],
                        |row| row.get(0),
                    )?;
                    attempt(connection, || {
                        apply_policy_in_transaction(
                            connection,
                            identity,
                            &PolicyMutation::RolePut {
                                role_id: role.role_id.clone(),
                                expected_revision: revision,
                                title: role.title.clone(),
                                description: role.description.clone(),
                                capabilities: held.union(&available).cloned().collect(),
                            },
                            now,
                        )
                    })?
                    .map(|_| true)
                }
            }
            None => attempt(connection, || {
                apply_policy_in_transaction(
                    connection,
                    identity,
                    &PolicyMutation::RolePut {
                        role_id: role.role_id.clone(),
                        expected_revision: 0,
                        title: role.title.clone(),
                        description: role.description.clone(),
                        capabilities: available.iter().cloned().collect(),
                    },
                    now,
                )?;
                if role.new_users {
                    connection.execute(
                        "UPDATE auth_roles SET assign_to_new_users=1 \
                        WHERE role_id=?1 AND state='active'",
                        [&role.role_id],
                    )?;
                }
                Ok(())
            })?
            .map(|()| true),
        };
        match recreated {
            Ok(true) => outcome.recreated.push(item),
            Ok(false) => {}
            Err(reason) => {
                let mut item = item;
                item["reason"] = Value::String(reason);
                outcome.skipped.push(item);
                for member in &role.members {
                    outcome.skipped.push(json!({
                        "kind": "assignment",
                        "roleId": role.role_id,
                        "role": role.title,
                        "userId": member.user_id,
                        "reason": "its role was skipped",
                    }));
                }
                continue;
            }
        }
        for member in &role.members {
            let mut item = json!({
                "kind": "assignment",
                "roleId": role.role_id,
                "role": role.title,
                "userId": member.user_id,
            });
            let result = match target(&member.user_id) {
                Err(reason) => Err(reason),
                Ok(_) if member.expires_at.is_some_and(|deadline| deadline <= now) => {
                    Err("the assignment had expired".to_owned())
                }
                Ok((principal, by)) => {
                    item["matchedUserId"] = Value::String(principal.clone());
                    item["matchedBy"] = Value::String(by.to_owned());
                    let held: bool = connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM auth_role_assignments a \
                        JOIN auth_roles r ON r.role_id=a.role_id \
                        AND r.identity_generation=a.role_generation \
                        WHERE a.principal_id=?1 AND a.role_id=?2 AND r.state='active')",
                        params![principal, role.role_id],
                        |row| row.get(0),
                    )?;
                    if held {
                        Ok(false)
                    } else {
                        attempt(connection, || {
                            apply_policy_in_transaction(
                                connection,
                                identity,
                                &PolicyMutation::RoleAssign {
                                    principal_id: principal.clone(),
                                    role_id: role.role_id.clone(),
                                    expected_revision: 0,
                                    expires_at: member.expires_at,
                                },
                                now,
                            )
                        })?
                        .map(|_| true)
                    }
                }
            };
            report(&mut outcome, item, result);
        }
    }
    for grant in &access.grants {
        let mut item = json!({
            "kind": "grant",
            "capability": grant.capability,
            "userId": grant.user_id,
        });
        let result = match target(&grant.user_id) {
            Err(reason) => Err(reason),
            Ok(_) if grant.expires_at.is_some_and(|deadline| deadline <= now) => {
                Err("the grant had expired".to_owned())
            }
            Ok(_) if !capability_exists(connection, &grant.capability)? => {
                Err("the capability is not in this installation's catalog".to_owned())
            }
            Ok((principal, by)) => {
                item["matchedUserId"] = Value::String(principal.clone());
                item["matchedBy"] = Value::String(by.to_owned());
                let held: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_direct_capabilities d \
                    JOIN auth_capability_identities ci ON ci.capability_id=d.capability_id \
                    AND ci.identity_generation=d.capability_generation \
                    WHERE d.principal_id=?1 AND d.capability_id=?2 AND ci.state='active')",
                    params![principal, grant.capability],
                    |row| row.get(0),
                )?;
                if held {
                    Ok(false)
                } else {
                    attempt(connection, || {
                        apply_policy_in_transaction(
                            connection,
                            identity,
                            &PolicyMutation::CapabilityGrant {
                                principal_id: principal.clone(),
                                capability_id: grant.capability.clone(),
                                expected_revision: 0,
                                expires_at: grant.expires_at,
                            },
                            now,
                        )
                    })?
                    .map(|_| true)
                }
            }
        };
        report(&mut outcome, item, result);
    }
    for app in &access.apps {
        for capability in &app.capabilities {
            let item = json!({
                "kind": "appConsent",
                "clientId": app.client_id,
                "capability": capability,
            });
            let result = attempt(connection, || {
                add_implied_consent(connection, identity, &app.client_id, capability, now)
            })?;
            report(&mut outcome, item, result);
        }
    }
    Ok(outcome)
}

/// File one record's result under recreated or skipped; one found already
/// present is neither.
fn report(outcome: &mut AccessOutcome, mut item: Value, result: Result<bool, String>) {
    match result {
        Ok(true) => outcome.recreated.push(item),
        Ok(false) => {}
        Err(reason) => {
            item["reason"] = Value::String(reason);
            outcome.skipped.push(item);
        }
    }
}

/// Add `capability` to app `client_id`'s implied consent, as saving the app
/// with it would. Whether anything changed.
fn add_implied_consent(
    connection: &Connection,
    identity: &Mutation,
    client_id: &str,
    capability: &str,
    now: i64,
) -> Result<bool, AuthError> {
    SqliteAuthorizationStore::require_administration(
        connection,
        &identity.actor,
        super::Administration::ManageClients,
    )?;
    SqliteAuthorizationStore::require_platform_for(connection, &identity.actor, [capability])?;
    if !capability_exists(connection, capability)? {
        return Err(AuthError::NotFound(format!(
            "capability {capability} is not in this installation's catalog"
        )));
    }
    let implied: String = connection
        .query_row(
            "SELECT implied_capabilities_json FROM auth_oauth_clients \
            WHERE client_id=?1 AND state!='deleted'",
            [client_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AuthError::NotFound(format!("no app {client_id} here")))?;
    let mut implied: Vec<String> = decode(&implied)?;
    if implied.iter().any(|held| held == capability) {
        return Ok(false);
    }
    implied.push(capability.to_owned());
    connection.execute(
        "UPDATE auth_oauth_clients SET implied_capabilities_json=?2,revision=revision+1,\
        updated_at=?3 WHERE client_id=?1",
        params![client_id, json(&implied)?, now],
    )?;
    super::revocation::enqueue_scope(
        connection,
        &super::revocation::EnforcementScope::Client(client_id.to_owned()),
        now,
    )?;
    Ok(true)
}
