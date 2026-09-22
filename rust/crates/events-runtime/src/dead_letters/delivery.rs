use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream, AckKind};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use trellis_rs::service::{EventVerificationFailure, ServerError};

use super::{DeadLetterJournal, JournalError, OriginalEvent, ReplayEnvelope, REPLAY_STREAM};
use crate::{EventAuthorizationInput, EventVerifier, EventsStore};

const ADVISORY_SUBJECTS: &str = "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.>";
const ADVISORY_DURABLE: &str = "events-advisories";

/// Resolved Core-owned Consumer binding used to classify delivery reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerBinding {
    /// Exact logical Consumer resource identity.
    pub resource_id: String,
    /// Grant owner kind used by the stable resource identity.
    pub owner_kind: String,
    /// Grant owner identity used by the stable resource identity.
    pub owner_id: String,
    /// Participant that owns the Consumer.
    pub participant_id: String,
    /// Participant-local Consumer resource name.
    pub local_name: String,
    /// Original event stream.
    pub stream: String,
    /// Original durable Consumer name.
    pub consumer_name: String,
    /// Exact domain event subjects selected by the Consumer.
    pub filter_subjects: Vec<String>,
    /// Replay durable Consumer name.
    pub replay_consumer_name: String,
}

/// Core resource-catalog lookup used before interpreting exhaustion evidence.
pub trait ConsumerBindingResolver: Send + Sync {
    /// Resolve one exact logical Consumer resource.
    fn by_resource<'a>(
        &'a self,
        resource_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>>;

    /// Resolve a physical stream/durable pair, returning `None` for Jobs or external consumers.
    fn by_consumer<'a>(
        &'a self,
        stream: &'a str,
        consumer_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>>;

    /// List active event Consumer bindings for live/catalog correlation.
    fn all(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ConsumerBinding>, String>> + Send + '_>>;
}

/// Participant-reported delivery outcome.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryOutcome {
    /// Delivery exhausted its retry budget.
    Exhausted,
    /// A targeted replay succeeded.
    Succeeded,
    /// A targeted replay cannot verify its original publisher context.
    Unreplayable,
}

/// Runtime backend input for `Consumers.ReportDelivery`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryReport {
    /// Exact Consumer resource identity.
    pub resource_id: String,
    /// Stream containing the reported delivery.
    pub source_stream: String,
    /// Stream sequence containing the reported delivery.
    pub source_sequence: u64,
    /// Broker delivery count.
    pub delivery_count: u64,
    /// Broker-issued JetStream acknowledgement subject for this exact delivery.
    pub delivery_proof: Option<String>,
    /// Replay generation, present only for targeted replay.
    pub replay_generation: Option<u64>,
    /// Terminal outcome.
    pub outcome: DeliveryOutcome,
    /// Bounded textual handler failure detail.
    pub error: Option<String>,
}

/// Result of accepting or safely ignoring one delivery report.
pub struct DeliveryReportResult {
    /// Current authoritative dead-letter state.
    pub transition: super::DeadLetterTransition,
    /// Whether the report described an already-finished replay generation.
    pub stale: bool,
}

/// Errors returned while validating and recording delivery outcomes.
#[derive(Debug, thiserror::Error)]
pub enum DeliveryReportError {
    /// The referenced Consumer or source message is unavailable.
    #[error("delivery evidence not found: {0}")]
    NotFound(String),
    /// The report contradicts its authoritative binding or replay envelope.
    #[error("invalid delivery report: {0}")]
    Validation(String),
    /// The report conflicts with current dead-letter lifecycle state.
    #[error("delivery report conflict: {0}")]
    Conflict(String),
    /// Authoritative storage or verification is temporarily unavailable.
    #[error("delivery reporting unavailable: {0}")]
    Unavailable(String),
}

/// Durable backend shared by participant reports and broker exhaustion advisories.
#[derive(Clone)]
pub struct DeliveryReporter {
    client: async_nats::Client,
    journal: DeadLetterJournal,
    resolver: Arc<dyn ConsumerBindingResolver>,
    verifier: EventVerifier,
}

