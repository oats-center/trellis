use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trellis_protocol::{SignedSessionRevocation, SESSION_REVOCATION_FORMAT_V1};

use super::authorization_sessions::{loses_authority, session_credential};
use super::policy::evaluate;
use super::sqlite::{decode, digest, id, json, AuthError, Mutation, SqliteAuthorizationStore};

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
    pub(crate) ephemeral_nkey: String,
    pub(crate) transport_policy_digest: String,
}

/// One owner's verified revocations survive mirror reconnects and absent entries.
/// This stores positive revocations only: a miss never establishes live trust.
#[derive(Debug)]
pub(crate) struct RevocationCache {
    instance: String,
    account: String,
    capacity: usize,
    records: std::collections::BTreeMap<String, trellis_protocol::SignedSessionRevocation>,
}

impl RevocationCache {
    pub(crate) fn new(
        instance: String,
        account: String,
        capacity: usize,
    ) -> Result<Self, AuthError> {
        if capacity == 0 {
            return Err(AuthError::Invalid(
                "revocation cache capacity must be positive".into(),
            ));
        }
        Ok(Self {
            instance,
            account,
            capacity,
            records: std::collections::BTreeMap::new(),
        })
    }

    /// Verify before touching known state; a forged mirror update cannot revoke
    /// anybody or erase a previously verified cutoff. No live entry is evicted.
    pub(crate) fn observe(
        &mut self,
        record: trellis_protocol::SignedSessionRevocation,
        issuer: &trellis_protocol::AuthorityIssuerKey,
        now: i64,
    ) -> Result<(), AuthError> {
        record.verify(issuer, &self.instance, &self.account)?;
        self.records
            .retain(|_, known| known.latest_context_expiry > now);
        if record.latest_context_expiry <= now {
            return Ok(());
        }
        if let Some(known) = self.records.get_mut(&record.authorization_session_id) {
            if known.effective_cutoff != record.effective_cutoff {
                return Err(AuthError::Conflict);
            }
            if record.latest_context_expiry > known.latest_context_expiry {
                *known = record;
            }
        } else {
            if self.records.len() >= self.capacity {
                return Err(AuthError::Unavailable);
            }
            self.records
                .insert(record.authorization_session_id.clone(), record);
        }
        Ok(())
    }

    pub(crate) fn cutoff(&self, session: &str, now: i64) -> Option<i64> {
        self.records
            .get(session)
            .filter(|record| record.latest_context_expiry > now)
            .map(|record| record.effective_cutoff)
    }
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
    let kind = match scope {
        EnforcementScope::Principal(_) => "principal",
        EnforcementScope::Role(_) => "role",
        EnforcementScope::Capability(_) => "capability",
        EnforcementScope::Api(_) => "api",
        EnforcementScope::Client(_) => "client",
        EnforcementScope::OidcProvider(_) => "oidcProvider",
        EnforcementScope::Login(_) => "login",
        EnforcementScope::Grant(_) => "grant",
        EnforcementScope::Identity(_) => "identity",
        EnforcementScope::Deployment(_) => "deployment",
        EnforcementScope::Issuer(_) => "issuer",
        EnforcementScope::Session(_) => "session",
        EnforcementScope::All => "all",
    };
    connection.execute("INSERT INTO auth_enforcement_work(work_id,scope_kind,scope_key,scope_json,state,next_attempt_at,created_at) VALUES(?1,?2,?3,?4,'queued',?5,?5)",params![work,kind,digest(scope)?,encoded,now])?;
    Ok(work)
}

