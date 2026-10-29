use super::Administration;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trellis_protocol::{SignedSessionRevocation, SESSION_REVOCATION_FORMAT_V1};
use trellis_runtime_apis::{apis::trellis_auth_v1::events, types::AuthSecurityEvent};

use super::authorization_sessions::{loses_authority, session_credential};
use super::policy::evaluate;
use super::sqlite::{
    decode, digest, id, json, AuthError, Connection, Mutation, SqliteAuthorizationStore,
};

fn enqueue_attachment_event<D>(
    connection: &Connection,
    attachment: &str,
    now: i64,
) -> Result<(), AuthError>
where
    D: trellis_rs::client::EventDescriptor<Event = AuthSecurityEvent>,
{
    let (principal, session): (String, String) = connection.query_row(
        "SELECT s.principal_id,a.authorization_session_id \
        FROM auth_attachments a \
        JOIN auth_authorization_sessions s ON \
        s.authorization_session_id=a.authorization_session_id \
        WHERE a.attachment_id=?1",
        [attachment],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let event = AuthSecurityEvent {
        event_id: id().parse().map_err(|_| AuthError::Unavailable)?,
        occurred_at: super::sqlite::timestamp(now)?,
        principal_id: Some(principal.parse().map_err(|_| AuthError::Unavailable)?),
        authorization_session_id: Some(session.parse().map_err(|_| AuthError::Unavailable)?),
        attachment_id: Some(attachment.parse().map_err(|_| AuthError::Unavailable)?),
        issuer_key_id: None,
        enforcement: None,
        extra: Default::default(),
    };
    enqueue_effect(
        connection,
        "event",
        &serde_json::json!({
            "subject": D::SUBJECT,
            "descriptor": D::descriptor_identity().map_err(|_| AuthError::Unavailable)?,
            "event": event,
        }),
        None,
        now,
    )?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum EnforcementScope {
    Principal(String),
    Role(String),
    Capability(String),
    Api(String),
    Client(String),
    OidcProvider(String),
    Login(String),
    Grant(String),
    Identity(String),
    Deployment(String),
    Issuer(String),
    Session(String),
    All,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Attachment {
    pub(crate) attachment_id: String,
    pub(crate) server_id: String,
    pub(crate) broker_client_id: u64,
    pub(crate) authorization_session_id: String,
    pub(crate) context_digest: String,
    pub(crate) ephemeral_nkey: String,
    pub(crate) broker_user_nkey: String,
    pub(crate) transport_policy_digest: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingEffect {
    pub(crate) action_id: String,
    pub(crate) kind: String,
    pub(crate) payload: Value,
    pub(crate) claim_token: String,
    pub(crate) attempts: u64,
}

pub(super) fn enqueue_scope(
    connection: &Connection,
    scope: &EnforcementScope,
    now: i64,
) -> Result<String, AuthError> {
    let work = id();
    let encoded = json(scope)?;
    // Work naming one credential or identity takes the targeted lane, so a
    // revocation never waits behind a scan of every session.
    let (kind, lane) = match scope {
        EnforcementScope::Principal(_) => ("principal", 0),
        EnforcementScope::Login(_) => ("login", 0),
        EnforcementScope::Grant(_) => ("grant", 0),
        EnforcementScope::Identity(_) => ("identity", 0),
        EnforcementScope::Deployment(_) => ("deployment", 0),
        EnforcementScope::Session(_) => ("session", 0),
        EnforcementScope::Role(_) => ("role", 1),
        EnforcementScope::Capability(_) => ("capability", 1),
        EnforcementScope::Api(_) => ("api", 1),
        EnforcementScope::Client(_) => ("client", 1),
        EnforcementScope::OidcProvider(_) => ("oidcProvider", 1),
        EnforcementScope::Issuer(_) => ("issuer", 1),
        EnforcementScope::All => ("all", 1),
    };
    connection.execute(
        "INSERT INTO auth_enforcement_work(work_id,scope_kind,scope_key,\
        scope_json,lane,state,next_attempt_at,created_at) \
        VALUES(?1,?2,?3,?4,?5,'queued',?6,?6)",
        params![work, kind, digest(scope)?, encoded, lane, now],
    )?;
    Ok(work)
}

/// How long a caller that revoked its own session waits for the signed
/// revocation and the kick that follows it, so it can receive and verify the
/// reply. Auth refuses the session from the cutoff; other providers learn it
/// when the revocation publishes.
const SELF_REVOCATION_GRACE_SECONDS: i64 = 5;

/// A caller whose own request retires its session gets the reply before its
/// revocation reaches it: verifying that reply reads material over the same
/// connection and rejects a reply to a session it already knows is revoked.
/// Only the caller's session waits, and only when the scope covers it.
pub(super) fn defer_caller_enforcement(
    connection: &Connection,
    session: &str,
    scope: &EnforcementScope,
    now: i64,
) -> Result<(), AuthError> {
    let (clause, key) = session_scope(scope);
    connection.execute(
        &format!(
            "UPDATE auth_authorization_sessions AS s \
            SET enforce_not_before=?2 \
            WHERE s.authorization_session_id=?3 AND {clause}"
        ),
        params![key, now + SELF_REVOCATION_GRACE_SECONDS, session],
    )?;
    Ok(())
}

pub(super) fn enqueue_effect(
    connection: &Connection,
    kind: &str,
    payload: &Value,
    predecessor: Option<&str>,
    now: i64,
) -> Result<String, AuthError> {
    // The predecessor is part of the identity: an identical payload queued
    // behind a different predecessor is a separate action, never a dropped edge.
    let action = digest(&(kind, payload, predecessor))?;
    connection.execute(
        "INSERT OR IGNORE INTO auth_post_commit_actions(action_id,kind,\
        payload_json,created_at,next_attempt_at,predecessor_action_id) \
        VALUES(?1,?2,?3,?4,?4,?5)",
        params![action, kind, json(payload)?, now, predecessor],
    )?;
    Ok(action)
}

pub(super) fn retire_session(
    store: &SqliteAuthorizationStore,
    connection: &Connection,
    session: &str,
    reason: &str,
    hard: bool,
    work: Option<&str>,
    now: i64,
) -> Result<bool, AuthError> {
    let row: Option<(String, i64, Option<i64>)> = connection
        .query_row(
            "SELECT state,latest_context_expiry,enforce_not_before \
            FROM auth_authorization_sessions \
            WHERE authorization_session_id=?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((state, latest, enforce_not_before)) = row else {
        return Err(AuthError::NotFound);
    };
    if state != "active" {
        return Ok(false);
    }
    store.current_signer(connection)?;
    let maximum = latest.saturating_add(i64::from(store.settings.clock_skew_seconds));
    // The cutoff is the authoritative commit time. Mirror retention includes
    // the accepted skew, but clock skew never postpones an explicit deny.
    let statement = SignedSessionRevocation {
        format: SESSION_REVOCATION_FORMAT_V1.into(),
        issuer_key_id: store.current_issuer(connection)?.key_id,
        trellis_instance_id: store.settings.instance_id.clone(),
        audience_nats_account: store.settings.account.clone(),
        authorization_session_id: session.into(),
        effective_cutoff: now,
        reason: reason.into(),
        issued_at: now,
        latest_context_expiry: maximum.max(now),
        extensions: Default::default(),
        critical: vec![],
        signature: String::new(),
    }
    .sign(&store.current_signer(connection)?)?;
    connection.execute(
        "UPDATE auth_authorization_sessions \
        SET state=?1,retired_at=?2,revision=revision+1 \
        WHERE authorization_session_id=?3 \
        AND state='active'",
        params![if hard { "revoked" } else { "superseded" }, now, session],
    )?;
    connection.execute(
        "INSERT INTO auth_authorization_revocations VALUES(?1,?2,?3,?4,?5,?6,?7,?2)",
        params![
            session,
            now,
            reason,
            if hard { "hard" } else { "reduction" },
            store.current_issuer(connection)?.key_id,
            json(&statement)?.as_bytes(),
            statement.latest_context_expiry
        ],
    )?;
    let publication = enqueue_effect(
        connection,
        "session_revoke",
        &serde_json::json!({
            "authorizationSessionId":session,
            "retentionDeadline":statement.latest_context_expiry
        }),
        None,
        now,
    )?;
    // Kicks follow the publication, so deferring it defers them too.
    connection.execute(
        "UPDATE auth_post_commit_actions \
        SET next_attempt_at=max(next_attempt_at,?2) \
        WHERE action_id=?1",
        params![publication, enforce_not_before.unwrap_or(now)],
    )?;
    let mut attachments = connection.prepare(
        "SELECT attachment_id,server_id,broker_client_id \
        FROM auth_attachments \
        WHERE authorization_session_id=?1 \
        AND state IN ('pending','admitted') \
        ORDER BY attachment_id",
    )?;
    for attachment in attachments.query_map([session], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u64>(2)?,
        ))
    })? {
        let (attachment, server, client) = attachment?;
        let action = enqueue_effect(
            connection,
            "kick",
            &serde_json::json!({
                "attachmentId":attachment,
                "serverId":server,
                "brokerClientId":client,
                "authorizationSessionId":session
            }),
            Some(&publication),
            now,
        )?;
        connection.execute(
            "UPDATE auth_post_commit_actions SET enforcement_work_id=?1 WHERE action_id=?2",
            params![work, action],
        )?;
        if let Some(work) = work {
            connection.execute(
                "UPDATE auth_enforcement_work SET pending_kicks=pending_kicks+1 WHERE work_id=?1",
                [work],
            )?;
        }
    }
    Ok(true)
}

fn session_scope(scope: &EnforcementScope) -> (&'static str, Option<&str>) {
    // Every cursor scans an indexed, stable session ID. Role and API scopes use
    // reverse relation indexes; they never load an entire affected population.
    match scope {
        EnforcementScope::Principal(key) => ("s.principal_id=?1", Some(key)),
        EnforcementScope::Login(key) => (
            "s.oauth_grant_id IN (SELECT oauth_grant_id \
            FROM auth_oauth_grants \
            WHERE login_session_id=?1)",
            Some(key),
        ),
        EnforcementScope::Grant(key) => ("s.oauth_grant_id=?1", Some(key)),
        EnforcementScope::Identity(key) => ("s.identity_key_id=?1", Some(key)),
        EnforcementScope::Client(key) => (
            "s.oauth_grant_id IN (SELECT oauth_grant_id \
            FROM auth_oauth_grants \
            WHERE client_id=?1)",
            Some(key),
        ),
        EnforcementScope::OidcProvider(key) => (
            "s.oauth_grant_id IN (SELECT g.oauth_grant_id \
            FROM auth_oauth_grants g \
            JOIN auth_login_sessions l USING(login_session_id) \
            WHERE l.provider_id=?1)",
            Some(key),
        ),
        EnforcementScope::Role(key) => (
            "(s.principal_id IN (SELECT principal_id \
            FROM auth_role_assignments \
            WHERE role_id=?1) OR s.oauth_grant_id IN (SELECT g.oauth_grant_id \
            FROM auth_oauth_grants g \
            JOIN auth_login_sessions l USING(login_session_id) \
            JOIN auth_oidc_role_mappings m ON m.provider_id=l.provider_id \
            JOIN auth_oidc_mapping_roles mr ON mr.mapping_id=m.mapping_id \
            WHERE mr.role_id=?1))",
            Some(key),
        ),
        EnforcementScope::Capability(key) => (
            "s.authorization_session_id IN (SELECT authorization_session_id \
            FROM auth_session_capabilities \
            WHERE capability_id=?1)",
            Some(key),
        ),
        EnforcementScope::Api(key) => (
            "s.authorization_session_id IN (SELECT authorization_session_id \
            FROM auth_session_apis \
            WHERE api_id=?1)",
            Some(key),
        ),
        EnforcementScope::Deployment(key) => (
            "s.identity_key_id IN (SELECT identity_key_id \
            FROM auth_provisioned_identities \
            WHERE deployment_id=?1)",
            Some(key),
        ),
        EnforcementScope::Issuer(key) => (
            "EXISTS(SELECT 1 FROM auth_session_authorities a \
            WHERE a.authorization_session_id=s.authorization_session_id \
            AND a.issuer_key_id=?1)",
            Some(key),
        ),
        EnforcementScope::Session(key) => ("s.authorization_session_id=?1", Some(key)),
        EnforcementScope::All => ("?1 IS NULL", None),
    }
}

impl SqliteAuthorizationStore {
    /// Verify the CONNECT credential and commit the exact admitted ACL with its
    /// logical authority before returning anything that can admit the socket.
    pub(crate) fn admit_connection(
        &self,
        credential: &[u8],
        ephemeral_nkey: &str,
        broker_user_nkey: &str,
        server_id: &str,
        broker_client_id: u64,
        now: i64,
    ) -> Result<(Attachment, trellis_protocol::TransportAuthorizationV1), AuthError> {
        if credential.is_empty() || credential.len() > 64 * 1024 || broker_client_id == 0 {
            return Err(AuthError::Denied);
        }
        let (kind, key) = nkeys::from_public_key(ephemeral_nkey).map_err(|_| AuthError::Denied)?;
        if nkeys::KeyPairType::from(kind) != nkeys::KeyPairType::User {
            return Err(AuthError::Denied);
        }
        use base64::Engine as _;
        let public = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
        let inbox = trellis_protocol::admission_inbox_prefix(&public)?;
        let selector: Value = serde_json::from_slice(credential)?;
        self.transaction(|connection| {
            let identity = match selector
                .pointer("/credential/format")
                .and_then(Value::as_str)
            {
                Some(trellis_protocol::OAUTH_ADMISSION_FORMAT_V1) => self
                    .oauth_admission_identity_in(
                        connection,
                        credential,
                        ephemeral_nkey,
                        &inbox,
                        now,
                    )?,
                Some(trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1) => self
                    .provisioned_admission_identity_in(
                        connection,
                        credential,
                        ephemeral_nkey,
                        &inbox,
                        now,
                    )?,
                _ => return Err(AuthError::Denied),
            };
            let (issued, plan) =
                match self.issue_admission_in_transaction(connection, &identity, None, now)? {
                    Ok(issued) => issued,
                    Err(error) => return Ok(Err(error)),
                };
            let policy = self.compile_transport(connection, &plan, &issued.authority.unsigned)?;
            let attachment = Attachment {
                attachment_id: id(),
                server_id: server_id.into(),
                broker_client_id,
                authorization_session_id: issued
                    .authority
                    .unsigned
                    .authorization_session_id
                    .clone(),
                context_digest: issued.digest,
                ephemeral_nkey: ephemeral_nkey.into(),
                broker_user_nkey: broker_user_nkey.into(),
                transport_policy_digest: policy.digest()?,
            };
            // Check the complete signed bootstrap response before admitting a
            // socket. An oversized resource/catalog projection rolls back both
            // authority issuance and attachment creation, not just ContextGet.
            let registry = self.attachment_registry(
                connection,
                &plan,
                &issued.authority.unsigned,
                &policy,
                &policy,
                &attachment.attachment_id,
            )?;
            super::own_context::ensure_first_reply_fits(
                &self.settings,
                &plan.issuer.key_id,
                &issued.authority.unsigned,
                &plan.authority_json,
                registry,
            )?;
            connection.execute(
                "INSERT INTO auth_attachments(attachment_id,server_id,broker_client_id,\
                authorization_session_id,context_digest,ephemeral_nkey,\
                broker_user_nkey,transport_policy_digest,transport_policy_json,state,\
                created_at,admitted_at) \
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'admitted',?10,?10)",
                params![
                    attachment.attachment_id,
                    attachment.server_id,
                    attachment.broker_client_id,
                    attachment.authorization_session_id,
                    attachment.context_digest,
                    attachment.ephemeral_nkey,
                    broker_user_nkey,
                    attachment.transport_policy_digest,
                    json(&policy)?,
                    now
                ],
            )?;
            enqueue_attachment_event::<events::ConnectionsOpened>(
                connection,
                &attachment.attachment_id,
                now,
            )?;
            Ok(Ok((attachment, policy)))
        })?
    }

    /// A disconnect closes only the exact server/CID/key association. A reused
    /// broker CID belonging to another ephemeral key is never affected.
    pub(crate) fn disconnect_attachment(
        &self,
        server: &str,
        cid: u64,
        nkey: &str,
        now: i64,
    ) -> Result<(), AuthError> {
        self.transaction(|connection| {
            let closed: Option<String> = connection
                .query_row(
                    "UPDATE auth_attachments \
                    SET state='closed',closed_at=?1 \
                    WHERE server_id=?2 AND broker_client_id=?3 \
                    AND (broker_user_nkey=?4 \
                    OR ?4='trellis.auth.v1:'||attachment_id) \
                    AND state IN ('pending','admitted') \
                    RETURNING attachment_id",
                    params![now, server, cid, nkey],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(attachment) = closed {
                enqueue_attachment_event::<events::ConnectionsClosed>(
                    connection,
                    &attachment,
                    now,
                )?;
            }
            Ok(())
        })
    }

    /// One bounded page commits retirements, effects and its cursor atomically.
    /// A crashed transaction replays its page; a committed page resumes by ID.
    pub(crate) fn enforce_next_page(&self, now: i64) -> Result<Option<String>, AuthError> {
        self.transaction(|connection| {
            let next: Option<(String, String, Option<String>)> = connection
                .query_row(
                    "SELECT work_id,scope_json,cursor \
                    FROM auth_enforcement_work \
                    WHERE state IN ('queued','running') \
                    AND next_attempt_at<=?1 \
                    ORDER BY lane,created_at,work_id \
                    LIMIT 1",
                    [now],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((work, encoded, cursor)) = next else {
                return Ok(None);
            };
            let scope: EnforcementScope = decode(&encoded)?;
            let (predicate, key) = session_scope(&scope);
            let sql = format!(
                "SELECT s.authorization_session_id \
                FROM auth_authorization_sessions s \
                WHERE s.state='active' \
                AND ({predicate}) AND s.authorization_session_id>?2 \
                ORDER BY s.authorization_session_id \
                LIMIT ?3"
            );
            let sessions = connection
                .prepare(&sql)?
                .query_map(
                    params![
                        key,
                        cursor.as_deref().unwrap_or(""),
                        self.settings.enforcement_page_size as u64
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let scope_hard = matches!(
                scope,
                EnforcementScope::Session(_) | EnforcementScope::Issuer(_)
            );
            let mut retired = 0;
            for session in &sessions {
                let current = evaluate(connection, &session_credential(connection, session)?, now);
                let hard =
                    scope_hard || matches!(&current, Err(AuthError::Denied | AuthError::NotFound));
                let should_retire = if hard {
                    true
                } else {
                    match current {
                        Ok(current) => {
                            !current.required_missing.is_empty()
                                || loses_authority(connection, session, &current, now)?
                        }
                        Err(
                            AuthError::Denied | AuthError::RequiredMissing | AuthError::NotFound,
                        ) => true,
                        Err(error) => return Err(error),
                    }
                };
                if should_retire
                    && retire_session(
                        self,
                        connection,
                        session,
                        if hard {
                            "hard_revocation"
                        } else {
                            "policy_reduction"
                        },
                        hard,
                        Some(&work),
                        now,
                    )?
                {
                    retired += 1;
                }
            }
            let completed = sessions.len() < self.settings.enforcement_page_size;
            connection.execute(
                "UPDATE auth_enforcement_work \
                SET state=?1,cursor=coalesce(?2,cursor),\
                scanned_sessions=scanned_sessions+?3,\
                retired_sessions=retired_sessions+?4,completed_at=CASE \
                WHEN ?1='completed' THEN ?5 \
                END WHERE work_id=?6",
                params![
                    if completed { "completed" } else { "running" },
                    sessions.last(),
                    sessions.len() as u64,
                    retired,
                    now,
                    work
                ],
            )?;
            Ok(Some(work))
        })
    }

    /// Claim leases prevent duplicate workers from acknowledging each other's
    /// work. A successor action is claimable only after its predecessor commits.
    pub(crate) fn claim_effects(
        &self,
        limit: usize,
        now: i64,
        lease_seconds: u32,
        events: bool,
    ) -> Result<Vec<PendingEffect>, AuthError> {
        if !(1..=256).contains(&limit) || lease_seconds == 0 {
            return Err(AuthError::Invalid("invalid effect claim bounds".into()));
        }
        self.transaction(|connection| {
            let mut statement = connection.prepare(
                "SELECT action_id,kind,payload_json,attempts \
                FROM auth_post_commit_actions a \
                WHERE next_attempt_at<=?1 \
                AND ((kind='event')=?3) \
                AND (claimed_until IS NULL \
                OR claimed_until<=?1) \
                AND (predecessor_action_id IS NULL \
                OR NOT EXISTS(SELECT 1 \
                FROM auth_post_commit_actions p \
                WHERE p.action_id=a.predecessor_action_id)) \
                ORDER BY created_at,action_id \
                LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![now, limit as u64, events], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut effects = Vec::new();
            for (action_id, kind, payload, attempts) in rows {
                let claim_token = id();
                connection.execute(
                    "UPDATE auth_post_commit_actions \
                    SET claimed_until=?1,claim_token=?2 \
                    WHERE action_id=?3",
                    params![
                        now.saturating_add(i64::from(lease_seconds)),
                        claim_token,
                        action_id
                    ],
                )?;
                effects.push(PendingEffect {
                    action_id,
                    kind,
                    payload: decode(&payload)?,
                    claim_token,
                    attempts,
                });
            }
            Ok(effects)
        })
    }

    /// False leaves expired or superseded work untouched for its next owner.
    pub(crate) fn complete_effect(
        &self,
        effect: &PendingEffect,
        now: i64,
    ) -> Result<bool, AuthError> {
        self.transaction(|connection| {
            let work: Option<Option<String>> = connection
                .query_row(
                    "SELECT enforcement_work_id \
                    FROM auth_post_commit_actions \
                    WHERE action_id=?1 AND claim_token=?2 \
                    AND claimed_until>?3",
                    params![effect.action_id, effect.claim_token, now],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(work) = work else {
                return Ok(false);
            };
            if effect.kind == "kick" {
                if let Some(attachment) = effect.payload["attachmentId"].as_str() {
                    if connection.execute(
                        "UPDATE auth_attachments \
                        SET state='closed',closed_at=?1 \
                        WHERE attachment_id=?2 \
                        AND state!='closed'",
                        params![now, attachment],
                    )? != 0
                    {
                        enqueue_attachment_event::<events::ConnectionsClosed>(
                            connection, attachment, now,
                        )?;
                    }
                    if connection.execute(
                        "UPDATE auth_attachments \
                            SET kicked_at=?1 WHERE attachment_id=?2 \
                            AND kicked_at IS NULL",
                        params![now, attachment],
                    )? != 0
                    {
                        enqueue_attachment_event::<events::ConnectionsKicked>(
                            connection, attachment, now,
                        )?;
                    }
                }
                if let Some(work) = work {
                    connection.execute(
                        "UPDATE auth_enforcement_work \
                        SET pending_kicks=max(0,pending_kicks-1) \
                        WHERE work_id=?1",
                        [work],
                    )?;
                }
            }
            connection.execute(
                "DELETE FROM auth_post_commit_actions \
                WHERE action_id=?1 AND claim_token=?2",
                params![effect.action_id, effect.claim_token],
            )?;
            Ok(true)
        })
    }

    /// False never reschedules an expired or superseded claim's work.
    pub(crate) fn retry_effect(&self, effect: &PendingEffect, now: i64) -> Result<bool, AuthError> {
        self.transaction(|connection| {
            // Persist a static diagnostic, not a provider error string or secret.
            // Equal jitter keeps workers that failed together from retrying together.
            let base = 1_i64
                .checked_shl(effect.attempts.min(8) as u32)
                .unwrap_or(256);
            let mut random = [0_u8; 8];
            getrandom::fill(&mut random).map_err(|_| AuthError::Unavailable)?;
            let delay = base - (u64::from_le_bytes(random) % (base as u64 / 2 + 1)) as i64;
            if connection.execute(
                "UPDATE auth_post_commit_actions \
                SET attempts=attempts+1,next_attempt_at=?1,claimed_until=NULL,\
                claim_token=NULL,last_error='delivery_unavailable' \
                WHERE action_id=?2 AND claim_token=?3 \
                AND claimed_until>?4",
                params![
                    now.saturating_add(delay),
                    effect.action_id,
                    effect.claim_token,
                    now
                ],
            )? != 1
            {
                return Ok(false);
            }
            // Count each failed kick once, not once per retry.
            if effect.attempts == 0 {
                connection.execute(
                    "UPDATE auth_enforcement_work \
                    SET failed_kicks=failed_kicks+1 \
                    WHERE work_id=(SELECT enforcement_work_id \
                    FROM auth_post_commit_actions \
                    WHERE action_id=?1 AND kind='kick')",
                    [&effect.action_id],
                )?;
            }
            Ok(true)
        })
    }

    pub(crate) fn resolve_revocation(
        &self,
        session: &str,
    ) -> Result<Option<SignedSessionRevocation>, AuthError> {
        self.transaction(|connection| {
            let bytes: Option<Vec<u8>> = connection
                .query_row(
                    "SELECT signed_bytes \
                    FROM auth_authorization_revocations \
                    WHERE authorization_session_id=?1",
                    [session],
                    |row| row.get(0),
                )
                .optional()?;
            bytes
                .map(|bytes| Ok(serde_json::from_slice(&bytes)?))
                .transpose()
        })
    }

    /// Hard roots and the scan intent commit before the first broker effect.
    /// Reuse root revocation inside a typed RPC result/replay transaction.
    pub(super) fn revoke_root_in_transaction(
        &self,
        connection: &Connection,
        identity: &Mutation,
        scope: &EnforcementScope,
        now: i64,
    ) -> Result<String, AuthError> {
        self.mutate_in_transaction(
            connection,
            identity,
            "root.revoke",
            scope,
            now,
            |connection| {
                if let Some(session) = &identity.session {
                    defer_caller_enforcement(connection, session, scope, now)?;
                }
                let owned = match scope {
                    EnforcementScope::Login(login) => connection.query_row(
                        "SELECT EXISTS(SELECT 1 \
                        FROM auth_login_sessions \
                        WHERE login_session_id=?1 \
                        AND principal_id=?2)",
                        params![login, identity.actor],
                        |row| row.get::<_, bool>(0),
                    )?,
                    EnforcementScope::Grant(grant) => connection.query_row(
                        "SELECT EXISTS(SELECT 1 \
                        FROM auth_oauth_grants \
                        WHERE oauth_grant_id=?1 \
                        AND principal_id=?2)",
                        params![grant, identity.actor],
                        |row| row.get::<_, bool>(0),
                    )?,
                    _ => false,
                };
                // Account managers end the sign-ins of accounts in their reach,
                // as a password reset does.
                let reachable = match scope {
                    EnforcementScope::Login(login) if !owned => {
                        let user: Option<String> = connection
                            .query_row(
                                "SELECT l.principal_id \
                                FROM auth_login_sessions l \
                                JOIN auth_principals p USING(principal_id) \
                                WHERE l.login_session_id=?1 AND p.kind='user'",
                                [login],
                                |row| row.get(0),
                            )
                            .optional()?;
                        match user.map(|user| {
                            Self::require_account_reach(connection, &identity.actor, &user)
                        }) {
                            Some(Ok(())) => true,
                            None | Some(Err(AuthError::Denied)) => false,
                            Some(Err(error)) => return Err(error),
                        }
                    }
                    _ => false,
                };
                if !owned && !reachable {
                    Self::require_administration(
                        connection,
                        &identity.actor,
                        if matches!(scope, EnforcementScope::Issuer(_)) {
                            Administration::ManagePlatform
                        } else {
                            Administration::ManagePrincipals
                        },
                    )?;
                }
                let changed = match scope {
                    EnforcementScope::Grant(grant) => {
                        connection.execute(
                            "UPDATE auth_oauth_families \
                            SET revoked_at=coalesce(revoked_at,?1),revision=revision+1 \
                            WHERE oauth_grant_id=?2 \
                            AND revoked_at IS NULL",
                            params![now, grant],
                        )?;
                        connection.execute(
                            "UPDATE auth_oauth_grants \
                            SET revoked_at=?1,revision=revision+1 \
                            WHERE oauth_grant_id=?2 \
                            AND revoked_at IS NULL",
                            params![now, grant],
                        )?
                    }
                    EnforcementScope::Login(login) => {
                        connection.execute(
                            "UPDATE auth_oauth_families \
                            SET revoked_at=coalesce(revoked_at,?1),revision=revision+1 \
                            WHERE oauth_grant_id IN (SELECT oauth_grant_id \
                            FROM auth_oauth_grants \
                            WHERE login_session_id=?2) \
                            AND revoked_at IS NULL",
                            params![now, login],
                        )?;
                        connection.execute(
                            "UPDATE auth_oauth_grants \
                            SET revoked_at=?1,revision=revision+1 \
                            WHERE login_session_id=?2 \
                            AND revoked_at IS NULL",
                            params![now, login],
                        )?;
                        connection.execute(
                            "UPDATE auth_login_sessions \
                            SET revoked_at=?1,revision=revision+1 \
                            WHERE login_session_id=?2 \
                            AND revoked_at IS NULL",
                            params![now, login],
                        )?
                    }
                    EnforcementScope::Identity(identity) => connection.execute(
                        "UPDATE auth_provisioned_identities \
                        SET state='revoked',revoked_at=?1 \
                        WHERE identity_key_id=?2 \
                        AND state='active'",
                        params![now, identity],
                    )?,
                    EnforcementScope::Session(session) => {
                        if retire_session(
                            self,
                            connection,
                            session,
                            "session_revoked",
                            true,
                            None,
                            now,
                        )? {
                            1
                        } else {
                            0
                        }
                    }
                    EnforcementScope::Issuer(issuer) => {
                        let current: bool = connection.query_row(
                            "SELECT EXISTS(SELECT 1 \
                            FROM auth_authorization_issuers \
                            WHERE key_id=?1 AND is_current=1)",
                            [issuer],
                            |row| row.get(0),
                        )?;
                        if current {
                            return Err(AuthError::Conflict);
                        }
                        connection.execute(
                            "UPDATE auth_authorization_issuers \
                            SET state='compromised',revoked_at=?1 \
                            WHERE key_id=?2 AND revoked_at IS NULL",
                            params![now, issuer],
                        )?
                    }
                    _ => return Err(AuthError::Invalid("unsupported hard root".into())),
                };
                if changed == 0 {
                    return Err(AuthError::Conflict);
                }
                enqueue_scope(connection, scope, now)
            },
        )
    }

    /// Prune expired API reviews, retaining immutable authorization history.
    /// OAuth artifacts belong to OAuthStorage's family-aware cleanup: generic
    /// expiry deletion could erase refresh-reuse evidence for a live family.
    pub(crate) fn prune_expired_artifacts(&self, now: i64) -> Result<usize, AuthError> {
        self.transaction(|connection| {
            Ok(connection.execute("DELETE FROM auth_api_reviews WHERE expires_at<=?1", [now])?)
        })
    }
}
