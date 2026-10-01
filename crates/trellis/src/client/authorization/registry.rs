use async_nats::{
    header::NATS_LAST_CONSUMER,
    jetstream::{self, stream},
    StatusCode, Subscriber,
};
use futures_util::StreamExt;
use std::{future::Future, pin::Pin, time::Duration};

use super::super::TrellisClientError;
use super::own_context::OwnTransitionGuard;
use super::types::AuthorizationRegistryBinding;

pub(crate) const REVOCATION_PREFIX: &str = "revocation.";

/// How one registry operation acquires its physical attachment lease.
///
/// Ordinary maintenance follows the exact published current generation. A scoped
/// own-candidate coverage warm instead carries one already-acquired pinned source,
/// so its cold read and initial revocation watch run on exactly that attachment
/// and never re-select a survivor or a published/default socket.
#[derive(Clone, Debug)]
pub(crate) enum RegistryAttachment {
    /// The exact published current generation (ordinary maintenance).
    Published,
    /// One scoped own-candidate coverage warm pinned to a single already-acquired
    /// attachment under the candidate's own corrected clock.
    PinnedOwnCandidate(PinnedOwnCandidateSource),
}

/// One pinned physical attachment for a scoped own-candidate coverage warm.
///
/// `lease` is the exact already-acquired generation lease, or `None` for a fixed
/// unmanaged bootstrap attachment (which uses the reader's own socket).
/// `clock_offset_ms` is the immutable server-clock offset captured from the exact
/// prepared candidate, so verification recomputes a fresh corrected time at each
/// use and a captured timestamp never freezes across the warm's awaits.
#[derive(Clone)]
pub(crate) struct PinnedOwnCandidateSource {
    lease: Option<crate::client::TransportLease>,
    clock_offset_ms: i64,
}

impl PinnedOwnCandidateSource {
    /// A managed pinned source on one exact already-acquired attachment lease.
    pub(crate) fn pinned(lease: crate::client::TransportLease, clock_offset_ms: i64) -> Self {
        Self {
            lease: Some(lease),
            clock_offset_ms,
        }
    }

    /// The fixed bootstrap attachment used when no generation manager is attached;
    /// the reader's own socket carries the warm. Used only by component tests whose
    /// provider has no generation manager.
    #[cfg(test)]
    pub(crate) fn fixed(clock_offset_ms: i64) -> Self {
        Self {
            lease: None,
            clock_offset_ms,
        }
    }

    /// The candidate's corrected "now" (Unix seconds) at the moment of the call.
    pub(crate) fn corrected_now_seconds(&self) -> Result<i64, TrellisClientError> {
        super::own_context::system_now_millis()?
            .checked_add(self.clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))
            .map(|now| now.div_euclid(1000))
    }

    /// The immutable server-clock offset captured from the exact candidate.
    pub(crate) fn clock_offset_ms(&self) -> i64 {
        self.clock_offset_ms
    }
}

impl std::fmt::Debug for PinnedOwnCandidateSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PinnedOwnCandidateSource")
            .field(
                "generation_id",
                &self.lease.as_ref().map(|lease| lease.generation_id()),
            )
            .field("clock_offset_ms", &self.clock_offset_ms)
            .finish()
    }
}

pub(crate) struct RegistryWatchEntry {
    pub(crate) key: String,
    pub(crate) value: Vec<u8>,
    pub(crate) removed: bool,
    pub(crate) revision: u64,
}

pub(crate) enum RegistryWatchEvent {
    Entry(RegistryWatchEntry),
    Initialized,
}

