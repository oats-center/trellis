use async_nats::{
    header::NATS_LAST_CONSUMER,
    jetstream::{self, stream},
    StatusCode, Subscriber,
};
use futures_util::StreamExt;
use std::{future::Future, pin::Pin, time::Duration};

use super::super::TrellisClientError;
use super::types::AuthorizationRegistryBinding;

pub(crate) const REVOCATION_PREFIX: &str = "revocation.";

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
    initial_boundary: u64,
    delivered: u64,
    initialized: bool,
    last_stream_revision: u64,
    heartbeat_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
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

    /// Acquire the current attachment for one registry operation.
    ///
    /// Returns the lease (held for the operation, or for a watch's whole life so
    /// it never outlives its socket) and the registry stream opened on that
    /// attachment. Without a manager the fixed bootstrap attachment is used, so
    /// runtime-internal callers keep their existing behaviour.
    async fn attachment(
        &self,
    ) -> Result<(Option<crate::client::TransportLease>, stream::Stream<()>), TrellisClientError>
    {
        let (lease, nats) = match &self.manager {
            Some(manager) => {
                let lease = manager
                    .acquire_for(
                        &[],
                        &[],
                        std::time::Instant::now() + Duration::from_secs(30),
                    )
                    .await?;
                let nats = lease.nats().clone();
                (Some(lease), nats)
            }
            None => (None, self.nats.clone()),
        };
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
    ) -> Result<Option<Vec<u8>>, TrellisClientError> {
        validate_digest_key(digest)?;
        // A finite read leases the current attachment for the operation only, so
        // it follows a cutover and never pins a superseded generation.
        let (_lease, contexts) = self.attachment().await?;
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
    ) -> Result<RegistryWatch, TrellisClientError> {
        validate_digest_key(digest)?;
        // The watch holds its attachment lease for the watch's whole life, so it
        // cannot outlive its socket. It is acquired at open on the generation
        // current at that time; moving an existing watch across a later cutover is
        // not performed here.
        let (lease, contexts) = self.attachment().await?;
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
