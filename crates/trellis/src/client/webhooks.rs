//! Webhook endpoints a service declares in its IDL, and general helpers for
//! decide handlers.
//!
//! Trellis serves each endpoint, checks that requests are genuine, and
//! delivers accepted requests as `trellis.webhooks@v1` `Received` events
//! through the endpoint's own durable consumer.

use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::stream::BoxStream;
use futures_util::StreamExt as _;

pub use crate::__types::trellis::{
    WebhookDecideRequest, WebhookDecision, WebhookDelivery, WebhookField, WebhookResponse,
    WebhookVerdict,
};

use super::{ConsumerDescriptor, ConsumerHandle, TrellisClientError};

/// Why a webhook body could not be read as text, JSON, or form fields.
#[derive(Debug, thiserror::Error)]
pub enum WebhookBodyError {
    /// The body is not UTF-8 text.
    #[error("the webhook body is not UTF-8 text")]
    NotText(#[source] std::str::Utf8Error),
    /// The body is not JSON of the requested shape.
    #[error("the webhook body is not the expected JSON: {0}")]
    NotJson(#[source] serde_json::Error),
}

/// Text, JSON, and form views of a received webhook body, for
/// [`WebhookDelivery`]. `body` stays the bytes as received, so a handler that
/// checks a signature still checks those bytes.
///
/// # Examples
///
/// ```
/// use trellis_rs::client::{WebhookBody as _, WebhookDelivery};
///
/// fn event_id(delivery: &WebhookDelivery) -> Option<String> {
///     let payload: serde_json::Value = delivery.json().ok()?;
///     payload["id"].as_str().map(str::to_owned)
/// }
/// ```
pub trait WebhookBody {
    /// The body as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookBodyError::NotText`] when the body is not UTF-8.
    fn text(&self) -> Result<&str, WebhookBodyError>;

    /// The body parsed as JSON into `T`, such as [`serde_json::Value`].
    ///
    /// # Errors
    ///
    /// Returns [`WebhookBodyError::NotJson`] when the body is not JSON that
    /// `T` accepts.
    fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, WebhookBodyError>;

    /// The fields of an `application/x-www-form-urlencoded` body, in order.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookBodyError::NotText`] when the body is not UTF-8.
    fn form(&self) -> Result<Vec<(String, String)>, WebhookBodyError>;
}

impl WebhookBody for WebhookDelivery {
    fn text(&self) -> Result<&str, WebhookBodyError> {
        std::str::from_utf8(&self.body.0).map_err(WebhookBodyError::NotText)
    }

    fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, WebhookBodyError> {
        serde_json::from_slice(&self.body.0).map_err(WebhookBodyError::NotJson)
    }

    fn form(&self) -> Result<Vec<(String, String)>, WebhookBodyError> {
        Ok(url::form_urlencoded::parse(self.text()?.as_bytes())
            .into_owned()
            .collect())
    }
}

/// Generated metadata for one declared webhook endpoint.
pub trait WebhookDescriptor: ConsumerDescriptor {
    /// Whether the endpoint declares a decide handler.
    const DECIDES: bool;
}

/// A webhook endpoint the service declared.
#[derive(Clone)]
pub struct WebhookHandle<D> {
    consumer: ConsumerHandle<D>,
    client: crate::generated::Client,
}

impl<D: WebhookDescriptor> std::fmt::Debug for WebhookHandle<D> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookHandle")
            .field("endpoint", &D::NAME)
            .finish_non_exhaustive()
    }
}

/// A running decide handler. Dropping it stops answering.
#[derive(Debug)]
pub struct WebhookDecideHandle {
    _task: tokio_util::task::AbortOnDropHandle<()>,
}

impl<D: WebhookDescriptor> WebhookHandle<D> {
    pub(crate) fn from_generated(
        consumer: ConsumerHandle<D>,
        client: crate::generated::Client,
    ) -> Self {
        Self { consumer, client }
    }

    /// The endpoint's durable consumer.
    pub fn consumer(&self) -> &ConsumerHandle<D> {
        &self.consumer
    }