impl std::fmt::Debug for DeliveryReporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeliveryReporter")
            .finish_non_exhaustive()
    }
}

impl DeliveryReporter {
    /// Construct a report backend over runtime-owned dependencies.
    pub fn new(
        client: async_nats::Client,
        journal: DeadLetterJournal,
        resolver: Arc<dyn ConsumerBindingResolver>,
        verifier: EventVerifier,
    ) -> Self {
        Self {
            client,
            journal,
            resolver,
            verifier,
        }
    }

    /// Persist a report after exact Consumer `Consume` authorization.
    pub async fn report(
        &self,
        authorized_resource_id: &str,
        report: DeliveryReport,
    ) -> Result<DeliveryReportResult, DeliveryReportError> {
        if authorized_resource_id != report.resource_id {
            return Err(DeliveryReportError::Validation(
                "report is not authorized for this Consumer".to_owned(),
            ));
        }
        let binding = self
            .resolver
            .by_resource(&report.resource_id)
            .await
            .map_err(DeliveryReportError::Unavailable)?
            .ok_or_else(|| DeliveryReportError::NotFound(report.resource_id.clone()))?;
        if report.source_stream != binding.stream && report.source_stream != REPLAY_STREAM {
            return Err(DeliveryReportError::Validation(
                "report does not match the Consumer stream".to_owned(),
            ));
        }
        self.verify_delivery(&binding, &report).await?;
        self.report_bound(binding, report).await
    }

    async fn report_bound(
        &self,
        binding: ConsumerBinding,
        report: DeliveryReport,
    ) -> Result<DeliveryReportResult, DeliveryReportError> {
        if report.source_stream == REPLAY_STREAM {
            return self.report_replay(binding, report).await;
        }
        if report.outcome != DeliveryOutcome::Exhausted
            || report.replay_generation.is_some()
            || report.source_stream != binding.stream
        {
            return Err(DeliveryReportError::Validation(
                "report does not match the original Consumer binding".to_owned(),
            ));
        }
        let original = self
            .original(&report.source_stream, report.source_sequence)
            .await?;
        if !binding
            .filter_subjects
            .iter()
            .any(|filter| subject_matches(filter, &original.subject))
        {
            return Err(DeliveryReportError::Validation(
                "source is not selected by the Consumer".to_owned(),
            ));
        }
        self.journal
            .create_dead(
                binding.resource_id,
                original,
                report.delivery_count,
                bounded_error(report.error),
            )
            .await
            .map(|(transition, _)| DeliveryReportResult {
                transition,
                stale: false,
            })
            .map_err(delivery_journal_error)
    }

    async fn verify_delivery(
        &self,
        binding: &ConsumerBinding,
        report: &DeliveryReport,
    ) -> Result<(), DeliveryReportError> {
        let consumer_name = if report.source_stream == REPLAY_STREAM {
            &binding.replay_consumer_name
        } else {
            &binding.consumer_name
        };
        let stream = jetstream::new(self.client.clone())
            .get_stream(&report.source_stream)
            .await
            .map_err(|error| DeliveryReportError::Unavailable(error.to_string()))?;
        let mut consumer = stream
            .get_consumer::<consumer::pull::Config>(consumer_name)
            .await
            .map_err(|error| DeliveryReportError::Unavailable(error.to_string()))?;
        let info = consumer
            .info()
            .await
            .map_err(|error| DeliveryReportError::Unavailable(error.to_string()))?;
        let delivery_proof = report.delivery_proof.as_deref().ok_or_else(|| {
            DeliveryReportError::Validation("delivery proof is required".to_owned())
        })?;
        let proof = parse_delivery_proof(delivery_proof)?;
        if proof.stream != report.source_stream
            || proof.consumer != *consumer_name
            || proof.stream_sequence != report.source_sequence
            || proof.delivery_count != report.delivery_count
            || report.delivery_count == 0
            || (info.config.max_deliver > 0
                && report.delivery_count > info.config.max_deliver as u64)
            || (report.outcome == DeliveryOutcome::Exhausted
                && info.config.max_deliver > 0
                && report.delivery_count != info.config.max_deliver as u64)
        {
            return Err(DeliveryReportError::Validation(
                "report does not match the current Consumer delivery".to_owned(),
            ));
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            self.client
                .request(delivery_proof.to_owned(), AckKind::Progress.into()),
        )
        .await
        .map_err(|_| {
            DeliveryReportError::Validation(
                "delivery proof is no longer pending at the broker".to_owned(),
            )
        })?
        .map_err(|error| DeliveryReportError::Validation(error.to_string()))?;
        Ok(())
    }

