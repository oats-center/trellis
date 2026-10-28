use super::Administration;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use trellis_idl::{
    api_digest, compare_implementation, compile_evidence, ActionKind, ActionSelection,
    InteractionDirection, PackageEvidence, PackageGraph,
};
use trellis_protocol::{
    ApiSurfaceKind, CatalogAction, CatalogActionIdentity, CatalogCapability,
    CatalogCapabilityMembership, PermissionAction, SignedCatalogSnapshot, U64s,
    CATALOG_SNAPSHOT_FORMAT_V1,
};

use super::policy::{apply_policy_in_transaction, PolicyMutation};
use super::revocation::EnforcementScope;
use super::sqlite::{
    decode, digest, id, json, AuthError, Connection, Mutation, SqliteAuthorizationStore,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ApiReview {
    pub(crate) review_id: String,
    pub(crate) api_id: String,
    pub(crate) definition_digest: String,
    pub(crate) expected_generation: u64,
    pub(crate) expected_revision: u64,
    pub(crate) compatible: bool,
    pub(crate) breaking_changes: Vec<String>,
    pub(crate) removed_capabilities: Vec<String>,
    pub(crate) consent_changes: Vec<String>,
    pub(crate) changed_actions: Vec<String>,
    pub(crate) affected_roles: BTreeMap<String, u64>,
    pub(crate) affected_mappings: BTreeMap<String, u64>,
    pub(crate) affected_providers: Vec<String>,
    pub(crate) affected_grants: Vec<String>,
    pub(crate) policy_digest: String,
    pub(crate) proposed: PackageEvidence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ApiAcceptance {
    pub(crate) review_id: String,
    pub(crate) reviewed_digest: String,
    pub(crate) force: bool,
    pub(crate) acknowledge_breakage: bool,
    pub(crate) confirm_consent_meanings: bool,
    /// Existing meanings explicitly declared materially changed by the operator.
    pub(crate) materially_changed: Vec<String>,
    pub(crate) policy_edits: Vec<PolicyMutation>,
}

pub(super) fn action_identity(selection: &ActionSelection) -> Vec<CatalogActionIdentity> {
    let kind = match selection.action.kind {
        ActionKind::Rpc => ApiSurfaceKind::Rpc,
        ActionKind::Operation => ApiSurfaceKind::Operation,
        ActionKind::Event => ApiSurfaceKind::Event,
        ActionKind::Live => ApiSurfaceKind::Live,
    };
    let directions = match selection.direction {
        InteractionDirection::Call => vec![PermissionAction::Call],
        InteractionDirection::Invoke => vec![
            PermissionAction::Invoke,
            PermissionAction::Observe,
            PermissionAction::Cancel,
            PermissionAction::Control,
        ],
        InteractionDirection::Publish => vec![PermissionAction::Publish],
        InteractionDirection::Subscribe => vec![PermissionAction::Subscribe],
    };
    directions
        .into_iter()
        .map(|direction| CatalogActionIdentity {
            kind,
            name: selection.action.name.clone(),
            direction,
        })
        .collect()
}

pub(super) fn resolve_api<'a>(
    graph: &'a PackageGraph,
    api: &str,
) -> Result<trellis_idl::ResolvedApi<'a>, AuthError> {
    graph
        .packages()
        .values()
        .flat_map(|package| package.apis().keys())
        .find(|id| id.as_str() == api)
        .and_then(|id| graph.api(id))
        .ok_or(AuthError::NotFound)
}