    /// Receive each accepted request. Delivery is at least once with no
    /// ordering guarantee, so processing must be safe to repeat.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint's consumer cannot be opened.
    pub async fn subscribe(
        &self,
    ) -> Result<BoxStream<'static, Result<WebhookDelivery, TrellisClientError>>, TrellisClientError>
    {
        self.consumer
            .subscribe::<crate::runtime_webhooks::events::Received>()
            .await
    }

    /// The subject this endpoint's decide calls arrive on, read from the
    /// endpoint's own event filter so it names exactly this deployment.
    fn decide_subject(&self) -> Option<String> {
        let filter = self.consumer.binding().filter_subjects.first()?;
        let mut tokens = filter.rsplit('.');
        let endpoint = tokens.next()?;
        let deployment = tokens.next()?;
        Some(format!("webhooks.v1.decide.{deployment}.{endpoint}"))
    }

    /// Answer the endpoint's decide calls. The handler runs after the standard
    /// verifiers and before duplicate detection, so it can run more than once
    /// for the same delivery. A handler that fails should return an error; no
    /// reply is sent, so a `sync` request gets 503 and an `async` one is tried
    /// again.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint declares no decide handler or the
    /// subscription cannot be opened.
    pub async fn decide<F, Fut, E>(
        &self,
        handler: F,
    ) -> Result<WebhookDecideHandle, TrellisClientError>
    where
        F: Fn(WebhookDecideRequest) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<WebhookDecision, E>> + Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        if !D::DECIDES {
            return Err(TrellisClientError::AuthorizationUnavailable(format!(
                "webhook {} declares no decide handler",
                D::NAME
            )));
        }
        let subject = self.decide_subject().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(format!(
                "webhook {} has no Trellis-provisioned binding",
                D::NAME
            ))
        })?;
        let lease = self
            .client
            .trellis_client()
            .acquire_transport(
                &[],
                std::slice::from_ref(&subject),
                Instant::now() + Duration::from_secs(10),
            )
            .await?;
        let nats = lease.nats().clone();
        let mut subscription = nats
            .queue_subscribe(subject, "trellis.webhooks.decide".into())
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
        nats.flush()
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
        let task = tokio::spawn(async move {
            let _lease = lease;
            while let Some(message) = subscription.next().await {
                let Some(reply) = message.reply.clone() else {
                    continue;
                };
                let Ok(request) = serde_json::from_slice::<WebhookDecideRequest>(&message.payload)
                else {
                    continue;
                };
                // The delivery ID is the reference Console's webhook records show.
                let delivery_id = request.delivery.delivery_id.to_string();
                let endpoint = request.delivery.endpoint.clone();
                match handler(request).await {
                    Ok(decision) => {
                        if let Ok(body) = serde_json::to_vec(&decision) {
                            let _ = nats.publish(reply, body.into()).await;
                        }
                    }
                    Err(error) => {
                        // The error's `Display` is the application's code and
                        // runs in this loop; a panic in it must not end the
                        // endpoint's decide subscription.
                        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            error.to_string()
                        }))
                        .unwrap_or_else(|_| "[unprintable cause]".to_owned());
                        tracing::error!(
                            delivery_id,
                            route = endpoint,
                            error_type = "UnexpectedError",
                            error,
                            "webhook decide handler failed",
                        );
                    }
                }
            }
        });
        Ok(WebhookDecideHandle {
            _task: tokio_util::task::AbortOnDropHandle::new(task),
        })
    }
}