    async fn report_replay(
        &self,
        binding: ConsumerBinding,
        report: DeliveryReport,
    ) -> Result<DeliveryReportResult, DeliveryReportError> {
        let replay_stream = jetstream::new(self.client.clone())
            .get_stream(REPLAY_STREAM)
            .await
            .map_err(|error| DeliveryReportError::Unavailable(error.to_string()))?;
        let raw = replay_stream
            .get_raw_message(report.source_sequence)
            .await
            .map_err(|error| {
                if matches!(error.kind(), stream::RawMessageErrorKind::NoMessageFound) {
                    DeliveryReportError::NotFound(report.source_sequence.to_string())
                } else {
                    DeliveryReportError::Unavailable(error.to_string())
                }
            })?;
        let envelope = serde_json::from_slice::<ReplayEnvelope>(&raw.payload)
            .map_err(|error| DeliveryReportError::Validation(error.to_string()))?;
        if envelope.resource_id != binding.resource_id
            || Some(envelope.generation) != report.replay_generation
            || raw.subject.as_str()
                != super::replay::replay_subject(
                    &binding.resource_id,
                    &envelope.dead_letter_id,
                    envelope.generation,
                )
        {
            return Err(DeliveryReportError::Validation(
                "report does not match the replay generation".to_owned(),
            ));
        }
        if let Some((current, _)) = self
            .journal
            .latest(&envelope.dead_letter_id)
            .await
            .map_err(delivery_journal_error)?
        {
            if current.generation > envelope.generation
                || (current.generation == envelope.generation
                    && matches!(
                        current.state,
                        super::DeadLetterState::Dead
                            | super::DeadLetterState::Resolved
                            | super::DeadLetterState::Dismissed
                    ))
            {
                return Ok(DeliveryReportResult {
                    transition: current,
                    stale: true,
                });
            }
        }
        self.journal
            .report_replay_outcome(
                &envelope.dead_letter_id,
                envelope.generation,
                report.outcome == DeliveryOutcome::Succeeded,
                report.delivery_count,
                bounded_error(report.error),
            )
            .await
            .map(|(transition, _)| DeliveryReportResult {
                transition,
                stale: false,
            })
            .map_err(delivery_journal_error)
    }

