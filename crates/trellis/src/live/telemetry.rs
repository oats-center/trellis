//! One truthful telemetry owner per live observation endpoint.
//!
//! The endpoint record owns exactly one of these; it derives both the seven
//! `trellis.live.*` families and the narrow Live-only projections from the
//! same local state. Instruments are never recorded from a detached closer,
//! UI status, or handle garbage collection.
//!
//! Boundary rules: a session moves one unit between phases and is removed at
//! actual local cleanup completion; an end is recorded exactly once at the
//! local terminal commit; a pending normal end is not yet a consumer terminal;
//! retained cleanup stays `closing` and additionally counts once in
//! `trellis.live.cleanup.pending`.

use std::time::Instant;

use opentelemetry::KeyValue;
use trellis_protocol::LiveSessionKind;

use crate::telemetry::instruments::{
    add_counter, add_updown, record_family_duration, CounterFamily, DurationFamily, UpDownFamily,
};

use super::types::LiveEnd;

/// Endpoint side dimension for every live family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveSide {
    Consumer,
    Provider,
}

impl LiveSide {
    fn as_str(self) -> &'static str {
        match self {
            Self::Consumer => "consumer",
            Self::Provider => "provider",
        }
    }
}

/// Phase dimension for `trellis.live.sessions`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LivePhase {
    Prepared,
    Activating,
    Active,
    Draining,
    Closing,
}

impl LivePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Closing => "closing",
        }
    }
}

/// Fixed rejection categories for `trellis.live.rejections`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveRejection {
    InvalidSignature,
    ForeignIdentity,
    InvalidProtocol,
    OverCapacity,
}

impl LiveRejection {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSignature => "invalid_signature",
            Self::ForeignIdentity => "foreign_identity",
            Self::InvalidProtocol => "invalid_protocol",
            Self::OverCapacity => "over_capacity",
        }
    }
}

/// Frame class dimension for `trellis.live.frames`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveFrameClass {
    Data,
    Control,
}

impl LiveFrameClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Control => "control",
        }
    }
}

/// Frame direction dimension for `trellis.live.frames`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveDirection {
    Send,
    Receive,
}

impl LiveDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Receive => "receive",
        }
    }
}

/// One endpoint's live telemetry owner.
pub(crate) struct LiveTelemetryOwner {
    kind: LiveSessionKind,
    side: LiveSide,
    phase: Option<LivePhase>,
    handshake_started: Option<Instant>,
    handshake_recorded: bool,
    ended: bool,
    removed: bool,
    cleanup_pending: bool,
}

impl LiveTelemetryOwner {
    /// Create one owner already in the prepared phase.
    pub(crate) fn new_prepared(kind: LiveSessionKind, side: LiveSide) -> Self {
        let mut owner = Self::new(kind, side);
        owner.prepared();
        owner
    }

