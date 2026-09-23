//! Client-side live open: bounded opening request, signed-offer verification,
//! prepared handle installation, activation, and the data/control pump.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use trellis_protocol::{
    derive_live_data_subject, LiveErrorCode, LiveFrame, LiveOffer, LiveOfferKind, LiveSessionKind,
    PermissionAtom, OPEN_RESERVATION_MS,
};

use crate::client::{AuthorizationProviderCache, TrellisClientError};

use super::authority::{LiveAuthorityGuard, LiveGuardRequirement, PinnedPeerIdentity};
use super::deadlines::{DeadlineAction, LiveDeadlines};
use super::subscription::{
    activate_control, authority_failure, consumer_failure, credit_control, end_ack_control,
    pulse_control, ConsumerControl, ConsumerCore, ConsumerPhase,
};
use super::types::{LiveCancellation, LiveEnd, LiveEndReason};

/// Inputs for one client-side live open.
pub(crate) struct ClientOpen<'a> {
    pub kind: LiveSessionKind,
    pub api_id: &'a str,
    /// Descriptor/binding-derived route the signed offer must echo.
    pub base_subject: &'a str,
    /// NATS subject the opening request is published on.
    ///
    /// Live observations publish on `base_subject`. Operation watch publishes on the
    /// operation control subject while the offer still binds `base_subject`.
    pub publish_subject: &'a str,
    pub body: Bytes,
    pub open_id: String,
    pub receive_max_payload_bytes: u64,
    /// Exact descriptor-derived Subscribe or Observe permission the remote
    /// provider's installed participant must hold for this route.
    pub permission: PermissionAtom,
}

/// One verified prepared observation, before its first iteration.
pub(crate) struct PreparedClientSession {
    pub offer: LiveOffer,
    pub peer: PinnedPeerIdentity,
    pub max_data_body_bytes: u64,
    pub context_digest: String,
    /// Absolute local opening/reservation deadline started before the opening
    /// exchange. It does not restart when the first poll runs.
    pub deadline: tokio::time::Instant,
    /// Retained peer-provider authority established during verification.
    pub provider_guard: LiveAuthorityGuard,
}

/// Outcome of one complete client open.
pub(crate) enum ClientOpenOutcome {
    Live(PreparedClientSession),
    Operation(PreparedClientSession),
}

