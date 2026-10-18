//! Session-key-proven NATS own-authority retrieval, with checked private promotion.

use std::{sync::Arc, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::StreamExt as _;
use trellis_protocol::{AdmissionGet, SignedAdmissionContext};

use super::own_context::{AuthorizationContextCache, PreparedOwnCandidate};
use super::transport::read_own_admission;
use crate::client::{SessionAuth, TransportGenerationManager, TrellisClientError};

/// Collaborators retained by the one logical connection's renewal worker.
pub(crate) struct AuthorizationRefreshRuntime {
    pub(crate) contexts: Arc<AuthorizationContextCache>,
    pub(crate) auth: Arc<SessionAuth>,
    pub(crate) generations: TransportGenerationManager,
}

/// Verified signed reply metadata, kept private until authority promotion.
pub(crate) struct OwnContextReply {
    pub(crate) signed: trellis_protocol::SignedSessionAuthority,
    pub(crate) desired: trellis_protocol::TransportAuthorizationV1,
    pub(crate) admitted: trellis_protocol::TransportAuthorizationV1,
    pub(crate) attachment_id: String,
    pub(crate) authorization: Option<serde_json::Value>,
}

/// Obtain issuer-authenticated own authority on an already admitted attachment.
/// The actual attachment, subject, reply, nonce and runtime are verified before preparation.
/// Status messages, malformed responses and timeouts never revoke a session.
pub(crate) async fn fetch_own_context(
    cache: &AuthorizationContextCache,
    auth: &SessionAuth,
    nats: &async_nats::Client,
    timeout: Duration,
) -> Result<OwnContextReply, TrellisClientError> {
    tokio::time::timeout(timeout, async {
        let broker_marker = read_own_admission(nats, timeout).await?;
        let attachment_id = broker_marker
            .authenticated_user
            .strip_prefix("trellis.auth.v1:")
            .and_then(|id| {
                ulid::Ulid::from_string(id)
                    .ok()
                    .filter(|parsed| parsed.to_string() == id)
            })
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "broker attachment identity malformed".into(),
                )
            })?
            .to_string();
        let subject = trellis_protocol::admission_get_subject(&auth.session_key)?;
        let reply = nats.new_inbox();
        let request = AdmissionGet {
            trellis_instance_id: cache.trust.instance_id.clone(),
            runtime_id: cache.runtime.runtime_id.clone(),
            session_public_key: auth.session_key.clone(),
            attachment_id: attachment_id.clone(),
            request_nonce: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>()),
            issued_at: super::super::proof::now_iat_seconds() as i64,
        };
        let proof = trellis_protocol::sign_admission_get(
            request.clone(),
            &subject,
            &reply,
            auth.live_signing_key(),
        )?;
        let mut responses = nats
            .subscribe(reply.clone())
            .await
            .map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))?;
        // Flush establishes subscription visibility before the request is sent.
        nats.flush()
            .await
            .map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))?;
        nats.publish_with_reply(
            subject.clone(),
            reply.clone(),
            serde_json::to_vec(&proof)?.into(),
        )
        .await
        .map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))?;
        let response = responses.next().await.ok_or_else(|| {
            TrellisClientError::TransportUnavailable("own-authority response unavailable".into())
        })?;
        if response.payload.len() > trellis_protocol::MAX_ADMISSION_RESPONSE_BYTES {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own-authority response too large".into(),
            ));
        }
        let envelope: SignedAdmissionContext = serde_json::from_slice(&response.payload)?;
        // The unverified identifier is only a bounded lookup into pinned trust.
        let issuer = cache
            .owner
            .issuer(nats, &envelope.response.issuer_key_id)
            .await?;
        let envelope = trellis_protocol::verify_admission_context(
            &response.payload,
            &issuer,
            &request,
            &subject,
            &reply,
            &cache.trust.account,
            super::super::proof::now_iat_seconds() as i64,
        )?;
        let signed = trellis_protocol::parse_session_authority(
            envelope.response.session_authority.as_bytes(),
            trellis_protocol::MAX_SESSION_AUTHORITY_BYTES,
        )?;
        let policy: trellis_protocol::TransportAuthorizationV1 = serde_json::from_value(
            envelope
                .response
                .registry
                .get("transportAuthorization")
                .cloned()
                .ok_or_else(|| {
                    TrellisClientError::AuthorizationUnavailable(
                        "signed transport policy unavailable".into(),
                    )
                })?,
        )?;
        policy.validate()?;
        if policy.account != cache.trust.account {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "signed transport account mismatch".into(),
            ));
        }
        let admitted: trellis_protocol::TransportAuthorizationV1 = serde_json::from_value(
            envelope
                .response
                .registry
                .get("admittedTransportAuthorization")
                .cloned()
                .ok_or_else(|| {
                    TrellisClientError::AuthorizationUnavailable(
                        "signed attachment policy unavailable".into(),
                    )
                })?,
        )?;
        admitted.validate()?;
        if admitted.account != cache.trust.account {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "signed attachment account mismatch".into(),
            ));
        }
        let attachment_id = envelope
            .response
            .registry
            .get("attachmentId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "signed attachment identity unavailable".into(),
                )
            })?
            .to_owned();
        if attachment_id != request.attachment_id {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "signed attachment identity mismatch".into(),
            ));
        }
        let current_marker = read_own_admission(nats, timeout).await?;
        if current_marker.authenticated_user != broker_marker.authenticated_user {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "broker attachment changed during own-authority read".into(),
            ));
        }
        Ok(OwnContextReply {
            signed,
            desired: policy,
            admitted,
            attachment_id,
            authorization: envelope.response.registry.get("authorization").cloned(),
        })
    })
    .await
    .map_err(|_| {
        TrellisClientError::AuthorizationUnavailable("own-authority read timed out".into())
    })?
}