/// A review fences all relevant policy edits, not just the catalog row. This
/// value stays inside Auth and is never a runtime authorization floor.
fn policy_snapshot(connection: &Connection, api: &str) -> Result<String, AuthError> {
    let mut values = Vec::new();
    for sql in [
    "SELECT r.role_id||':'||r.identity_generation||':'||r.revision FROM auth_roles r JOIN auth_role_capabilities m ON m.role_id=r.role_id AND m.role_generation=r.identity_generation JOIN auth_capability_identities c ON c.capability_id=m.capability_id AND c.identity_generation=m.capability_generation WHERE c.api_id=?1 ORDER BY 1",
    "SELECT DISTINCT m.mapping_id||':'||m.revision \
    FROM auth_oidc_role_mappings m \
    JOIN auth_oidc_mapping_roles mr ON mr.mapping_id=m.mapping_id \
    JOIN auth_role_capabilities r ON r.role_id=mr.role_id \
    AND r.role_generation=mr.role_generation \
    JOIN auth_capability_identities c ON c.capability_id=r.capability_id \
    AND c.identity_generation=r.capability_generation \
    WHERE c.api_id=?1 ORDER BY 1",
    "SELECT d.principal_id||':'||d.capability_id||':'||d.capability_generation||':'||d.revision \
    FROM auth_direct_capabilities d \
    JOIN auth_capability_identities c ON c.capability_id=d.capability_id \
    AND c.identity_generation=d.capability_generation \
    WHERE c.api_id=?1 ORDER BY 1",
    "SELECT b.deployment_id||':'||b.revision \
    FROM auth_deployment_bindings b \
    WHERE EXISTS(SELECT 1 \
    FROM json_each(b.provided_apis_json) \
    WHERE json_extract(value,'$.apiId')=?1) \
    ORDER BY 1",
] {
    values.push(
        connection
            .prepare(sql)?
            .query_map([api], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
}
    digest(&values)
}

/// Only the APIs Trellis ships may use this namespace, which is also where
/// administrative capabilities live.
const RESERVED_API_PREFIX: &str = "trellis.";

/// Definition digests of the APIs Trellis ships, by API ID.
fn builtin_api_digests() -> &'static BTreeMap<String, String> {
    static DIGESTS: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    DIGESTS.get_or_init(|| {
        let mut digests = BTreeMap::new();
        for (apis, evidence) in
            super::initialization::builtin_package_evidence().expect("built-in evidence decodes")
        {
            let graph = compile_evidence(evidence).expect("built-in contracts compile");
            for api in apis {
                let resolved = resolve_api(&graph, api).expect("built-in API resolves");
                digests.insert(
                    (*api).to_owned(),
                    api_digest(&graph, resolved.definition().identity())
                        .expect("built-in API digests"),
                );
            }
        }
        digests
    })
}

/// The consent meaning of a capability or group.
struct Meaning<'a> {
    /// Its `grants` wording changed, so users approve it again.
    regranted: bool,
    title: &'a str,
    description: &'a str,
    grants: &'a str,
}