    /// Create one owner for a prepared endpoint record.
    pub(crate) fn new(kind: LiveSessionKind, side: LiveSide) -> Self {
        Self {
            kind,
            side,
            phase: None,
            handshake_started: None,
            handshake_recorded: false,
            ended: false,
            removed: false,
            cleanup_pending: false,
        }
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            LiveSessionKind::Standalone => "live",
            LiveSessionKind::Operation => "operation_watch",
        }
    }

    fn base(&self) -> Vec<KeyValue> {
        vec![
            KeyValue::new("trellis.kind", self.kind_str()),
            KeyValue::new("trellis.side", self.side.as_str()),
        ]
    }

    fn transition(&mut self, next: LivePhase) {
        if self.phase == Some(next) {
            return;
        }
        let mut previous = self.base();
        if let Some(phase) = self.phase {
            previous.push(KeyValue::new("trellis.phase", phase.as_str()));
            add_updown(UpDownFamily::LiveSessions, -1, &previous);
        }
        self.phase = Some(next);
        let mut attributes = self.base();
        attributes.push(KeyValue::new("trellis.phase", next.as_str()));
        add_updown(UpDownFamily::LiveSessions, 1, &attributes);
    }

    fn record_handshake(&mut self) {
        if self.handshake_recorded {
            return;
        }
        let Some(started) = self.handshake_started else {
            return;
        };
        self.handshake_recorded = true;
        record_family_duration(
            DurationFamily::LiveHandshake,
            started.elapsed(),
            &self.base(),
        );
    }

    /// The record now owns a prepared session.
    pub(crate) fn prepared(&mut self) {
        self.transition(LivePhase::Prepared);
    }

    /// The first activation control was initiated.
    pub(crate) fn activating(&mut self) {
        if self.handshake_started.is_none() {
            self.handshake_started = Some(Instant::now());
        }
        self.transition(LivePhase::Activating);
    }

    /// The local side committed ACTIVE.
    pub(crate) fn active(&mut self) {
        self.record_handshake();
        self.transition(LivePhase::Active);
    }

    /// A verified normal end was admitted and the queue is draining.
    pub(crate) fn draining(&mut self) {
        self.transition(LivePhase::Draining);
    }

    /// The endpoint entered its close exchange.
    pub(crate) fn closing(&mut self) {
        self.transition(LivePhase::Closing);
    }

    /// Commit the one local terminal outcome.
    ///
    /// A prepared failure/cancel/expiry records `trellis.live.ends` without a
    /// Live active/end pair.
    pub(crate) fn end(&mut self, end: &LiveEnd) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.record_handshake();
        if self.phase != Some(LivePhase::Closing) {
            self.transition(LivePhase::Closing);
        }
        let mut attributes = self.base();
        attributes.push(KeyValue::new("trellis.reason", end.reason().as_str()));
        add_counter(CounterFamily::LiveEnds, 1, &attributes);
    }

    /// Retained cleanup exceeded the shared grace.
    pub(crate) fn cleanup_exceeded_grace(&mut self) {
        if self.cleanup_pending || self.removed {
            return;
        }
        self.cleanup_pending = true;
        add_updown(UpDownFamily::LiveCleanupPending, 1, &self.base());
    }

    /// Actual owned cleanup settled.
    pub(crate) fn cleanup_finished(&mut self) {
        if self.cleanup_pending {
            self.cleanup_pending = false;
            add_updown(UpDownFamily::LiveCleanupPending, -1, &self.base());
        }
        if self.removed {
            return;
        }
        self.removed = true;
        if let Some(phase) = self.phase.take() {
            let mut attributes = self.base();
            attributes.push(KeyValue::new("trellis.phase", phase.as_str()));
            add_updown(UpDownFamily::LiveSessions, -1, &attributes);
        }
    }

    /// Count one admitted outgoing or verified incoming live frame.
    pub(crate) fn frame(&self, class: LiveFrameClass, direction: LiveDirection) {
        let mut attributes = self.base();
        attributes.push(KeyValue::new("trellis.class", class.as_str()));
        attributes.push(KeyValue::new("trellis.direction", direction.as_str()));
        add_counter(CounterFamily::LiveFrames, 1, &attributes);
    }

    /// Count one rejected message by fixed category.
    pub(crate) fn rejection(&self, reason: LiveRejection) {
        let mut attributes = self.base();
        attributes.push(KeyValue::new("trellis.reason", reason.as_str()));
        add_counter(CounterFamily::LiveRejections, 1, &attributes);
    }

    /// Transfer retained serialized payload accounting.
    pub(crate) fn buffered(&self, delta: i64) {
        if delta == 0 {
            return;
        }
        add_updown(UpDownFamily::LiveBufferedBytes, delta, &self.base());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::telemetry::capture::{meter_test_lock, process_capture, MetricCapture};

    /// Canonical attribute key so a series matches regardless of order.
    fn attr_key(attributes: &[(String, String)]) -> String {
        let mut parts: Vec<String> = attributes
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        parts.sort();
        parts.join(",")
    }

    /// Canonical attribute key from literal pairs, matching [`attr_key`].
    fn key(pairs: &[(&str, &str)]) -> String {
        let mut parts: Vec<String> = pairs
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        parts.sort();
        parts.join(",")
    }

    /// Cumulative up/down or counter values keyed by attributes.
    fn values(capture: &MetricCapture, name: &str) -> BTreeMap<String, f64> {
        capture.flush();
        capture
            .points_for(name)
            .into_iter()
            .map(|point| (attr_key(&point.attributes), point.value))
            .collect()
    }

    /// Cumulative histogram sample counts keyed by attributes.
    fn counts(capture: &MetricCapture, name: &str) -> BTreeMap<String, f64> {
        capture.flush();
        capture
            .points_for(name)
            .into_iter()
            .map(|point| (attr_key(&point.attributes), point.count as f64))
            .collect()
    }

    /// Signed change between two cumulative snapshots.
    ///
    /// The exporter is process-wide, so other live tests running in parallel
    /// record their own consumer-side series into it. This module asserts the
    /// provider-owned series (`trellis.side=provider`/`server`) exactly; the
    /// unrelated consumer-side drift is discarded here.
    fn delta(
        before: &BTreeMap<String, f64>,
        after: &BTreeMap<String, f64>,
    ) -> BTreeMap<String, f64> {
        let mut result = BTreeMap::new();
        for (name, value) in after {
            let change = *value - before.get(name).copied().unwrap_or(0.0);
            if change != 0.0 {
                result.insert(name.clone(), change);
            }
        }
        for (name, value) in before {
            if !after.contains_key(name) && *value != 0.0 {
                result.insert(name.clone(), -*value);
            }
        }
        result.retain(|name, _| {
            name.contains("trellis.side=provider") || name.contains("trellis.side=server")
        });
        result
    }

    fn base_key() -> String {
        key(&[("trellis.kind", "live"), ("trellis.side", "provider")])
    }

    fn phase_key(phase: &str) -> String {
        key(&[
            ("trellis.kind", "live"),
            ("trellis.side", "provider"),
            ("trellis.phase", phase),
        ])
    }

    fn reason_key(reason: &str) -> String {
        key(&[
            ("trellis.kind", "live"),
            ("trellis.side", "provider"),
            ("trellis.reason", reason),
        ])
    }

    fn live_end_key(reason: &str) -> String {
        key(&[("trellis.side", "server"), ("trellis.reason", reason)])
    }

    fn frame_key(class: &str, direction: &str) -> String {
        key(&[
            ("trellis.kind", "live"),
            ("trellis.side", "provider"),
            ("trellis.class", class),
            ("trellis.direction", direction),
        ])
    }

    #[tokio::test]
    async fn buffered_frames_and_rejections_record_exact_deltas() {
        let _guard = meter_test_lock().await;
        let capture = process_capture();

        let owner = LiveTelemetryOwner::new(LiveSessionKind::Standalone, LiveSide::Provider);

        let buffered = UpDownFamily::LiveBufferedBytes.name();
        let net_before = values(&capture, buffered);
        owner.buffered(8192);
        let after_add = values(&capture, buffered);
        assert_eq!(
            delta(&net_before, &after_add),
            BTreeMap::from([(base_key(), 8192.0)]),
            "buffering adds the retained byte count"
        );
        owner.buffered(-8192);
        assert_eq!(
            delta(&after_add, &values(&capture, buffered)),
            BTreeMap::from([(base_key(), -8192.0)]),
            "releasing buffered bytes subtracts them"
        );
        assert_eq!(
            delta(&net_before, &values(&capture, buffered)),
            BTreeMap::new(),
            "buffered add and release net to zero"
        );

        let frames = CounterFamily::LiveFrames.name();
        let frames_before = values(&capture, frames);
        owner.frame(LiveFrameClass::Data, LiveDirection::Send);
        owner.frame(LiveFrameClass::Data, LiveDirection::Receive);
        owner.frame(LiveFrameClass::Control, LiveDirection::Send);
        owner.frame(LiveFrameClass::Control, LiveDirection::Receive);
        assert_eq!(
            delta(&frames_before, &values(&capture, frames)),
            BTreeMap::from([
                (frame_key("data", "send"), 1.0),
                (frame_key("data", "receive"), 1.0),
                (frame_key("control", "send"), 1.0),
                (frame_key("control", "receive"), 1.0),
            ]),
            "each frame class and direction is counted once"
        );

        let rejections = CounterFamily::LiveRejections.name();
        let rejections_before = values(&capture, rejections);
        owner.rejection(LiveRejection::InvalidSignature);
        owner.rejection(LiveRejection::ForeignIdentity);
        owner.rejection(LiveRejection::InvalidProtocol);
        owner.rejection(LiveRejection::OverCapacity);
        assert_eq!(
            delta(&rejections_before, &values(&capture, rejections)),
            BTreeMap::from([
                (reason_key("invalid_signature"), 1.0),
                (reason_key("foreign_identity"), 1.0),
                (reason_key("invalid_protocol"), 1.0),
                (reason_key("over_capacity"), 1.0),
            ]),
            "each fixed rejection reason is counted once"
        );
    }

    #[tokio::test]
    async fn cleanup_pending_tracks_grace_and_removes_the_session() {
        let _guard = meter_test_lock().await;
        let capture = process_capture();

        let pending = UpDownFamily::LiveCleanupPending.name();
        let sessions = UpDownFamily::LiveSessions.name();

        let mut owner =
            LiveTelemetryOwner::new_prepared(LiveSessionKind::Standalone, LiveSide::Provider);
        let pending_before = values(&capture, pending);
        let sessions_before = values(&capture, sessions);

        owner.cleanup_exceeded_grace();
        assert_eq!(
            delta(&pending_before, &values(&capture, pending)),
            BTreeMap::from([(base_key(), 1.0)]),
            "retained cleanup counts once as pending"
        );
        assert_eq!(
            delta(&sessions_before, &values(&capture, sessions)),
            BTreeMap::new(),
            "pending cleanup keeps the session in its phase"
        );

        owner.cleanup_finished();
        assert_eq!(
            delta(&pending_before, &values(&capture, pending)),
            BTreeMap::new(),
            "cleanup completion releases the pending count"
        );
        assert_eq!(
            delta(&sessions_before, &values(&capture, sessions)),
            BTreeMap::from([(phase_key("prepared"), -1.0)]),
            "cleanup completion removes the session series"
        );
    }
}
