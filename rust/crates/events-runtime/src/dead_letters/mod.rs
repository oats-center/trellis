//! Durable per-Consumer dead-letter lifecycle.

mod delivery;
mod journal;
mod projector;
mod replay;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use journal::{dead_letter_id, DeadLetterJournal, JournalError};
pub use projector::{start_dead_letter_projector, DeadLetterProjectorHandle};
pub use replay::{start_replay_dispatcher, ReplayDispatcherHandle};

/// Authoritative Consumer dead-letter stream.
pub const DLQ_STREAM: &str = "trellis_consumer_dlq";
/// Subject wildcard retained by the Consumer dead-letter stream.
pub const DLQ_SUBJECTS: &str = "_trellis.consumer.dlq.>";
/// Durable targeted replay stream.
pub const REPLAY_STREAM: &str = "trellis_consumer_replays";
/// Subject wildcard retained by the targeted replay stream.
pub const REPLAY_SUBJECTS: &str = "_trellis.consumer.replay.>";

/// Current dead-letter lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeadLetterState {
    /// Awaiting management.
    Dead,
    /// Replay intent is durable but not yet dispatched.
    ReplayPending,
    /// One targeted replay generation has been dispatched.
    Replaying,
    /// Replay completed successfully.
    Resolved,
    /// An operator dismissed the entry.
    Dismissed,
}

/// Cause recorded for one journal transition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeadLetterCause {
    /// Original or replay delivery exhausted.
    Exhausted,
    /// Manual replay was accepted.
    ReplayRequested,
    /// Targeted replay publication was confirmed.
    ReplayDispatched,
    /// Targeted replay succeeded.
    ReplaySucceeded,
    /// An operator dismissed the dead letter.
    Dismissed,
}

/// Immutable original event evidence retained in the first journal record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OriginalEvent {
    /// Original JetStream name.
    pub stream: String,
    /// Original stream sequence.
    pub sequence: u64,
    /// Event identifier, when supplied by the publisher.
    pub event_id: Option<String>,
    /// Original concrete subject.
    pub subject: String,
    /// Original payload bytes.
    pub payload_bytes: Vec<u8>,
    /// Original message headers, preserving repeated values.
    pub headers: BTreeMap<String, Vec<String>>,
    /// Resolved API identity, when known.
    pub api_id: Option<String>,
    /// Resolved event name, when known.
    pub event_name: Option<String>,
    /// Immutable authorization-context digest.
    pub context_digest: Option<String>,
    /// Historical verification classification.
    pub verification_status: String,
}

/// One authoritative dead-letter lifecycle transition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeadLetterTransition {
    /// Deterministic dead-letter identity.
    pub id: String,
    /// Exact logical Consumer resource identity.
    pub resource_id: String,
    /// Monotonic lifecycle revision.
    pub revision: u64,
    /// Previous transition's subject sequence, or zero for creation.
    pub previous_subject_sequence: u64,
    /// Manual replay generation.
    pub generation: u64,
    /// Current lifecycle state.
    pub state: DeadLetterState,
    /// Transition timestamp.
    pub occurred_at: String,
    /// Idempotent management command identifier.
    pub request_id: Option<String>,
    /// Digest of immutable command content.
    pub request_digest: Option<String>,
    /// Transition cause.
    pub cause: DeadLetterCause,
    /// Immutable original evidence, present only at revision one.
    pub original: Option<OriginalEvent>,
    /// Stream sequence of the revision-one journal record.
    pub original_record_sequence: u64,
    /// Delivery count observed for this generation.
    pub deliveries: u64,
    /// Last bounded handler failure detail.
    pub last_error: Option<String>,
    /// Confirmed targeted replay stream sequence.
    pub replay_stream_sequence: Option<u64>,
}

/// Immutable targeted replay work item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayEnvelope {
    /// Dead-letter identity.
    pub dead_letter_id: String,
    /// Replay generation.
    pub generation: u64,
    /// Exact Consumer resource identity.
    pub resource_id: String,
    /// Revision-one journal stream sequence.
    pub original_record_sequence: u64,
    /// Original domain subject used only for matching and proof verification.
    pub original_subject: String,
    /// Original payload bytes.
    pub original_payload_bytes: Vec<u8>,
    /// Original proof-bearing headers.
    pub original_headers: BTreeMap<String, Vec<String>>,
}

pub use delivery::{
    start_exhaustion_advisory_loop, ConsumerBinding, ConsumerBindingResolver, DeliveryOutcome,
    DeliveryReport, DeliveryReportError, DeliveryReportResult, DeliveryReporter,
    ExhaustionAdvisoryHandle, MaxDeliveriesAdvisory,
};
pub use replay::replay_filter_subject;