    async fn original(
        &self,
        stream_name: &str,
        sequence: u64,
    ) -> Result<OriginalEvent, DeliveryReportError> {
        let event_stream = jetstream::new(self.client.clone())
            .get_stream(stream_name)
            .await
            .map_err(|error| DeliveryReportError::Unavailable(error.to_string()))?;
        let raw = event_stream
            .get_raw_message(sequence)
            .await
            .map_err(|error| {
                if matches!(error.kind(), stream::RawMessageErrorKind::NoMessageFound) {
                    DeliveryReportError::NotFound(sequence.to_string())
                } else {
                    DeliveryReportError::Unavailable(error.to_string())
                }
            })?;
        let headers = raw
            .headers
            .iter()
            .map(|(name, values)| {
                (
                    name.to_string(),
                    values
                        .iter()
                        .map(|value| value.as_str().to_owned())
                        .collect(),
                )
            })
            .collect();
        let header = |name: &str| {
            raw.headers
                .get(name)
                .map(|value| value.as_str().to_owned())
                .filter(|value| !value.is_empty())
        };
        let verification_status = match (
            header("session-key"),
            header("proof"),
            header("authorization-context"),
            header("Nats-Msg-Id"),
            header("Trellis-Event-Time"),
            header("Trellis-Event-Descriptor"),
        ) {
            (
                Some(session_key),
                Some(proof),
                Some(context),
                Some(event_id),
                Some(event_time),
                Some(descriptor_identity),
            ) => {
                match (self.verifier)(EventAuthorizationInput {
                    subject: raw.subject.to_string(),
                    descriptor_identity,
                    payload: raw.payload.to_vec(),
                    session_key,
                    proof,
                    authorization_context: context,
                    event_id,
                    event_time,
                })
                .await
                {
                    Ok(_) => "verified".to_owned(),
                    Err(EventVerificationFailure::Rejected(error)) => format!("rejected:{error}"),
                    Err(EventVerificationFailure::Retryable(error)) => {
                        return Err(DeliveryReportError::Unavailable(error));
                    }
                }
            }
            _ => "malformed".to_owned(),
        };
        let (api_id, event_name) = resolve_event_identity(&raw.subject);
        Ok(OriginalEvent {
            stream: stream_name.to_owned(),
            sequence,
            event_id: header("Nats-Msg-Id"),
            subject: raw.subject.to_string(),
            payload_bytes: raw.payload.to_vec(),
            headers,
            api_id,
            event_name,
            context_digest: header("authorization-context"),
            verification_status,
        })
    }
}

struct DeliveryProof {
    stream: String,
    consumer: String,
    delivery_count: u64,
    stream_sequence: u64,
}

fn parse_delivery_proof(subject: &str) -> Result<DeliveryProof, DeliveryReportError> {
    let tokens = subject
        .strip_prefix("$JS.ACK.")
        .ok_or_else(|| DeliveryReportError::Validation("invalid delivery proof".to_owned()))?
        .split('.')
        .collect::<Vec<_>>();
    let offset = if tokens.len() >= 9 {
        2
    } else if tokens.len() == 7 {
        0
    } else {
        return Err(DeliveryReportError::Validation(
            "invalid delivery proof".to_owned(),
        ));
    };
    let parse = |index: usize| {
        tokens[index]
            .parse::<u64>()
            .map_err(|_| DeliveryReportError::Validation("invalid delivery proof".to_owned()))
    };
    Ok(DeliveryProof {
        stream: tokens[offset].to_owned(),
        consumer: tokens[offset + 1].to_owned(),
        delivery_count: parse(offset + 2)?,
        stream_sequence: parse(offset + 3)?,
    })
}

/// Parsed JetStream max-deliveries advisory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct MaxDeliveriesAdvisory {
    /// Broker advisory schema identity.
    #[serde(rename = "type")]
    pub advisory_type: String,
    /// Broker advisory instance identity.
    pub id: String,
    /// Source stream.
    pub stream: String,
    /// Source durable Consumer.
    pub consumer: String,
    /// Source stream sequence.
    #[serde(rename = "stream_seq", alias = "streamSeq")]
    pub stream_sequence: u64,
    /// Observed delivery count.
    #[serde(alias = "num_deliveries")]
    pub deliveries: u64,
    /// Broker advisory timestamp.
    pub timestamp: String,
}

const MAX_DELIVERIES_ADVISORY_TYPE: &str = "io.nats.jetstream.advisory.v1.max_deliver";

fn valid_advisory(subject: &str, advisory: &MaxDeliveriesAdvisory) -> bool {
    advisory.advisory_type == MAX_DELIVERIES_ADVISORY_TYPE
        && !advisory.id.is_empty()
        && advisory.stream_sequence > 0
        && advisory.deliveries > 0
        && time::OffsetDateTime::parse(
            &advisory.timestamp,
            &time::format_description::well_known::Rfc3339,
        )
        .is_ok()
        && subject
            == format!(
                "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.{}.{}",
                advisory.stream, advisory.consumer
            )
}