/// Add the health check Trellis keeps for each of the service's webhook
/// endpoints, named `webhook:<endpoint>`. Trellis reports an endpoint
/// degraded while it is disabled, while a setting its verifiers read is
/// unset, or while its latest verified request was rejected; a check Trellis
/// does not answer reads as degraded.
pub(crate) fn add_health_checks(client: &super::TrellisClient) {
    let Ok(received) = trellis_protocol::derive_event_subject("trellis.webhooks@v1", "Received")
    else {
        return;
    };
    let received = format!("{received}.");
    for (name, binding) in client.availability().consumer_bindings() {
        // The filter ends in the endpoint's deployment and name tokens.
        let Some(tokens) = binding
            .filter_subjects
            .first()
            .and_then(|filter| filter.strip_prefix(&received))
        else {
            continue;
        };
        let subject = format!("webhooks.v1.health.{tokens}");
        let generations = client.transport_generations();
        client.health().add(format!("webhook:{name}"), move || {
            let (generations, subject) = (generations.clone(), subject.clone());
            async move {
                let deadline = Instant::now() + Duration::from_secs(5);
                let report = async {
                    let lease = generations
                        .acquire_for(std::slice::from_ref(&subject), &[], deadline)
                        .await
                        .ok()?;
                    let reply = tokio::time::timeout_at(
                        deadline.into(),
                        lease.nats().request(subject, bytes::Bytes::new()),
                    )
                    .await
                    .ok()?
                    .ok()?;
                    serde_json::from_slice::<serde_json::Value>(&reply.payload).ok()
                }
                .await;
                match report {
                    Some(report) if report["status"] == "ok" => super::ServiceHealthCheck::ok(),
                    Some(report) => super::ServiceHealthCheck::degraded(
                        report["summary"]
                            .as_str()
                            .unwrap_or("Trellis reported this endpoint degraded"),
                    ),
                    None => super::ServiceHealthCheck::degraded(
                        "Trellis did not report this endpoint's state",
                    ),
                }
            }
        });
    }
}

/// Hash algorithms [`webhook_hmac`] supports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebhookHmacAlgorithm {
    /// HMAC-SHA256.
    Sha256,
    /// HMAC-SHA512.
    Sha512,
}

/// Compute an HMAC of `message` with `key`.
#[must_use]
pub fn webhook_hmac(algorithm: WebhookHmacAlgorithm, key: &[u8], message: &[u8]) -> Vec<u8> {
    use hmac::{KeyInit as _, Mac as _};
    match algorithm {
        WebhookHmacAlgorithm::Sha256 => {
            let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("any key length");
            mac.update(message);
            mac.finalize().into_bytes().to_vec()
        }
        WebhookHmacAlgorithm::Sha512 => {
            let mut mac = hmac::Hmac::<sha2::Sha512>::new_from_slice(key).expect("any key length");
            mac.update(message);
            mac.finalize().into_bytes().to_vec()
        }
    }
}

/// Compare two byte strings in time that depends only on their lengths.
#[must_use]
pub fn webhook_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

/// Check a hex, base64, or base64url signature against an HMAC of `message`,
/// comparing in constant time.
#[must_use]
pub fn verify_webhook_hmac(
    algorithm: WebhookHmacAlgorithm,
    key: &[u8],
    message: &[u8],
    signature: &str,
) -> bool {
    let expected = webhook_hmac(algorithm, key, message);
    let decoded = decode_hex(signature)
        .or_else(|| {
            base64::engine::general_purpose::STANDARD
                .decode(signature)
                .ok()
        })
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(signature.trim_end_matches('='))
                .ok()
        });
    decoded.is_some_and(|decoded| webhook_equal(&decoded, &expected))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

/// Whether a timestamp the sender signed is within `tolerance` of the time
/// Trellis received the request.
#[must_use]
pub fn within_webhook_window(
    sent_at: time::OffsetDateTime,
    received_at: time::OffsetDateTime,
    tolerance: Duration,
) -> bool {
    (received_at - sent_at).unsigned_abs() <= tolerance
}

/// Build the response a `sync` decide handler sends the sender. Trellis
/// refuses HTML and script types, cookies, and more than 16 headers or 64 KiB.
#[must_use]
pub fn webhook_response(status: u16, content_type: Option<&str>, body: &[u8]) -> WebhookResponse {
    serde_json::from_value(serde_json::json!({
        "status": status,
        "headers": content_type
            .map(|value| vec![serde_json::json!({"name": "content-type", "value": value})])
            .unwrap_or_default(),
        "body": base64::engine::general_purpose::STANDARD.encode(body),
    }))
    .expect("a webhook response is always valid")
}