pub(super) fn enqueue_effect(
    connection: &Connection,
    kind: &str,
    payload: &Value,
    predecessor: Option<&str>,
    now: i64,
) -> Result<String, AuthError> {
    let action = digest(&(kind, payload))?;
    connection.execute("INSERT OR IGNORE INTO auth_post_commit_actions(action_id,kind,payload_json,created_at,next_attempt_at,predecessor_action_id) VALUES(?1,?2,?3,?4,?4,?5)",params![action,kind,json(payload)?,now,predecessor])?;
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
    let row:Option<(String,i64)>=connection.query_row("SELECT state,latest_context_expiry FROM auth_authorization_sessions WHERE authorization_session_id=?1",[session],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some((state, latest)) = row else {
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
        issuer_key_id: store.issuer().key_id,
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
    .sign(&store.signer)?;
    connection.execute("UPDATE auth_authorization_sessions SET state=?1,retired_at=?2,revision=revision+1 WHERE authorization_session_id=?3 AND state='active'",params![if hard {"revoked"} else {"superseded"},now,session])?;
    connection.execute(
        "INSERT INTO auth_authorization_revocations VALUES(?1,?2,?3,?4,?5,?6,?7,?2)",
        params![
            session,
            now,
            reason,
            if hard { "hard" } else { "reduction" },
            store.issuer().key_id,
            json(&statement)?.as_bytes(),
            statement.latest_context_expiry
        ],
    )?;
    let publication = enqueue_effect(
        connection,
        "session_revoke",
        &serde_json::json!({"authorizationSessionId":session,"retentionDeadline":statement.latest_context_expiry}),
        None,
        now,
    )?;
    let mut attachments=connection.prepare("SELECT attachment_id,server_id,broker_client_id FROM auth_attachments WHERE authorization_session_id=?1 AND state IN ('pending','admitted') ORDER BY attachment_id")?;
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
            &serde_json::json!({"attachmentId":attachment,"serverId":server,"brokerClientId":client,"authorizationSessionId":session}),
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
        EnforcementScope::Principal(key)=>("s.principal_id=?1",Some(key)),
        EnforcementScope::Login(key)=>("s.oauth_grant_id IN (SELECT oauth_grant_id FROM auth_oauth_grants WHERE login_session_id=?1)",Some(key)),
        EnforcementScope::Grant(key)=>("s.oauth_grant_id=?1",Some(key)),
        EnforcementScope::Identity(key)=>("s.identity_key_id=?1",Some(key)),
        EnforcementScope::Client(key)=>("s.oauth_grant_id IN (SELECT oauth_grant_id FROM auth_oauth_grants WHERE client_id=?1)",Some(key)),
        EnforcementScope::OidcProvider(key)=>("s.oauth_grant_id IN (SELECT g.oauth_grant_id FROM auth_oauth_grants g JOIN auth_login_sessions l USING(login_session_id) WHERE l.provider_id=?1)",Some(key)),
        EnforcementScope::Role(key)=>("(s.principal_id IN (SELECT principal_id FROM auth_role_assignments WHERE role_id=?1) OR s.oauth_grant_id IN (SELECT g.oauth_grant_id FROM auth_oauth_grants g JOIN auth_login_sessions l USING(login_session_id) JOIN auth_oidc_role_mappings m ON m.provider_id=l.provider_id WHERE m.role_id=?1))",Some(key)),
        EnforcementScope::Capability(key)=>("(EXISTS(SELECT 1 FROM json_each(s.approved_capabilities_json) WHERE json_extract(value,'$.capabilityId')=?1))",Some(key)),
        EnforcementScope::Api(key)=>("(EXISTS(SELECT 1 FROM auth_session_authorities a,json_each(a.api_snapshots_json) j WHERE a.authorization_session_id=s.authorization_session_id AND json_extract(j.value,'$.apiId')=?1))",Some(key)),
        EnforcementScope::Deployment(key)=>("s.identity_key_id IN (SELECT identity_key_id FROM auth_provisioned_identities WHERE deployment_id=?1)",Some(key)),
        EnforcementScope::Issuer(key)=>("EXISTS(SELECT 1 FROM auth_session_authorities a WHERE a.authorization_session_id=s.authorization_session_id AND a.issuer_key_id=?1)",Some(key)),
        EnforcementScope::Session(key)=>("s.authorization_session_id=?1",Some(key)),
        EnforcementScope::All=>("?1 IS NULL",None),
    }
}

impl SqliteAuthorizationStore {
    /// Reauthorize in the attachment transaction so a delayed worker or offline
    /// broker cannot admit a retired logical ID after a policy commit.
    pub(crate) fn admit_attachment(
        &self,
        attachment: &Attachment,
        now: i64,
    ) -> Result<(), AuthError> {
        let outcome=self.transaction(|connection| {
            let active:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_authorization_sessions WHERE authorization_session_id=?1 AND state='active' AND hard_deadline>?2)",params![attachment.authorization_session_id,now],|row|row.get(0))?;
            if !active {return Err(AuthError::Retired);}
            let session_key:String=connection.query_row("SELECT session_public_key FROM auth_authorization_sessions WHERE authorization_session_id=?1",[&attachment.authorization_session_id],|row|row.get(0))?;
            let (prefix,key)=nkeys::from_public_key(&attachment.ephemeral_nkey).map_err(|_|AuthError::Denied)?;
            use base64::Engine as _;
            if nkeys::KeyPairType::from(prefix)!=nkeys::KeyPairType::User || base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)!=session_key {return Err(AuthError::Denied);}
            let current=evaluate(connection,&session_credential(connection,&attachment.authorization_session_id)?,now);
            let reduced=match &current {Ok(current)=>!current.required_missing.is_empty() || loses_authority(connection,&attachment.authorization_session_id,current,now)?,Err(AuthError::Denied|AuthError::RequiredMissing|AuthError::NotFound)=>true,Err(_)=>false};
            if reduced {retire_session(self,connection,&attachment.authorization_session_id,"admission_reduction",false,None,now)?;return Ok(Err(AuthError::Retired));}
            current?;
            connection.execute("INSERT INTO auth_attachments(attachment_id,server_id,broker_client_id,authorization_session_id,ephemeral_nkey,transport_policy_digest,state,created_at,admitted_at) VALUES(?1,?2,?3,?4,?5,?6,'admitted',?7,?7)",params![attachment.attachment_id,attachment.server_id,attachment.broker_client_id,attachment.authorization_session_id,attachment.ephemeral_nkey,attachment.transport_policy_digest,now])?;
            Ok(Ok(()))
        })?;
        outcome
    }

    /// One bounded page commits retirements, effects and its cursor atomically.
    /// A crashed transaction replays its page; a committed page resumes by ID.
    pub(crate) fn enforce_next_page(&self, now: i64) -> Result<Option<String>, AuthError> {
        self.transaction(|connection| {
            let next:Option<(String,String,Option<String>)>=connection.query_row("SELECT work_id,scope_json,cursor FROM auth_enforcement_work WHERE state IN ('queued','running','failed') AND next_attempt_at<=?1 ORDER BY created_at,work_id LIMIT 1",[now],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            let Some((work,encoded,cursor))=next else {return Ok(None);};
            let scope:EnforcementScope=decode(&encoded)?;
            let (predicate,key)=session_scope(&scope);
            let sql=format!("SELECT s.authorization_session_id FROM auth_authorization_sessions s WHERE s.state='active' AND ({predicate}) AND s.authorization_session_id>?2 ORDER BY s.authorization_session_id LIMIT ?3");
            let sessions=connection.prepare(&sql)?.query_map(params![key,cursor.as_deref().unwrap_or(""),self.settings.enforcement_page_size as u64],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let hard=matches!(scope,EnforcementScope::Session(_)|EnforcementScope::Issuer(_));
            let mut retired=0;
            for session in &sessions {
                let current=evaluate(connection,&session_credential(connection,session)?,now);
                let should_retire=if hard {true} else {match current {
                    Ok(current)=>!current.required_missing.is_empty() || loses_authority(connection,session,&current,now)?,
                    Err(AuthError::Denied|AuthError::RequiredMissing|AuthError::NotFound)=>true,
                    Err(error)=>return Err(error),
                }};
                if should_retire && retire_session(self,connection,session,if hard {"hard_revocation"} else {"policy_reduction"},hard,Some(&work),now)? {retired+=1;}
            }
            let completed=sessions.len()<self.settings.enforcement_page_size;
            connection.execute("UPDATE auth_enforcement_work SET state=?1,cursor=coalesce(?2,cursor),scanned_sessions=scanned_sessions+?3,retired_sessions=retired_sessions+?4,completed_at=CASE WHEN ?1='completed' THEN ?5 END WHERE work_id=?6",params![if completed {"completed"} else {"running"},sessions.last(),sessions.len() as u64,retired,now,work])?;
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
    ) -> Result<Vec<PendingEffect>, AuthError> {
        if !(1..=256).contains(&limit) || lease_seconds == 0 {
            return Err(AuthError::Invalid("invalid effect claim bounds".into()));
        }
        self.transaction(|connection| {
            let mut statement=connection.prepare("SELECT action_id,kind,payload_json,attempts FROM auth_post_commit_actions a WHERE next_attempt_at<=?1 AND (claimed_until IS NULL OR claimed_until<=?1) AND (predecessor_action_id IS NULL OR NOT EXISTS(SELECT 1 FROM auth_post_commit_actions p WHERE p.action_id=a.predecessor_action_id)) ORDER BY created_at,action_id LIMIT ?2")?;
            let rows=statement.query_map(params![now,limit as u64],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,u64>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut effects=Vec::new();
            for (action_id,kind,payload,attempts) in rows {
                let claim_token=id();
                connection.execute("UPDATE auth_post_commit_actions SET claimed_until=?1,claim_token=?2 WHERE action_id=?3",params![now.saturating_add(i64::from(lease_seconds)),claim_token,action_id])?;
                effects.push(PendingEffect{action_id,kind,payload:decode(&payload)?,claim_token,attempts});
            }
            Ok(effects)
        })
    }

    pub(crate) fn complete_effect(
        &self,
        effect: &PendingEffect,
        now: i64,
    ) -> Result<(), AuthError> {
        self.transaction(|connection| {
            let work:Option<Option<String>>=connection.query_row("SELECT enforcement_work_id FROM auth_post_commit_actions WHERE action_id=?1 AND claim_token=?2 AND claimed_until>?3",params![effect.action_id,effect.claim_token,now],|row|row.get(0)).optional()?;
            let Some(work)=work else {return Err(AuthError::Conflict);};
            if effect.kind=="kick" {
                if let Some(attachment)=effect.payload["attachmentId"].as_str() {connection.execute("UPDATE auth_attachments SET state='closed',closed_at=?1 WHERE attachment_id=?2",params![now,attachment])?;}
                if let Some(work)=work {connection.execute("UPDATE auth_enforcement_work SET pending_kicks=max(0,pending_kicks-1) WHERE work_id=?1",[work])?;}
            }
            connection.execute("DELETE FROM auth_post_commit_actions WHERE action_id=?1 AND claim_token=?2",params![effect.action_id,effect.claim_token])?;
            Ok(())
        })
    }

    pub(crate) fn retry_effect(&self, effect: &PendingEffect, now: i64) -> Result<(), AuthError> {
        self.transaction(|connection| {
            // Persist a static diagnostic, not a provider error string or secret.
            let delay=1_i64.checked_shl(effect.attempts.min(8) as u32).unwrap_or(256);
            if connection.execute("UPDATE auth_post_commit_actions SET attempts=attempts+1,next_attempt_at=?1,claimed_until=NULL,claim_token=NULL,last_error='delivery_unavailable' WHERE action_id=?2 AND claim_token=?3 AND claimed_until>?4",params![now.saturating_add(delay),effect.action_id,effect.claim_token,now])?!=1 {return Err(AuthError::Conflict);}
            connection.execute("UPDATE auth_enforcement_work SET failed_kicks=failed_kicks+1 WHERE work_id=(SELECT enforcement_work_id FROM auth_post_commit_actions WHERE action_id=?1 AND kind='kick')",[&effect.action_id])?;
            Ok(())
        })
    }

    pub(crate) fn resolve_revocation(
        &self,
        session: &str,
    ) -> Result<Option<SignedSessionRevocation>, AuthError> {
        self.transaction(|connection| {
            let bytes:Option<Vec<u8>>=connection.query_row("SELECT signed_bytes FROM auth_authorization_revocations WHERE authorization_session_id=?1",[session],|row|row.get(0)).optional()?;
            bytes.map(|bytes|Ok(serde_json::from_slice(&bytes)?)).transpose()
        })
    }

    /// Hard roots and the scan intent commit before the first broker effect.
    pub(crate) fn revoke_root(
        &self,
        identity: &Mutation,
        scope: &EnforcementScope,
        now: i64,
    ) -> Result<String, AuthError> {
        self.mutate(identity,"root.revoke",scope,now,|connection| {
            Self::require_privilege(connection,&identity.actor,if matches!(scope, EnforcementScope::Issuer(_)) {"privileges.manage"} else {"principals.manage"})?;
            let changed=match scope {
                EnforcementScope::Grant(grant)=> {
                    connection.execute("UPDATE auth_oauth_families SET revoked_at=coalesce(revoked_at,?1),revision=revision+1 WHERE oauth_grant_id=?2 AND revoked_at IS NULL",params![now,grant])?;
                    connection.execute("UPDATE auth_oauth_grants SET revoked_at=?1,revision=revision+1 WHERE oauth_grant_id=?2 AND revoked_at IS NULL",params![now,grant])?
                },
                EnforcementScope::Login(login)=> {
                    connection.execute("UPDATE auth_oauth_families SET revoked_at=coalesce(revoked_at,?1),revision=revision+1 WHERE oauth_grant_id IN (SELECT oauth_grant_id FROM auth_oauth_grants WHERE login_session_id=?2) AND revoked_at IS NULL",params![now,login])?;
                    connection.execute("UPDATE auth_oauth_grants SET revoked_at=?1,revision=revision+1 WHERE login_session_id=?2 AND revoked_at IS NULL",params![now,login])?;
                    connection.execute("UPDATE auth_login_sessions SET revoked_at=?1,revision=revision+1 WHERE login_session_id=?2 AND revoked_at IS NULL",params![now,login])?
                },
                EnforcementScope::Identity(identity)=>connection.execute("UPDATE auth_provisioned_identities SET state='revoked',revoked_at=?1 WHERE identity_key_id=?2 AND state='active'",params![now,identity])?,
                EnforcementScope::Session(session)=> {if retire_session(self,connection,session,"session_revoked",true,None,now)? {1} else {0}},
                EnforcementScope::Issuer(issuer)=> {
                    let current:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_authorization_issuers WHERE key_id=?1 AND is_current=1)",[issuer],|row|row.get(0))?;
                    if current {return Err(AuthError::Conflict);}
                    connection.execute("UPDATE auth_authorization_issuers SET state='compromised',revoked_at=?1 WHERE key_id=?2 AND revoked_at IS NULL",params![now,issuer])?
                },
                _=>return Err(AuthError::Invalid("unsupported hard root".into())),
            };
            if changed==0 {return Err(AuthError::Conflict);}
            enqueue_scope(connection,scope,now)
        })
    }

    /// Only expired transient artifacts are pruned. Authority, certificate,
    /// cutoff, key and audit history remains available for historical events.
    pub(crate) fn prune_expired_artifacts(&self, now: i64) -> Result<usize, AuthError> {
        self.transaction(|connection| {
            connection.execute("DELETE FROM auth_api_reviews WHERE expires_at<=?1", [now])?;
            Ok(connection.execute(
                "DELETE FROM auth_oauth_artifacts WHERE expires_at<=?1",
                [now],
            )?)
        })
    }
}
