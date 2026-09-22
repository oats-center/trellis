use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use super::super::application::repository::{OutboxRepository, PostCommitActionClaim};
use super::super::{AuthorizationStateError, IdempotencyResultRecord, PostCommitActionRecord};
use super::common::{
    decode_enum, decode_json, encode_enum, encode_json, from_sql_u32, map_write_error, sql_error,
};
use super::validation::post_commit_action_identity_equal;
use super::SqliteAuthorizationStore;

#[async_trait]
impl OutboxRepository for SqliteAuthorizationStore {
    async fn get_idempotency_result(
        &self,
        purpose: &str,
        signer_id: &str,
        request_id: &str,
    ) -> Result<Option<IdempotencyResultRecord>, AuthorizationStateError> {
        let purpose = purpose.to_owned();
        let signer_id = signer_id.to_owned();
        let request_id = request_id.to_owned();
        self.run_read(move |connection| {
            load_idempotency_result(connection, &purpose, &signer_id, &request_id)
        })
        .await
    }

    async fn list_ready_post_commit_actions(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<PostCommitActionRecord>, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("now", now)?;
        let limit = i64::try_from(limit).map_err(|_| {
            AuthorizationStateError::InvalidRecord("limit exceeds SQLite range".to_owned())
        })?;
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT action.action_id, action.kind, action.payload_json, action.created_at,
                            action.attempts, action.next_attempt_at, action.claimed_until,
                            action.last_error, action.predecessor_action_id
                     FROM auth_post_commit_actions AS action
                     WHERE action.next_attempt_at <= ?1
                       AND (action.claimed_until IS NULL OR action.claimed_until <= ?1)
                       AND (action.predecessor_action_id IS NULL OR NOT EXISTS (
                           SELECT 1 FROM auth_post_commit_actions AS predecessor
                           WHERE predecessor.action_id = action.predecessor_action_id
                       ))
                     ORDER BY action.next_attempt_at, action.rowid LIMIT ?2",
                )
                .map_err(sql_error)?;
            let actions = statement
                .query_map(params![now, limit], decode_post_commit_action)
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(actions)
        })
        .await
    }

    async fn claim_post_commit_action(
        &self,
        action_id: &str,
        now: i64,
        claimed_until: i64,
    ) -> Result<Option<PostCommitActionClaim>, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("now", now)?;
        super::super::domain::require_protocol_timestamp("claimedUntil", claimed_until)?;
        if claimed_until <= now {
            return Err(AuthorizationStateError::InvalidRecord(
                "claimedUntil must follow now".to_owned(),
            ));
        }
        let action_id = action_id.to_owned();
        let claim_token = ulid::Ulid::new().to_string();
        self.run(move |connection| {
            let changed = connection
                .execute(
                    "UPDATE auth_post_commit_actions
                     SET claimed_until = ?1, claim_token = ?2, attempts = attempts + 1
                     WHERE action_id = ?3 AND next_attempt_at <= ?4
                       AND (claimed_until IS NULL OR claimed_until <= ?4)
                       AND attempts < ?5",
                    params![
                        claimed_until,
                        claim_token,
                        action_id,
                        now,
                        i64::from(u32::MAX)
                    ],
                )
                .map_err(map_write_error)?;
            if changed == 0 {
                return Ok(None);
            }
            Ok(
                load_post_commit_action(connection, &action_id)?.map(|action| {
                    PostCommitActionClaim {
                        action,
                        token: claim_token,
                    }
                }),
            )
        })
        .await
    }

    async fn prepare_post_commit_event_delivery(
        &self,
        action_id: &str,
        claim_token: &str,
        delivery: serde_json::Value,
    ) -> Result<serde_json::Value, AuthorizationStateError> {
        let action_id = action_id.to_owned();
        let claim_token = claim_token.to_owned();
        self.run(move |connection| {
            let current = load_post_commit_action(connection, &action_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.kind != super::super::PostCommitActionKind::Event
                || !post_commit_claim_matches(connection, &action_id, &claim_token)?
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let existing = connection
                .query_row(
                    "SELECT event_delivery_json FROM auth_post_commit_actions WHERE action_id = ?1",
                    [&action_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(sql_error)?;
            if let Some(existing) = existing {
                return decode_json(existing).map_err(sql_error);
            }
            connection
                .execute(
                    "UPDATE auth_post_commit_actions SET event_delivery_json = ?1
                     WHERE action_id = ?2 AND claim_token = ?3
                       AND event_delivery_json IS NULL",
                    params![encode_json(&delivery)?, action_id, claim_token,],
                )
                .map_err(map_write_error)?;
            connection
                .query_row(
                    "SELECT event_delivery_json FROM auth_post_commit_actions
                     WHERE action_id = ?1 AND claim_token = ?2",
                    params![action_id, claim_token],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(sql_error)?
                .ok_or(AuthorizationStateError::StorageConflict)
                .and_then(|saved| decode_json(saved).map_err(sql_error))
        })
        .await
    }

    async fn fail_post_commit_action(
        &self,
        action_id: &str,
        claim_token: &str,
        next_attempt_at: i64,
        error: String,
    ) -> Result<PostCommitActionRecord, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("nextAttemptAt", next_attempt_at)?;
        super::super::domain::require_nonempty("error", &error)?;
        let action_id = action_id.to_owned();
        let claim_token = claim_token.to_owned();
        self.run(move |connection| {
            let changed = connection
                .execute(
                    "UPDATE auth_post_commit_actions SET next_attempt_at = ?1, claimed_until = NULL, claim_token = NULL, last_error = ?2
                  WHERE action_id = ?3 AND claim_token = ?4",
                    params![
                        next_attempt_at,
                        error,
                        action_id,
                        claim_token,
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            load_post_commit_action(connection, &action_id)?
                .ok_or(AuthorizationStateError::StorageConflict)
        })
        .await
    }

    async fn acknowledge_post_commit_action(
        &self,
        action_id: &str,
        claim_token: &str,
    ) -> Result<(), AuthorizationStateError> {
        let action_id = action_id.to_owned();
        let claim_token = claim_token.to_owned();
        self.run(move |connection| {
            if !post_commit_claim_matches(connection, &action_id, &claim_token)? {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let changed = connection
                .execute(
                    "DELETE FROM auth_post_commit_actions WHERE action_id = ?1 AND claim_token = ?2",
                    params![action_id, claim_token],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            Ok(())
        })
        .await
    }
}

impl SqliteAuthorizationStore {
    pub(crate) async fn enqueue_post_commit_actions(
        &self,
        actions: Vec<PostCommitActionRecord>,
    ) -> Result<(), AuthorizationStateError> {
        for action in &actions {
            super::super::application::validation::validate_post_commit_action(action)?;
        }
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            insert_sql_post_commit_actions(&transaction, &actions)?;
            transaction.commit().map_err(sql_error)
        })
        .await
    }

    pub(crate) async fn enqueue_idempotent_post_commit_actions(
        &self,
        idempotency: IdempotencyResultRecord,
        actions: Vec<PostCommitActionRecord>,
    ) -> Result<bool, AuthorizationStateError> {
        super::super::application::validation::validate_idempotency_and_actions(
            &idempotency,
            &actions,
        )?;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if sqlite_idempotency_replay(&transaction, &idempotency)?.is_some() {
                return Ok(false);
            }
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(true)
        })
        .await
    }
}

fn post_commit_claim_matches(
    connection: &Connection,
    action_id: &str,
    claim_token: &str,
) -> Result<bool, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_post_commit_actions WHERE action_id = ?1 AND claim_token = ?2)",
            params![action_id, claim_token],
            |row| row.get(0),
        )
        .map_err(sql_error)
}

