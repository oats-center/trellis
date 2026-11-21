//! `trellis.webhooks@v1`: endpoints and request records. Trellis checks the
//! caller's `attempts.*` capability before a handler runs.

use trellis_idl::WebhookVerifier;
use trellis_rs::service::{DeclaredRpcError, ServerError};
use trellis_runtime_apis::apis::trellis_webhooks_v1::Provider;

use super::{Endpoint, Webhooks};

fn declared(error_type: &'static str, message: &str) -> ServerError {
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        error_type,
        message,
        [("message", serde_json::Value::String(message.to_owned()))],
    ))
}

fn not_found(what: &str) -> ServerError {
    declared("trellis.webhooks@v1::NotFoundError", what)
}

fn unavailable(what: &str) -> ServerError {
    declared("trellis.webhooks@v1::Unavailable", what)
}

fn invalid(what: &str) -> ServerError {
    declared("trellis.webhooks@v1::ValidationError", what)
}

fn unexpected(error: impl std::fmt::Display) -> ServerError {
    tracing::error!(%error, "webhooks RPC failed");
    declared("trellis.webhooks@v1::UnexpectedError", "Request failed")
}

fn wire<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, ServerError> {
    serde_json::from_value(value).map_err(unexpected)
}

impl Webhooks {
    /// The URL senders use for one URL ID of an endpoint.
    fn url(&self, endpoint: &Endpoint, id: &str) -> String {
        format!(
            "{}/webhooks/{}/{id}",
            self.inner.origin, endpoint.record.service_name
        )
    }

    async fn describe(&self, endpoint: &Endpoint) -> Result<serde_json::Value, ServerError> {
        let record = &endpoint.record;
        let definition = &record.definition;
        let (deployment, name) = endpoint.key();
        let last_previous = self
            .inner
            .records
            .previous_match(&deployment, &name)
            .await
            .map_err(unexpected)?;
        let mut secrets = Vec::new();
        for verifier in &definition.verifiers {
            let fields: Vec<&String> = match verifier {
                WebhookVerifier::Hmac(hmac) => vec![&hmac.key],
                WebhookVerifier::Token { secret, .. } => vec![secret],
                WebhookVerifier::Basic { password, .. } => vec![password],
                _ => Vec::new(),
            };
            for field in fields {
                secrets.push(serde_json::json!({
                    "name": field,
                    "isSet": endpoint.text(field).is_some(),
                    "previousAcceptedUntil": endpoint.previous_accepted_until(field).map(super::rfc3339),
                    "previousLastMatchedAt": last_previous.map(super::rfc3339),
                }));
            }
        }
        let since = super::now_ms() - 86_400_000;
        let recent = self
            .inner
            .records
            .counts(&deployment, &name, since)
            .await
            .map_err(unexpected)?;
        let settings = &endpoint.settings;
        Ok(serde_json::json!({
            "service": record.deployment_id,
            "endpoint": record.endpoint,
            "title": record.title,
            "description": record.description,
            "url": self.url(endpoint, &record.current_url),
            "previousUrls": record.previous_urls.iter().map(|id| self.url(endpoint, id)).collect::<Vec<_>>(),
            "enabled": record.endpoint_enabled && record.deployment_enabled,
            "mode": match definition.mode { trellis_idl::WebhookMode::Async => "async", trellis_idl::WebhookMode::Sync => "sync" },
            "methods": definition.methods,
            "verifiers": definition.verifiers.iter().map(trellis_idl::webhook::describe_verifier).collect::<Vec<_>>(),
            "decides": definition.decide.is_some(),
            "unauthenticated": definition.verifiers.contains(&WebhookVerifier::None),
            "secrets": secrets,
            "settings": {
                "requestsPerSecond": settings.requests_per_second,
                "burst": settings.burst,
                "failuresPerMinute": settings.failures_per_minute,
                "maxBodyBytes": settings.max_body.min(self.inner.ceiling).to_string(),
                "dedupeWindowMs": settings.dedupe_window_ms.to_string(),
                "syncDecideDeadlineMs": settings.sync_decide_deadline_ms.to_string(),
                "asyncDecideDeadlineMs": settings.async_decide_deadline_ms.to_string(),
                "rotationOverlapMs": settings.rotation_overlap_ms.to_string(),
                "recordRetentionMs": settings.record_retention_ms.to_string(),
                "intakeRetentionMs": settings.intake_retention_ms.to_string(),
                "intakeMaxBytes": settings.intake_max_bytes.to_string(),
            },
            "recent": recent.into_iter().map(|(outcome, count)| serde_json::json!({"outcome": outcome, "count": count.to_string()})).collect::<Vec<_>>(),
        }))
    }

