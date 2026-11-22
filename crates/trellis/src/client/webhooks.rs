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
                        tracing::error!(
                            delivery_id,
                            route = endpoint,
                            error_type = "UnexpectedError",
                            %error,
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