pub(in crate::platform::auth) fn sqlite_idempotency_replay(
    connection: &Connection,
    input: &IdempotencyResultRecord,
) -> Result<Option<serde_json::Value>, AuthorizationStateError> {
    if let Some(existing) = load_idempotency_result(
        connection,
        &input.purpose,
        &input.signer_id,
        &input.request_id,
    )? {
        if existing.request_digest != input.request_digest {
            tracing::warn!(
                purpose = %input.purpose,
                signer_id = %input.signer_id,
                request_id = %input.request_id,
                "idempotency request digest conflict"
            );
            return Err(AuthorizationStateError::StorageConflict);
        }
        return Ok(Some(existing.result));
    }
    let scope_exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_idempotency_results WHERE scope_key = ?1)",
            [&input.scope_key],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sql_error)?;
    if scope_exists {
        tracing::warn!(
            purpose = %input.purpose,
            signer_id = %input.signer_id,
            request_id = %input.request_id,
            "idempotency scope conflict"
        );
        return Err(AuthorizationStateError::StorageConflict);
    }
    Ok(None)
}

pub(in crate::platform::auth) fn insert_sql_idempotency_and_actions(
    connection: &Connection,
    idempotency: &IdempotencyResultRecord,
    actions: &[PostCommitActionRecord],
) -> Result<(), AuthorizationStateError> {
    if sqlite_idempotency_replay(connection, idempotency)?.is_some() {
        return Err(AuthorizationStateError::StorageConflict);
    }
    for (index, action) in actions.iter().enumerate() {
        if actions[..index].iter().any(|existing| {
            existing.action_id == action.action_id
                && !post_commit_action_identity_equal(existing, action)
        }) {
            return Err(AuthorizationStateError::StorageConflict);
        }
        if load_post_commit_action(connection, &action.action_id)?
            .is_some_and(|existing| !post_commit_action_identity_equal(&existing, action))
        {
            return Err(AuthorizationStateError::StorageConflict);
        }
    }
    connection
    .execute(
        "INSERT INTO auth_idempotency_results (scope_key, purpose, signer_id, request_id, request_digest, result_json, created_at, expires_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            idempotency.scope_key,
            idempotency.purpose,
            idempotency.signer_id,
            idempotency.request_id,
            idempotency.request_digest,
            encode_json(&idempotency.result)?,
            idempotency.created_at,
            idempotency.expires_at
        ],
    )
    .map_err(map_write_error)?;
    insert_sql_post_commit_actions(connection, actions)
}

