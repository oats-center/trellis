//! Verification after intake: `hmac`, the decide handler, duplicates, and the
//! `Received` event. `sync` requests run it before Trellis answers; stored
//! `async` requests run it from the intake stream, and so does verifying a
//! stored request again.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use trellis_idl::{WebhookMode, WebhookVerifier};

use super::records::Outcome;
use super::request::{Check, Disposition, StoredRequest};
use super::verify::{self, Failure, KeyMatch};
use super::{Endpoint, Webhooks};

/// Largest decide response body Trellis sends a sender.
const MAX_RESPONSE_BODY: usize = 64 << 10;
/// Most headers a decide response may set.
const MAX_RESPONSE_HEADERS: usize = 16;
/// Content types a decide response may have; nothing a browser would run.
const RESPONSE_TYPES: &[&str] = &[
    "text/plain",
    "application/json",
    "application/xml",
    "text/xml",
    "application/soap+xml",
    "application/x-www-form-urlencoded",
];
/// Headers a decide response may not set: cookies, hop-by-hop headers, and
/// those Trellis sets itself.
const REFUSED_RESPONSE_HEADERS: &[&str] = &[
    "set-cookie",
    "set-cookie2",
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "content-length",
    "content-security-policy",
    "x-content-type-options",
    "cache-control",
    "strict-transport-security",
    "x-frame-options",
    "referrer-policy",
    "access-control-allow-origin",
    "access-control-allow-credentials",
];

/// An HTTP answer from the service's decide handler, already checked.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    #[serde(with = "body")]
    pub(crate) body: Vec<u8>,
}

