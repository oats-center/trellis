use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};

use super::super::{
    AuthorizationStateError, CapabilityGroupRecord, IdempotencyResultRecord, IdempotentOutcome,
    PortalGrantBindingRecord, PortalGrantOverrideRecord, PortalGrantProvenance,
};
use super::common::{decode_json, encode_json, map_write_error, sql_error};
use super::outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay};
use super::SqliteAuthorizationStore;

pub(crate) const ADMIN_CAPABILITY_GROUP_CAPABILITIES: &[&str] = &[
    "trellis.auth::admin",
    "trellis.auth::authorities.mutate",
    "trellis.auth::authorities.read",
    "trellis.auth::capabilities.delegate",
    "trellis.auth::capabilities.read",
    "trellis.auth::connections.kick",
    "trellis.auth::connections.read",
    "trellis.auth::deployments.mutate",
    "trellis.auth::deployments.read",
    "trellis.auth::devices.mutate",
    "trellis.auth::devices.read",
    "trellis.auth::devices.review",
    "trellis.auth::portals.mutate",
    "trellis.auth::portals.read",
    "trellis.auth::services.mutate",
    "trellis.auth::services.read",
    "trellis.auth::sessions.read",
    "trellis.auth::sessions.revoke",
    "trellis.auth::users.mutate",
    "trellis.auth::users.read",
];

