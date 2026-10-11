//! Authenticated first-context retrieval for an already admitted runtime.

use futures_util::StreamExt as _;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use trellis_protocol::{
    parse_session_authority, sign_admission_context, verify_admission_get, AdmissionContext,
    AuthorityIssuerKey, AuthorityIssuerState, SignedAdmissionContext, SignedAdmissionGet,
    ADMISSION_GET_DOMAIN_V1,
};

use super::{
    authorization_sessions::{loses_authority, session_credential},
    policy::evaluate,
    revocation::retire_session,
    AuthError, SqliteAuthorizationStore,
};

/// Operational-account responder; ordinary clients receive only their exact
/// ephemeral-key-derived publish subject from Callout's complete ACL.
pub(crate) struct OwnContextService {
    subscriber: async_nats::Subscriber,
    client: async_nats::Client,
    store: Arc<SqliteAuthorizationStore>,
}

impl OwnContextService {
    pub(crate) async fn start(
        client: async_nats::Client,
        store: Arc<SqliteAuthorizationStore>,
    ) -> Result<Self, AuthError> {
        let subscriber = client
            .queue_subscribe(
                "trellis.auth.admission.*.get",
                "trellis.auth.admission".into(),
            )
            .await
            .map_err(|_| AuthError::Unavailable)?;
        client.flush().await.map_err(|_| AuthError::Unavailable)?;
        Ok(Self {
            subscriber,
            client,
            store,
        })
    }

    pub(crate) async fn run(
        mut self,
        shutdown: impl Future<Output = ()> + Send,
    ) -> Result<(), AuthError> {
        tokio::pin!(shutdown);
        let mut requests = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _=&mut shutdown=>break,
                completed=requests.join_next(), if !requests.is_empty()=> {
                    match completed {
                        Some(Err(_))=>return Err(AuthError::Unavailable),
                        Some(Ok(Err(_)))=>tracing::warn!("own-context response publication unavailable"),
                        _=>{}
                    }
                }
                message=self.subscriber.next()=> {
                    let Some(message)=message else {return Err(AuthError::Unavailable);};
                    let Some(reply)=message.reply else {continue;};
                    // Never use Auth's operational publish rights as a deputy for
                    // an arbitrary reply destination, including error/overload paths.
                    if message.payload.len()>4096 {continue;}
                    let Ok(request)=serde_json::from_slice::<SignedAdmissionGet>(&message.payload) else {continue;};
                    let Ok(inbox)=trellis_protocol::admission_inbox_prefix(&request.request.session_public_key) else {continue;};
                    if trellis_protocol::admission_get_subject(&request.request.session_public_key).ok().as_deref()!=Some(message.subject.as_str())
                        || !reply.starts_with(&format!("{inbox}.")) || reply.len()>512
                        || reply.split('.').any(|part|part.is_empty() || part.contains(['*','>']) || part.chars().any(char::is_whitespace)) {continue;}
                    if requests.len()>=32 {
                        self.client.publish(reply,br#"{"error":"admission_timeout"}"#.as_slice().into()).await.map_err(|_|AuthError::Unavailable)?;
                        continue;
                    }
                    let store=self.store.clone();
                    let client=self.client.clone();
                    requests.spawn(async move {
                        let result=SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|time|i64::try_from(time.as_secs()).ok())
                            .ok_or(AuthError::Unavailable)
                            .and_then(|now|store.own_context(&message.payload,message.subject.as_str(),reply.as_str(),now))
                            .and_then(|response|serde_json::to_vec(&response).map_err(AuthError::from));
                        let bytes=result.unwrap_or_else(|_|br#"{"error":"authentication_required"}"#.to_vec());
                        client.publish(reply,bytes.into()).await.map_err(|_|AuthError::Unavailable)
                    });
                }
            }
        }
        self.subscriber
            .unsubscribe()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let drained = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(result) = requests.join_next().await {
                result.map_err(|_| AuthError::Unavailable)??;
            }
            Ok::<(), AuthError>(())
        })
        .await;
        match drained {
            Ok(result) => result,
            Err(_) => {
                requests.abort_all();
                Err(AuthError::Unavailable)
            }
        }
    }
}

