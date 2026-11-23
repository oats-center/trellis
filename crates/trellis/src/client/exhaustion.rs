//! Writes JetStream refuses for lack of room.

/// A write JetStream refused because a resource has no room left.
///
/// KV, store, State, job and event writes return it, inside their own error
/// types, when JetStream refuses for lack of storage, memory, streams or
/// consumers, or because a stream holds as much as its limits allow. Nothing
/// was written. Writing again succeeds only once room is made, such as by
/// deleting data or raising the broker's limits.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{kind} '{name}' has no room: {reason}")]
pub struct ResourceExhausted {
    /// What kind of resource refused the write: `kv`, `store`, `state`,
    /// `jobs` or `events`.
    pub kind: String,
    /// Which one: the KV, store or State resource's name, the job queue's
    /// type, or the event's subject.
    pub name: String,
    /// What JetStream ran out of, in a few words.
    pub reason: String,
}

impl ResourceExhausted {
    /// The refusal `text` describes for the `kind` resource `name`, when
    /// `text` is a JetStream refusal for lack of room.
    pub(crate) fn from_text(kind: &str, name: &str, text: &str) -> Option<Self> {
        exhaustion_in(text).map(|exhaustion| Self {
            kind: kind.to_owned(),
            name: name.to_owned(),
            reason: exhaustion.describe().to_owned(),
        })
    }
}

/// Why JetStream refused to create or write something.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exhaustion {
    /// The server or account cannot reserve or store more file storage.
    Storage,
    /// The server or account cannot reserve or store more memory.
    Memory,
    /// The account has as many streams as its limit allows.
    Streams,
    /// The stream or account has as many consumers as its limit allows.
    Consumers,
    /// The stream holds as many bytes or messages as its own limits allow and
    /// refuses new ones.
    Full,
}

impl Exhaustion {
    /// The refusal in a few words, for logs, health, and callers.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Storage => "JetStream storage is exhausted",
            Self::Memory => "JetStream memory is exhausted",
            Self::Streams => "JetStream's stream limit is reached",
            Self::Consumers => "JetStream's consumer limit is reached",
            Self::Full => "the stream is full",
        }
    }
}

/// Recognizes a JetStream refusal for lack of room in error text.
///
/// JetStream errors carry their code in the text async-nats renders,
/// `<description> (code <status>, error code <code>)`, and callers often keep
/// only that text, so this reads the codes from it, falling back to the
/// descriptions JetStream gives without a code.
#[doc(hidden)]
pub fn exhaustion_in(text: &str) -> Option<Exhaustion> {
    let text = text.to_ascii_lowercase();
    let code = |code: u32| text.contains(&format!("error code {code})"));
    if code(10047) || code(10002) || code(10023) {
        // Account resources (10002) and insufficient resources (10023) are
        // storage for file streams, which every Trellis stream is.
        Some(Exhaustion::Storage)
    } else if code(10028) {
        Some(Exhaustion::Memory)
    } else if code(10027) {
        Some(Exhaustion::Streams)
    } else if code(10026) {
        Some(Exhaustion::Consumers)
    } else if code(10077) && text.contains("maximum") {
        // "maximum bytes exceeded", "maximum messages exceeded", and
        // "maximum messages per subject exceeded".
        Some(Exhaustion::Full)
    } else if text.contains("insufficient storage resources")
        || text.contains("insufficient resources")
    {
        Some(Exhaustion::Storage)
    } else if text.contains("insufficient memory resources") {
        Some(Exhaustion::Memory)
    } else {
        None
    }
}
