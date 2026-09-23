use async_nats::jetstream::{self, stream};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use sha2::{Digest, Sha256};

use super::{DeadLetterCause, DeadLetterState, DeadLetterTransition, OriginalEvent, DLQ_STREAM};
use crate::storage::now_timestamp_string;

const EXPECTED_LAST_SUBJECT_SEQUENCE: &str = "Nats-Expected-Last-Subject-Sequence";

/// Errors returned by the authoritative dead-letter journal.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// JetStream operation failed.
    #[error("dead-letter journal transport failed: {0}")]
    Nats(String),
    /// Journal document encoding failed.
    #[error("dead-letter journal encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
    /// The requested entry does not exist.
    #[error("dead letter not found: {0}")]
    NotFound(String),
    /// The expected revision or lifecycle state is stale.
    #[error("dead-letter lifecycle conflict: {0}")]
    Conflict(String),
    /// Original evidence required for replay is unavailable.
    #[error("dead-letter original evidence is unavailable")]
    UnreplayableOriginal,
}

/// JetStream-backed authoritative Consumer dead-letter journal.
#[derive(Clone)]
pub struct DeadLetterJournal {
    context: jetstream::Context,
    stream: stream::Stream,
}

impl std::fmt::Debug for DeadLetterJournal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeadLetterJournal")
            .finish_non_exhaustive()
    }
}

impl DeadLetterJournal {
    /// Open the pre-provisioned authoritative journal stream.
    pub async fn open(client: async_nats::Client) -> Result<Self, JournalError> {
        let context = jetstream::new(client);
        let stream = context
            .get_stream(DLQ_STREAM)
            .await
            .map_err(|error| JournalError::Nats(error.to_string()))?;
        Ok(Self { context, stream })
    }

