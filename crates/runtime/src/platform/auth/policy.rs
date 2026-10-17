use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use trellis_protocol::{
    AuthorityApi, AuthorityProvider, CapabilityAuthority, PlatformPrivilege, SessionBinding, U64s,
};

use super::authorization_sessions::Credential;
use super::revocation::EnforcementScope;
use super::sqlite::{decode, json, AuthError, Mutation, SqliteAuthorizationStore};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct CapabilitySelection {
    pub(crate) required: Vec<String>,
    pub(crate) optional: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Evaluation {
    pub(crate) principal_id: String,
    pub(crate) principal_kind: trellis_protocol::PrincipalKind,
    pub(crate) binding: SessionBinding,
    pub(crate) login_session_id: Option<String>,
    pub(crate) selection: CapabilitySelection,
    pub(crate) capabilities: Vec<CapabilityAuthority>,
    pub(crate) required_missing: Vec<String>,
    pub(crate) apis: Vec<AuthorityApi>,
    pub(crate) providers: Vec<AuthorityProvider>,
    pub(crate) resource_bindings_digest: Option<String>,
    pub(crate) platform_privileges: Vec<PlatformPrivilege>,
    pub(crate) platform_privilege_deadline: Option<i64>,
    pub(crate) capability_deadlines: BTreeMap<String, i64>,
    pub(crate) root_not_after: i64,
    /// Issuance limit for the complete current set, not a shared root limit.
    pub(crate) not_after: i64,
    pub(crate) explanations: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum PolicyMutation {
    RolePut {
        role_id: String,
        expected_revision: u64,
        title: String,
        description: String,
        capabilities: Vec<String>,
    },
    RoleDelete {
        role_id: String,
        expected_revision: u64,
    },
    RoleAssign {
        principal_id: String,
        role_id: String,
        expected_revision: u64,
        expires_at: Option<i64>,
    },
    RoleRevoke {
        principal_id: String,
        role_id: String,
        expected_revision: u64,
    },
    CapabilityGrant {
        principal_id: String,
        capability_id: String,
        expected_revision: u64,
        expires_at: Option<i64>,
    },
    CapabilityRevoke {
        principal_id: String,
        capability_id: String,
        expected_revision: u64,
    },
    PrivilegeAssign {
        principal_id: String,
        privileges: Vec<PlatformPrivilege>,
        expected_revision: u64,
    },
    PrivilegeRevoke {
        assignment_id: String,
        expected_revision: u64,
    },
    ClientState {
        client_id: String,
        expected_revision: u64,
        state: String,
    },
    PrincipalState {
        principal_id: String,
        expected_revision: u64,
        state: String,
    },
    ConsentRevoke {
        principal_id: String,
        client_id: String,
        binding_kind: String,
        binding_value: String,
        capability_id: Option<String>,
    },
    OidcMappingPut {
        mapping_id: String,
        provider_id: String,
        claim_name: String,
        claim_value: String,
        role_ids: Vec<String>,
        expected_revision: u64,
    },
    OidcMappingDelete {
        mapping_id: String,
        expected_revision: u64,
    },
}

pub(super) fn capability(
    connection: &Connection,
    capability_id: &str,
) -> Result<CapabilityAuthority, AuthError> {
    let row = connection.query_row("SELECT identity_generation,consent_revision FROM auth_capability_identities WHERE capability_id=?1 AND state='active'", [capability_id], |row| Ok((row.get::<_,u64>(0)?, row.get::<_,u64>(1)?))).optional()?.ok_or(AuthError::NotFound)?;
    Ok(CapabilityAuthority {
        capability_id: capability_id.into(),
        identity_generation: U64s::new(row.0),
        consent_revision: U64s::new(row.1),
    })
}

pub(super) fn role(connection: &Connection, role_id: &str) -> Result<(u64, u64), AuthError> {
    connection.query_row("SELECT identity_generation,revision FROM auth_roles WHERE role_id=?1 AND state='active'", [role_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?.ok_or(AuthError::NotFound)
}

#[derive(Debug)]
pub(super) struct Entitlement {
    pub(super) deadline: Option<i64>,
    pub(super) sources: Vec<String>,
}

/// The one entitlement union used by display, approval, issuance and enforcement.
/// Deadlines are unioned per capability: losing one path cannot remove another.
pub(super) fn entitlements(
    connection: &Connection,
    principal: &str,
    login: Option<&str>,
    now: i64,
) -> Result<BTreeMap<String, Entitlement>, AuthError> {
    let mut values: BTreeMap<String, Entitlement> = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT c.capability_id,d.expires_at,'direct' FROM auth_direct_capabilities d JOIN auth_capability_identities c ON c.capability_id=d.capability_id AND c.identity_generation=d.capability_generation WHERE d.principal_id=?1 AND c.state='active' AND (d.expires_at IS NULL OR d.expires_at>?2)
         UNION ALL SELECT c.capability_id,a.expires_at,'role:'||r.role_id FROM auth_role_assignments a JOIN auth_roles r ON r.role_id=a.role_id AND r.identity_generation=a.role_generation JOIN auth_role_capabilities m ON m.role_id=r.role_id AND m.role_generation=r.identity_generation JOIN auth_capability_identities c ON c.capability_id=m.capability_id AND c.identity_generation=m.capability_generation WHERE a.principal_id=?1 AND r.state='active' AND c.state='active' AND (a.expires_at IS NULL OR a.expires_at>?2)")?;
    let mut sources = statement
        .query_map(params![principal, now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some(login) = login {
        // Only verified claims saved by the login boundary are consulted. No
        // caller claim payload or dependency revision enters this calculation.
        // Refresh only the selected upstream identity: another linked subject
        // at the same provider cannot remove or supply this login's roles.
        let claims: Option<(String,i64,String)> = connection.query_row(
            "SELECT latest.verified_claims_json,latest.fresh_until,latest.provider_id FROM auth_login_sessions own JOIN auth_login_sessions latest ON latest.principal_id=own.principal_id AND latest.provider_id=own.provider_id AND latest.upstream_subject=own.upstream_subject JOIN auth_oidc_providers p ON p.provider_id=latest.provider_id WHERE own.login_session_id=?1 AND own.principal_id=?2 AND own.revoked_at IS NULL AND own.expires_at>?3 AND latest.observation_order=(SELECT max(observation_order) FROM auth_login_sessions observation WHERE observation.principal_id=own.principal_id AND observation.provider_id=own.provider_id AND observation.upstream_subject=own.upstream_subject) AND latest.revoked_at IS NULL AND latest.expires_at>?3 AND latest.fresh_until>?3 AND p.state='active'", params![login,principal,now], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        if let Some((claims, fresh_until, provider)) = claims {
            let claims: serde_json::Value = decode(&claims)?;
            let mut mappings = connection.prepare("SELECT m.claim_name,m.claim_value,c.capability_id,m.mapping_id FROM auth_oidc_role_mappings m JOIN auth_oidc_mapping_roles mr ON mr.mapping_id=m.mapping_id JOIN auth_roles r ON r.role_id=mr.role_id AND r.identity_generation=mr.role_generation JOIN auth_role_capabilities rc ON rc.role_id=r.role_id AND rc.role_generation=r.identity_generation JOIN auth_capability_identities c ON c.capability_id=rc.capability_id AND c.identity_generation=rc.capability_generation WHERE m.provider_id=?1 AND r.state='active' AND c.state='active'")?;
            for row in mappings.query_map([provider], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })? {
                let (name, value, capability, mapping) = row?;
                let claim = claims.pointer(&name);
                if claim.is_some_and(|claim| {
                    claim.as_str() == Some(&value)
                        || claim.as_array().is_some_and(|items| {
                            items.iter().any(|item| item.as_str() == Some(&value))
                        })
                }) {
                    sources.push((capability, Some(fresh_until), format!("oidc:{mapping}")));
                }
            }
        }
    }
    for (capability, deadline, source) in sources {
        values
            .entry(capability)
            .and_modify(|current| {
                current.deadline = match (current.deadline, deadline) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
                current.sources.push(source.clone());
            })
            .or_insert(Entitlement {
                deadline,
                sources: vec![source],
            });
    }
    Ok(values)
}

pub(super) fn evaluate(
    connection: &Connection,
    credential: &Credential,
    now: i64,
) -> Result<Evaluation, AuthError> {
    let root = super::authorization_sessions::credential_snapshot(connection, credential, now)?;
    let entitled = entitlements(
        connection,
        &root.principal_id,
        root.login_session_id.as_deref(),
        now,
    )?;
    let mut result = Evaluation {
        principal_id: root.principal_id,
        principal_kind: root.principal_kind,
        binding: root.binding,
        login_session_id: root.login_session_id,
        selection: root.selection,
        capabilities: vec![],
        required_missing: vec![],
        apis: vec![],
        providers: vec![],
        resource_bindings_digest: root.resource_bindings_digest,
        platform_privileges: root.platform_privileges,
        platform_privilege_deadline: root.platform_privilege_deadline,
        capability_deadlines: BTreeMap::new(),
        root_not_after: root.not_after,
        not_after: root
            .not_after
            .min(root.platform_privilege_deadline.unwrap_or(i64::MAX)),
        explanations: BTreeMap::new(),
    };
    let required: BTreeSet<_> = result.selection.required.iter().cloned().collect();
    let selected: BTreeSet<_> = required
        .iter()
        .chain(&result.selection.optional)
        .cloned()
        .collect();
    let mut apis = BTreeMap::new();
    for selected in selected {
        let current = match capability(connection, &selected) {
            Ok(value) => Some(value),
            Err(AuthError::NotFound) => None,
            Err(error) => return Err(error),
        };
        let consent = !(selected == super::deployments::SELF_HEALTH_CAPABILITY
            && result.principal_kind == trellis_protocol::PrincipalKind::User)
            && current.as_ref().is_some_and(|current| {
                root.approved
                    .as_ref()
                    .is_none_or(|approved| approved.contains(current))
            });
        if let (Some(current), Some(Entitlement { deadline, sources }), true) =
            (current, entitled.get(&selected), consent)
        {
            // Entitlement unions and consent bounds apply only to this right.
            if let Some(deadline) = deadline
                .iter()
                .chain(root.consent_deadlines.get(&selected))
                .min()
            {
                result
                    .capability_deadlines
                    .insert(selected.clone(), *deadline);
                result.not_after = result.not_after.min(*deadline);
            }
            result
                .explanations
                .insert(selected.clone(), sources.clone());
            let api: AuthorityApi = connection.query_row("SELECT a.api_id,a.generation,a.accepted_revision,s.snapshot_digest FROM auth_capability_identities c JOIN auth_accepted_apis a USING(api_id) JOIN auth_api_verification_snapshots s ON s.api_id=a.api_id AND s.generation=a.generation AND s.accepted_revision=a.accepted_revision WHERE c.capability_id=?1 AND c.state='active'", [&selected], |row| Ok(AuthorityApi { api_id: row.get(0)?, generation: U64s::new(row.get(1)?), accepted_revision: U64s::new(row.get(2)?), catalog_snapshot_digest: row.get(3)? }))?;
            apis.insert(api.api_id.clone(), api);
            result.capabilities.push(current);
        } else if required.contains(&selected) {
            result.required_missing.push(selected);
        }
    }
    // Provider rights and resource ownership are independent of caller grants.
    for provider in root.providers {
        let api: AuthorityApi = connection.query_row("SELECT a.api_id,a.generation,a.accepted_revision,s.snapshot_digest FROM auth_accepted_apis a JOIN auth_api_verification_snapshots s ON s.api_id=a.api_id AND s.generation=a.generation AND s.accepted_revision=a.accepted_revision WHERE a.api_id=?1 AND a.generation=?2", params![provider.api_id,provider.generation.get()], |row| Ok(AuthorityApi { api_id: row.get(0)?, generation: U64s::new(row.get(1)?), accepted_revision: U64s::new(row.get(2)?), catalog_snapshot_digest: row.get(3)? })).optional()?.ok_or(AuthError::Denied)?;
        apis.insert(api.api_id.clone(), api);
        result.providers.push(provider);
    }
    result.apis = apis.into_values().collect();
    if result.not_after <= now {
        return Err(AuthError::Denied);
    }
    Ok(result)
}

/// Apply policy and its enforcement intents within the caller's aggregate transaction.
pub(super) fn apply_policy_in_transaction(
    connection: &Connection,
    identity: &Mutation,
    mutation: &PolicyMutation,
    now: i64,
) -> Result<u64, AuthError> {
    let privilege = match mutation {
        PolicyMutation::PrivilegeAssign { .. } | PolicyMutation::PrivilegeRevoke { .. } => {
            "privileges.manage"
        }
        PolicyMutation::ClientState { .. } => "clients.manage",
        PolicyMutation::PrincipalState { .. } => "principals.manage",
        PolicyMutation::ConsentRevoke { principal_id, .. } if principal_id == &identity.actor => "",
        PolicyMutation::ConsentRevoke { .. } => "principals.manage",
        _ => "roles.manage",
    };
    if !privilege.is_empty() {
        SqliteAuthorizationStore::require_privilege(connection, &identity.actor, privilege)?;
    }
    let (scope, revision) = apply_policy(connection, mutation, now)?;
    SqliteAuthorizationStore::policy_changed(connection, &scope, now)?;
    Ok(revision)
}

pub(super) fn apply_policy(
    connection: &Connection,
    mutation: &PolicyMutation,
    now: i64,
) -> Result<(EnforcementScope, u64), AuthError> {
    match mutation {
        PolicyMutation::RolePut {
            role_id,
            expected_revision,
            title,
            description,
            capabilities,
        } => {
            if ulid::Ulid::from_string(role_id).is_err()
                || title.is_empty()
                || title.len() > 256
                || description.len() > 4096
            {
                return Err(AuthError::Invalid("invalid role".into()));
            }
            let current = role(connection, role_id);
            let generation = match current {
                Ok((generation, revision)) if revision == *expected_revision => {
                    connection.execute("UPDATE auth_roles SET title=?1,description=?2,revision=revision+1,updated_at=?3 WHERE role_id=?4 AND identity_generation=?5",params![title,description,now,role_id,generation])?;
                    generation
                }
                Err(AuthError::NotFound) if *expected_revision == 0 => {
                    let generation:u64 = connection.query_row("SELECT coalesce(max(identity_generation),0)+1 FROM auth_roles WHERE role_id=?1",[role_id],|row|row.get(0))?;
                    connection.execute(
                        "INSERT INTO auth_roles VALUES(?1,?2,1,?3,?4,'active',?5,?5)",
                        params![role_id, generation, title, description, now],
                    )?;
                    generation
                }
                Err(error) if !matches!(error, AuthError::NotFound) => return Err(error),
                _ => return Err(AuthError::Conflict),
            };
            connection.execute(
                "DELETE FROM auth_role_capabilities WHERE role_id=?1 AND role_generation=?2",
                params![role_id, generation],
            )?;
            for name in capabilities.iter().collect::<BTreeSet<_>>() {
                let cap = capability(connection, name)?;
                connection.execute(
                    "INSERT INTO auth_role_capabilities VALUES(?1,?2,?3,?4)",
                    params![role_id, generation, name, cap.identity_generation.get()],
                )?;
            }
            Ok((
                EnforcementScope::Role(role_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::RoleDelete {
            role_id,
            expected_revision,
        } => {
            let (generation, revision) = role(connection, role_id)?;
            if revision != *expected_revision {
                return Err(AuthError::Conflict);
            }
            connection.execute("UPDATE auth_roles SET state='deleted',revision=revision+1,updated_at=?1 WHERE role_id=?2 AND identity_generation=?3",params![now,role_id,generation])?;
            Ok((EnforcementScope::Role(role_id.clone()), revision + 1))
        }
        PolicyMutation::RoleAssign {
            principal_id,
            role_id,
            expected_revision,
            expires_at,
        } => {
            if expires_at.is_some_and(|deadline| deadline <= now) {
                return Err(AuthError::Invalid("expired assignment".into()));
            }
            let (generation, _) = role(connection, role_id)?;
            let current:Option<u64>=connection.query_row("SELECT revision FROM auth_role_assignments WHERE principal_id=?1 AND role_id=?2 AND role_generation=?3",params![principal_id,role_id,generation],|row|row.get(0)).optional()?;
            if current.unwrap_or(0) != *expected_revision {
                return Err(AuthError::Conflict);
            }
            connection.execute("INSERT INTO auth_role_assignments (assignment_id,principal_id,role_id,role_generation,revision,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(principal_id,role_id,role_generation) DO UPDATE SET revision=excluded.revision,expires_at=excluded.expires_at",params![super::sqlite::id(),principal_id,role_id,generation,expected_revision+1,expires_at,now])?;
            Ok((
                EnforcementScope::Principal(principal_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::RoleRevoke {
            principal_id,
            role_id,
            expected_revision,
        } => {
            let (generation, _) = role(connection, role_id)?;
            if connection.execute("DELETE FROM auth_role_assignments WHERE principal_id=?1 AND role_id=?2 AND role_generation=?3 AND revision=?4",params![principal_id,role_id,generation,expected_revision])?!=1 {return Err(AuthError::Conflict);}
            Ok((
                EnforcementScope::Principal(principal_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::CapabilityGrant {
            principal_id,
            capability_id,
            expected_revision,
            expires_at,
        } => {
            if expires_at.is_some_and(|deadline| deadline <= now) {
                return Err(AuthError::Invalid("expired grant".into()));
            }
            let cap = capability(connection, capability_id)?;
            if capability_id == super::deployments::SELF_HEALTH_CAPABILITY {
                let native: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_principals WHERE principal_id=?1 AND kind IN ('service','device'))",[principal_id],|row|row.get(0))?;
                if !native {
                    return Err(AuthError::Denied);
                }
                super::deployments::native_health_capability(connection)?;
            }
            let current:Option<u64>=connection.query_row("SELECT revision FROM auth_direct_capabilities WHERE principal_id=?1 AND capability_id=?2 AND capability_generation=?3",params![principal_id,capability_id,cap.identity_generation.get()],|row|row.get(0)).optional()?;
            if current.unwrap_or(0) != *expected_revision {
                return Err(AuthError::Conflict);
            }
            connection.execute("INSERT INTO auth_direct_capabilities (grant_id,principal_id,capability_id,capability_generation,revision,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(principal_id,capability_id,capability_generation) DO UPDATE SET revision=excluded.revision,expires_at=excluded.expires_at",params![super::sqlite::id(),principal_id,capability_id,cap.identity_generation.get(),expected_revision+1,expires_at,now])?;
            Ok((
                EnforcementScope::Principal(principal_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::CapabilityRevoke {
            principal_id,
            capability_id,
            expected_revision,
        } => {
            let cap = capability(connection, capability_id)?;
            if connection.execute("DELETE FROM auth_direct_capabilities WHERE principal_id=?1 AND capability_id=?2 AND capability_generation=?3 AND revision=?4",params![principal_id,capability_id,cap.identity_generation.get(),expected_revision])?!=1 {return Err(AuthError::Conflict);}
            Ok((
                EnforcementScope::Principal(principal_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::PrivilegeAssign {
            principal_id,
            privileges,
            expected_revision,
        } => {
            let protected: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator WHERE principal_id=?1)",
                [principal_id],
                |row| row.get(0),
            )?;
            if protected {
                let mut statement = connection.prepare(
                    "SELECT privilege FROM auth_platform_privileges WHERE principal_id=?1",
                )?;
                for existing in
                    statement.query_map([principal_id], |row| row.get::<_, String>(0))?
                {
                    let existing: PlatformPrivilege =
                        serde_json::from_value(serde_json::Value::String(existing?))?;
                    if !privileges.contains(&existing) {
                        return Err(AuthError::Denied);
                    }
                }
            }
            let current: Option<u64> = connection
                .query_row(
                    "SELECT revision FROM auth_platform_assignments WHERE principal_id=?1",
                    [principal_id],
                    |row| row.get(0),
                )
                .optional()?;
            if current.unwrap_or(0) != *expected_revision {
                return Err(AuthError::Conflict);
            }
            connection.execute("INSERT INTO auth_platform_assignments VALUES(?1,?2,?3,?4) ON CONFLICT(principal_id) DO UPDATE SET revision=excluded.revision", params![super::sqlite::id(), principal_id, expected_revision+1, now])?;
            connection.execute(
                "DELETE FROM auth_platform_privileges WHERE principal_id=?1",
                [principal_id],
            )?;
            for privilege in privileges.iter().collect::<BTreeSet<_>>() {
                connection.execute(
                    "INSERT INTO auth_platform_privileges VALUES(?1,?2)",
                    params![principal_id, serde_json::to_value(privilege)?.as_str()],
                )?;
            }
            Ok((
                EnforcementScope::Principal(principal_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::PrivilegeRevoke {
            assignment_id,
            expected_revision,
        } => {
            let principal_id: String = connection.query_row("SELECT principal_id FROM auth_platform_assignments WHERE assignment_id=?1 AND revision=?2", params![assignment_id,expected_revision], |row| row.get(0)).optional()?.ok_or(AuthError::Conflict)?;
            let protected: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator WHERE principal_id=?1)",
                [&principal_id],
                |row| row.get(0),
            )?;
            if protected {
                return Err(AuthError::Denied);
            }
            connection.execute(
                "DELETE FROM auth_platform_assignments WHERE assignment_id=?1 AND revision=?2",
                params![assignment_id, expected_revision],
            )?;
            Ok((
                EnforcementScope::Principal(principal_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::ClientState {
            client_id,
            expected_revision,
            state,
        } => {
            if !["active", "disabled", "deleted"].contains(&state.as_str()) {
                return Err(AuthError::Invalid("invalid client state".into()));
            }
            if connection.execute("UPDATE auth_oauth_clients SET state=?1,revision=revision+1,updated_at=?2 WHERE client_id=?3 AND revision=?4 AND state!='deleted'",params![state,now,client_id,expected_revision])?!=1 {return Err(AuthError::Conflict);}
            Ok((
                EnforcementScope::Client(client_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::PrincipalState {
            principal_id,
            expected_revision,
            state,
        } => {
            if !["active", "disabled", "revoked"].contains(&state.as_str()) {
                return Err(AuthError::Invalid("invalid principal state".into()));
            }
            let protected: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator WHERE principal_id=?1)",
                [principal_id],
                |row| row.get(0),
            )?;
            if protected && state != "active" {
                return Err(AuthError::Denied);
            }
            if connection.execute("UPDATE auth_principals SET state=?1,disabled_at=CASE WHEN ?1='disabled' THEN ?2 END,revoked_at=CASE WHEN ?1='revoked' THEN ?2 END,updated_at=?2,version=version+1 WHERE principal_id=?3 AND version=?4 AND state!='revoked'",params![state,now,principal_id,expected_revision])?!=1 {return Err(AuthError::Conflict);}
            Ok((
                EnforcementScope::Principal(principal_id.clone()),
                expected_revision + 1,
            ))
        }
        PolicyMutation::ConsentRevoke {
            principal_id,
            client_id,
            binding_kind,
            binding_value,
            capability_id,
        } => {
            connection.execute("UPDATE auth_remembered_consent SET decision='declined',revision=revision+1,updated_at=?1 WHERE principal_id=?2 AND client_id=?3 AND binding_kind=?4 AND binding_value=?5 AND (?6 IS NULL OR capability_id=?6)",params![now,principal_id,client_id,binding_kind,binding_value,capability_id])?;
            // Originally approved choices are never reintroduced at token renewal.
            let mut statement=connection.prepare("SELECT oauth_grant_id,approved_capabilities_json FROM auth_oauth_grants WHERE principal_id=?1 AND client_id=?2 AND binding_kind=?3 AND binding_value=?4 AND revoked_at IS NULL")?;
            let rows = statement
                .query_map(
                    params![principal_id, client_id, binding_kind, binding_value],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (grant, approved) in rows {
                let mut approved: Vec<CapabilityAuthority> = decode(&approved)?;
                approved.retain(|cap| {
                    capability_id
                        .as_ref()
                        .is_some_and(|id| id != &cap.capability_id)
                });
                connection.execute("UPDATE auth_oauth_grants SET approved_capabilities_json=?1,revision=revision+1,revoked_at=CASE WHEN ?2 IS NULL THEN ?3 ELSE revoked_at END WHERE oauth_grant_id=?4",params![json(&approved)?,capability_id,now,grant])?;
            }
            Ok((EnforcementScope::Principal(principal_id.clone()), 1))
        }
        PolicyMutation::OidcMappingPut {
            mapping_id,
            provider_id,
            claim_name,
            claim_value,
            role_ids,
            expected_revision,
        } => {
            if ulid::Ulid::from_string(mapping_id).is_err() || !claim_name.starts_with('/') {
                return Err(AuthError::Invalid("invalid OIDC mapping".into()));
            }
            let current: Option<u64> = connection
                .query_row(
                    "SELECT revision FROM auth_oidc_role_mappings WHERE mapping_id=?1",
                    [mapping_id],
                    |row| row.get(0),
                )
                .optional()?;
            if current.unwrap_or(0) != *expected_revision {
                return Err(AuthError::Conflict);
            }
            connection.execute("INSERT INTO auth_oidc_role_mappings VALUES(?1,?2,?3,?4,?5) ON CONFLICT(mapping_id) DO UPDATE SET provider_id=excluded.provider_id,claim_name=excluded.claim_name,claim_value=excluded.claim_value,revision=excluded.revision",params![mapping_id,provider_id,claim_name,claim_value,expected_revision+1])?;
            connection.execute(
                "DELETE FROM auth_oidc_mapping_roles WHERE mapping_id=?1",
                [mapping_id],
            )?;
            for role_id in role_ids.iter().collect::<BTreeSet<_>>() {
                let (generation, _) = role(connection, role_id)?;
                connection.execute(
                    "INSERT INTO auth_oidc_mapping_roles VALUES(?1,?2,?3)",
                    params![mapping_id, role_id, generation],
                )?;
            }
            Ok((EnforcementScope::All, expected_revision + 1))
        }
        PolicyMutation::OidcMappingDelete {
            mapping_id,
            expected_revision,
        } => {
            if connection.execute(
                "DELETE FROM auth_oidc_role_mappings WHERE mapping_id=?1 AND revision=?2",
                params![mapping_id, expected_revision],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            Ok((EnforcementScope::All, expected_revision + 1))
        }
    }
}