pub(crate) struct RegistryWatch {
    client: async_nats::Client,
    subscription: Subscriber,
    subject: String,
    key: String,
    /// Attachment lease for this watch's whole life: the watch never outlives its
    /// socket, and releasing the lease lets a superseded generation be reclaimed.
    _lease: Option<crate::client::TransportLease>,
    /// Reader and digest that carry this coverage binding across a transport
    /// replacement make-before-break.
    reader: AuthorizationRegistryReader,
    digest: String,
    initial_boundary: u64,
    delivered: u64,
    initialized: bool,
    last_stream_revision: u64,
    heartbeat_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl RegistryWatch {
    /// Subscribe to current-generation publication changes for this binding.
    pub(crate) fn subscribe_publication(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<Option<u64>>> {
        self.reader.subscribe_publication()
    }

    /// The exact transport generation whose attachment this watch is leased on.
    ///
    /// `None` for a fixed (unmanaged) attachment that never hands over.
    pub(crate) fn generation_id(&self) -> Option<u64> {
        self._lease.as_ref().map(|lease| lease.generation_id())
    }

    /// This binding's registry reader, for a guarded replacement commit.
    pub(crate) fn reader(&self) -> AuthorizationRegistryReader {
        self.reader.clone()
    }

    /// Carry this coverage binding to the exact `expected` published generation.
    ///
    /// The replacement opens and initializes on the exact published attachment
    /// for `expected` **while this authoritative watch stays observed and
    /// leased**: the old watch's next event is selected concurrently with the
    /// owned preparation future and with publication changes, so a genuine
    /// revocation, error or end on the old binding is surfaced immediately, and
    /// a publication superseding `expected` cancels the preparation rather than
    /// waiting for a target that can no longer commit. Only a successor that
    /// reaches its initialization barrier and then commits through `commit` —
    /// which revalidates the authoritative provider binding under the manager's
    /// publication guard — replaces the old binding, releasing the old exact
    /// lease as the old watch is dropped; otherwise the healthy old binding
    /// stays in force. A genuine successor revocation observed before its
    /// initialization barrier is surfaced immediately through the same
    /// digest-global reducer without installing the successor. Dropping the
    /// preparation future destroys any provisional consumer and its exact lease.
    pub(crate) async fn hand_over<F>(&mut self, expected: u64, commit: F) -> CoverageHandover
    where
        F: FnOnce(&mut RegistryWatch, Box<RegistryWatch>) -> bool,
    {
        let reader = self.reader.clone();
        let digest = self.digest.clone();
        let mut publication = self.subscribe_publication();
        // Follow an already-published target immediately; a same-id renewal is a
        // no-op and no published current is never a new target.
        if let Some(receiver) = publication.as_ref() {
            if *receiver.borrow() != Some(expected) {
                return CoverageHandover::Superseded;
            }
        }
        let prepare = prepare_replacement(reader, digest, expected);
        tokio::pin!(prepare);
        tokio::select! {
            // Prefer the authoritative old binding's observation when both are
            // ready: its loss must never be hidden behind successor setup.
            biased;
            event = futures_util::StreamExt::next(&mut *self) => {
                CoverageHandover::Observed(event)
            }
            prepared = &mut prepare => match prepared {
                Ok(PreparedReplacement::Revoked(event)) => {
                    CoverageHandover::Observed(Some(Ok(event)))
                }
                Ok(PreparedReplacement::Ready(successor)) => {
                    if commit(self, successor) {
                        CoverageHandover::Retained
                    } else {
                        CoverageHandover::SetupFailed
                    }
                }
                Err(_) => CoverageHandover::SetupFailed,
            },
            _ = publication_changed(&mut publication) => CoverageHandover::Superseded,
        }
    }
}

/// Wait for the next real publication-identity change on this connection.
///
/// Pending forever when there is no publication receiver, so an unmanaged
/// binding's `select!` simply keeps observing its watch.
pub(crate) async fn publication_changed(
    publication: &mut Option<tokio::sync::watch::Receiver<Option<u64>>>,
) {
    match publication.as_mut() {
        Some(receiver) => {
            let _ = receiver.changed().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Outcome of a make-before-break coverage handover.
///
/// Distinguishes a retained successor from an authoritative observation on the
/// old binding (including its end), and keeps replacement setup failure separate:
/// a failed successor is neither revocation nor global cache loss.
pub(crate) enum CoverageHandover {
    /// The successor reached its initialization barrier and replaced the old
    /// binding; the old exact lease was released and no observation applies.
    Retained,
    /// An authoritative observation for the terminal reducer: a genuine
    /// revocation `Entry` (old binding, or an initialized successor's snapshot),
    /// an old-watch error, or a binding-local end (`None`).
    Observed(Option<Result<RegistryWatchEvent, TrellisClientError>>),
    /// The successor could not be opened or initialized, or its guarded commit
    /// was refused because the target, provider binding, or coverage was no
    /// longer authoritative. The healthy old binding stays in force; this is
    /// neither revocation nor global coverage loss, so the caller may pace a
    /// bounded retry of the same target.
    SetupFailed,
    /// The published current changed away from the target that was being
    /// prepared, or was replaced before preparation started. No provisional
    /// watcher was installed; the caller must recompute the target and need not
    /// wait out a retry interval.
    Superseded,
}

/// Result of preparing a successor coverage watch.
enum PreparedReplacement {
    /// The successor reached its initialization barrier and can be installed.
    Ready(Box<RegistryWatch>),
    /// A genuine revocation `Entry` was observed *before* initialization. It is
    /// digest-global evidence and must reach the reducer immediately, without
    /// awaiting initialization or installing the successor.
    Revoked(RegistryWatchEvent),
}

/// Open a replacement watch on the current attachment and drive it to its
/// initialization barrier.
///
/// Owns the reader and digest so the caller can keep observing the authoritative
/// old watch concurrently. Dropping this future (for example when the old watch
/// yields first) drops any provisional consumer and its exact lease.
async fn prepare_replacement(
    reader: AuthorizationRegistryReader,
    digest: String,
    expected: u64,
) -> Result<PreparedReplacement, TrellisClientError> {
    let mut replacement = reader
        .watch_revocation(&digest, Some(expected), RegistryAttachment::Published)
        .await?;
    match futures_util::StreamExt::next(&mut replacement).await {
        Some(Ok(RegistryWatchEvent::Initialized)) => {
            Ok(PreparedReplacement::Ready(Box::new(replacement)))
        }
        Some(Ok(RegistryWatchEvent::Entry(entry))) => {
            // A genuine revocation observed before initialization is digest-global
            // evidence and is surfaced immediately, without awaiting
            // initialization or installing the successor; unusable provisional
            // evidence is a setup failure that leaves the healthy old binding in
            // force.
            if super::provider_cache::genuine_revocation_at(&entry, &digest).is_some() {
                Ok(PreparedReplacement::Revoked(RegistryWatchEvent::Entry(
                    entry,
                )))
            } else {
                Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization revocation evidence is unusable".into(),
                ))
            }
        }
        Some(Err(error)) => Err(error),
        None => Err(TrellisClientError::AuthorizationUnavailable(
            "authorization revocation watch ended during handoff".into(),
        )),
    }
}
impl futures_util::Stream for RegistryWatch {
    type Item = Result<RegistryWatchEvent, TrellisClientError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if !self.initialized && self.delivered >= self.initial_boundary {
            self.initialized = true;
            return std::task::Poll::Ready(Some(Ok(RegistryWatchEvent::Initialized)));
        }
        loop {
            let heartbeat_timeout = Duration::from_secs(10);
            if self
                .heartbeat_sleep
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(heartbeat_timeout)))
                .as_mut()
                .poll(cx)
                .is_ready()
            {
                self.heartbeat_sleep = None;
                return std::task::Poll::Ready(Some(Err(
                    TrellisClientError::AuthorizationUnavailable(
                        "authorization revocation watch missed heartbeats".into(),
                    ),
                )));
            }
            match self.subscription.poll_next_unpin(cx) {
                std::task::Poll::Ready(Some(message)) => {
                    self.heartbeat_sleep = None;
                    if let Some(status) = message.status {
                        if status == StatusCode::IDLE_HEARTBEAT {
                            if let Some(reply) = message.reply.clone() {
                                let client = self.client.clone();
                                let subject = self.subject.clone();
                                tokio::spawn(async move {
                                    tracing::debug!(%subject, %reply, "publishing authorization revocation watch heartbeat response");
                                    if let Err(error) =
                                        client.publish(reply.clone(), Vec::new().into()).await
                                    {
                                        tracing::warn!(%subject, %reply, %error, "authorization revocation watch heartbeat response publish failed");
                                    }
                                });
                            }
                            let last_consumer_sequence = message
                                .headers
                                .as_ref()
                                .and_then(|headers| headers.get(NATS_LAST_CONSUMER))
                                .map(|value| value.as_str().parse::<u64>())
                                .transpose()
                                .map_err(|error| {
                                    TrellisClientError::AuthorizationUnavailable(format!(
                                        "authorization revocation heartbeat is invalid: {error}"
                                    ))
                                });
                            match last_consumer_sequence {
                                Ok(Some(sequence)) => {
                                    if let Err(error) =
                                        validate_heartbeat_progress(self.delivered, sequence)
                                    {
                                        return std::task::Poll::Ready(Some(Err(error)));
                                    }
                                }
                                Ok(None) if message.reply.is_some() => {}
                                Ok(None) => {
                                    return std::task::Poll::Ready(Some(Err(
                                        TrellisClientError::AuthorizationUnavailable(
                                            "authorization revocation heartbeat is invalid".into(),
                                        ),
                                    )))
                                }
                                Err(error) => return std::task::Poll::Ready(Some(Err(error))),
                            }
                            continue;
                        }
                        return std::task::Poll::Ready(Some(Err(
                            TrellisClientError::AuthorizationUnavailable(format!(
                                "authorization revocation watch returned status {status}"
                            )),
                        )));
                    }
                    if message.subject.as_str() != self.subject {
                        return std::task::Poll::Ready(Some(Err(
                            TrellisClientError::AuthorizationUnavailable(
                                "authorization watch subject is invalid".into(),
                            ),
                        )));
                    };
                    let (stream_sequence, consumer_sequence) = match message
                        .reply
                        .as_deref()
                        .ok_or("missing reply metadata")
                        .and_then(parse_delivery_sequences)
                    {
                        Ok(info) => info,
                        Err(error) => {
                            return std::task::Poll::Ready(Some(Err(
                                TrellisClientError::AuthorizationUnavailable(format!(
                                    "authorization revocation metadata is invalid: {error}"
                                )),
                            )))
                        }
                    };
                    if consumer_sequence != self.delivered.saturating_add(1)
                        || stream_sequence <= self.last_stream_revision
                    {
                        return std::task::Poll::Ready(Some(Err(
                            TrellisClientError::AuthorizationUnavailable(
                                "authorization revocation watch sequence gap".into(),
                            ),
                        )));
                    }
                    self.delivered = consumer_sequence;
                    self.last_stream_revision = stream_sequence;
                    let removed = match message
                        .headers
                        .as_ref()
                        .and_then(|headers| headers.get("KV-Operation"))
                        .map(|value| value.as_str())
                        .unwrap_or("PUT")
                    {
                        "PUT" => false,
                        "DEL" | "PURGE" => true,
                        _ => {
                            return std::task::Poll::Ready(Some(Err(
                                TrellisClientError::AuthorizationUnavailable(
                                    "authorization revocation operation is invalid".into(),
                                ),
                            )))
                        }
                    };
                    return std::task::Poll::Ready(Some(Ok(RegistryWatchEvent::Entry(
                        RegistryWatchEntry {
                            key: self.key.clone(),
                            value: message.payload.to_vec(),
                            removed,
                            revision: stream_sequence,
                        },
                    ))));
                }
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

fn validate_heartbeat_progress(
    delivered: u64,
    last_consumer_sequence: u64,
) -> Result<(), TrellisClientError> {
    if last_consumer_sequence > delivered {
        return Err(TrellisClientError::AuthorizationUnavailable(
            "authorization revocation watch sequence gap".into(),
        ));
    }
    Ok(())
}

fn parse_delivery_sequences(reply: &str) -> Result<(u64, u64), &'static str> {
    let tokens = reply
        .strip_prefix("$JS.ACK.")
        .ok_or("invalid reply metadata")?
        .split('.')
        .collect::<Vec<_>>();
    let (stream, consumer) = if tokens.len() >= 9 {
        (tokens.get(5), tokens.get(6))
    } else if tokens.len() == 7 {
        (tokens.get(3), tokens.get(4))
    } else {
        return Err("invalid reply metadata");
    };
    Ok((
        stream
            .ok_or("invalid reply metadata")?
            .parse()
            .map_err(|_| "invalid reply metadata")?,
        consumer
            .ok_or("invalid reply metadata")?
            .parse()
            .map_err(|_| "invalid reply metadata")?,
    ))
}

/// Exact context/revocation reads and watches on the server-assigned registry.
#[derive(Clone)]
pub(crate) struct AuthorizationRegistryReader {
    nats: async_nats::Client,
    binding: AuthorizationRegistryBinding,
    /// The connection's generation manager when attached. Registry I/O then
    /// acquires the current generation's attachment, so reads and watches follow
    /// a transport cutover instead of pinning the initial connection.
    manager: Option<crate::client::TransportGenerationManager>,
}

impl AuthorizationRegistryReader {
    pub(crate) async fn open(
        nats: async_nats::Client,
        binding: &AuthorizationRegistryBinding,
        manager: Option<crate::client::TransportGenerationManager>,
    ) -> Result<Self, TrellisClientError> {
        if binding.context_bucket.trim().is_empty() {
            return Err(TrellisClientError::Bootstrap(
                "authorization registry bucket is empty".into(),
            ));
        }
        Ok(Self {
            nats,
            binding: binding.clone(),
            manager,
        })
    }

    /// Subscribe to the connection's publication-identity changes.
    ///
    /// Carries the published current generation id (`None` when there is none)
    /// and changes only on a real identity change. `None` for a fixed
    /// (unmanaged) attachment, which never hands over.
    pub(crate) fn subscribe_publication(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<Option<u64>>> {
        self.manager
            .as_ref()
            .map(crate::client::TransportGenerationManager::subscribe_publication)
    }

    /// Run a guarded coverage commit against the exact published current generation.
    ///
    /// Delegates to the connection's generation manager so a caller can
    /// revalidate its authoritative provider binding and perform its synchronous
    /// binding swap while the manager's publication guard is held. Without a
    /// manager there is no publication to guard, so replacement is unavailable.
    pub(crate) fn commit_published_if_current(
        &self,
        transition: &OwnTransitionGuard<'_>,
        expected: u64,
        commit: impl FnOnce() -> bool,
    ) -> Result<bool, TrellisClientError> {
        self.manager
            .as_ref()
            .ok_or_else(|| {
                TrellisClientError::TransportUnavailable(
                    "coverage replacement requires a transport generation manager".into(),
                )
            })?
            .commit_published_if_current(transition, expected, commit)
    }

    /// Whether the logical transport connection has latched a terminal cause.
    ///
    /// A terminal connection can never adopt another generation, so coverage
    /// replacement stops retrying while the authoritative binding is still
    /// observed to its own loss. `false` for a fixed (unmanaged) attachment.
    pub(crate) fn transport_terminal(&self) -> bool {
        self.manager
            .as_ref()
            .is_some_and(|manager| manager.terminal().is_some())
    }

    /// Acquire the exact attachment for one registry operation.
    ///
    /// Ordinary maintenance uses the manager's internal
    /// [`TransportGenerationManager::acquire_published`] — the exact published
    /// current generation, never a draining substitute, and with no CONNECT
    /// routing-credential validation — because this is maintenance on an
    /// already-admitted socket. A scoped own-candidate warm instead carries one
    /// already-acquired [`RegistryAttachment::PinnedOwnCandidate`] source and is
    /// used exactly as given: no survivor is re-selected and no published/default
    /// socket is re-acquired. Without a manager the fixed bootstrap attachment is
    /// used, so runtime-internal callers keep their existing behaviour.
    ///
    /// When `expected` is set the acquired lease must be exactly that published
    /// generation, validated before any registry or consumer IO: a superseded
    /// target is refused rather than silently served by a substitute attachment.
    async fn attachment(
        &self,
        expected: Option<u64>,
        attachment: RegistryAttachment,
    ) -> Result<(Option<crate::client::TransportLease>, stream::Stream<()>), TrellisClientError>
    {
        let (lease, nats) = match &attachment {
            RegistryAttachment::Published => match &self.manager {
                Some(manager) => {
                    let lease = manager.acquire_published()?;
                    let nats = lease.nats().clone();
                    (Some(lease), nats)
                }
                None => (None, self.nats.clone()),
            },
            RegistryAttachment::PinnedOwnCandidate(source) => {
                let nats = source
                    .lease
                    .as_ref()
                    .map(|lease| lease.nats().clone())
                    .unwrap_or_else(|| self.nats.clone());
                (source.lease.clone(), nats)
            }
        };
        if let (Some(expected), Some(lease)) = (expected, lease.as_ref()) {
            if lease.generation_id() != expected {
                return Err(TrellisClientError::TransportUnavailable(
                    "authorization registry target is no longer the published current attachment"
                        .into(),
                ));
            }
        }
        let contexts = jetstream::new(nats)
            .get_stream_no_info(format!("KV_{}", self.binding.context_bucket))
            .await
            .map_err(|error| {
                TrellisClientError::AuthorizationUnavailable(format!(
                    "cannot open authorization registry: {error}"
                ))
            })?;
        Ok((lease, contexts))
    }

    pub(crate) async fn get_context(
        &self,
        digest: &str,
        attachment: RegistryAttachment,
    ) -> Result<Option<Vec<u8>>, TrellisClientError> {
        validate_digest_key(digest)?;
        // A finite read leases the current attachment for the operation only, so
        // it follows a cutover and never pins a superseded generation.
        let (_lease, contexts) = self.attachment(None, attachment).await?;
        let subject = format!("$KV.{}.{digest}", self.binding.context_bucket);
        match contexts.direct_get_last_for_subject(&subject).await {
            Ok(message)
                if message
                    .headers
                    .get("KV-Operation")
                    .is_none_or(|value| value.as_str() == "PUT") =>
            {
                Ok(Some(message.payload.to_vec()))
            }
            Ok(_) => Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context was removed".into(),
            )),
            Err(error) if matches!(error.kind(), stream::DirectGetErrorKind::NotFound) => Ok(None),
            Err(error) => Err(TrellisClientError::AuthorizationUnavailable(format!(
                "cannot read authorization context: {error}"
            ))),
        }
    }

    pub(crate) async fn watch_revocation(
        &self,
        digest: &str,
        expected: Option<u64>,
        attachment: RegistryAttachment,
    ) -> Result<RegistryWatch, TrellisClientError> {
        validate_digest_key(digest)?;
        // The watch holds its attachment lease for the watch's whole life, so it
        // cannot outlive its socket. It is acquired on the generation current at
        // that time; moving an existing watch across a later cutover is performed
        // by `hand_over`, which validates the exact `expected` lease before any
        // consumer IO.
        let (lease, contexts) = self.attachment(expected, attachment).await?;
        let nats = lease
            .as_ref()
            .map(|lease| lease.nats().clone())
            .unwrap_or_else(|| self.nats.clone());
        let subject = format!(
            "$KV.{}.{REVOCATION_PREFIX}{digest}",
            self.binding.context_bucket
        );
        let key = format!("{REVOCATION_PREFIX}{digest}");
        let deliver_subject = nats.new_inbox();
        let consumer_name = format!(
            "TrellisAuth{}",
            deliver_subject.rsplit('.').next().unwrap_or_default()
        );
        let mut consumer = contexts
            .create_consumer(jetstream::consumer::push::Config {
                deliver_subject: deliver_subject.clone(),
                name: Some(consumer_name),
                filter_subject: subject.clone(),
                deliver_policy: jetstream::consumer::DeliverPolicy::LastPerSubject,
                ack_policy: jetstream::consumer::AckPolicy::None,
                flow_control: true,
                idle_heartbeat: Duration::from_secs(5),
                inactive_threshold: Duration::from_secs(10),
                num_replicas: 1,
                memory_storage: true,
                ..Default::default()
            })
            .await
            .map_err(|error| {
                TrellisClientError::AuthorizationUnavailable(format!(
                    "cannot create authorization revocation watch: {error}"
                ))
            })?;
        let subscription = nats.subscribe(deliver_subject).await.map_err(|error| {
            TrellisClientError::AuthorizationUnavailable(format!(
                "cannot consume authorization revocation watch: {error}"
            ))
        })?;
        nats.flush().await.map_err(|error| {
            TrellisClientError::AuthorizationUnavailable(format!(
                "cannot establish authorization revocation watch: {error}"
            ))
        })?;
        let info = consumer.info().await.map_err(|error| {
            TrellisClientError::AuthorizationUnavailable(format!(
                "cannot inspect authorization revocation watch: {error}"
            ))
        })?;
        Ok(RegistryWatch {
            client: nats,
            subscription,
            subject,
            key,
            _lease: lease,
            reader: self.clone(),
            digest: digest.to_owned(),
            initial_boundary: info
                .delivered
                .consumer_sequence
                .saturating_add(info.num_pending),
            delivered: 0,
            initialized: false,
            last_stream_revision: 0,
            heartbeat_sleep: None,
        })
    }
}

pub(crate) fn validate_digest_key(digest: &str) -> Result<(), TrellisClientError> {
    if digest.len() != 43
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(TrellisClientError::Bootstrap(
            "authorization context digest is invalid".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_delivery_sequences, validate_heartbeat_progress};

    #[test]
    fn heartbeat_progress_requires_every_prior_delivery() {
        assert!(validate_heartbeat_progress(2, 1).is_ok());
        assert!(validate_heartbeat_progress(2, 2).is_ok());
        assert!(validate_heartbeat_progress(2, 3).is_err());
    }

    #[test]
    fn delivery_metadata_supports_current_and_legacy_replies() {
        assert_eq!(
            parse_delivery_sequences("$JS.ACK._.account.stream.consumer.1.8.3.1.0"),
            Ok((8, 3))
        );
        assert_eq!(
            parse_delivery_sequences("$JS.ACK.stream.consumer.1.8.3.1.0"),
            Ok((8, 3))
        );
        assert!(parse_delivery_sequences("$JS.ACK.invalid").is_err());
    }
}
