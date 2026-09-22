use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use trellis_protocol::{AuthorizationIssuerKey, AuthorizationIssuerState};
use trellis_runtime_apis::apis::trellis_auth_v1::events::IssuersRevoked;

use super::super::{
    auth_event_subject,
    context::{
        revoke_sql_contexts, AuthorizationContextRevocationReason, AuthorizationContextSelector,
    },
    domain::require_protocol_timestamp,
    AuthorizationStateError, IdempotencyResultRecord, PostCommitActionKind, PostCommitActionRecord,
};
use super::{
    common::{map_write_error, sql_error},
    outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay},
    SqliteAuthorizationStore,
};

impl SqliteAuthorizationStore {
    /// Select a new configured signer without revoking the previous key or its contexts.
    pub(crate) async fn activate_issuer(
        &self,
        issuer: AuthorizationIssuerKey,
        now_ms: i64,
    ) -> Result<(), AuthorizationStateError> {
        require_protocol_timestamp("now", now_ms)?;
        issuer
            .verifying_key()
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        if issuer.state != AuthorizationIssuerState::Active {
            return Err(AuthorizationStateError::InvalidRecord(
                "configured issuer must be active".to_owned(),
            ));
        }
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let existing = transaction.query_row(
                "SELECT public_key, is_current, revoked_at FROM auth_authorization_issuers WHERE key_id = ?1",
                [&issuer.key_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?, row.get::<_, Option<i64>>(2)?)),
            ).optional().map_err(sql_error)?;
            if let Some((public_key, is_current, revoked_at)) = existing {
                if public_key != issuer.public_key || revoked_at.is_some() {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "configured issuer was revoked or has conflicting key material".to_owned(),
                    ));
                }
                if is_current {
                    return Ok(());
                }
                transaction.execute("UPDATE auth_authorization_issuers SET is_current = 0 WHERE is_current = 1", []).map_err(map_write_error)?;
                transaction.execute("UPDATE auth_authorization_issuers SET is_current = 1 WHERE key_id = ?1", [&issuer.key_id]).map_err(map_write_error)?;
                return transaction.commit().map_err(sql_error);
            }
            transaction.execute("UPDATE auth_authorization_issuers SET is_current = 0 WHERE is_current = 1", []).map_err(map_write_error)?;
            transaction.execute(
                "INSERT INTO auth_authorization_issuers (key_id, public_key, is_current, created_at) VALUES (?1, ?2, 1, ?3)",
                params![issuer.key_id, issuer.public_key, now_ms],
            ).map_err(map_write_error)?;
            transaction.commit().map_err(sql_error)
        }).await
    }

    /// Return retained public verification material, including retired and revoked keys.
    pub(crate) async fn get_issuer_key(
        &self,
        key_id: String,
        now_ms: i64,
    ) -> Result<Option<AuthorizationIssuerKey>, AuthorizationStateError> {
        require_protocol_timestamp("now", now_ms)?;
        self.run_read(move |connection| {
            connection.query_row(
                "SELECT public_key, is_current, live_until_seconds, revoked_at FROM auth_authorization_issuers WHERE key_id = ?1",
                [&key_id],
                |row| {
                    let state = if row.get::<_, Option<i64>>(3)?.is_some() {
                        AuthorizationIssuerState::Revoked
                    } else if row.get::<_, bool>(1)? || row.get::<_, i64>(2)? >= now_ms.div_euclid(1_000) {
                        AuthorizationIssuerState::Active
                    } else {
                        AuthorizationIssuerState::Retired
                    };
                    Ok(AuthorizationIssuerKey { key_id: key_id.clone(), public_key: row.get(0)?, state })
                },
            ).optional().map_err(sql_error)
        }).await
    }

    /// Irreversibly revoke a noncurrent issuer with its event and context/kick intents.
    pub(crate) async fn revoke_issuer(
        &self,
        actor: super::super::MutationActor,
        key_id: String,
        revoked_by: String,
        reason: Option<String>,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        require_protocol_timestamp("now", idempotency.created_at)?;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            super::grants::require_current_actor(
                &transaction,
                &actor,
                true,
                idempotency.created_at,
            )?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let (is_current, revoked_at) = transaction.query_row(
                "SELECT is_current, revoked_at FROM auth_authorization_issuers WHERE key_id = ?1",
                [&key_id], |row| Ok((row.get::<_, bool>(0)?, row.get::<_, Option<i64>>(1)?)),
            ).optional().map_err(sql_error)?.ok_or(AuthorizationStateError::IssuerMissing)?;
            if is_current {
                return Err(AuthorizationStateError::CurrentIssuerConflict);
            }
            let mut actions = Vec::new();
            if revoked_at.is_none() {
                transaction.execute(
                    "UPDATE auth_authorization_issuers SET revoked_at = ?1 WHERE key_id = ?2 AND revoked_at IS NULL AND is_current = 0",
                    params![idempotency.created_at, key_id],
                ).map_err(map_write_error)?;
                revoke_sql_contexts(
                    &transaction, &AuthorizationContextSelector::Issuer(key_id.clone()),
                    AuthorizationContextRevocationReason::IssuerRevoked, idempotency.created_at.div_euclid(1_000),
                )?;
                let mut payload = json!({
                    "eventType": "Auth.Issuers.Revoked",
                    "eventId": ulid::Ulid::new().to_string(), "occurredAt": idempotency.created_at,
                    "keyId": key_id, "revokedBy": revoked_by,
                });
                if let Some(reason) = reason {
                    payload["reason"] = json!(reason);
                }
                payload["eventSubject"] = json!(auth_event_subject::<IssuersRevoked>(&payload)?);
                actions.push(PostCommitActionRecord {
                    action_id: trellis_protocol::digest_json(&json!({"event": "Auth.Issuers.Revoked", "keyId": key_id}))
                        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
                    predecessor_action_id: None, kind: PostCommitActionKind::Event, payload,
                    created_at: idempotency.created_at, attempts: 0, next_attempt_at: idempotency.created_at,
                    claimed_until: None, last_error: None,
                });
            }
            idempotency.result = json!({"keyId": key_id, "state": "revoked"});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::SigningKey;
    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::platform::auth::{rpc::rpc_idempotency, OutboxRepository};

    #[tokio::test]
    async fn rotation_and_idempotent_revocation_remain_separate(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let store = SqliteAuthorizationStore::open_in_memory()?;
        let [first, second] = [7, 8].map(|byte| {
            let key = SigningKey::from_bytes(&[byte; 32]).verifying_key();
            AuthorizationIssuerKey {
                key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes())),
                public_key: URL_SAFE_NO_PAD.encode(key.as_bytes()),
                state: AuthorizationIssuerState::Active,
            }
        });
        let now = 1_735_689_600_000;
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, now,
            )
            .await?;
        let admin = actor.principal_id.clone();
        let request_id = ulid::Ulid::new().to_string();
        let input = json!({"keyId": first.key_id, "reason": "compromised"});
        let idempotency = rpc_idempotency("Auth.Issuers.Revoke", &admin, &request_id, &input, now)?;

        store.activate_issuer(first.clone(), now).await?;
        assert_eq!(
            store
                .revoke_issuer(
                    actor.clone(),
                    first.key_id.clone(),
                    admin.clone(),
                    Some("compromised".to_owned()),
                    idempotency.clone(),
                )
                .await,
            Err(AuthorizationStateError::CurrentIssuerConflict)
        );
        assert!(store
            .list_ready_post_commit_actions(now, 100)
            .await?
            .is_empty());

        store.activate_issuer(second.clone(), now).await?;
        assert!(store
            .list_ready_post_commit_actions(now, 100)
            .await?
            .is_empty());
        assert_eq!(
            store
                .get_issuer_key(first.key_id.clone(), now)
                .await?
                .unwrap()
                .state,
            AuthorizationIssuerState::Retired
        );
        store.activate_issuer(first.clone(), now).await?;
        assert_eq!(
            store
                .get_issuer_key(first.key_id.clone(), now)
                .await?
                .unwrap()
                .state,
            AuthorizationIssuerState::Active
        );
        store.activate_issuer(second.clone(), now).await?;

        let result = store
            .revoke_issuer(
                actor.clone(),
                first.key_id.clone(),
                admin.clone(),
                Some("compromised".to_owned()),
                idempotency.clone(),
            )
            .await?;
        assert_eq!(result, json!({"keyId": first.key_id, "state": "revoked"}));
        assert_eq!(
            store
                .revoke_issuer(
                    actor.clone(),
                    first.key_id.clone(),
                    admin.clone(),
                    Some("compromised".to_owned()),
                    idempotency,
                )
                .await?,
            result
        );
        let actions = store.list_ready_post_commit_actions(now, 100).await?;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, PostCommitActionKind::Event);
        assert_eq!(actions[0].payload["eventType"], "Auth.Issuers.Revoked");
        assert_eq!(actions[0].payload["reason"], "compromised");

        let repeated = rpc_idempotency(
            "Auth.Issuers.Revoke",
            &admin,
            &ulid::Ulid::new().to_string(),
            &input,
            now,
        )?;
        assert_eq!(
            store
                .revoke_issuer(
                    actor.clone(),
                    first.key_id.clone(),
                    admin.clone(),
                    Some("compromised".to_owned()),
                    repeated,
                )
                .await?,
            result
        );
        assert_eq!(
            store.list_ready_post_commit_actions(now, 100).await?.len(),
            1
        );
        let changed = rpc_idempotency(
            "Auth.Issuers.Revoke",
            &admin,
            &request_id,
            &json!({"keyId": first.key_id, "reason": "different"}),
            now,
        )?;
        assert_eq!(
            store
                .revoke_issuer(
                    actor,
                    first.key_id.clone(),
                    admin,
                    Some("different".to_owned()),
                    changed,
                )
                .await,
            Err(AuthorizationStateError::StorageConflict)
        );
        let revoked = store
            .get_issuer_key(first.key_id.clone(), now)
            .await?
            .unwrap();
        assert_eq!(revoked.state, AuthorizationIssuerState::Revoked);
        assert_eq!(revoked.public_key, first.public_key);
        assert!(store.activate_issuer(first, now).await.is_err());
        assert_eq!(
            store.get_issuer_key(second.key_id.clone(), now).await?,
            Some(second)
        );
        Ok(())
    }
}