/// Handle for durable Consumer exhaustion advisory processing.
pub struct ExhaustionAdvisoryHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl ExhaustionAdvisoryHandle {
    /// Stop advisory processing.
    pub async fn stop(self) {
        if let Some(task) = self.task {
            task.abort();
            let _ = task.await;
        }
    }

    /// Wait for advisory processing to terminate.
    pub async fn wait(&mut self) -> Result<(), ServerError> {
        match self.task.as_mut() {
            Some(task) => task
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?,
            None => Ok(()),
        }
    }
}

/// Start the Events-owned durable advisory consumer on the shared capture stream.
pub async fn start_exhaustion_advisory_loop(
    client: async_nats::Client,
    capture_stream: &str,
    reporter: DeliveryReporter,
    events_store: EventsStore,
) -> Result<ExhaustionAdvisoryHandle, ServerError> {
    let stream = jetstream::new(client)
        .get_stream(capture_stream)
        .await
        .map_err(nats)?;
    let consumer = stream
        .get_or_create_consumer(
            ADVISORY_DURABLE,
            consumer::pull::Config {
                durable_name: Some(ADVISORY_DURABLE.to_owned()),
                filter_subject: ADVISORY_SUBJECTS.to_owned(),
                deliver_policy: consumer::DeliverPolicy::All,
                ack_policy: consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .map_err(nats)?;
    let task = tokio::spawn(async move {
        loop {
            let mut messages = consumer
                .fetch()
                .max_messages(100)
                .expires(Duration::from_millis(500))
                .messages()
                .await
                .map_err(nats)?;
            while let Some(message) = messages.next().await {
                let message = message.map_err(nats)?;
                let advisory =
                    match serde_json::from_slice::<MaxDeliveriesAdvisory>(&message.payload) {
                        Ok(advisory) => advisory,
                        Err(_) => {
                            message.ack().await.map_err(nats)?;
                            continue;
                        }
                    };
                if !valid_advisory(message.subject.as_str(), &advisory) {
                    message.ack().await.map_err(nats)?;
                    continue;
                }
                let binding = match reporter
                    .resolver
                    .by_consumer(&advisory.stream, &advisory.consumer)
                    .await
                {
                    Ok(Some(binding)) => binding,
                    Ok(None) => {
                        message.ack().await.map_err(nats)?;
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "retrying unclassified Consumer exhaustion advisory");
                        message
                            .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                            .await
                            .map_err(nats)?;
                        continue;
                    }
                };
                let replay_generation = if binding.replay_consumer_name == advisory.consumer {
                    match replay_generation(&reporter.client, advisory.stream_sequence).await {
                        Ok(generation) => generation,
                        Err(error) => {
                            tracing::warn!(%error, "retrying replay exhaustion advisory");
                            message
                                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                                .await
                                .map_err(nats)?;
                            continue;
                        }
                    }
                } else {
                    None
                };
                let report = DeliveryReport {
                    resource_id: binding.resource_id.clone(),
                    source_stream: advisory.stream,
                    source_sequence: advisory.stream_sequence,
                    delivery_count: advisory.deliveries,
                    delivery_proof: None,
                    replay_generation,
                    outcome: DeliveryOutcome::Exhausted,
                    error: Some("broker max deliveries exceeded".to_owned()),
                };
                match reporter.report_bound(binding, report).await {
                    Ok(_) => message.ack().await.map_err(nats)?,
                    Err(DeliveryReportError::Unavailable(error)) => {
                        tracing::warn!(%error, "retrying Consumer exhaustion advisory");
                        message
                            .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                            .await
                            .map_err(nats)?;
                    }
                    Err(DeliveryReportError::NotFound(_)) => {
                        events_store
                            .record_retention_gap(advisory.stream_sequence)
                            .map_err(|error| ServerError::Nats(error.to_string()))?;
                        tracing::error!(
                            stream_sequence = advisory.stream_sequence,
                            "Consumer exhaustion source expired; Events coverage is incomplete"
                        );
                        message.ack().await.map_err(nats)?;
                    }
                    Err(error) => {
                        tracing::error!(%error, "discarding invalid Consumer exhaustion advisory");
                        message.ack().await.map_err(nats)?;
                    }
                }
            }
        }
    });
    Ok(ExhaustionAdvisoryHandle { task: Some(task) })
}

fn bounded_error(error: Option<String>) -> Option<String> {
    error.map(|error| error.chars().take(4096).collect())
}

async fn replay_generation(
    client: &async_nats::Client,
    sequence: u64,
) -> Result<Option<u64>, ServerError> {
    let stream = jetstream::new(client.clone())
        .get_stream(REPLAY_STREAM)
        .await
        .map_err(nats)?;
    let raw = stream.get_raw_message(sequence).await.map_err(nats)?;
    serde_json::from_slice::<ReplayEnvelope>(&raw.payload)
        .map(|envelope| Some(envelope.generation))
        .map_err(nats)
}

fn delivery_journal_error(error: JournalError) -> DeliveryReportError {
    match error {
        JournalError::NotFound(id) => DeliveryReportError::NotFound(id),
        JournalError::Conflict(details) => DeliveryReportError::Conflict(details),
        JournalError::UnreplayableOriginal => {
            DeliveryReportError::Conflict("original evidence is unavailable".to_owned())
        }
        other => DeliveryReportError::Unavailable(other.to_string()),
    }
}

fn resolve_event_identity(subject: &str) -> (Option<String>, Option<String>) {
    use base64::Engine as _;

    let mut tokens = subject.split('.');
    if tokens.next() != Some("events") || tokens.next() != Some("v1") {
        return (None, None);
    }
    let api_id = tokens.next().and_then(|token| {
        String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(token)
                .ok()?,
        )
        .ok()
    });
    (api_id, tokens.next().map(str::to_owned))
}