impl SqliteAuthorizationStore {
    pub(crate) async fn ensure_admin_capability_group(
        &self,
        now: i64,
    ) -> Result<(), AuthorizationStateError> {
        let capabilities = ADMIN_CAPABILITY_GROUP_CAPABILITIES
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let current = load_capability_group(&transaction, "admin")?;
            let group = CapabilityGroupRecord {
                group_key: "admin".to_owned(),
                display_name: "Administrator".to_owned(),
                description:
                    "Full authority required to administer Trellis accounts and capabilities."
                        .to_owned(),
                capabilities,
                included_groups: Vec::new(),
                created_at: current.as_ref().map_or(now, |group| group.created_at),
                updated_at: now,
                version: 1,
            };
            if current.as_ref().is_some_and(|current| {
                current.version == 1
                    && current.display_name == group.display_name
                    && current.description == group.description
                    && current.capabilities == group.capabilities
                    && current.included_groups == group.included_groups
            }) {
                return Ok(());
            }
            let mut groups = load_capability_groups(&transaction)?
                .into_iter()
                .map(|record| (record.group_key.clone(), record))
                .collect::<BTreeMap<_, _>>();
            groups.insert(group.group_key.clone(), group.clone());
            validate_capability_groups(&groups)?;
            transaction
                .execute(
                    "INSERT INTO auth_capability_groups (
                         group_key, display_name, description, capabilities_json,
                         included_groups_json, created_at, updated_at, version
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(group_key) DO UPDATE SET
                         display_name = excluded.display_name,
                         description = excluded.description,
                         capabilities_json = excluded.capabilities_json,
                         included_groups_json = excluded.included_groups_json,
                         updated_at = excluded.updated_at,
                         version = excluded.version",
                    params![
                        group.group_key,
                        group.display_name,
                        group.description,
                        encode_json(&group.capabilities)?,
                        encode_json(&group.included_groups)?,
                        group.created_at,
                        group.updated_at,
                        group.version,
                    ],
                )
                .map_err(map_write_error)?;
            transaction.commit().map_err(sql_error)
        })
        .await
    }

    pub(crate) async fn list_capability_groups(
        &self,
    ) -> Result<Vec<CapabilityGroupRecord>, AuthorizationStateError> {
        self.run_read(|connection| load_capability_groups(connection))
            .await
    }

    pub(crate) async fn get_capability_group(
        &self,
        group_key: &str,
    ) -> Result<Option<CapabilityGroupRecord>, AuthorizationStateError> {
        let group_key = group_key.to_owned();
        self.run_read(move |connection| load_capability_group(connection, &group_key))
            .await
    }

    pub(crate) async fn put_capability_group(
        &self,
        group: CapabilityGroupRecord,
        expected_version: Option<u64>,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<IdempotentOutcome<CapabilityGroupRecord>, AuthorizationStateError> {
        if group.group_key == "admin" {
            return Err(AuthorizationStateError::InvalidRecord(
                "the built-in admin capability group is read-only".to_owned(),
            ));
        }
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_capability_group(&transaction, &group.group_key)?;
            if current.as_ref().map(|record| record.version) != expected_version {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let mut groups = load_capability_groups(&transaction)?
                .into_iter()
                .map(|record| (record.group_key.clone(), record))
                .collect::<BTreeMap<_, _>>();
            groups.insert(group.group_key.clone(), group.clone());
            validate_capability_groups(&groups)?;
            for policy in load_portal_grant_overrides(&transaction, None, None)? {
                validate_policy(&policy, &groups)?;
            }
            transaction
                .execute(
                    "INSERT INTO auth_capability_groups (
                         group_key, display_name, description, capabilities_json,
                         included_groups_json, created_at, updated_at, version
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(group_key) DO UPDATE SET
                         display_name = excluded.display_name,
                         description = excluded.description,
                         capabilities_json = excluded.capabilities_json,
                         included_groups_json = excluded.included_groups_json,
                         updated_at = excluded.updated_at,
                         version = excluded.version",
                    params![
                        group.group_key,
                        group.display_name,
                        group.description,
                        encode_json(&group.capabilities)?,
                        encode_json(&group.included_groups)?,
                        group.created_at,
                        group.updated_at,
                        group.version,
                    ],
                )
                .map_err(map_write_error)?;
            idempotency.result = serde_json::to_value(&group)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &[])?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(group))
        })
        .await
    }

    pub(crate) async fn delete_capability_group(
        &self,
        group_key: &str,
        expected_version: u64,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<IdempotentOutcome<bool>, AuthorizationStateError> {
        if group_key == "admin" {
            return Err(AuthorizationStateError::InvalidRecord(
                "the built-in admin capability group is read-only".to_owned(),
            ));
        }
        let group_key = group_key.to_owned();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_capability_group(&transaction, &group_key)?;
            let Some(current) = current else {
                idempotency.result = serde_json::Value::Bool(false);
                insert_sql_idempotency_and_actions(&transaction, &idempotency, &[])?;
                transaction.commit().map_err(sql_error)?;
                return Ok(IdempotentOutcome::Applied(false));
            };
            if current.version != expected_version {
                return Err(AuthorizationStateError::StorageConflict);
            }
            if load_capability_groups(&transaction)?.iter().any(|group| {
                group.group_key != group_key && group.included_groups.contains(&group_key)
            }) || load_portal_grant_overrides(&transaction, None, None)?
                .iter()
                .any(|policy| policy_references_group(policy, &group_key))
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let changed = transaction
                .execute(
                    "DELETE FROM auth_capability_groups WHERE group_key = ?1 AND version = ?2",
                    params![group_key, expected_version],
                )
                .map_err(map_write_error)?;
            idempotency.result = serde_json::Value::Bool(changed == 1);
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &[])?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(changed == 1))
        })
        .await
    }

    pub(crate) async fn list_portal_grant_overrides(
        &self,
        portal_id: Option<&str>,
        participant_id: Option<&str>,
    ) -> Result<Vec<PortalGrantOverrideRecord>, AuthorizationStateError> {
        let portal_id = portal_id.map(str::to_owned);
        let participant_id = participant_id.map(str::to_owned);
        self.run_read(move |connection| {
            load_portal_grant_overrides(connection, portal_id.as_deref(), participant_id.as_deref())
        })
        .await
    }

    pub(crate) async fn get_portal_grant_override(
        &self,
        portal_id: &str,
        participant_id: &str,
    ) -> Result<Option<PortalGrantOverrideRecord>, AuthorizationStateError> {
        Ok(self
            .list_portal_grant_overrides(Some(portal_id), Some(participant_id))
            .await?
            .into_iter()
            .next())
    }

    pub(crate) async fn put_portal_grant_override(
        &self,
        policy: PortalGrantOverrideRecord,
        expected_version: Option<u64>,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<IdempotentOutcome<PortalGrantOverrideRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_portal_grant_overrides(
                &transaction,
                Some(&policy.portal_id),
                Some(&policy.participant_id),
            )?
            .into_iter()
            .next();
            if current.as_ref().map(|record| record.version) != expected_version {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let groups = load_capability_groups(&transaction)?
                .into_iter()
                .map(|group| (group.group_key.clone(), group))
                .collect::<BTreeMap<_, _>>();
            validate_policy(&policy, &groups)?;
            transaction
                .execute(
                    "INSERT INTO auth_portal_grant_overrides (
                         portal_id, participant_id, direct_capabilities_json,
                         capability_group_keys_json, role_mappings_json,
                         created_at, updated_at, version
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(portal_id, participant_id) DO UPDATE SET
                         direct_capabilities_json = excluded.direct_capabilities_json,
                         capability_group_keys_json = excluded.capability_group_keys_json,
                         role_mappings_json = excluded.role_mappings_json,
                         updated_at = excluded.updated_at,
                         version = excluded.version",
                    params![
                        policy.portal_id,
                        policy.participant_id,
                        encode_json(&policy.direct_capabilities)?,
                        encode_json(&policy.capability_group_keys)?,
                        encode_json(&policy.role_mappings)?,
                        policy.created_at,
                        policy.updated_at,
                        policy.version,
                    ],
                )
                .map_err(map_write_error)?;
            idempotency.result = serde_json::to_value(&policy)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &[])?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(policy))
        })
        .await
    }

    pub(crate) async fn remove_portal_grant_override(
        &self,
        portal_id: &str,
        participant_id: &str,
        expected_version: u64,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<IdempotentOutcome<Option<PortalGrantOverrideRecord>>, AuthorizationStateError> {
        let portal_id = portal_id.to_owned();
        let participant_id = participant_id.to_owned();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current =
                load_portal_grant_overrides(&transaction, Some(&portal_id), Some(&participant_id))?
                    .into_iter()
                    .next();
            let Some(current) = current else {
                idempotency.result = serde_json::Value::Null;
                insert_sql_idempotency_and_actions(&transaction, &idempotency, &[])?;
                transaction.commit().map_err(sql_error)?;
                return Ok(IdempotentOutcome::Applied(None));
            };
            if current.version != expected_version {
                return Err(AuthorizationStateError::StorageConflict);
            }
            transaction
                .execute(
                    "DELETE FROM auth_portal_grant_overrides
                     WHERE portal_id = ?1 AND participant_id = ?2 AND version = ?3",
                    params![portal_id, participant_id, expected_version],
                )
                .map_err(map_write_error)?;
            idempotency.result = serde_json::to_value(&current)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &[])?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(Some(current)))
        })
        .await
    }

    pub(crate) async fn list_portal_grant_bindings(
        &self,
    ) -> Result<Vec<PortalGrantBindingRecord>, AuthorizationStateError> {
        self.run_read(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT owner_id, participant_id, provenance_json, revision, updated_at
                     FROM auth_grant_bindings
                     WHERE owner_kind = 'user' AND provenance_json IS NOT NULL
                     ORDER BY owner_id, participant_id",
                )
                .map_err(sql_error)?;
            let records = statement
                .query_map([], decode_portal_grant_binding)
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(records)
        })
        .await
    }
}