    /// Verify a stored request again with the current verifiers and secrets,
    /// against its original bytes and receive time.
    async fn reverify(&self, attempt_id: &str) -> Result<serde_json::Value, ServerError> {
        let records = &self.inner.records;
        let attempt = records
            .get(attempt_id)
            .await
            .map_err(unexpected)?
            .ok_or_else(|| not_found("no such request"))?;
        let sequence = attempt.stream_sequence.ok_or_else(|| {
            unavailable("this request was not stored, so it cannot be verified again")
        })?;
        let request = self
            .stored(&attempt.service, sequence)
            .await
            .map_err(unexpected)?
            .ok_or_else(|| unavailable("the request has left the intake stream"))?;
        let endpoint = self
            .directory()
            .by_name
            .get(&(attempt.service.clone(), attempt.endpoint.clone()))
            .cloned()
            .ok_or_else(|| unavailable("the endpoint is not served"))?;
        let verdict = self
            .verify(&endpoint, &request, trellis_idl::WebhookMode::Async, 1)
            .await;
        let outcome = verdict.outcome(attempt_id, true);
        self.count(&endpoint.record.endpoint, outcome.outcome);
        records
            .set_outcome(attempt_id, outcome)
            .await
            .map_err(unexpected)?;
        let attempt = records
            .get(attempt_id)
            .await
            .map_err(unexpected)?
            .ok_or_else(|| not_found("no such request"))?;
        serde_json::to_value(attempt).map_err(unexpected)
    }
}

/// Register the `trellis.webhooks@v1` handlers on the platform's provider.
pub(crate) fn register<P>(provider: &mut Provider<'_, P>, webhooks: Webhooks)
where
    P: trellis_rs::generated::ParticipantDescriptor,
{
    let shared = webhooks.clone();
    provider.register_endpoints_list(move |_context, input| {
        let webhooks = shared.clone();
        async move {
            let directory = webhooks.directory();
            let mut endpoints = Vec::new();
            for endpoint in directory.by_name.values() {
                if input
                    .service
                    .as_ref()
                    .is_some_and(|service| *service != endpoint.record.deployment_id)
                {
                    continue;
                }
                endpoints.push(webhooks.describe(endpoint).await?);
            }
            wire(serde_json::json!({ "endpoints": endpoints }))
        }
    });
    let shared = webhooks.clone();
    provider.register_attempts_query(move |_context, input| {
        let webhooks = shared.clone();
        async move {
            let limit = input.limit.unwrap_or(50).clamp(1, 500);
            let since = match &input.since {
                Some(since) => Some(
                    time::OffsetDateTime::parse(
                        &since.to_string(),
                        &time::format_description::well_known::Rfc3339,
                    )
                    .map_err(|_| invalid("since must be a timestamp"))?
                    .unix_timestamp()
                        * 1000,
                ),
                None => None,
            };
            let attempts = webhooks
                .inner
                .records
                .query(
                    &input.service,
                    input.endpoint.clone(),
                    input
                        .outcome
                        .as_ref()
                        .map(|outcome| outcome.as_str().to_owned()),
                    since,
                    input.before.as_ref().map(ToString::to_string),
                    limit + 1,
                )
                .await
                .map_err(unexpected)?;
            let more = attempts.len() > limit as usize;
            let attempts: Vec<_> = attempts.into_iter().take(limit as usize).collect();
            let next = more
                .then(|| attempts.last().map(|attempt| attempt.attempt_id.clone()))
                .flatten();
            wire(serde_json::json!({ "attempts": attempts, "next": next }))
        }
    });
    let shared = webhooks.clone();
    provider.register_attempts_inspect(move |_context, input| {
        let webhooks = shared.clone();
        async move {
            let attempt = webhooks
                .inner
                .records
                .get(&input.attempt_id.to_string())
                .await
                .map_err(unexpected)?
                .ok_or_else(|| not_found("no such request"))?;
            let delivery = match attempt.stream_sequence {
                Some(sequence) => webhooks
                    .stored(&attempt.service, sequence)
                    .await
                    .ok()
                    .flatten()
                    .map(|request| request.delivery(&request.attempt_id, &request.checks, None)),
                None => None,
            };
            wire(serde_json::json!({ "attempt": attempt, "delivery": delivery }))
        }
    });
    let shared = webhooks;
    provider.register_attempts_reverify(move |_context, input| {
        let webhooks = shared.clone();
        async move {
            let attempt = webhooks.reverify(&input.attempt_id.to_string()).await?;
            wire(serde_json::json!({ "attempt": attempt }))
        }
    });
    provider.register_decide(|_context, _input| async move {
        Err(invalid(
            "Decide describes what a service's decide handler receives; Trellis never serves it",
        ))
    });
}