fn subject_matches(pattern: &str, subject: &str) -> bool {
    let mut subject = subject.split('.');
    for token in pattern.split('.') {
        if token == ">" {
            return subject.next().is_some();
        }
        let Some(actual) = subject.next() else {
            return false;
        };
        if token != "*" && token != actual {
            return false;
        }
    }
    subject.next().is_none()
}

fn nats(error: impl std::fmt::Display) -> ServerError {
    ServerError::Nats(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_event_identity, subject_matches, valid_advisory, MaxDeliveriesAdvisory,
        MAX_DELIVERIES_ADVISORY_TYPE,
    };

    #[test]
    fn advisory_evidence_matches_its_broker_subject() {
        let advisory = MaxDeliveriesAdvisory {
            advisory_type: MAX_DELIVERIES_ADVISORY_TYPE.to_owned(),
            id: "advisory-1".to_owned(),
            stream: "trellis".to_owned(),
            consumer: "consumer-1".to_owned(),
            stream_sequence: 42,
            deliveries: 6,
            timestamp: "2026-03-28T12:05:00Z".to_owned(),
        };
        assert!(valid_advisory(
            "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.trellis.consumer-1",
            &advisory
        ));
        assert!(!valid_advisory(
            "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.trellis.other",
            &advisory
        ));
    }

    #[test]
    fn consumer_subject_match_is_token_aware() {
        assert!(subject_matches(
            "events.v1.orders.*",
            "events.v1.orders.Created"
        ));
        assert!(subject_matches(
            "events.v1.orders.>",
            "events.v1.orders.Created.eu"
        ));
        assert!(!subject_matches("events.v1.orders.>", "events.v1.orders"));
        assert!(!subject_matches(
            "events.v1.orders.*",
            "events.v1.orders.Created.eu"
        ));
        assert!(!subject_matches(
            "events.v1.order.>",
            "events.v1.orders.Created"
        ));
    }

    #[test]
    fn event_identity_decodes_the_qualified_api_token() {
        assert_eq!(
            resolve_event_identity("events.v1.YWNtZS1vcmRlcnMucnVudGltZUB2MQ.Changed.customer"),
            (
                Some("acme-orders.runtime@v1".to_owned()),
                Some("Changed".to_owned())
            )
        );
    }
}