/// Perform one bounded opening exchange and verify the signed offer.
///
/// # Errors
///
/// Returns a setup error for an incompatible peer, invalid offer, mismatched
/// identity/limits, or a lost opening reply. No prepared handle is returned.
pub(crate) async fn open_client_session(
    client: &crate::client::TrellisClient,
    provider: &AuthorizationProviderCache,
    open: ClientOpen<'_>,
) -> Result<PreparedClientSession, TrellisClientError> {
    // The local reservation budget starts before the opening exchange and is
    // never restarted by activation. The provider keeps its own independent
    // allocation-relative deadline; no clock synchronization is assumed.
    let deadline = tokio::time::Instant::now() + reservation_budget();
    let step_timeout = || {
        deadline
            .min(tokio::time::Instant::now() + Duration::from_millis(client.timeout_ms().max(1)))
    };
    let consumer = client.own_pinned_identity()?;
    let consumer_digest = client.authorization_context_digest()?;
    let subject = open.publish_subject.to_owned();
    let reply = format!(
        "{}.{}",
        client.inbox_prefix(),
        LIVE_INBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let headers = client.signed_headers(&subject, &reply, &open.body)?;
    let request_id = headers
        .get("request-id")
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut subscriber =
        tokio::time::timeout_at(step_timeout(), client.nats().subscribe(reply.clone()))
            .await
            .map_err(|_| TrellisClientError::Timeout)?
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    tokio::time::timeout_at(
        step_timeout(),
        client
            .nats()
            .publish_with_reply_and_headers(subject, reply, headers, open.body.clone()),
    )
    .await
    .map_err(|_| TrellisClientError::Timeout)?
    .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;

    let response = tokio::time::timeout_at(step_timeout(), subscriber.next())
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .ok_or(TrellisClientError::Timeout)?;

    verify_offer(
        client,
        provider,
        &open,
        &request_id,
        &response,
        deadline,
        &consumer,
        &consumer_digest,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn verify_offer(
    client: &crate::client::TrellisClient,
    provider: &AuthorizationProviderCache,
    open: &ClientOpen<'_>,
    request_id: &str,
    response: &async_nats::Message,
    deadline: tokio::time::Instant,
    consumer: &PinnedPeerIdentity,
    consumer_digest: &str,
) -> Result<PreparedClientSession, TrellisClientError> {
    trellis_protocol::validate_control_body(&response.payload)
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&response.payload)
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    let kind = value.get("type").and_then(|kind| kind.as_str());
    if kind != Some("offer") {
        // A signed open-error or any non-offer body is a setup failure; it is
        // never interpreted as a session offer.
        let code = value
            .get("code")
            .and_then(|code| code.as_str())
            .or_else(|| value.get("type").and_then(|kind| kind.as_str()))
            .unwrap_or("invalid_request");
        return Err(TrellisClientError::LiveProtocol(format!(
            "live open rejected with '{code}': {value}"
        )));
    }
    let offer: LiveOffer = serde_json::from_value(value)
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    if offer.kind != LiveOfferKind::Offer {
        return Err(TrellisClientError::LiveProtocol(
            "live open response is not an offer".into(),
        ));
    }
    if offer.open_id != open.open_id {
        return Err(TrellisClientError::LiveProtocol(
            "offer answers a different logical open".into(),
        ));
    }
    // The offer must carry the provider's current context and a proof that
    // binds this exact response subject and raw bytes.
    let context_digest = response
        .headers
        .as_ref()
        .and_then(|headers| headers.get("authorization-context"))
        .map(ToString::to_string)
        .ok_or_else(|| {
            TrellisClientError::LiveProtocol("offer omitted its authorization context".into())
        })?;
    let session_key = response
        .headers
        .as_ref()
        .and_then(|headers| headers.get("session-key"))
        .map(ToString::to_string)
        .ok_or_else(|| TrellisClientError::LiveProtocol("offer omitted its signer".into()))?;
    let proof = response
        .headers
        .as_ref()
        .and_then(|headers| headers.get("trellis-live-proof"))
        .map(ToString::to_string)
        .ok_or_else(|| TrellisClientError::LiveProtocol("offer omitted its proof".into()))?;
    trellis_protocol::verify_live_server_proof_encoded(
        &trellis_protocol::LiveServerProof::parse(proof)
            .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?,
        &context_digest,
        response.subject.as_str(),
        &response.payload,
        &session_key,
    )
    .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    let policy = provider
        .policy()
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    let lease = provider
        .resolve_context(&context_digest, policy.now_unix_seconds)
        .await
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    let peer = PinnedPeerIdentity::from_signed(lease.signed_context());
    if peer.session_key != session_key {
        return Err(TrellisClientError::LiveProtocol(
            "offer context does not bind the signing session key".into(),
        ));
    }
    if trellis_protocol::encode_subject_token(&session_key) != offer.provider.session_key {
        return Err(TrellisClientError::LiveProtocol(
            "offer signer does not match its advertised identity".into(),
        ));
    }
    if offer.request_id != request_id {
        return Err(TrellisClientError::LiveProtocol(
            "offer answers a different opening request".into(),
        ));
    }
    // The exact subjects must recompute from this session's identities.
    let expected_data = derive_live_data_subject(
        &offer.provider.connection_id,
        &offer.consumer.connection_id,
        &offer.session_id,
    )
    .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    if expected_data != offer.data_subject {
        return Err(TrellisClientError::LiveProtocol(
            "offer data subject is not canonical for this session".into(),
        ));
    }
    let expected_control = trellis_protocol::derive_live_observe_subject(
        &offer.base_subject,
        &offer.provider.connection_id,
        &offer.session_id,
    )
    .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    if expected_control != offer.control_subject {
        return Err(TrellisClientError::LiveProtocol(
            "offer control subject is not canonical for this session".into(),
        ));
    }
    // The selected deployment must match the consumer's installed binding.
    let selected = provider
        .provider_deployment_id(open.api_id)
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    verify_offer_claims(open, &offer, &selected, &peer, consumer, consumer_digest)?;
    let negotiated = trellis_protocol::negotiate_max_data_body_bytes(
        open.receive_max_payload_bytes,
        client.nats().max_payload() as u64,
    )
    .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    if offer.limits.max_data_body_bytes > negotiated {
        return Err(TrellisClientError::LiveProtocol(
            "offer exceeds the negotiated data body limit".into(),
        ));
    }
    if offer.kind != LiveOfferKind::Offer {
        return Err(TrellisClientError::LiveProtocol(
            "unsupported live offer kind".into(),
        ));
    }
    // Retain the remote observer/provider evidence for the whole session, not
    // just for the opening exchange. Every later frame must still satisfy it.
    let provider_guard = LiveAuthorityGuard::retain(
        provider,
        &context_digest,
        LiveGuardRequirement::PeerProvider {
            expected: peer.clone(),
        },
    )
    .await
    .map_err(|lost| {
        TrellisClientError::AuthorizationUnavailable(format!("provider guard: {lost:?}"))
    })?;
    Ok(PreparedClientSession {
        max_data_body_bytes: offer.limits.max_data_body_bytes,
        offer,
        peer,
        context_digest,
        deadline,
        provider_guard,
    })
}

/// Install one prepared Live observation as an owned public handle.
///
/// The handle owns the caller's prepared state and the exact data
/// subscription; the first poll starts activation and the pump.
///
/// # Errors
///
/// Returns a setup error when the data subscription cannot be flushed or the
/// consumer admission bound is reached.
pub(crate) async fn install_live_handle<D>(
    client: &crate::client::TrellisClient,
    prepared: PreparedClientSession,
) -> Result<crate::live::subscription::LiveSubscription<D::Event>, TrellisClientError>
where
    D: crate::generated::LiveDescriptor,
    D::Event: crate::generated::Codec + Send + 'static,
{
    install_prepared_handle(client, prepared, |value| {
        <D::Event as crate::generated::Codec>::decode(value)
            .map(Some)
            .map_err(|error| TrellisClientError::Codec(error.to_string()))
    })
    .await
}

/// Install one prepared Operation watch observation as an owned public handle.
///
/// Data frames are JSON snapshot/event envelopes decoded by `decode`. A `None`
/// result skips the frame (keepalive) after releasing credit.
///
/// # Errors
///
/// Returns a setup error when the data subscription cannot be flushed or the
/// consumer admission bound is reached.
pub(crate) async fn install_operation_watch_handle<T, F>(
    client: &crate::client::TrellisClient,
    prepared: PreparedClientSession,
    decode: F,
) -> Result<crate::live::subscription::LiveSubscription<T>, TrellisClientError>
where
    T: Send + 'static,
    F: Fn(serde_json::Value) -> Result<Option<T>, TrellisClientError> + Send + 'static,
{
    install_prepared_handle(client, prepared, decode).await
}

async fn install_prepared_handle<T, F>(
    client: &crate::client::TrellisClient,
    prepared: PreparedClientSession,
    decode: F,
) -> Result<crate::live::subscription::LiveSubscription<T>, TrellisClientError>
where
    T: Send + 'static,
    F: Fn(serde_json::Value) -> Result<Option<T>, TrellisClientError> + Send + 'static,
{
    if tokio::time::Instant::now() >= prepared.deadline {
        return Err(TrellisClientError::Timeout);
    }
    let manager = client.live_manager().ok_or_else(|| {
        TrellisClientError::Bootstrap("live manager is unavailable for this connection".into())
    })?;
    manager.is_available().map_err(|unavailable| {
        TrellisClientError::AuthorizationUnavailable(format!(
            "live manager unavailable: {unavailable:?}"
        ))
    })?;
    let permit = manager.clone().admit_consumer().map_err(|code| {
        TrellisClientError::LiveProtocol(format!("admission rejected: {code:?}"))
    })?;
    let core = Arc::new(ConsumerCore::new(
        prepared.offer.session_id.clone(),
        prepared.offer.session_kind,
    ));
    core.set_phase(ConsumerPhase::Prepared);
    let provider_guard = std::sync::Arc::new(prepared.provider_guard);
    let control = Arc::new(ConsumerControl {
        nats: client.nats(),
        auth: client.auth_handle(),
        contexts: client.authorization_contexts_handle()?,
        inbox_prefix: client.inbox_prefix().to_owned(),
        session_id: prepared.offer.session_id.clone(),
        control_subject: prepared.offer.control_subject.clone(),
        pinned_session_key: prepared.peer.session_key.clone(),
        pinned_identity: prepared.peer.clone(),
        provider_guard: provider_guard.clone(),
        close_started: std::sync::atomic::AtomicBool::new(false),
        last_control_seq: std::sync::atomic::AtomicU64::new(0),
    });
    let cancellation = LiveCancellation::new();
    let pump = ConsumerPump::new(
        core.clone(),
        control.clone(),
        cancellation.clone(),
        prepared.max_data_body_bytes,
        prepared.deadline,
        provider_guard.clone(),
        permit,
    );
    let drain = pump.spawn(client.nats(), prepared.offer.data_subject.clone(), decode);
    Ok(crate::live::subscription::LiveSubscription::new(
        core,
        drain,
        control,
        cancellation,
        provider_guard,
    ))
}

/// Reject an offer whose route or session kind does not match the open.
/// Verify every advertised claim in a parsed, proof-verified live offer against
/// the independently selected provider binding and the opener's pinned local
/// identity. This is the production decision; its tests call the same function.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_offer_claims(
    open: &ClientOpen<'_>,
    offer: &LiveOffer,
    selected_provider_deployment_id: &str,
    peer: &PinnedPeerIdentity,
    consumer: &PinnedPeerIdentity,
    consumer_digest: &str,
) -> Result<(), TrellisClientError> {
    verify_offer_identity(open, offer)?;
    // The independently selected binding is the evidence that this deployment's
    // participant implements the API; the offered provider deployment must be
    // exactly that binding, never the offer's own claim.
    if selected_provider_deployment_id != offer.provider.deployment_id {
        return Err(TrellisClientError::LiveProtocol(
            "offer provider is not the selected deployment for this API".into(),
        ));
    }
    // Every advertised provider field must match the verified signed context; a
    // valid signature does not authorize silently changing them later.
    if peer.connection_id != offer.provider.connection_id
        || peer.principal_id != offer.provider.principal_id
        || peer.participant_id != offer.provider.participant_id
        || peer.deployment_id.as_deref().unwrap_or("") != offer.provider.deployment_id
        || peer.instance_id.as_deref().unwrap_or("") != offer.provider.instance_id
    {
        return Err(TrellisClientError::LiveProtocol(
            "offer provider tuple does not match its verified context".into(),
        ));
    }
    // The offered consumer tuple must be this opener's actual current local
    // identity, resolved before the offer was accepted.
    if consumer_digest.is_empty()
        || consumer.connection_id != offer.consumer.connection_id
        || consumer.principal_id != offer.consumer.principal_id
        || consumer.participant_id != offer.consumer.participant_id
        || trellis_protocol::encode_subject_token(&consumer.session_key)
            != offer.consumer.session_key
    {
        return Err(TrellisClientError::LiveProtocol(
            "offer consumer does not match the opening caller".into(),
        ));
    }
    Ok(())
}

fn verify_offer_identity(
    open: &ClientOpen<'_>,
    offer: &LiveOffer,
) -> Result<(), TrellisClientError> {
    if offer.base_subject != open.base_subject {
        return Err(TrellisClientError::LiveProtocol(
            "offer base subject does not match the opening route".into(),
        ));
    }
    if offer.session_kind != open.kind {
        return Err(TrellisClientError::LiveProtocol(
            "offer session kind does not match the opening kind".into(),
        ));
    }
    Ok(())
}

/// Build one `open-error`-shaped setup failure from a raw body.
#[must_use]
pub(crate) fn open_error_from_body(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("code")
        .and_then(|code| code.as_str())
        .map(str::to_owned)
}

/// Return the opening reservation budget.
#[must_use]
pub(crate) fn reservation_budget() -> Duration {
    Duration::from_millis(OPEN_RESERVATION_MS)
}

/// Sleep until an optional deadline; wait forever when none is scheduled.
async fn wait_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Return the earlier of two optional monotonic deadlines.
fn earliest_deadline(
    first: Option<tokio::time::Instant>,
    second: Option<tokio::time::Instant>,
) -> Option<tokio::time::Instant> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

/// Convert a retained guard's wall-clock expiry into a monotonic wake.
///
/// Signed authorization validity uses the existing wall-clock policy; only the
/// local wake is monotonic.
fn guard_deadline(guard: &LiveAuthorityGuard) -> Option<tokio::time::Instant> {
    let expires_at = guard.expires_at_seconds();
    let now = crate::client::now_iat_seconds() as i64;
    let remaining = expires_at.saturating_sub(now).max(0) as u64;
    Some(tokio::time::Instant::now() + Duration::from_secs(remaining))
}

/// Releases the consumer session when the pump task actually exits.
struct PumpCleanup<T>(Arc<ConsumerCore<T>>);

impl<T> Drop for PumpCleanup<T> {
    fn drop(&mut self) {
        self.0.cleanup_finished();
    }
}

/// One live data pump over a verified prepared session.
pub(crate) struct ConsumerPump<T> {
    core: Arc<ConsumerCore<T>>,
    control: Arc<ConsumerControl>,
    cancellation: LiveCancellation,
    max_data_body_bytes: u64,
    /// Absolute opening/reservation deadline; activation must beat it.
    deadline: tokio::time::Instant,
    /// Retained provider authority, checked on admission and on change events.
    guard: Arc<LiveAuthorityGuard>,
    /// Consumer admission retained until this pump settles.
    permit: Option<super::manager::ConsumerPermit>,
}

impl<T> ConsumerPump<T> {
    /// Create one pump for a prepared session.
    #[must_use]
    pub(crate) fn new(
        core: Arc<ConsumerCore<T>>,
        control: Arc<ConsumerControl>,
        cancellation: LiveCancellation,
        max_data_body_bytes: u64,
        deadline: tokio::time::Instant,
        guard: Arc<LiveAuthorityGuard>,
        permit: super::manager::ConsumerPermit,
    ) -> Self {
        Self {
            core,
            control,
            cancellation,
            max_data_body_bytes,
            deadline,
            guard,
            permit: Some(permit),
        }
    }

    /// Run activation then the data/control loop until a terminal outcome.
    ///
    /// The consumer becomes ACTIVE only after a matching pulse acknowledgement
    /// verifies; ordinary data or a stale challenge never renews liveness. The
    /// monotonic deadline owner supplies peer, credit and draining deadlines.
    ///
    /// # Errors
    ///
    /// Never returns an error: every failure commits one terminal outcome on
    /// the consumer core so the application sees exactly one diagnosis.
    pub(crate) fn spawn<F>(
        self,
        nats: async_nats::Client,
        data_subject: String,
        decode: F,
    ) -> tokio::task::JoinHandle<()>
    where
        T: Send + 'static,
        F: Fn(serde_json::Value) -> Result<Option<T>, TrellisClientError> + Send + 'static,
    {
        tokio::spawn(async move {
            // Actual local cleanup completion is this task's exit, including
            // every early return and the aborted-by-Drop path.
            let _cleanup = PumpCleanup(Arc::clone(&self.core));
            let mut guard_changes = self.guard.subscribe_changes();
            // A prepared handle keeps an active deadline and admission even
            // when the application never iterates it: the reservation starts
            // before the opening exchange and does not restart here.
            tokio::select! {
                _ = self.core.start.notified() => {}
                _ = self.cancellation.cancelled() => return,
                _ = tokio::time::sleep_until(self.deadline) => {
                    self.core.commit_end(consumer_failure(
                        LiveErrorCode::SetupTimeout,
                        "live prepared session expired before activation",
                    ));
                    self.core.wake();
                    return;
                }
            }
            let session_id = self.core.session_id.clone();
            let mut deadlines = LiveDeadlines::prepared_until(self.deadline);
            let mut subscription = match nats.subscribe(data_subject).await {
                Ok(subscription) => subscription,
                Err(_) => {
                    self.core.commit_end(consumer_failure(
                        LiveErrorCode::Disconnected,
                        "live data subscription could not be installed",
                    ));
                    return;
                }
            };
            if nats.flush().await.is_err() {
                self.core.commit_end(consumer_failure(
                    LiveErrorCode::Disconnected,
                    "live data subscription could not be flushed",
                ));
                return;
            }
            self.core.set_phase(ConsumerPhase::Activating);
            if let Err(lost) = self.guard.check_now() {
                self.core.commit_end(authority_failure(&lost));
                return;
            }

            // Activation: bounded fresh-proof retries within the reservation.
            let reservation_deadline = self.deadline;
            let activate = activate_control(&session_id);
            let activate_seq = self.control.next_control_seq();
            loop {
                let attempt = tokio::select! {
                    _ = self.cancellation.cancelled() => {
                        self.core.discard_queue();
                        self.core.commit_end(LiveEnd::new(LiveEndReason::Cancelled, None));
                        return;
                    }
                    result = self.control.send_control(&activate, activate_seq) => result,
                };
                match attempt {
                    Ok(ack) if ack.state != trellis_protocol::LiveSessionState::Closed => break,
                    Ok(_) => {
                        self.core.commit_end(consumer_failure(
                            LiveErrorCode::SetupTimeout,
                            "live session closed during activation",
                        ));
                        return;
                    }
                    Err(_) if tokio::time::Instant::now() < reservation_deadline => continue,
                    Err(_) => {
                        self.core.commit_end(consumer_failure(
                            LiveErrorCode::SetupTimeout,
                            "live activation did not complete within the reservation",
                        ));
                        return;
                    }
                }
            }

            let mut next_expected: u64 = 1;
            let mut last_credit_sent: u64 = 0;
            // One logical credit control is outstanding at a time; retries
            // reuse its identical body and sequence, and newer consumption is
            // coalesced into the next logical control.
            let mut pending_credit: Option<(u64, u64, u64)> = None;
            loop {
                if self.core.committed_end().is_some() {
                    return;
                }
                if matches!(self.core.phase(), ConsumerPhase::Draining) {
                    // The data subscription and peer pulse work are stopped;
                    // only the bounded local drain and its stall deadline
                    // remain. Local handoff progress re-arms that clock.
                    if self.core.drain_complete().is_some() {
                        return;
                    }
                    let next = deadlines.next_due();
                    tokio::select! {
                        _ = self.cancellation.cancelled() => {
                            self.core.discard_queue();
                            self.core.commit_end(LiveEnd::new(LiveEndReason::Cancelled, None));
                            return;
                        }
                        _ = wait_until(next) => {
                            let now = tokio::time::Instant::now();
                            if deadlines.evaluate(now) == Some(DeadlineAction::ConsumerStalled) {
                                self.core.discard_queue();
                                self.core.commit_end(consumer_failure(
                                    LiveErrorCode::ConsumerSlow,
                                    "live draining queue was not consumed",
                                ));
                                return;
                            }
                        }
                        _ = self.core.end_notify.notified() => {
                            let consumed = self.core.consumed_seq();
                            if consumed > last_credit_sent {
                                deadlines.note_consumption(
                                    tokio::time::Instant::now(),
                                    consumed - last_credit_sent,
                                );
                                last_credit_sent = consumed;
                            }
                            if self.core.drain_complete().is_some() {
                                return;
                            }
                        }
                    }
                    continue;
                }
                let next = earliest_deadline(deadlines.next_due(), guard_deadline(&self.guard));
                tokio::select! {
                    _ = self.cancellation.cancelled() => {
                        self.core.discard_queue();
                        self.core.commit_end(LiveEnd::new(LiveEndReason::Cancelled, None));
                        return;
                    }
                    _ = self.core.credit_notify.notified() => {
                        let consumed = self.core.consumed_seq();
                        if consumed > last_credit_sent {
                            deadlines.note_consumption(
                                tokio::time::Instant::now(),
                                consumed - last_credit_sent,
                            );
                        }
                    }
                    _ = guard_changes.recv() => {
                        if let Err(lost) = self.guard.check_now() {
                            self.core.discard_queue();
                            self.core.commit_end(authority_failure(&lost));
                            return;
                        }
                    }
                    _ = wait_until(next) => {
                        let now = tokio::time::Instant::now();
                        if let Err(lost) = self.guard.check_now() {
                            self.core.discard_queue();
                            self.core.commit_end(authority_failure(&lost));
                            return;
                        }
                        match deadlines.evaluate(now) {
                            Some(DeadlineAction::ReservationExpired) => {
                                self.core.commit_end(consumer_failure(
                                    LiveErrorCode::SetupTimeout,
                                    "live activation did not complete within the reservation",
                                ));
                                return;
                            }
                            Some(DeadlineAction::PeerInactive) => {
                                self.core.commit_end(consumer_failure(
                                    LiveErrorCode::PeerLost,
                                    "live provider silent past the inactivity bound",
                                ));
                                return;
                            }
                            Some(DeadlineAction::CreditDue) => {
                                deadlines.credit_sent();
                                if pending_credit.is_none() {
                                    let consumed = self.core.consumed_seq();
                                    if consumed > last_credit_sent {
                                        pending_credit = Some((
                                            self.control.next_control_seq(),
                                            self.core.received_seq(),
                                            consumed,
                                        ));
                                    }
                                }
                                if let Some((control_seq, received, consumed)) = pending_credit {
                                    let acked = self
                                        .control
                                        .send_control(
                                            &credit_control(
                                                &session_id,
                                                control_seq,
                                                received,
                                                consumed,
                                            ),
                                            control_seq,
                                        )
                                        .await
                                        .is_ok();
                                    if acked {
                                        last_credit_sent = last_credit_sent.max(consumed);
                                        pending_credit = None;
                                    } else {
                                        // Retain the logical control and re-arm one
                                        // bounded retry with the identical body.
                                        deadlines.note_consumption(now, 0);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    message = subscription.next() => {
                        let Some(message) = message else {
                            self.core.commit_end(consumer_failure(
                                LiveErrorCode::PeerLost,
                                "live delivery subscription ended",
                            ));
                            return;
                        };
                        // Incoming-frame admission performs a local current-state
                        // check before any frame influences state.
                        if let Err(lost) = self.guard.check_now() {
                            self.core.discard_queue();
                            self.core.commit_end(authority_failure(&lost));
                            return;
                        }
                        // Frames must authenticate against the pinned provider
                        // before they influence state; garbage is discarded.
                        let Ok(frame) = trellis_protocol::parse_live_frame(
                            &message.payload,
                            self.max_data_body_bytes,
                        ) else {
                            if let Ok(telemetry) = self.core.telemetry.lock() {
                                telemetry.rejection(
                                    super::telemetry::LiveRejection::InvalidProtocol,
                                );
                            }
                            continue;
                        };
                        if !verify_provider_frame(&self.control, &message, &frame) {
                            if let Ok(telemetry) = self.core.telemetry.lock() {
                                telemetry.rejection(
                                    super::telemetry::LiveRejection::InvalidSignature,
                                );
                            }
                            continue;
                        }
                        if let Ok(telemetry) = self.core.telemetry.lock() {
                            telemetry.frame(
                                super::telemetry::LiveFrameClass::Control,
                                super::telemetry::LiveDirection::Receive,
                            );
                            if matches!(frame, LiveFrame::Data(_)) {
                                telemetry.frame(
                                    super::telemetry::LiveFrameClass::Data,
                                    super::telemetry::LiveDirection::Receive,
                                );
                            }
                        }
                        match &frame {
                            LiveFrame::Data(data) => {
                                let seq = data.seq.get();
                                if seq > next_expected {
                                    self.core.commit_end(consumer_failure(
                                        LiveErrorCode::DeliveryGap,
                                        "live delivery sequence gap",
                                    ));
                                    return;
                                }
                                if seq < next_expected {
                                    // Already received; discard without credit.
                                    continue;
                                }
                                next_expected = seq + 1;
                                match decode(data.value.clone()) {
                                    Ok(Some(value)) => {
                                        // Charge the complete received DATA body,
                                        // not a re-serialized value projection.
                                        let encoded_len = message.payload.len() as u64;
                                        if !self.core.admit(super::subscription::AdmittedItem {
                                            value,
                                            encoded_len,
                                        }) {
                                            self.core.commit_end(consumer_failure(
                                                LiveErrorCode::ConsumerSlow,
                                                "bounded live ingress exceeded",
                                            ));
                                            return;
                                        }
                                        self.core.record_received();
                                    }
                                    Ok(None) => {
                                        // Keepalive and other filtered frames still
                                        // occupy their ordered sequence slot and
                                        // release credit once the prefix is clear.
                                        self.core.record_received();
                                        if !self.core.release_filtered() {
                                            self.core.commit_end(consumer_failure(
                                                LiveErrorCode::ConsumerSlow,
                                                "bounded live ingress exceeded",
                                            ));
                                            return;
                                        }
                                    }
                                    Err(error) => {
                                        self.core.commit_end(consumer_failure(
                                            LiveErrorCode::ProtocolError,
                                            error.to_string(),
                                        ));
                                        return;
                                    }
                                }
                            }
                            LiveFrame::Challenge(challenge) => {
                                if challenge.last_sent_seq.get() > self.core.received_seq() {
                                    self.core.commit_end(consumer_failure(
                                        LiveErrorCode::DeliveryGap,
                                        "live challenge referred to unreceived data",
                                    ));
                                    return;
                                }
                                let control_seq = self.control.next_control_seq();
                                let pulse = pulse_control(
                                    &session_id,
                                    control_seq,
                                    &challenge.challenge_id,
                                    self.core.received_seq(),
                                    self.core.consumed_seq(),
                                );
                                let ack = tokio::select! {
                                    _ = self.cancellation.cancelled() => {
                                        self.core.discard_queue();
                                        self.core.commit_end(LiveEnd::new(LiveEndReason::Cancelled, None));
                                        return;
                                    }
                                    result = self.control.send_control(&pulse, control_seq) => result,
                                };
                                // Only a matching, authenticated pulse ack commits
                                // activation or renews the fresh-round-trip clock.
                                if let Ok(ack) = ack {
                                    if ack.state == trellis_protocol::LiveSessionState::Active {
                                        let now = tokio::time::Instant::now();
                                        if matches!(self.core.phase(), ConsumerPhase::Activating) {
                                            deadlines.commit_active(now, false);
                                            self.core.set_phase(ConsumerPhase::Active);
                                        } else {
                                            deadlines.fresh_round_trip(now, false);
                                        }
                                    }
                                }
                            }
                            LiveFrame::End(end) => {
                                if end.final_seq.get() != self.core.received_seq() {
                                    self.core.commit_end(consumer_failure(
                                        LiveErrorCode::DeliveryGap,
                                        "live end did not follow the complete sequence",
                                    ));
                                    return;
                                }
                                // One end-ack confirms transport receipt, not
                                // application processing; queued items drain
                                // locally afterwards.
                                let control_seq = self.control.next_control_seq();
                                let _ = self
                                    .control
                                    .send_control(
                                        &end_ack_control(
                                            &session_id,
                                            control_seq,
                                            end.final_seq.get(),
                                            self.core.received_seq(),
                                            self.core.consumed_seq(),
                                        ),
                                        control_seq,
                                    )
                                    .await;
                                let end = match &end.terminal.error {
                                    Some(error) => LiveEnd::new(
                                        end.terminal.reason,
                                        Some(std::sync::Arc::new(super::types::LiveStreamError::new(
                                            error.code,
                                            error.message.clone(),
                                        ))),
                                    ),
                                    None => LiveEnd::new(end.terminal.reason, None),
                                };
                                if end.is_complete() {
                                    self.core.set_pending_end(end);
                                    self.core.set_phase(ConsumerPhase::Draining);
                                    deadlines.begin_draining();
                                    deadlines.data_admitted(tokio::time::Instant::now());
                                } else {
                                    self.core.commit_end(end);
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        })
    }
}

/// Verify one provider-origin frame against the pinned session identity.
///
/// A frame that fails authentication is discarded without influencing state,
/// so injected garbage cannot close a valid session.
fn verify_provider_frame(
    control: &ConsumerControl,
    message: &async_nats::Message,
    frame: &LiveFrame,
) -> bool {
    let headers = message.headers.as_ref();
    let context_digest = headers
        .and_then(|headers| headers.get("authorization-context"))
        .map(ToString::to_string);
    let session_key = headers
        .and_then(|headers| headers.get("session-key"))
        .map(ToString::to_string);
    let proof = headers
        .and_then(|headers| headers.get("trellis-live-proof"))
        .map(ToString::to_string);
    let (Some(context_digest), Some(session_key), Some(proof)) =
        (context_digest, session_key, proof)
    else {
        return false;
    };
    if session_key != control.pinned_session_key
        || session_key != control.pinned_identity.session_key
    {
        return false;
    }
    // The session id binds every frame to this observation.
    if frame.session_id() != control.session_id {
        return false;
    }
    let Ok(proof) = trellis_protocol::LiveServerProof::parse(proof) else {
        return false;
    };
    // The exact provider context is resolved and verified asynchronously by
    // the pump before it inspects frame state; this synchronous pre-check only
    // rejects frames that cannot be authentic at all.
    trellis_protocol::verify_live_server_proof_encoded(
        &proof,
        &context_digest,
        &message.subject,
        &message.payload,
        &session_key,
    )
    .is_ok()
}

static LIVE_INBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
mod tests {
    use super::{verify_offer_claims, verify_offer_identity, ClientOpen};
    use bytes::Bytes;
    use trellis_protocol::{
        LiveOffer, LiveOfferConsumer, LiveOfferKind, LiveOfferLimits, LiveOfferProvider,
        LiveSessionKind, HEARTBEAT_INTERVAL_MS, OPEN_RESERVATION_MS, PEER_INACTIVITY_MS,
        WINDOW_BYTES, WINDOW_FRAMES,
    };

    fn offer(kind: LiveSessionKind, base_subject: &str) -> LiveOffer {
        LiveOffer {
            format: trellis_protocol::LIVE_VERSION.into(),
            kind: LiveOfferKind::Offer,
            session_kind: kind,
            open_id: "open".into(),
            request_id: "req".into(),
            session_id: "session".into(),
            base_subject: base_subject.into(),
            data_subject: "live.v1.data.A.B.C".into(),
            control_subject: format!("{base_subject}.observe.A.C"),
            provider: LiveOfferProvider {
                connection_id: "P".into(),
                session_key: "K".into(),
                principal_id: "pr".into(),
                participant_id: "pa".into(),
                deployment_id: "dep".into(),
                instance_id: "inst".into(),
            },
            consumer: LiveOfferConsumer {
                connection_id: "C".into(),
                session_key: "KC".into(),
                principal_id: "pru".into(),
                participant_id: "pau".into(),
            },
            limits: LiveOfferLimits {
                max_data_body_bytes: 1_044_480,
                window_frames: WINDOW_FRAMES,
                window_bytes: WINDOW_BYTES,
                reservation_ms: OPEN_RESERVATION_MS,
                heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
                peer_inactivity_ms: PEER_INACTIVITY_MS,
                consumer_stall_ms: trellis_protocol::CONSUMER_STALL_MS,
            },
        }
    }

    fn open<'a>(
        kind: LiveSessionKind,
        base_subject: &'a str,
        publish_subject: &'a str,
    ) -> ClientOpen<'a> {
        ClientOpen {
            kind,
            api_id: "api@v1",
            base_subject,
            publish_subject,
            body: Bytes::new(),
            open_id: "open".into(),
            receive_max_payload_bytes: 1024,
            permission: trellis_protocol::PermissionAtom::new(
                trellis_protocol::PermissionTarget::api_surface(
                    "api@v1",
                    trellis_protocol::ApiSurfaceKind::Live,
                    "Watch".to_owned(),
                )
                .expect("api surface"),
                trellis_protocol::PermissionAction::Subscribe,
            )
            .expect("permission"),
        }
    }

    #[test]
    fn live_open_publishes_on_the_same_base_subject() {
        let base = "live.v1.route.Watch";
        let open = open(LiveSessionKind::Standalone, base, base);
        verify_offer_identity(&open, &offer(LiveSessionKind::Standalone, base))
            .expect("live offer");
    }

    #[test]
    fn operation_watch_offer_binds_the_operation_route_not_control() {
        let base = "operation.v1.Billing.Refund";
        let publish = "operation.v1.Billing.Refund.control";
        let open = open(LiveSessionKind::Operation, base, publish);
        verify_offer_identity(&open, &offer(LiveSessionKind::Operation, base))
            .expect("operation watch offer");
    }

    #[test]
    fn operation_watch_rejects_control_subject_as_offer_base() {
        let base = "operation.v1.Billing.Refund";
        let publish = "operation.v1.Billing.Refund.control";
        let open = open(LiveSessionKind::Operation, base, publish);
        let error = verify_offer_identity(&open, &offer(LiveSessionKind::Operation, publish))
            .expect_err("control is not the offer base");
        assert!(error.to_string().contains("base subject"));
    }

    #[test]
    fn operation_watch_rejects_live_session_kind() {
        let base = "operation.v1.Billing.Refund";
        let open = open(
            LiveSessionKind::Operation,
            base,
            "operation.v1.Billing.Refund.control",
        );
        let error = verify_offer_identity(&open, &offer(LiveSessionKind::Standalone, base))
            .expect_err("kind must match");
        assert!(error.to_string().contains("session kind"));
    }

    fn consumer_identity() -> super::PinnedPeerIdentity {
        super::PinnedPeerIdentity {
            connection_id: "C".into(),
            session_key: "consumer".into(),
            principal_id: "pru".into(),
            participant_id: "pau".into(),
            deployment_id: None,
            instance_id: None,
        }
    }

    fn provider_identity() -> super::PinnedPeerIdentity {
        super::PinnedPeerIdentity {
            connection_id: "P".into(),
            session_key: "K".into(),
            principal_id: "pr".into(),
            participant_id: "pa".into(),
            deployment_id: Some("dep".into()),
            instance_id: Some("inst".into()),
        }
    }

    fn signed_offer(kind: LiveSessionKind, base: &str) -> LiveOffer {
        let mut offer = offer(kind, base);
        offer.consumer.session_key = trellis_protocol::encode_subject_token("consumer");
        offer
    }

    #[test]
    fn offer_from_a_non_selected_deployment_is_rejected() {
        let base = "live.v1.route.Watch";
        let open = open(LiveSessionKind::Standalone, base, base);
        let error = verify_offer_claims(
            &open,
            &signed_offer(LiveSessionKind::Standalone, base),
            "dep-other",
            &provider_identity(),
            &consumer_identity(),
            "digest",
        )
        .expect_err("selected deployment must match");
        assert!(error.to_string().contains("selected deployment"));
    }

    #[test]
    fn offer_with_a_mutated_provider_tuple_is_rejected() {
        let base = "live.v1.route.Watch";
        let open = open(LiveSessionKind::Standalone, base, base);
        let mut mutated = provider_identity();
        mutated.instance_id = Some("inst-mutated".into());
        let error = verify_offer_claims(
            &open,
            &signed_offer(LiveSessionKind::Standalone, base),
            "dep",
            &mutated,
            &consumer_identity(),
            "digest",
        )
        .expect_err("mutated provider tuple must be rejected");
        assert!(error.to_string().contains("verified context"));
    }

    #[test]
    fn offer_with_a_mutated_consumer_tuple_is_rejected() {
        let base = "live.v1.route.Watch";
        let open = open(LiveSessionKind::Standalone, base, base);
        let mut mutated = consumer_identity();
        mutated.participant_id = "other".into();
        let error = verify_offer_claims(
            &open,
            &signed_offer(LiveSessionKind::Standalone, base),
            "dep",
            &provider_identity(),
            &mutated,
            "digest",
        )
        .expect_err("mutated consumer tuple must be rejected");
        assert!(error.to_string().contains("opening caller"));
    }

    #[test]
    fn selected_deployment_and_complete_tuples_are_accepted() {
        let base = "live.v1.route.Watch";
        let open = open(LiveSessionKind::Standalone, base, base);
        verify_offer_claims(
            &open,
            &signed_offer(LiveSessionKind::Standalone, base),
            "dep",
            &provider_identity(),
            &consumer_identity(),
            "digest",
        )
        .expect("the selected provider's complete offer is accepted");
    }
}