    /// Read the leader-served latest transition for an entry.
    pub async fn latest(
        &self,
        id: &str,
    ) -> Result<Option<(DeadLetterTransition, u64)>, JournalError> {
        let subject = subject(id);
        match self.stream.get_last_raw_message_by_subject(&subject).await {
            Ok(message) => {
                let mut transition: DeadLetterTransition =
                    serde_json::from_slice(&message.payload)?;
                if transition.revision == 1 && transition.original_record_sequence == 0 {
                    transition.original_record_sequence = message.sequence;
                }
                Ok(Some((transition, message.sequence)))
            }
            Err(error)
                if matches!(
                    error.kind(),
                    stream::LastRawMessageErrorKind::NoMessageFound
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(JournalError::Nats(error.to_string())),
        }
    }

    /// Create generation zero after delivery exhaustion.
    pub async fn create_dead(
        &self,
        resource_id: String,
        original: OriginalEvent,
        deliveries: u64,
        last_error: Option<String>,
    ) -> Result<(DeadLetterTransition, u64), JournalError> {
        let id = dead_letter_id(&resource_id, &original.stream, original.sequence);
        if let Some(existing) = self.latest(&id).await? {
            let existing_original = self.original(&existing.0).await?;
            if existing.0.resource_id != resource_id
                || existing_original.as_ref() != Some(&original)
            {
                return Err(JournalError::Conflict(
                    "dead-letter identity has different original evidence".to_owned(),
                ));
            }
            if existing.0.deliveries == deliveries && existing.0.last_error == last_error {
                return Ok(existing);
            }
            if existing.0.last_error.as_deref() != Some("broker max deliveries exceeded") {
                return Ok(existing);
            }
            let corrected = DeadLetterTransition {
                revision: existing.0.revision + 1,
                previous_subject_sequence: existing.1,
                original: None,
                original_record_sequence: if existing.0.original_record_sequence == 0 {
                    existing.1
                } else {
                    existing.0.original_record_sequence
                },
                deliveries,
                last_error,
                ..existing.0
            };
            let (corrected, sequence) = self.append(&corrected, existing.1).await?;
            return Ok((corrected, sequence));
        }
        let transition = DeadLetterTransition {
            id,
            resource_id,
            revision: 1,
            previous_subject_sequence: 0,
            generation: 0,
            state: DeadLetterState::Dead,
            occurred_at: now_timestamp_string(),
            request_id: None,
            request_digest: None,
            cause: DeadLetterCause::Exhausted,
            original: Some(original.clone()),
            original_record_sequence: 0,
            deliveries,
            last_error,
            replay_stream_sequence: None,
        };
        let (mut confirmed, sequence) = match self.append(&transition, 0).await {
            Ok(confirmed) => confirmed,
            Err(JournalError::Conflict(_)) => {
                let existing = self.latest(&transition.id).await?.ok_or_else(|| {
                    JournalError::Conflict("concurrent creation was not visible".to_owned())
                })?;
                let existing_original = self.original(&existing.0).await?;
                return if existing.0.resource_id == transition.resource_id
                    && existing_original.as_ref() == Some(&original)
                {
                    Ok(existing)
                } else {
                    Err(JournalError::Conflict(
                        "concurrent creation has different original evidence".to_owned(),
                    ))
                };
            }
            Err(error) => return Err(error),
        };
        confirmed.original_record_sequence = sequence;
        Ok((confirmed, sequence))
    }

    /// Accept an idempotent manual replay command.
    pub async fn request_replay(
        &self,
        id: &str,
        expected_revision: u64,
        request_id: String,
        request_digest: String,
    ) -> Result<(DeadLetterTransition, u64), JournalError> {
        if let Some(existing) = self.find_command(id, &request_id).await? {
            return if existing.0.request_digest.as_deref() == Some(&request_digest) {
                Ok(existing)
            } else {
                Err(JournalError::Conflict(
                    "request id has different content".to_owned(),
                ))
            };
        }
        let (current, sequence) = self.required_latest(id).await?;
        if current.revision != expected_revision {
            return Err(JournalError::Conflict(
                "expected revision is stale".to_owned(),
            ));
        }
        if !matches!(
            current.state,
            DeadLetterState::Dead | DeadLetterState::Dismissed
        ) {
            return Err(JournalError::Conflict(format!(
                "cannot replay state {:?}",
                current.state
            )));
        }
        if self.original(&current).await?.is_none() {
            return Err(JournalError::UnreplayableOriginal);
        }
        let next = successor(
            &current,
            sequence,
            DeadLetterState::ReplayPending,
            DeadLetterCause::ReplayRequested,
            Some(request_id),
            Some(request_digest),
            current.generation + 1,
            0,
            None,
            None,
        );
        self.append(&next, sequence).await
    }

    /// Record confirmed targeted replay publication.
    pub async fn mark_replaying(
        &self,
        current: &DeadLetterTransition,
        expected_subject_sequence: u64,
        replay_stream_sequence: u64,
    ) -> Result<(DeadLetterTransition, u64), JournalError> {
        let latest = self.required_latest(&current.id).await?;
        if latest.0.generation != current.generation
            || latest.0.state != DeadLetterState::ReplayPending
        {
            return Ok(latest);
        }
        let next = successor(
            &latest.0,
            expected_subject_sequence,
            DeadLetterState::Replaying,
            DeadLetterCause::ReplayDispatched,
            None,
            None,
            current.generation,
            0,
            None,
            Some(replay_stream_sequence),
        );
        self.append(&next, expected_subject_sequence).await
    }

    /// Record replay success or fresh generation exhaustion.
    pub async fn report_replay_outcome(
        &self,
        id: &str,
        generation: u64,
        succeeded: bool,
        deliveries: u64,
        last_error: Option<String>,
    ) -> Result<(DeadLetterTransition, u64), JournalError> {
        let (current, sequence) = self.required_latest(id).await?;
        if generation < current.generation {
            return Ok((current, sequence));
        }
        if generation != current.generation
            || !matches!(
                current.state,
                DeadLetterState::ReplayPending | DeadLetterState::Replaying
            )
        {
            return Err(JournalError::Conflict(
                "replay generation is not active".to_owned(),
            ));
        }
        let next = successor(
            &current,
            sequence,
            if succeeded {
                DeadLetterState::Resolved
            } else {
                DeadLetterState::Dead
            },
            if succeeded {
                DeadLetterCause::ReplaySucceeded
            } else {
                DeadLetterCause::Exhausted
            },
            None,
            None,
            generation,
            deliveries,
            last_error,
            current.replay_stream_sequence,
        );
        self.append(&next, sequence).await
    }

    /// Dismiss a dead entry under expected revision and idempotency checks.
    pub async fn dismiss(
        &self,
        id: &str,
        expected_revision: u64,
        request_id: String,
        request_digest: String,
    ) -> Result<(DeadLetterTransition, u64), JournalError> {
        if let Some(existing) = self.find_command(id, &request_id).await? {
            return if existing.0.request_digest.as_deref() == Some(&request_digest) {
                Ok(existing)
            } else {
                Err(JournalError::Conflict(
                    "request id has different content".to_owned(),
                ))
            };
        }
        let (current, sequence) = self.required_latest(id).await?;
        if current.revision != expected_revision || current.state != DeadLetterState::Dead {
            return Err(JournalError::Conflict(
                "dead letter is not dismissible".to_owned(),
            ));
        }
        let next = successor(
            &current,
            sequence,
            DeadLetterState::Dismissed,
            DeadLetterCause::Dismissed,
            Some(request_id),
            Some(request_digest),
            current.generation,
            current.deliveries,
            current.last_error.clone(),
            current.replay_stream_sequence,
        );
        self.append(&next, sequence).await
    }

    /// Load immutable original evidence from the revision-one record.
    pub async fn original(
        &self,
        transition: &DeadLetterTransition,
    ) -> Result<Option<OriginalEvent>, JournalError> {
        if let Some(original) = transition.original.clone() {
            return Ok(Some(original));
        }
        self.stream
            .get_raw_message(transition.original_record_sequence)
            .await
            .map_err(|error| JournalError::Nats(error.to_string()))
            .and_then(|message| {
                serde_json::from_slice::<DeadLetterTransition>(&message.payload)
                    .map_err(JournalError::from)
                    .map(|first| first.original)
            })
    }

    async fn required_latest(&self, id: &str) -> Result<(DeadLetterTransition, u64), JournalError> {
        self.latest(id)
            .await?
            .ok_or_else(|| JournalError::NotFound(id.to_owned()))
    }

    async fn find_command(
        &self,
        id: &str,
        request_id: &str,
    ) -> Result<Option<(DeadLetterTransition, u64)>, JournalError> {
        let subject = subject(id);
        let mut start = 1;
        loop {
            let message = match self
                .stream
                .get_first_raw_message_by_subject(&subject, start)
                .await
            {
                Ok(message) => message,
                Err(error)
                    if matches!(error.kind(), stream::RawMessageErrorKind::NoMessageFound) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(JournalError::Nats(error.to_string())),
            };
            let sequence = message.sequence;
            let transition = serde_json::from_slice::<DeadLetterTransition>(&message.payload)?;
            if transition.request_id.as_deref() == Some(request_id) {
                return Ok(Some((transition, sequence)));
            }
            start = sequence.saturating_add(1);
        }
    }

    async fn find_revision(
        &self,
        id: &str,
        revision: u64,
    ) -> Result<Option<(DeadLetterTransition, u64)>, JournalError> {
        let subject = subject(id);
        let mut start = 1;
        loop {
            let message = match self
                .stream
                .get_first_raw_message_by_subject(&subject, start)
                .await
            {
                Ok(message) => message,
                Err(error)
                    if matches!(error.kind(), stream::RawMessageErrorKind::NoMessageFound) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(JournalError::Nats(error.to_string())),
            };
            let sequence = message.sequence;
            let transition = serde_json::from_slice::<DeadLetterTransition>(&message.payload)?;
            match transition.revision.cmp(&revision) {
                std::cmp::Ordering::Equal => return Ok(Some((transition, sequence))),
                std::cmp::Ordering::Greater => return Ok(None),
                std::cmp::Ordering::Less => start = sequence.saturating_add(1),
            }
        }
    }

    async fn append(
        &self,
        transition: &DeadLetterTransition,
        expected_subject_sequence: u64,
    ) -> Result<(DeadLetterTransition, u64), JournalError> {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(
            EXPECTED_LAST_SUBJECT_SEQUENCE,
            expected_subject_sequence.to_string(),
        );
        let ack = self
            .context
            .publish_with_headers(
                subject(&transition.id),
                headers,
                Bytes::from(serde_json::to_vec(transition)?),
            )
            .await
            .map_err(|error| JournalError::Nats(error.to_string()))?;
        // One authoritative dead-letter transition operation, recorded here
        // and never re-emitted by a projector rebuild.
        let action = transition_action(&transition.state, &transition.cause);
        match ack.await {
            Ok(ack) => {
                record_dead_letter_transition(action, "ok");
                Ok((transition.clone(), ack.sequence))
            }
            Err(error) => {
                if let Some(existing) = self
                    .find_revision(&transition.id, transition.revision)
                    .await?
                {
                    return if same_effect(&existing.0, transition) {
                        record_dead_letter_transition(action, "ok");
                        Ok(existing)
                    } else {
                        record_dead_letter_transition(action, "error");
                        Err(JournalError::Conflict(
                            "revision has different transition content".to_owned(),
                        ))
                    };
                }
                match self.latest(&transition.id).await? {
                    Some((_, sequence)) if sequence != expected_subject_sequence => {
                        record_dead_letter_transition(action, "error");
                        Err(JournalError::Conflict(error.to_string()))
                    }
                    _ => {
                        record_dead_letter_transition(action, "error");
                        Err(JournalError::Nats(error.to_string()))
                    }
                }
            }
        }
    }
}

/// Bounded catalog action for one dead-letter transition.
fn transition_action(state: &DeadLetterState, cause: &DeadLetterCause) -> &'static str {
    match (state, cause) {
        (DeadLetterState::ReplayPending, _) => "replay_requested",
        (DeadLetterState::Replaying, _) => "replay_published",
        (DeadLetterState::Resolved, _) => "replay_succeeded",
        (DeadLetterState::Dismissed, _) => "dismissed",
        (DeadLetterState::Dead, DeadLetterCause::Exhausted) => "exhausted",
        (DeadLetterState::Dead, _) => "replay_failed",
    }
}