pub(in crate::platform::auth) fn insert_sql_post_commit_actions(
    connection: &Connection,
    actions: &[PostCommitActionRecord],
) -> Result<(), AuthorizationStateError> {
    let mut predecessor_action_id: Option<&str> = None;
    for action in actions {
        let action_predecessor_id = action
            .predecessor_action_id
            .as_deref()
            .or(predecessor_action_id);
        if load_post_commit_action(connection, &action.action_id)?.is_none() {
            connection
            .execute(
                "INSERT INTO auth_post_commit_actions (action_id, kind, payload_json, created_at, attempts, next_attempt_at, claimed_until, last_error, predecessor_action_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    action.action_id,
                    encode_enum(action.kind)?,
                    encode_json(&action.payload)?,
                    action.created_at,
                    i64::from(action.attempts),
                    action.next_attempt_at,
                    action.claimed_until,
                    action.last_error,
                    action_predecessor_id,
                ],
            )
            .map_err(map_write_error)?;
        }
        predecessor_action_id = Some(&action.action_id);
    }
    Ok(())
}

pub(in crate::platform::auth) fn load_idempotency_result(
    connection: &Connection,
    purpose: &str,
    signer_id: &str,
    request_id: &str,
) -> Result<Option<IdempotencyResultRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT scope_key, purpose, signer_id, request_id, request_digest, result_json, created_at, expires_at FROM auth_idempotency_results WHERE purpose = ?1 AND signer_id = ?2 AND request_id = ?3",
        params![purpose, signer_id, request_id],
        |row| {
            Ok(IdempotencyResultRecord {
                scope_key: row.get(0)?,
                purpose: row.get(1)?,
                signer_id: row.get(2)?,
                request_id: row.get(3)?,
                request_digest: row.get(4)?,
                result: decode_json(row.get(5)?)?,
                created_at: row.get(6)?,
                expires_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(sql_error)
}

pub(in crate::platform::auth) fn decode_post_commit_action(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PostCommitActionRecord> {
    Ok(PostCommitActionRecord {
        predecessor_action_id: row.get(8)?,
        action_id: row.get(0)?,
        kind: decode_enum(row.get(1)?)?,
        payload: decode_json(row.get(2)?)?,
        created_at: row.get(3)?,
        attempts: from_sql_u32(row.get(4)?)?,
        next_attempt_at: row.get(5)?,
        claimed_until: row.get(6)?,
        last_error: row.get(7)?,
    })
}

pub(in crate::platform::auth) fn load_post_commit_action(
    connection: &Connection,
    action_id: &str,
) -> Result<Option<PostCommitActionRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT action_id, kind, payload_json, created_at, attempts, next_attempt_at, claimed_until, last_error, predecessor_action_id FROM auth_post_commit_actions WHERE action_id = ?1",
        [action_id],
        decode_post_commit_action,
    )
    .optional()
    .map_err(sql_error)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::platform::auth::{
        IdempotencyResultRecord, PostCommitActionKind, PostCommitActionRecord,
    };

    #[tokio::test]
    async fn event_delivery_is_prepared_once_by_the_current_claim(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const NOW: i64 = 1_700_000_000_000;
        let store = SqliteAuthorizationStore::open_in_memory()?;
        let idempotency = IdempotencyResultRecord {
            scope_key: "s".repeat(43),
            purpose: "purpose".to_owned(),
            signer_id: "signer".to_owned(),
            request_id: "request".to_owned(),
            request_digest: "r".repeat(43),
            result: json!({"ok": true}),
            created_at: NOW,
            expires_at: NOW + 60_000,
        };
        let action_id = "a".repeat(43);
        let action = PostCommitActionRecord {
            predecessor_action_id: None,
            action_id: action_id.clone(),
            kind: PostCommitActionKind::Event,
            payload: json!({"eventId": "event", "occurredAt": NOW}),
            created_at: NOW,
            attempts: 0,
            next_attempt_at: NOW,
            claimed_until: None,
            last_error: None,
        };
        store
            .run(move |connection| {
                insert_sql_idempotency_and_actions(connection, &idempotency, &[action])
            })
            .await
            .expect("insert event action");

        let claimed_until = NOW + 30_000;
        let claimed = store
            .claim_post_commit_action(&action_id, NOW, claimed_until)
            .await
            .expect("claim event")
            .expect("claimed event");
        assert!(store
            .claim_post_commit_action(&action_id, NOW, claimed_until)
            .await
            .expect("competing claim")
            .is_none());
        let first = json!({"proof": "first"});
        assert_eq!(
            store
                .prepare_post_commit_event_delivery(&action_id, &claimed.token, first.clone(),)
                .await
                .expect("prepare first delivery"),
            first
        );
        assert_eq!(
            store
                .prepare_post_commit_event_delivery(
                    &action_id,
                    &claimed.token,
                    json!({"proof": "replacement"}),
                )
                .await
                .expect("read winning delivery"),
            first
        );

        store
            .fail_post_commit_action(&action_id, &claimed.token, NOW + 31_000, "retry".to_owned())
            .await
            .expect("schedule retry");
        let reclaimed_until = NOW + 61_000;
        let reclaimed = store
            .claim_post_commit_action(&action_id, NOW + 31_000, reclaimed_until)
            .await
            .expect("reclaim event")
            .expect("reclaimed event");
        assert!(store
            .acknowledge_post_commit_action(&action_id, &claimed.token)
            .await
            .is_err());
        assert!(store
            .fail_post_commit_action(&action_id, &claimed.token, NOW + 62_000, "stale".to_owned(),)
            .await
            .is_err());
        assert!(store
            .prepare_post_commit_event_delivery(
                &action_id,
                &claimed.token,
                json!({"proof": "stale"}),
            )
            .await
            .is_err());
        assert_eq!(
            store
                .prepare_post_commit_event_delivery(
                    &action_id,
                    &reclaimed.token,
                    json!({"proof": "new"}),
                )
                .await
                .expect("read prepared delivery after retry"),
            first
        );
        Ok(())
    }
}