pub(crate) async fn read_own_context(
    cache: &AuthorizationContextCache,
    auth: &SessionAuth,
    nats: &async_nats::Client,
    timeout: Duration,
) -> Result<PreparedOwnCandidate, TrellisClientError> {
    let response = fetch_own_context(cache, auth, nats, timeout).await?;
    cache
        .prepare_authority(
            nats,
            &response.signed,
            response.desired,
            response.authorization,
        )
        .await
}

/// Renew privately, acquire an exact compatible carrier or fresh admitted stage,
/// and publish only at the final checked transport boundary.
pub(crate) async fn install_prepared_authorization(
    runtime: &AuthorizationRefreshRuntime,
    carrier: Option<crate::client::transport_generations::TransportLease>,
) -> Result<String, TrellisClientError> {
    let _refresh = runtime.contexts.refresh.lock().await;
    if let Some(cause) = runtime.generations.terminal() {
        return Err(cause.client_error());
    }
    let lease = match carrier {
        Some(carrier) => carrier,
        None => runtime.generations.acquire_published()?,
    };
    let candidate = tokio::select! {
        biased;
        cause = runtime.generations.wait_terminal() => return Err(cause.client_error()),
        result = read_own_context(&runtime.contexts, &runtime.auth, lease.nats(), Duration::from_secs(10)) => result?,
    };
    drop(lease);
    let preparation = tokio::select! {
        biased;
        cause = runtime.generations.wait_terminal() => return Err(cause.client_error()),
        result = runtime.generations.prepare_own_coverage(&candidate) => result?,
    };
    let transition = runtime.contexts.lock_own_transition()?;
    let commit = || {
        if let Some(cause) = runtime.generations.terminal() {
            return Err(cause.client_error());
        }
        runtime
            .contexts
            .promote_locked(&transition, &candidate.context_digest, &candidate.instance)
    };
    match preparation.stage {
        Some(stage) => stage.finish(&transition, commit)?,
        None => runtime.generations.commit_candidate_on_carrier(
            &transition,
            &candidate,
            &preparation.lease,
            commit,
        )?,
    }
    runtime.generations.authorization_promoted();
    Ok(candidate.context_digest)
}

/// Start one bounded renewal loop and one shared-owner retirement subscription.
pub(crate) fn spawn_authorization_context_refresh_task(
    runtime: AuthorizationRefreshRuntime,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut changes = runtime.contexts.owner.subscribe_changes();
        loop {
            let delay = runtime
                .contexts
                .refresh_delay()
                .unwrap_or(Duration::from_secs(1));
            tokio::select! {
                _ = runtime.generations.wait_terminal() => return,
                _ = runtime.contexts.wait_refresh_request() => {},
                _ = tokio::time::sleep(delay) => {},
                result = changes.recv() => {
                    if matches!(result, Err(tokio::sync::broadcast::error::RecvError::Closed)) { return; }
                    if let Ok(authority) = runtime.contexts.owner.own_authority() {
                        if runtime.contexts.owner.current_trust().is_ok_and(|trust| {
                            trust.issuer.key_id == authority.authority().issuer_key_id
                        }) {
                            continue;
                        }
                    } else {
                        runtime.contexts.suspend();
                        runtime.generations.authorization_promoted();
                        if runtime.contexts.owner.staged_own_authority().is_ok() {
                            // Coverage-only recovery belongs to the mirror supervisor.
                            continue;
                        }
                    }
                }
            }
            if runtime.contexts.owner.own_authority().is_err() {
                runtime.contexts.suspend();
            }
            if let Err(error) = install_prepared_authorization(&runtime, None).await {
                tracing::warn!(%error, "own-authority renewal unavailable");
                if runtime.contexts.owner.own_authority().is_err() {
                    runtime.contexts.suspend();
                }
                if runtime
                    .generations
                    .pace_or_terminal(Duration::from_secs(1))
                    .await
                    .is_some()
                {
                    return;
                }
            }
        }
    })
}