/// Records one authoritative dead-letter transition operation.
fn record_dead_letter_transition(action: &'static str, outcome: &'static str) {
    trellis_rs::telemetry::instruments::add_counter(
        trellis_rs::telemetry::instruments::CounterFamily::DeadLetterTransitions,
        1,
        &[
            trellis_rs::telemetry::KeyValue::new("trellis.action", action),
            trellis_rs::telemetry::KeyValue::new("trellis.outcome", outcome),
        ],
    );
}

/// Compute the independent per-Consumer dead-letter identity.
#[must_use]
pub fn dead_letter_id(resource_id: &str, stream: &str, sequence: u64) -> String {
    let mut digest = Sha256::new();
    let sequence = sequence.to_be_bytes();
    for part in [resource_id.as_bytes(), stream.as_bytes(), &sequence] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

fn subject(id: &str) -> String {
    format!("_trellis.consumer.dlq.{id}")
}

fn same_effect(left: &DeadLetterTransition, right: &DeadLetterTransition) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.occurred_at.clear();
    right.occurred_at.clear();
    left == right
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the persisted transition fields"
)]
fn successor(
    current: &DeadLetterTransition,
    previous_subject_sequence: u64,
    state: DeadLetterState,
    cause: DeadLetterCause,
    request_id: Option<String>,
    request_digest: Option<String>,
    generation: u64,
    deliveries: u64,
    last_error: Option<String>,
    replay_stream_sequence: Option<u64>,
) -> DeadLetterTransition {
    DeadLetterTransition {
        id: current.id.clone(),
        resource_id: current.resource_id.clone(),
        revision: current.revision + 1,
        previous_subject_sequence,
        generation,
        state,
        occurred_at: now_timestamp_string(),
        request_id,
        request_digest,
        cause,
        original: None,
        original_record_sequence: if current.original_record_sequence == 0 {
            previous_subject_sequence
        } else {
            current.original_record_sequence
        },
        deliveries,
        last_error,
        replay_stream_sequence,
    }
}

#[cfg(test)]
mod tests {
    use super::dead_letter_id;

    #[test]
    fn identity_is_consumer_scoped_and_length_framed() {
        assert_ne!(
            dead_letter_id("consumer-a", "events", 42),
            dead_letter_id("consumer-b", "events", 42)
        );
        assert_ne!(dead_letter_id("ab", "c", 42), dead_letter_id("a", "bc", 42));
    }
}