mod body {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        base64::engine::general_purpose::STANDARD
            .decode(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

/// What verification concluded.
#[derive(Debug)]
pub(crate) enum Verdict {
    /// Genuine; published as `Received` with the event ID `delivery_id`.
    Accepted {
        delivery_id: String,
        matched: Option<KeyMatch>,
        decided: Option<String>,
        answer: Option<Answer>,
        status: u16,
    },
    /// Genuine, and the service asked for no event.
    Ignored {
        matched: Option<KeyMatch>,
        answer: Option<Answer>,
        status: u16,
    },
    /// Seen before within the dedupe window; the original's answer.
    Duplicate {
        matched: Option<KeyMatch>,
        answer: Option<Answer>,
        status: u16,
    },
    /// Not genuine.
    Rejected {
        reason: String,
        matched: Option<KeyMatch>,
        decided: Option<String>,
    },
    /// A secret is not set yet.
    Unset(String),
    /// The decide handler failed or missed its deadline.
    DecideFailed(String),
    /// The decide handler's response broke a rule.
    InvalidDecision(String),
    /// Storage refused the event.
    Unavailable(String),
}

impl Verdict {
    /// The record for this verdict.
    pub(crate) fn outcome(&self, pending: bool) -> Outcome {
        let key = |matched: &Option<KeyMatch>| matched.map(KeyMatch::as_str);
        match self {
            Self::Accepted {
                delivery_id,
                matched,
                decided,
                status,
                ..
            } => Outcome {
                outcome: "accepted",
                matched_key: key(matched),
                verdict: decided.clone(),
                status: Some(*status),
                delivery_id: Some(delivery_id.clone()),
                ..Outcome::default()
            },
            Self::Ignored {
                matched, status, ..
            } => Outcome {
                outcome: "ignored",
                matched_key: key(matched),
                verdict: Some("ignore".into()),
                status: Some(*status),
                ..Outcome::default()
            },
            Self::Duplicate {
                matched, status, ..
            } => Outcome {
                outcome: "duplicate",
                matched_key: key(matched),
                status: Some(*status),
                ..Outcome::default()
            },
            Self::Rejected {
                reason,
                matched,
                decided,
            } => Outcome {
                outcome: "rejected",
                reason: Some(reason.clone()),
                matched_key: key(matched),
                verdict: decided.clone(),
                status: (!pending).then_some(401),
                ..Outcome::default()
            },
            Self::Unset(reason) | Self::DecideFailed(reason) | Self::Unavailable(reason) => {
                Outcome {
                    outcome: if pending { "pending" } else { "unavailable" },
                    reason: Some(reason.clone()),
                    status: (!pending).then_some(503),
                    ..Outcome::default()
                }
            }
            Self::InvalidDecision(reason) => Outcome {
                outcome: "invalid_decision",
                reason: Some(reason.clone()),
                status: (!pending).then_some(502),
                ..Outcome::default()
            },
        }
    }
}

/// What the decide handler answered, on the wire.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Decision {
    verdict: Option<String>,
    response: Option<DecisionResponse>,
    reason: Option<String>,
    dedupe_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DecisionResponse {
    status: serde_json::Value,
    #[serde(default)]
    headers: Vec<DecisionField>,
    #[serde(default)]
    body: String,
}

#[derive(Debug, Deserialize)]
struct DecisionField {
    name: String,
    value: String,
}

/// Check a decide response before Trellis sends it from its own origin.
fn checked(response: DecisionResponse) -> Result<Answer, String> {
    let status = response
        .status
        .as_u64()
        .or_else(|| response.status.as_str().and_then(|text| text.parse().ok()))
        .and_then(|status| u16::try_from(status).ok())
        .filter(|status| (200..=599).contains(status))
        .ok_or("response status outside 200 to 599")?;
    let body = base64::engine::general_purpose::STANDARD
        .decode(&response.body)
        .map_err(|_| "response body is not base64")?;
    if body.len() > MAX_RESPONSE_BODY {
        return Err("response body over 64 KiB".into());
    }
    if response.headers.len() > MAX_RESPONSE_HEADERS {
        return Err("more than 16 response headers".into());
    }
    let mut headers = Vec::new();
    for field in response.headers {
        let name = field.name.to_ascii_lowercase();
        if REFUSED_RESPONSE_HEADERS.contains(&name.as_str()) || name.starts_with("proxy-") {
            return Err(format!("response header {name} refused"));
        }
        if axum::http::HeaderName::from_bytes(name.as_bytes()).is_err()
            || axum::http::HeaderValue::from_str(&field.value).is_err()
        {
            return Err("response header invalid".into());
        }
        if name == "content-type" {
            let media = field
                .value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if !RESPONSE_TYPES.contains(&media.as_str()) {
                return Err(format!("response content type {media} refused"));
            }
        }
        headers.push((name, field.value));
    }
    Ok(Answer {
        status,
        headers,
        body,
    })
}

/// How long a request that claimed a delivery ID may take to publish before
/// another request with that ID takes the claim over.
const CLAIM_STALE_MS: i64 = 30_000;
/// How long a request waits for another one handling the same delivery ID.
const CLAIM_WAIT: Duration = Duration::from_secs(5);

/// A delivery ID's entry in the dedupe bucket. A request claims it before
/// publishing, so requests with the same ID publish once even when they
/// arrive together, and fills in its answer once it is done.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kept {
    expires_at_ms: i64,
    /// The `Received` event's ID. A claim taken over keeps it, so a publish
    /// that did land before an interruption is stored once.
    delivery_id: String,
    /// When the claim was made; 0 once its publish failed.
    claimed_at_ms: i64,
    /// The original's answer, absent while the claiming request publishes.
    status: Option<u16>,
    answer: Option<Answer>,
}

/// What claiming a delivery ID found.
enum Claim {
    /// This request publishes, as `delivery_id`, then finishes the entry at
    /// `revision`.
    Owned { revision: u64, delivery_id: String },
    /// Seen before within the dedupe window.
    Duplicate { status: u16, answer: Option<Answer> },
}

impl Webhooks {
    /// Run every check after intake and, for a genuine request, publish it.
    pub(crate) async fn verify(
        &self,
        endpoint: &Endpoint,
        request: &StoredRequest,
        mode: WebhookMode,
        attempt: u32,
    ) -> Verdict {
        let definition = &endpoint.record.definition;
        let now = super::now_ms();
        // Intake checks stand only under the policy they ran under, and only
        // while each secret they matched still accepts the value matched.
        // Under another policy every verifier runs again where the stored
        // request allows it; a credential removed at intake cannot be checked
        // again, so the request is refused.
        let same_policy = request.policy == endpoint.policy;
        let rejected = |reason: String| Verdict::Rejected {
            reason,
            matched: None,
            decided: None,
        };
        let mut checks = Vec::new();
        if same_policy {
            for (field, generation) in &request.matched {
                if !endpoint.accepts(field, *generation, now) {
                    return rejected(format!(
                        "secret {field} changed after intake and no longer accepts the value \
                        the request matched; the credential check can't be rerun"
                    ));
                }
            }
            checks.clone_from(&request.checks);
        }
        let mut matched = None;
        for verifier in &definition.verifiers {
            match verifier {
                WebhookVerifier::Hmac(hmac) => {
                    let secrets = endpoint.secrets(&hmac.key, now);
                    match verify::hmac(request, hmac, &secrets) {
                        Ok(key) => {
                            let captured = request.generations.get(&hmac.key).copied();
                            if let Some(generation) = endpoint.match_generation(&hmac.key, captured)
                            {
                                let (deployment, name) = endpoint.key();
                                let _ = self
                                    .inner
                                    .records
                                    .note_match(
                                        &deployment,
                                        &name,
                                        &hmac.key,
                                        generation,
                                        key,
                                        request.received_at_ms,
                                    )
                                    .await;
                            }
                            matched = matched.or(Some(key));
                            checks.push(Check {
                                kind: "hmac".into(),
                                key: Some(key.as_str().into()),
                            });
                        }
                        Err(Failure::Unset(name)) => {
                            return Verdict::Unset(format!("secret {name} not set"))
                        }
                        Err(Failure::Rejected(reason)) => {
                            return Verdict::Rejected {
                                reason,
                                matched,
                                decided: None,
                            }
                        }
                    }
                }
                _ if same_policy => {}
                WebhookVerifier::Address { ranges } => {
                    let allowed = request
                        .client_address
                        .parse()
                        .is_ok_and(|client| endpoint.allows(ranges, client));
                    if !allowed {
                        return rejected("client address outside the allowed ranges".into());
                    }
                    checks.push(Check {
                        kind: "address".into(),
                        key: None,
                    });
                }
                WebhookVerifier::Token { .. } | WebhookVerifier::Basic { .. } => {
                    return rejected(
                        "verifier policy changed after intake; credential checks can't be rerun"
                            .into(),
                    );
                }
                WebhookVerifier::None => checks.push(Check {
                    kind: "none".into(),
                    key: None,
                }),
            }
        }
        let mut dedupe_key = definition
            .dedupe
            .as_deref()
            .and_then(|template| verify::render(template, request, None).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|key| !key.is_empty());
        let mut answer = None;
        let mut decided = None;
        let mut ignore = false;
        if definition.decide.is_some() {
            let deadline_ms = endpoint.settings.decide_deadline_ms(mode);
            let started = Instant::now();
            let decision = self
                .decide(
                    endpoint,
                    request,
                    &checks,
                    dedupe_key.as_deref(),
                    mode,
                    attempt,
                    deadline_ms,
                )
                .await;
            let label = match &decision {
                Ok(decision) => decision.verdict.clone().unwrap_or_else(|| "reject".into()),
                Err(_) => "failed".into(),
            };
            trellis_rs::telemetry::instruments::record_family_duration(
                trellis_rs::telemetry::instruments::DurationFamily::WebhookDecide,
                started.elapsed(),
                &[
                    opentelemetry::KeyValue::new(
                        "trellis.webhook.endpoint",
                        endpoint.record.endpoint.clone(),
                    ),
                    opentelemetry::KeyValue::new("trellis.outcome", label),
                ],
            );
            let decision = match decision {
                Ok(decision) => decision,
                Err(reason) => return Verdict::DecideFailed(reason),
            };
            let verdict = decision.verdict.as_deref().unwrap_or("reject");
            if mode == WebhookMode::Sync {
                if let Some(response) = decision.response {
                    match checked(response) {
                        Ok(checked) => answer = Some(checked),
                        Err(reason) => return Verdict::InvalidDecision(reason),
                    }
                }
            }
            match verdict {
                "accept" => decided = Some("accept".to_owned()),
                "ignore" => ignore = true,
                _ => {
                    return Verdict::Rejected {
                        reason: decision
                            .reason
                            .map(|reason| reason.chars().take(200).collect())
                            .unwrap_or_else(|| "the service rejected it".into()),
                        matched,
                        decided: Some("reject".into()),
                    }
                }
            }
            checks.push(Check {
                kind: "decide".into(),
                key: None,
            });
            if let Some(key) = decision.dedupe_key.filter(|key| !key.is_empty()) {
                dedupe_key = Some(key);
            }
        }
        let status = answer
            .as_ref()
            .map_or(definition.status, |answer| answer.status);
        // Claim an ID only once a request passed verification, so a forged
        // request with a captured ID cannot get the real one dropped.
        let kept_key = dedupe_key
            .as_deref()
            .map(|key| super::dedupe_entry(&request.deployment_id, &request.endpoint, key));
        let mut claimed = None;
        let mut delivery_id = request.attempt_id.clone();
        if let Some(entry) = &kept_key {
            match self.claim(entry, &request.attempt_id, endpoint).await {
                Ok(Claim::Duplicate { status, answer }) => {
                    return Verdict::Duplicate {
                        matched,
                        answer,
                        status,
                    }
                }
                Ok(Claim::Owned {
                    revision,
                    delivery_id: claimed_id,
                }) => {
                    claimed = Some(revision);
                    delivery_id = claimed_id;
                }
                Err(reason) => return Verdict::Unavailable(reason),
            }
        }
        let window = i64::try_from(endpoint.settings.dedupe_window_ms).unwrap_or(i64::MAX);
        let finish = |claimed_at_ms: i64, done: bool| Kept {
            expires_at_ms: super::now_ms().saturating_add(window),
            delivery_id: delivery_id.clone(),
            claimed_at_ms,
            status: done.then_some(status),
            answer: answer.clone().filter(|_| done),
        };
        if !ignore {
            if let Err(reason) = self
                .publish(request, &delivery_id, &checks, dedupe_key.as_deref())
                .await
            {
                // Release the claim at once, keeping its event ID, so the
                // sender's retry publishes the same event.
                if let (Some(entry), Some(revision)) = (&kept_key, claimed) {
                    if let Ok(value) = serde_json::to_vec(&finish(0, false)) {
                        let _ = self
                            .inner
                            .dedupe
                            .update(entry, value.into(), revision)
                            .await;
                    }
                }
                return Verdict::Unavailable(reason);
            }
        }
        if let (Some(entry), Some(revision)) = (&kept_key, claimed) {
            if let Ok(value) = serde_json::to_vec(&finish(super::now_ms(), true)) {
                // Losing this leaves the claim to go stale, after which a
                // retry publishes the same event again.
                let _ = self
                    .inner
                    .dedupe
                    .update(entry, value.into(), revision)
                    .await;
            }
        }
        if ignore {
            Verdict::Ignored {
                matched,
                answer,
                status,
            }
        } else {
            Verdict::Accepted {
                delivery_id,
                matched,
                decided,
                answer,
                status,
            }
        }
    }