pub(super) fn load_capability_groups(
    connection: &Connection,
) -> Result<Vec<CapabilityGroupRecord>, AuthorizationStateError> {
    let mut statement = connection
        .prepare(
            "SELECT group_key, display_name, description, capabilities_json,
                    included_groups_json, created_at, updated_at, version
             FROM auth_capability_groups ORDER BY group_key",
        )
        .map_err(sql_error)?;
    let records = statement
        .query_map([], decode_capability_group)
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    Ok(records)
}

pub(super) fn load_capability_group(
    connection: &Connection,
    group_key: &str,
) -> Result<Option<CapabilityGroupRecord>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT group_key, display_name, description, capabilities_json,
                    included_groups_json, created_at, updated_at, version
             FROM auth_capability_groups WHERE group_key = ?1",
            [group_key],
            decode_capability_group,
        )
        .optional()
        .map_err(sql_error)
}

fn decode_capability_group(row: &rusqlite::Row<'_>) -> rusqlite::Result<CapabilityGroupRecord> {
    Ok(CapabilityGroupRecord {
        group_key: row.get(0)?,
        display_name: row.get(1)?,
        description: row.get(2)?,
        capabilities: decode_json(row.get(3)?)?,
        included_groups: decode_json(row.get(4)?)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        version: row.get(7)?,
    })
}