impl SqliteAuthorizationStore {
    /// Verify possession and actual NATS routing before reading any context.
    /// Recheck live policy inside the same snapshot used to sign the reply.
    pub(crate) fn own_context(
        &self,
        bytes: &[u8],
        subject: &str,
        reply: &str,
        now: i64,
    ) -> Result<SignedAdmissionContext, AuthError> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(AuthError::Denied);
        }
        let request: SignedAdmissionGet = serde_json::from_slice(bytes)?;
        verify_admission_get(&request, subject, reply, &self.settings.instance_id, now)?;
        self.transaction(|connection| {
            self.current_signer(connection)?;
            let count:i64=connection.query_row(
                "SELECT count(DISTINCT s.authorization_session_id) FROM auth_authorization_sessions s JOIN auth_attachments a USING(authorization_session_id) WHERE s.runtime_id=?1 AND s.session_public_key=?2 AND s.state='active' AND s.hard_deadline>?3 AND a.state IN ('pending','admitted')",
                params![request.request.runtime_id,request.request.session_public_key,now],|row|row.get(0))?;
            // A key reused across different bindings is ambiguous, not permission
            // to choose whichever principal has the most authority.
            if count!=1 {return Err(AuthError::Denied);}
            let record:Option<(String,Vec<u8>)>=connection.query_row(
                "SELECT s.authorization_session_id,c.signed_bytes FROM auth_authorization_sessions s JOIN auth_attachments a USING(authorization_session_id) JOIN auth_session_authorities c ON c.context_digest=a.context_digest AND c.authorization_session_id=s.authorization_session_id WHERE s.runtime_id=?1 AND s.session_public_key=?2 AND s.state='active' AND s.hard_deadline>?3 AND a.state IN ('pending','admitted') AND c.not_before<=?3 AND c.expires_at>?3 ORDER BY c.rowid DESC LIMIT 1",
                params![request.request.runtime_id,request.request.session_public_key,now],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            let (session,bytes)=record.ok_or(AuthError::Denied)?;
            let authority=parse_session_authority(&bytes,64*1024)?;
            if !reply.starts_with(&format!("{}.",authority.unsigned.inbox_prefix)) {return Err(AuthError::Denied);}
            let current=evaluate(connection,&session_credential(connection,&session)?,now);
            let reduced=match &current {
                Ok(current)=>!current.required_missing.is_empty() || loses_authority(connection,&session,current,now)?,
                Err(AuthError::Denied|AuthError::RequiredMissing|AuthError::NotFound)=>true,
                Err(_)=>false,
            };
            if reduced {
                retire_session(self,connection,&session,"own_context_reduction",false,None,now)?;
                return Ok(Err(AuthError::Retired));
            }
            current?;
            let mut catalogs=Vec::new();
            for api in &authority.unsigned.apis {
                let bytes:Vec<u8>=connection.query_row("SELECT signed_bytes FROM auth_api_verification_snapshots WHERE snapshot_digest=?1",[&api.catalog_snapshot_digest],|row|row.get(0))?;
                catalogs.push(serde_json::from_slice::<Value>(&bytes)?);
            }
            let mut providers=Vec::new();
            for provider in &authority.unsigned.provider_bindings {
                let bytes:Vec<u8>=connection.query_row("SELECT signed_bytes FROM auth_provider_certificates WHERE certificate_digest=?1",[&provider.provider_certificate_digest],|row|row.get(0))?;
                providers.push(serde_json::from_slice::<Value>(&bytes)?);
            }
            let mut issuers=Vec::new();
            let mut statement=connection.prepare("SELECT key_id,public_key,state FROM auth_authorization_issuers ORDER BY key_id")?;
            for row in statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))? {
                let (key_id,public_key,state)=row?;
                if issuers.len()>=256 {return Err(AuthError::Unavailable);}
                issuers.push(AuthorityIssuerKey {key_id,public_key,state:match state.as_str(){"active"=>AuthorityIssuerState::Active,"retired"=>AuthorityIssuerState::Retired,_=>AuthorityIssuerState::Revoked}});
            }
            let mut rotations=Vec::new();
            let mut statement=connection.prepare("SELECT signed_bytes FROM auth_issuer_rotations ORDER BY sequence")?;
            for row in statement.query_map([],|row|row.get::<_,Vec<u8>>(0))? {
                if rotations.len()>=256 {return Err(AuthError::Unavailable);}
                rotations.push(serde_json::from_slice::<Value>(&row?)?);
            }
            let response=sign_admission_context(AdmissionContext {
                format:ADMISSION_GET_DOMAIN_V1.into(),issuer_key_id:self.issuer().key_id,
                trellis_instance_id:self.settings.instance_id.clone(),audience_nats_account:self.settings.account.clone(),
                runtime_id:request.request.runtime_id,request_nonce:request.request.request_nonce,
                request_subject:subject.into(),reply_subject:reply.into(),
                session_authority:String::from_utf8(bytes).map_err(|_|AuthError::Denied)?,
                registry:json!({"catalogs":catalogs,"providers":providers,"issuers":issuers,"rotations":rotations}),
                issued_at:now,expires_at:authority.unsigned.expires_at.min(now.saturating_add(30)),
            },&self.signer)?;
            Ok(Ok(response))
        })?
    }
}