    /// Claim a delivery ID's dedupe entry before publishing: create it, take
    /// over an expired entry or a stale claim, or wait briefly for the request
    /// that holds it to finish and answer as a duplicate.
    async fn claim(
        &self,
        entry: &str,
        attempt_id: &str,
        endpoint: &Endpoint,
    ) -> Result<Claim, String> {
        use async_nats::jetstream::kv::Operation;
        let unavailable = || "dedupe storage unavailable".to_owned();
        let window = i64::try_from(endpoint.settings.dedupe_window_ms).unwrap_or(i64::MAX);
        let deadline = Instant::now() + CLAIM_WAIT;
        loop {
            let now = super::now_ms();
            let claim = |delivery_id: &str| {
                serde_json::to_vec(&Kept {
                    expires_at_ms: now.saturating_add(window),
                    delivery_id: delivery_id.to_owned(),
                    claimed_at_ms: now,
                    status: None,
                    answer: None,
                })
                .map_err(|error| error.to_string())
            };
            let found = self
                .inner
                .dedupe
                .entry(entry)
                .await
                .map_err(|_| unavailable())?
                .filter(|found| found.operation == Operation::Put);
            let written = match found {
                None => self
                    .inner
                    .dedupe
                    .create(entry, claim(attempt_id)?.into())
                    .await
                    .ok()
                    .map(|revision| (revision, attempt_id.to_owned())),
                Some(found) => {
                    let kept = serde_json::from_slice::<Kept>(&found.value)
                        .ok()
                        .filter(|kept| kept.expires_at_ms > now);
                    let delivery_id = match kept {
                        Some(Kept {
                            status: Some(status),
                            answer,
                            ..
                        }) => return Ok(Claim::Duplicate { status, answer }),
                        Some(kept) if kept.claimed_at_ms.saturating_add(CLAIM_STALE_MS) > now => {
                            // Another request is publishing this delivery.
                            if Instant::now() >= deadline {
                                return Err("the same delivery is still being handled".into());
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                        Some(kept) => kept.delivery_id,
                        None => attempt_id.to_owned(),
                    };
                    self.inner
                        .dedupe
                        .update(entry, claim(&delivery_id)?.into(), found.revision)
                        .await
                        .ok()
                        .map(|revision| (revision, delivery_id))
                }
            };
            match written {
                Some((revision, delivery_id)) => {
                    return Ok(Claim::Owned {
                        revision,
                        delivery_id,
                    })
                }
                // Another request changed the entry first; look again.
                None if Instant::now() < deadline => {}
                None => return Err(unavailable()),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn decide(
        &self,
        endpoint: &Endpoint,
        request: &StoredRequest,
        checks: &[Check],
        dedupe_key: Option<&str>,
        mode: WebhookMode,
        attempt: u32,
        deadline_ms: u64,
    ) -> Result<Decision, String> {
        let deadline = super::now_ms().saturating_add(i64::try_from(deadline_ms).unwrap_or(0));
        let payload = serde_json::json!({
            "delivery": request.delivery(&request.attempt_id, checks, dedupe_key),
            "mode": match mode { WebhookMode::Async => "async", WebhookMode::Sync => "sync" },
            "attempt": attempt,
            "deadline": super::rfc3339(deadline),
        });
        let subject =
            super::decide_subject(&endpoint.record.deployment_id, &endpoint.record.endpoint);
        let reply = tokio::time::timeout(
            Duration::from_millis(deadline_ms),
            self.inner.nats.request(
                subject,
                serde_json::to_vec(&payload).unwrap_or_default().into(),
            ),
        )
        .await
        .map_err(|_| "decide handler missed its deadline".to_owned())?
        .map_err(|error| match error.kind() {
            async_nats::RequestErrorKind::NoResponders => "no decide handler is running".to_owned(),
            _ => "decide call failed".to_owned(),
        })?;
        serde_json::from_slice(&reply.payload).map_err(|_| "decide handler failed".to_owned())
    }

    /// Publish the `Received` event as `delivery_id`, which is also its
    /// JetStream message ID, so a publish retried after a lost confirmation
    /// is stored once.
    async fn publish(
        &self,
        request: &StoredRequest,
        delivery_id: &str,
        checks: &[Check],
        dedupe_key: Option<&str>,
    ) -> Result<(), String> {
        let publisher = self
            .inner
            .publisher
            .borrow()
            .clone()
            .ok_or("Trellis is not connected yet")?;
        let payload = serde_json::to_vec(&request.delivery(delivery_id, checks, dedupe_key))
            .map_err(|error| error.to_string())?;
        let descriptor = trellis_protocol::encode_event_descriptor_identity(
            "trellis.webhooks@v1",
            "Received",
            2,
        )
        .map_err(|error| error.to_string())?;
        let event = trellis_rs::client::PreparedTrellisEvent::restore(
            Self::received_subject(&request.deployment_id, &request.endpoint),
            payload.into(),
            descriptor,
            delivery_id.to_owned(),
            super::rfc3339(super::now_ms()),
        )
        .map_err(|error| error.to_string())?;
        publisher
            .publish_prepared(&event)
            .await
            .map_err(|_| "event storage unavailable".to_owned())
    }

    /// Verify one stored `async` request from the intake stream.
    pub(crate) async fn verify_stored(&self, message: async_nats::jetstream::Message) {
        use async_nats::jetstream::AckKind;
        let Ok(request) = serde_json::from_slice::<StoredRequest>(&message.payload) else {
            let _ = message.ack_with(AckKind::Term).await;
            return;
        };
        let attempt = message
            .info()
            .map(|info| u32::try_from(info.delivered).unwrap_or(u32::MAX))
            .unwrap_or(1);
        let endpoint = self
            .directory()
            .by_name
            .get(&(request.deployment_id.clone(), request.endpoint.clone()))
            .cloned();
        let Some(endpoint) = endpoint else {
            // The endpoint is detached or its deployment gone; try later.
            let _ = message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(60))))
                .await;
            return;
        };
        if request.disposition == Disposition::Archived {
            // A rejected sync request stored for verifying again later.
            let _ = message.ack().await;
            return;
        }
        // Queued work was answered 2xx, so it is verified as async whatever
        // mode the endpoint has since been changed to.
        let verdict = self
            .verify(&endpoint, &request, WebhookMode::Async, attempt)
            .await;
        let retry = match &verdict {
            Verdict::Unset(_) => Some(Duration::from_secs(10)),
            Verdict::Unavailable(_) => Some(Duration::from_secs(5)),
            // 5 s, 30 s, 2 min, and 10 min, then every 10 min until the
            // request leaves the intake stream.
            Verdict::DecideFailed(_) => Some(Duration::from_secs(match attempt {
                0 | 1 => 5,
                2 => 30,
                3 => 120,
                _ => 600,
            })),
            _ => None,
        };
        let outcome = verdict.outcome(true);
        self.count(&endpoint.record.endpoint, outcome.outcome);
        let _ = self
            .inner
            .records
            .set_outcome(&request.attempt_id, outcome)
            .await;
        let _ = match retry {
            Some(delay) => message.ack_with(AckKind::Nak(Some(delay))).await,
            None => message.ack().await,
        };
    }

    /// Count a request outcome in `trellis.webhooks.requests`.
    pub(crate) fn count(&self, endpoint: &str, outcome: &str) {
        trellis_rs::telemetry::instruments::add_counter(
            trellis_rs::telemetry::instruments::CounterFamily::WebhookRequests,
            1,
            &[
                opentelemetry::KeyValue::new("trellis.webhook.endpoint", endpoint.to_owned()),
                opentelemetry::KeyValue::new("trellis.outcome", outcome.to_owned()),
            ],
        );
    }
}