/// Update an active capability or group identity's meaning, or start a new
/// identity generation. Auth owns the consent revision: it advances only when
/// the `grants` wording changes. Returns the generation and consent revision.
fn upsert_identity(
    connection: &Connection,
    api_id: &str,
    id: &str,
    kind: &str,
    meaning: Meaning<'_>,
    now: i64,
) -> Result<(u64, u64), AuthError> {
    let current: Option<(u64, u64)> = connection
        .query_row(
            "SELECT identity_generation,consent_revision \
            FROM auth_capability_identities \
            WHERE capability_id=?1 AND state='active'",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((generation, consent_revision)) = current {
        let consent_revision = consent_revision + u64::from(meaning.regranted);
        connection.execute(
            "UPDATE auth_capability_identities \
            SET consent_revision=?1,title=?2,description=?3,grants=?4 \
            WHERE capability_id=?5 AND identity_generation=?6",
            params![
                consent_revision,
                meaning.title,
                meaning.description,
                meaning.grants,
                id,
                generation
            ],
        )?;
        return Ok((generation, consent_revision));
    }
    let generation: u64 = connection.query_row(
        "SELECT coalesce(max(identity_generation),0)+1 \
        FROM auth_capability_identities WHERE capability_id=?1",
        [id],
        |row| row.get(0),
    )?;
    connection.execute(
        "INSERT INTO auth_capability_identities \
        VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8,NULL,?9)",
        params![
            id,
            generation,
            api_id,
            1,
            meaning.title,
            meaning.description,
            meaning.grants,
            now,
            kind
        ],
    )?;
    Ok((generation, 1))
}

impl SqliteAuthorizationStore {
    pub(crate) fn review_api(
        &self,
        actor: &str,
        proposed: PackageEvidence,
        api_id: &str,
        now: i64,
    ) -> Result<ApiReview, AuthError> {
        self.transaction(|connection| {
            Self::require_administration(connection, actor, Administration::AcceptApis)?;
            self.review_api_in_transaction(connection, proposed, api_id, now)
        })
    }

    pub(super) fn review_api_in_transaction(
        &self,
        connection: &Connection,
        proposed: PackageEvidence,
        api_id: &str,
        now: i64,
    ) -> Result<ApiReview, AuthError> {
        let graph = compile_evidence(proposed.clone())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let candidate = resolve_api(&graph, api_id)?;
        let definition_digest = api_digest(&graph, candidate.definition().identity())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        if api_id.starts_with(RESERVED_API_PREFIX)
            && builtin_api_digests().get(api_id) != Some(&definition_digest)
        {
            return Err(AuthError::Invalid(format!(
                "API IDs starting with `{RESERVED_API_PREFIX}` are reserved for Trellis"
            )));
        }
        {
            let existing: Option<(u64, u64, String)> = connection
                .query_row(
                    "SELECT generation,accepted_revision,definition_json \
                    FROM auth_accepted_apis \
                    WHERE api_id=?1",
                    [api_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let mut review = ApiReview {
                review_id: id(),
                api_id: api_id.into(),
                definition_digest,
                expected_generation: 0,
                expected_revision: 0,
                compatible: true,
                breaking_changes: vec![],
                removed_capabilities: vec![],
                consent_changes: vec![],
                changed_actions: vec![],
                affected_roles: BTreeMap::new(),
                affected_mappings: BTreeMap::new(),
                affected_providers: vec![],
                affected_grants: vec![],
                policy_digest: policy_snapshot(connection, api_id)?,
                proposed,
            };
            if let Some((generation, revision, definition)) = existing {
                review.expected_generation = generation;
                review.expected_revision = revision;
                let previous = compile_evidence(decode(&definition)?)
                    .map_err(|error| AuthError::Invalid(error.to_string()))?;
                let old = resolve_api(&previous, api_id)?;
                let comparison = compare_implementation(old, candidate);
                review.compatible = comparison.compatible;
                review.breaking_changes = comparison
                    .issues
                    .into_iter()
                    .map(|issue| format!("{}: {}", issue.path, issue.message))
                    .collect();
                // Consent covers the `grants` wording the user approved; the
                // designer changes it when the meaning changes. Other edits,
                // including what a capability allows or a group's members,
                // reach approved apps within the same API version.
                for (capability, definition) in old.definition().capabilities() {
                    match candidate.definition().capabilities().get(capability) {
                        None => review.removed_capabilities.push(capability.to_string()),
                        Some(next) if next.grants != definition.grants => {
                            review.consent_changes.push(capability.to_string());
                        }
                        _ => {}
                    }
                }
                for (group, definition) in old.definition().groups() {
                    match candidate.definition().groups().get(group) {
                        None => review.removed_capabilities.push(group.to_string()),
                        Some(next) if next.grants != definition.grants => {
                            review.consent_changes.push(group.to_string());
                        }
                        _ => {}
                    }
                }
                let old_actions = old
                    .definition()
                    .actions()
                    .keys()
                    .map(|action| format!("{:?}:{}", action.kind, action.name))
                    .collect::<BTreeSet<_>>();
                let next_actions = candidate
                    .definition()
                    .actions()
                    .keys()
                    .map(|action| format!("{:?}:{}", action.kind, action.name))
                    .collect::<BTreeSet<_>>();
                review.changed_actions = old_actions
                    .symmetric_difference(&next_actions)
                    .cloned()
                    .collect();
            }
            let mut statement=connection.prepare("SELECT DISTINCT r.role_id,r.revision FROM auth_roles r JOIN auth_role_capabilities m ON m.role_id=r.role_id AND m.role_generation=r.identity_generation JOIN auth_capability_identities c ON c.capability_id=m.capability_id AND c.identity_generation=m.capability_generation WHERE c.api_id=?1 AND r.state='active' ORDER BY r.role_id")?;
            review.affected_roles = statement
                .query_map([api_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut statement = connection.prepare(
                "SELECT DISTINCT m.mapping_id,m.revision \
                FROM auth_oidc_role_mappings m \
                JOIN auth_oidc_mapping_roles mr ON mr.mapping_id=m.mapping_id \
                JOIN auth_role_capabilities r ON r.role_id=mr.role_id \
                AND r.role_generation=mr.role_generation \
                JOIN auth_capability_identities c ON c.capability_id=r.capability_id \
                AND c.identity_generation=r.capability_generation \
                WHERE c.api_id=?1 ORDER BY m.mapping_id",
            )?;
            review.affected_mappings = statement
                .query_map([api_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            review.affected_providers = connection
                .prepare(
                    "SELECT DISTINCT deployment_id \
                    FROM auth_provider_certificates \
                    WHERE api_id=?1 ORDER BY deployment_id",
                )?
                .query_map([api_id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            review.affected_grants = connection
                .prepare(
                    "SELECT DISTINCT g.oauth_grant_id \
                    FROM auth_oauth_grants g,json_each(g.approved_capabilities_json) j \
                    JOIN auth_capability_identities c ON \
                    c.capability_id=json_extract(j.value,'$.capabilityId') \
                    WHERE c.api_id=?1 AND g.revoked_at IS NULL \
                    ORDER BY g.oauth_grant_id",
                )?
                .query_map([api_id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            connection.execute(
                "INSERT INTO auth_api_reviews(review_id,api_id,review_json,created_at,\
                expires_at) \
                VALUES(?1,?2,?3,?4,?5)",
                params![
                    review.review_id,
                    api_id,
                    json(&review)?,
                    now,
                    now.saturating_add(900)
                ],
            )?;
            Ok(review)
        }
    }

    pub(crate) fn accept_api(
        &self,
        identity: &Mutation,
        acceptance: &ApiAcceptance,
        now: i64,
    ) -> Result<SignedCatalogSnapshot, AuthError> {
        self.mutate(identity, "api.accept", acceptance, now, |connection| {
            Self::require_acceptance(connection, &identity.actor, acceptance.force)?;
            self.accept_api_in_transaction(connection, identity, acceptance, now)
        })
    }

    /// Accepting needs `apis.accept`; a force replacement also needs
    /// `apis.forceReplace`. A service update accepts its own compatible APIs
    /// under the update's authority instead.
    pub(super) fn require_acceptance(
        connection: &Connection,
        actor: &str,
        force: bool,
    ) -> Result<(), AuthError> {
        Self::require_administration(connection, actor, Administration::AcceptApis)?;
        if force {
            Self::require_administration(connection, actor, Administration::ForceReplaceApis)?;
        }
        Ok(())
    }

    pub(super) fn accept_api_in_transaction(
        &self,
        connection: &Connection,
        identity: &Mutation,
        acceptance: &ApiAcceptance,
        now: i64,
    ) -> Result<SignedCatalogSnapshot, AuthError> {
        {
            self.current_signer(connection)?;
            let review: String = connection
                .query_row(
                    "SELECT review_json FROM auth_api_reviews \
                    WHERE review_id=?1 AND expires_at>?2 \
                    AND consumed_at IS NULL",
                    params![acceptance.review_id, now],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::Conflict)?;
            let review: ApiReview = decode(&review)?;
            if acceptance.reviewed_digest != review.definition_digest
                || (!review.compatible && !acceptance.force)
                || (acceptance.force
                    && (!acceptance.acknowledge_breakage || !acceptance.confirm_consent_meanings))
                || acceptance
                    .materially_changed
                    .iter()
                    .any(|capability| !review.consent_changes.contains(capability))
            {
                return Err(AuthError::Denied);
            }
            if review.policy_digest != policy_snapshot(connection, &review.api_id)? {
                return Err(AuthError::Conflict);
            }
            let current: Option<(u64, u64)> = connection
                .query_row(
                    "SELECT generation,accepted_revision FROM auth_accepted_apis WHERE api_id=?1",
                    [&review.api_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if current.unwrap_or((0, 0)) != (review.expected_generation, review.expected_revision) {
                return Err(AuthError::Conflict);
            }
            let graph = compile_evidence(review.proposed.clone())
                .map_err(|error| AuthError::Invalid(error.to_string()))?;
            let resolved = resolve_api(&graph, &review.api_id)?;
            let api = resolved.definition();
            let generation = if acceptance.force {
                review.expected_generation + 1
            } else {
                review.expected_generation.max(1)
            };
            let revision = if acceptance.force {
                1
            } else {
                review.expected_revision + 1
            };
            let old_snapshot: Option<String> = connection
                .query_row(
                    "SELECT signed_bytes \
                    FROM auth_api_verification_snapshots \
                    WHERE api_id=?1 AND generation=?2 \
                    AND accepted_revision=?3",
                    params![
                        review.api_id,
                        review.expected_generation,
                        review.expected_revision
                    ],
                    |row| {
                        row.get::<_, Vec<u8>>(0)
                            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    },
                )
                .optional()?;
            let old_snapshot = old_snapshot
                .map(|value| decode::<SignedCatalogSnapshot>(&value))
                .transpose()?;
            // Clear child rows before changing their parent's generation. History
            // remains only in immutable verification snapshots, not serving rows.
            connection.execute(
                "DELETE FROM auth_api_actions WHERE api_id=?1",
                [&review.api_id],
            )?;
            connection.execute(
                "INSERT INTO auth_accepted_apis \
                VALUES(?1,?2,?3,?4,?5,?6,?7,1) \
                ON CONFLICT(api_id) \
                DO UPDATE SET generation=excluded.generation,\
                accepted_revision=excluded.accepted_revision,\
                definition_digest=excluded.definition_digest,\
                definition_json=excluded.definition_json,\
                accepted_at=excluded.accepted_at,accepted_by=excluded.accepted_by,\
                revision=auth_accepted_apis.revision+1",
                params![
                    review.api_id,
                    generation,
                    revision,
                    review.definition_digest,
                    json(&review.proposed)?,
                    now,
                    identity.actor
                ],
            )?;
            for removed in &review.removed_capabilities {
                connection.execute(
                    "UPDATE auth_capability_identities \
                    SET state='deleted',deleted_at=?1 \
                    WHERE capability_id=?2 \
                    AND state='active'",
                    params![now, removed],
                )?;
            }
            let mut snapshot = SignedCatalogSnapshot {
                format: CATALOG_SNAPSHOT_FORMAT_V1.into(),
                issuer_key_id: self.current_issuer(connection)?.key_id,
                trellis_instance_id: self.settings.instance_id.clone(),
                audience_nats_account: self.settings.account.clone(),
                api_id: review.api_id.clone(),
                generation: U64s::new(generation),
                accepted_revision: U64s::new(revision),
                definition_digest: review.definition_digest.clone(),
                actions: vec![],
                capabilities: vec![],
                issued_at: now,
                extensions: Default::default(),
                critical: vec![],
                signature: String::new(),
            };
            let mut actions = BTreeMap::new();
            // All actions are recorded, including actions no capability currently
            // grants. Later membership of an existing action gets its own fence.
            for action in api.actions().keys() {
                let directions = match action.kind {
                    ActionKind::Rpc => vec![InteractionDirection::Call],
                    ActionKind::Operation => vec![InteractionDirection::Invoke],
                    ActionKind::Event => vec![
                        InteractionDirection::Publish,
                        InteractionDirection::Subscribe,
                    ],
                    ActionKind::Live => vec![InteractionDirection::Subscribe],
                };
                for direction in directions {
                    for identity in action_identity(&ActionSelection {
                        action: action.clone(),
                        direction,
                    }) {
                        let introduction = old_snapshot
                            .as_ref()
                            .filter(|_| !acceptance.force)
                            .and_then(|old| old.actions.iter().find(|old| old.identity == identity))
                            .map_or(revision, |old| old.introduced_revision.get());
                        actions.insert(
                            json(&identity)?,
                            CatalogAction {
                                identity,
                                introduced_revision: U64s::new(introduction),
                            },
                        );
                    }
                }
            }
            for (key, action) in &actions {
                connection.execute(
                    "INSERT INTO auth_api_actions VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        review.api_id,
                        generation,
                        key,
                        action.introduced_revision.get(),
                        json(&action.identity)?,
                        json(&super::transport::ActionRoute::derive(
                            api,
                            &action.identity
                        )?)?
                    ],
                )?;
            }
            snapshot.actions = actions.into_values().collect();
            for (name, definition) in api.capabilities() {
                let (identity_generation, consent_revision) = upsert_identity(
                    connection,
                    &review.api_id,
                    name.as_str(),
                    "capability",
                    Meaning {
                        regranted: review.consent_changes.contains(&name.to_string()),
                        title: &definition.title,
                        description: &definition.description,
                        grants: &definition.grants,
                    },
                    now,
                )?;
                let mut memberships = Vec::new();
                for selection in &definition.allows {
                    for action in action_identity(selection) {
                        let member_since = old_snapshot
                            .as_ref()
                            .filter(|_| !acceptance.force)
                            .and_then(|old| {
                                old.capabilities.iter().find(|capability| {
                                    capability.capability_id == name.as_str()
                                        && capability.identity_generation.get()
                                            == identity_generation
                                })
                            })
                            .and_then(|capability| {
                                capability
                                    .memberships
                                    .iter()
                                    .find(|member| member.action == action)
                            })
                            .map_or(revision, |member| member.member_since_revision.get());
                        connection.execute(
                            "INSERT INTO auth_capability_actions VALUES(?1,?2,?3,?4,?5,?6)",
                            params![
                                name.as_str(),
                                identity_generation,
                                review.api_id,
                                generation,
                                json(&action)?,
                                member_since
                            ],
                        )?;
                        memberships.push(CatalogCapabilityMembership {
                            action,
                            member_since_revision: U64s::new(member_since),
                        });
                    }
                }
                snapshot.capabilities.push(CatalogCapability {
                    capability_id: name.to_string(),
                    identity_generation: U64s::new(identity_generation),
                    consent_revision: U64s::new(consent_revision),
                    memberships,
                });
            }
            for (id, group) in api.groups() {
                let (generation, _) = upsert_identity(
                    connection,
                    &review.api_id,
                    id.as_str(),
                    "group",
                    Meaning {
                        regranted: review.consent_changes.contains(&id.to_string()),
                        title: &group.title,
                        description: &group.description,
                        grants: &group.grants,
                    },
                    now,
                )?;
                connection.execute(
                    "DELETE FROM auth_capability_group_members \
                    WHERE group_id=?1 AND group_generation=?2",
                    params![id.as_str(), generation],
                )?;
                for member in &group.members {
                    connection.execute(
                        "INSERT INTO auth_capability_group_members VALUES(?1,?2,?3)",
                        params![id.as_str(), generation, member.as_str()],
                    )?;
                }
            }
            for edit in &acceptance.policy_edits {
                if !matches!(
                    edit,
                    PolicyMutation::RolePut { .. }
                        | PolicyMutation::RoleDelete { .. }
                        | PolicyMutation::OidcMappingPut { .. }
                        | PolicyMutation::OidcMappingDelete { .. }
                ) {
                    return Err(AuthError::Denied);
                }
                apply_policy_in_transaction(connection, identity, edit, now)?;
            }
            let snapshot = snapshot.sign(&self.current_signer(connection)?)?;
            connection.execute(
                "INSERT INTO auth_api_verification_snapshots VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    snapshot.digest()?,
                    review.api_id,
                    generation,
                    revision,
                    self.current_issuer(connection)?.key_id,
                    json(&snapshot)?.as_bytes(),
                    now
                ],
            )?;
            connection.execute(
                "UPDATE auth_api_reviews SET consumed_at=?1 WHERE review_id=?2",
                params![now, review.review_id],
            )?;
            Self::policy_changed(
                connection,
                &EnforcementScope::Api(review.api_id.clone()),
                now,
            )?;
            super::revocation::enqueue_effect(
                connection,
                "authority_publish",
                &serde_json::json!({"kind":"catalog","digest":snapshot.digest()?}),
                None,
                now,
            )?;
            Ok(snapshot)
        }
    }

    pub(crate) fn current_catalog(&self, api_id: &str) -> Result<SignedCatalogSnapshot, AuthError> {
        self.transaction(|connection| {
            let bytes: Vec<u8> = connection
                .query_row(
                    "SELECT s.signed_bytes \
                    FROM auth_accepted_apis a \
                    JOIN auth_api_verification_snapshots s ON s.api_id=a.api_id \
                    AND s.generation=a.generation \
                    AND s.accepted_revision=a.accepted_revision \
                    WHERE a.api_id=?1",
                    [api_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            Ok(serde_json::from_slice(&bytes)?)
        })
    }
}