pub(super) fn load_portal_grant_overrides(
    connection: &Connection,
    portal_id: Option<&str>,
    participant_id: Option<&str>,
) -> Result<Vec<PortalGrantOverrideRecord>, AuthorizationStateError> {
    let mut statement = connection
        .prepare(
            "SELECT portal_id, participant_id, direct_capabilities_json,
                    capability_group_keys_json, role_mappings_json,
                    created_at, updated_at, version
             FROM auth_portal_grant_overrides
             WHERE (?1 IS NULL OR portal_id = ?1)
               AND (?2 IS NULL OR participant_id = ?2)
             ORDER BY portal_id, participant_id",
        )
        .map_err(sql_error)?;
    let records = statement
        .query_map(params![portal_id, participant_id], |row| {
            Ok(PortalGrantOverrideRecord {
                portal_id: row.get(0)?,
                participant_id: row.get(1)?,
                direct_capabilities: decode_json(row.get(2)?)?,
                capability_group_keys: decode_json(row.get(3)?)?,
                role_mappings: decode_json(row.get(4)?)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                version: row.get(7)?,
            })
        })
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    Ok(records)
}

pub(super) fn decode_portal_grant_binding(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PortalGrantBindingRecord> {
    let provenance: PortalGrantProvenance = decode_json(row.get(2)?)?;
    Ok(PortalGrantBindingRecord {
        principal_id: row.get(0)?,
        participant_id: row.get(1)?,
        portal_id: provenance.portal_id,
        provider_id: provenance.provider_id,
        roles: provenance.roles,
        effective_policy_digest: provenance.effective_policy_digest,
        grant_revision: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn validate_capability_groups(
    groups: &BTreeMap<String, CapabilityGroupRecord>,
) -> Result<(), AuthorizationStateError> {
    for group in groups.values() {
        if !is_canonical_group_key(&group.group_key)
            || group.display_name.is_empty()
            || group.description.is_empty()
            || !is_sorted_unique_nonempty(&group.capabilities)
            || !is_sorted_unique_nonempty(&group.included_groups)
            || group
                .included_groups
                .iter()
                .any(|key| !groups.contains_key(key))
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "capability group is invalid".to_owned(),
            ));
        }
        visit_group(&group.group_key, groups, &mut BTreeSet::new())?;
    }
    Ok(())
}

fn visit_group(
    key: &str,
    groups: &BTreeMap<String, CapabilityGroupRecord>,
    visiting: &mut BTreeSet<String>,
) -> Result<(), AuthorizationStateError> {
    if !visiting.insert(key.to_owned()) {
        return Err(AuthorizationStateError::InvalidRecord(
            "capability group cycle".to_owned(),
        ));
    }
    for included in &groups[key].included_groups {
        visit_group(included, groups, visiting)?;
    }
    visiting.remove(key);
    Ok(())
}

fn validate_policy(
    policy: &PortalGrantOverrideRecord,
    groups: &BTreeMap<String, CapabilityGroupRecord>,
) -> Result<(), AuthorizationStateError> {
    let valid = !policy.portal_id.is_empty()
        && !policy.participant_id.is_empty()
        && is_sorted_unique_nonempty(&policy.direct_capabilities)
        && is_sorted_unique_nonempty(&policy.capability_group_keys)
        && policy
            .capability_group_keys
            .iter()
            .all(|key| groups.contains_key(key))
        && policy.role_mappings.windows(2).all(|pair| {
            (&pair[0].provider_id, &pair[0].role) < (&pair[1].provider_id, &pair[1].role)
        })
        && policy.role_mappings.iter().all(|mapping| {
            !mapping.provider_id.is_empty()
                && mapping.provider_id != "local"
                && !mapping.role.is_empty()
                && is_sorted_unique_nonempty(&mapping.direct_capabilities)
                && is_sorted_unique_nonempty(&mapping.capability_group_keys)
                && mapping
                    .capability_group_keys
                    .iter()
                    .all(|key| groups.contains_key(key))
        });
    if valid {
        Ok(())
    } else {
        Err(AuthorizationStateError::InvalidRecord(
            "portal grant override is invalid".to_owned(),
        ))
    }
}

fn policy_references_group(policy: &PortalGrantOverrideRecord, group_key: &str) -> bool {
    policy
        .capability_group_keys
        .iter()
        .any(|key| key == group_key)
        || policy.role_mappings.iter().any(|mapping| {
            mapping
                .capability_group_keys
                .iter()
                .any(|key| key == group_key)
        })
}

fn is_canonical_group_key(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn is_sorted_unique_nonempty(values: &[String]) -> bool {
    values.iter().all(|value| !value.is_empty()) && values.windows(2).all(|pair| pair[0] < pair[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::auth::{IdempotencyResultRecord, PortalRoleMapping};
    use serde_json::Value;

    fn idempotency(request_id: &str) -> IdempotencyResultRecord {
        IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&serde_json::json!([
                "test.policy",
                "test",
                request_id
            ]))
            .unwrap(),
            purpose: "test.policy".to_owned(),
            signer_id: "test".to_owned(),
            request_id: request_id.to_owned(),
            request_digest: trellis_protocol::digest_json(&Value::String(request_id.to_owned()))
                .unwrap(),
            result: Value::Null,
            created_at: 1,
            expires_at: 100,
        }
    }

    fn group(key: &str, included_groups: &[&str], capabilities: &[&str]) -> CapabilityGroupRecord {
        CapabilityGroupRecord {
            group_key: key.to_owned(),
            display_name: key.to_owned(),
            description: key.to_owned(),
            capabilities: capabilities
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            included_groups: included_groups
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            created_at: 1,
            updated_at: 1,
            version: 1,
        }
    }

    async fn seed_portal(store: &SqliteAuthorizationStore) {
        store
            .run(|connection| {
                connection.execute(
                    "INSERT INTO auth_login_portals (
                         portal_id, display_name, entry_url, builtin, disabled, removed,
                         local_registration_enabled, provider_ids_json, created_at, updated_at, version
                     ) VALUES ('portal', 'Portal', NULL, 0, 0, 0, 1, '[]', 1, 1, 1)",
                    [],
                ).map_err(map_write_error)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn capability_groups_validate_graph_and_references() {
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        store
            .put_capability_group(
                group("base", &[], &["app::read"]),
                None,
                idempotency("base-create"),
            )
            .await
            .unwrap();
        store
            .put_capability_group(
                group("nested", &["base"], &["app::write"]),
                None,
                idempotency("nested-create"),
            )
            .await
            .unwrap();
        assert!(store
            .put_capability_group(
                group("base", &["nested"], &["app::read"]),
                Some(1),
                idempotency("base-cycle")
            )
            .await
            .is_err());
        assert!(store
            .delete_capability_group("base", 1, idempotency("base-delete-referenced"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn admin_group_reconciles_to_platform_definition() {
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        store.ensure_admin_capability_group(1).await.unwrap();
        let initial = store.get_capability_group("admin").await.unwrap().unwrap();
        assert_eq!(initial.version, 1);
        assert_eq!(
            initial.capabilities,
            ADMIN_CAPABILITY_GROUP_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect::<Vec<_>>()
        );

        store.ensure_admin_capability_group(2).await.unwrap();
        assert_eq!(
            store
                .get_capability_group("admin")
                .await
                .unwrap()
                .unwrap()
                .version,
            1,
            "no-op reconciliation must preserve version 1"
        );

        store
            .run(|connection| {
                connection
                    .execute(
                        "UPDATE auth_capability_groups
                         SET display_name = 'Changed', capabilities_json = '[\"custom::admin\"]',
                             version = 9007199254740990
                         WHERE group_key = 'admin'",
                        [],
                    )
                    .map_err(map_write_error)?;
                Ok(())
            })
            .await
            .unwrap();
        store.ensure_admin_capability_group(3).await.unwrap();

        let repaired = store.get_capability_group("admin").await.unwrap().unwrap();
        assert_eq!(repaired.display_name, "Administrator");
        assert_eq!(repaired.capabilities, initial.capabilities);
        assert_eq!(
            repaired.version, 1,
            "stale development projection must be replaced at version 1"
        );

        store.ensure_admin_capability_group(4).await.unwrap();
        assert_eq!(
            store
                .get_capability_group("admin")
                .await
                .unwrap()
                .unwrap()
                .version,
            1,
            "no-op reconciliation after repair must remain version 1"
        );
    }

    #[tokio::test]
    async fn portal_policy_round_trips_and_blocks_referenced_group_delete() {
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        seed_portal(&store).await;
        store
            .put_capability_group(
                group("operators", &[], &["app::read"]),
                None,
                idempotency("operators-create"),
            )
            .await
            .unwrap();
        let policy = PortalGrantOverrideRecord {
            portal_id: "portal".to_owned(),
            participant_id: "app".to_owned(),
            direct_capabilities: vec!["app::base".to_owned()],
            capability_group_keys: vec!["operators".to_owned()],
            role_mappings: vec![PortalRoleMapping {
                provider_id: "oidc".to_owned(),
                role: "Admin".to_owned(),
                direct_capabilities: vec!["app::write".to_owned()],
                capability_group_keys: Vec::new(),
            }],
            created_at: 1,
            updated_at: 1,
            version: 1,
        };
        store
            .put_portal_grant_override(policy.clone(), None, idempotency("policy-create"))
            .await
            .unwrap();
        assert_eq!(
            store
                .get_portal_grant_override("portal", "app")
                .await
                .unwrap(),
            Some(policy)
        );
        let mut reserved_policy = store
            .get_portal_grant_override("portal", "app")
            .await
            .unwrap()
            .unwrap();
        reserved_policy.direct_capabilities = vec![
            "trellis.auth::users.mutate".to_owned(),
            "app::base".to_owned(),
        ];
        reserved_policy.version = 2;
        assert!(store
            .put_portal_grant_override(reserved_policy, Some(1), idempotency("policy-reserved"))
            .await
            .is_err());
        let mut reserved_group = group("operators", &[], &["trellis.auth::users.mutate"]);
        reserved_group.version = 2;
        assert!(store
            .put_capability_group(reserved_group, Some(1), idempotency("operators-reserved"))
            .await
            .is_ok());
        assert!(store
            .delete_capability_group("operators", 2, idempotency("operators-delete-referenced"))
            .await
            .is_err());
        assert!(matches!(
            store
                .remove_portal_grant_override("portal", "app", 1, idempotency("policy-remove"))
                .await
                .unwrap(),
            IdempotentOutcome::Applied(Some(_))
        ));
        // Replaying the same removal after the row is gone returns the
        // originally committed record instead of an empty result.
        let replayed = store
            .remove_portal_grant_override("portal", "app", 1, idempotency("policy-remove"))
            .await
            .unwrap();
        let IdempotentOutcome::Replayed(value) = replayed else {
            panic!("expected the committed removal result to replay");
        };
        let replayed: PortalGrantOverrideRecord = serde_json::from_value(value).unwrap();
        assert_eq!(replayed.participant_id, "app");
        assert_eq!(replayed.version, 1);
        assert!(matches!(
            store
                .delete_capability_group("operators", 2, idempotency("operators-delete"))
                .await
                .unwrap(),
            IdempotentOutcome::Applied(true)
        ));
    }
}
