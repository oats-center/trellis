//! Local context-bound request and event verification for connected services.
//!
//! Connected services verify v2 request/event proofs entirely against
//! in-process state: the digest-keyed [`AuthorizationProviderCache`] (verified
//! caller contexts resolved through the connected NATS authorization registry,
//! live revocation state, and verification policy) plus the exact permission
//! surface recorded at route/event registration time. Verification performs no
//! SQLite, HTTP, Auth RPC, or NATS registry I/O on cache hits: cache hits read
//! only memory. Unknown context digests are resolved from the registry at most
//! once per digest, and revocation watch updates are applied immediately.

use base64::Engine;
use bytes::Bytes;
use futures_util::future::BoxFuture;

use crate::client::{
    AuthorizationProviderCache, AuthorizationVerificationCore, EventVerificationInput,
    RequestVerificationInput,
};
use crate::service::{
    RequestContext, RequestValidation, RequestValidator, RoutePermission, ServerError,
};

pub use crate::client::VerifiedCaller;

/// Local request/event verifier for one connected service runtime.
///
/// A verifier without a provider cache (constructed before the connected
/// authorization registry is available) denies every request/event: local
/// verification fails closed until provider evidence is ready.
#[derive(Clone)]
pub struct LocalAuthVerifier {
    provider: Option<AuthorizationProviderCache>,
    /// Versioned API/contract identity of the connected service, used to build
    /// exact permission atoms for its own routed surfaces.
    contract_id: String,
    verification: AuthorizationVerificationCore,
}

impl LocalAuthVerifier {
    /// Build a verifier over the client's provider cache and the connected
    /// service's versioned contract/API identity.
    #[doc = concat!("Trellis API operation `", stringify!(new), "`.")]
    pub fn new(provider: Option<AuthorizationProviderCache>, api_id: impl Into<String>) -> Self {
        Self {
            provider,
            contract_id: api_id.into(),
            verification: AuthorizationVerificationCore::new(),
        }
    }

    pub(crate) fn api_id(&self) -> &str {
        &self.contract_id
    }

    /// Verify a v1 request proof against the digest-keyed context and exact route.
    async fn verify_request(
        &self,
        subject: &str,
        payload: &[u8],
        context: &RequestContext,
    ) -> Result<RequestValidation, ServerError> {
        self.verify_request_inner(subject, payload, context, true)
            .await
    }

    async fn verify_request_inner(
        &self,
        subject: &str,
        payload: &[u8],
        context: &RequestContext,
        require_exact_permission: bool,
    ) -> Result<RequestValidation, ServerError> {
        let session_key = context
            .session_key
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ServerError::MissingSessionKey {
                subject: subject.to_string(),
            })?;
        let proof = context
            .proof
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ServerError::MissingProof {
                subject: subject.to_string(),
            })?;
        let authorization_context = context.authorization_context.clone().ok_or_else(|| {
            ServerError::MissingAuthorizationContext {
                subject: subject.to_string(),
            }
        })?;
        let reply = context
            .reply_to
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ServerError::MissingReply {
                subject: subject.to_string(),
            })?;
        let iat = context.iat.ok_or_else(|| {
            ServerError::Nats(format!("authenticated request for '{subject}' has no iat"))
        })?;
        let request_id = context.request_id.clone().ok_or_else(|| {
            ServerError::Nats(format!(
                "authenticated request for '{subject}' has no request-id"
            ))
        })?;
        let route_permission = context.required_permission.as_ref();
        if require_exact_permission && route_permission.is_none() {
            tracing::warn!(
                subject,
                "authenticated request has no exact route permission"
            );
            return Ok(RequestValidation::denied());
        }

        // Resolve the presented context digest through the provider cache;
        // cache hits are memory-only, unknown digests resolve once through the
        // connected NATS authorization registry.
        let Some(provider) = self.provider.as_ref() else {
            tracing::warn!(subject, "local authorization provider unavailable");
            return Ok(RequestValidation::denied());
        };
        if !provider
            .health()
            .map(|health| health.healthy)
            .unwrap_or(false)
        {
            tracing::warn!(subject, "local authorization provider is not ready");
            return Ok(RequestValidation::denied());
        }
        let policy = match provider.policy() {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(subject, %error, "local authorization policy unavailable");
                return Ok(RequestValidation::denied());
            }
        };
        let verified = match provider
            .resolve_context(&authorization_context, policy.now_unix_seconds)
            .await
        {
            Ok(verified) => verified,
            Err(error) => {
                tracing::warn!(subject, %error, "local authorization context unavailable");
                return Ok(RequestValidation::denied());
            }
        };
        // Memory-only revocation read: the provider watch applies revocations
        // immediately and never un-revokes a digest. A revoked context denies.
        match provider.revocation_time(&authorization_context) {
            Ok(Some(_)) => {
                return Ok(RequestValidation::denied());
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(subject, %error, "local revocation evidence unavailable");
                return Ok(RequestValidation::denied());
            }
        }
        let permission = match route_permission
            .map(RoutePermission::permission_atom)
            .transpose()
        {
            Ok(permission) => permission,
            Err(error) => {
                tracing::debug!(subject, %error, "invalid route permission");
                return Ok(RequestValidation::denied());
            }
        };
        let required_permissions = permission.as_slice();
        let verified = match self.verification.verify_request(RequestVerificationInput {
            context: &verified,
            context_digest: &authorization_context,
            subject,
            payload,
            iat,
            request_id: &request_id,
            reply_subject: Some(&reply),
            proof: &proof,
            policy: &policy,
            required_permissions,
        }) {
            Ok(verified) => verified,
            Err(error) => {
                tracing::debug!(subject, %error, "local request proof rejected");
                return Ok(RequestValidation::denied());
            }
        };
        let caller = verified.caller().clone();
        if caller.session_key != session_key {
            return Ok(RequestValidation::denied());
        }
        let result = RequestValidation {
            allowed: true,
            caller: Some(caller),
            inbox_prefix: Some(verified.caller().inbox_prefix.clone()),
        };
        if provider
            .revocation_time(&authorization_context)
            .map_or(true, |revoked_at| revoked_at.is_some())
        {
            return Ok(RequestValidation::denied());
        }
        Ok(result)
    }

    /// Verify a v1 event proof against the digest-keyed context, historical
    /// policy, and the publisher's exact `Publish` atom for `event_name`.
    ///
    /// `event_name` is the API-local event name derived from the registered
    /// event descriptor, never parsed from the subject on the hot path.
    pub(crate) async fn verify_event(
        &self,
        subject: &str,
        payload: &[u8],
        headers: Option<&async_nats::header::HeaderMap>,
        expected_descriptor_identity: &str,
    ) -> Result<super::runtime_facade::ServiceEventPublisherContext, EventVerificationFailure> {
        let Some(headers) = headers else {
            return Err(EventVerificationFailure::rejected(
                "missing event proof headers",
            ));
        };
        let session_key = required_event_header(headers, "session-key")?;
        let proof = required_event_header(headers, "proof")?;
        let authorization_context = required_event_header(headers, "authorization-context")?;
        let event_id = required_event_header(headers, "Nats-Msg-Id")?;
        let event_time = required_event_header(headers, "Trellis-Event-Time")?;
        let descriptor_identity = required_event_header(headers, "Trellis-Event-Descriptor")?;

        // Historical resolution retains signed contexts after expiry; the
        // strict eventTime window is enforced below by the protocol verifier.
        let Some(provider) = self.provider.as_ref() else {
            tracing::warn!(subject, "local authorization provider unavailable");
            return Err(EventVerificationFailure::retryable(format!(
                "local authorization context unavailable for {subject}"
            )));
        };
        if !provider
            .health()
            .map(|health| health.healthy)
            .unwrap_or(false)
        {
            tracing::warn!(subject, "local authorization provider is not ready");
            return Err(EventVerificationFailure::retryable(format!(
                "local authorization context unavailable for {subject}"
            )));
        }
        let policy = match provider.policy() {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(subject, %error, "local event policy unavailable");
                return Err(EventVerificationFailure::retryable(format!(
                    "local event policy unavailable for {subject}"
                )));
            }
        };
        let context = match provider
            .resolve_event_context_for_verification(
                &authorization_context,
                time::OffsetDateTime::parse(
                    &event_time,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|value| value.unix_timestamp())
                .unwrap_or(policy.now_unix_seconds),
            )
            .await
        {
            Ok(context) => context,
            Err(error) => {
                tracing::warn!(subject, %error, "local event context resolution failed");
                return Err(error);
            }
        };
        if session_key
            != base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(context.session_key().as_bytes())
        {
            return Err(EventVerificationFailure::rejected(
                "session key does not match authorization context",
            ));
        }
        // Memory-only revocation read; the provider watch applies revocations
        // immediately. A revoked context denies.
        let revoked_at = match provider.revocation_time(&authorization_context) {
            Ok(Some(revoked_at)) => Some(revoked_at),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(subject, %error, "local revocation evidence unavailable");
                return Err(EventVerificationFailure::retryable(format!(
                    "local revocation evidence unavailable for {subject}"
                )));
            }
        };
        // The caller supplies the API identity and event name from its
        // precompiled descriptor; the verifier never infers either from the
        // transport subject or the connected service's own participant.
        if descriptor_identity != expected_descriptor_identity {
            return Err(EventVerificationFailure::rejected(
                "event descriptor identity does not match the registered listener",
            ));
        }
        let verified_event = self
            .verification
            .verify_event(EventVerificationInput {
                context: &context,
                context_digest: &authorization_context,
                subject,
                descriptor_identity: &descriptor_identity,
                payload,
                event_id: &event_id,
                event_time: &event_time,
                proof: &proof,
                policy: &policy,
                revoked_at,
            })
            .map_err(|error| {
                tracing::debug!(subject, %error, "local event proof rejected");
                EventVerificationFailure::rejected(format!("invalid event proof: {error}"))
            })?;
        let publisher = verified_event.publisher();
        let result = super::runtime_facade::ServiceEventPublisherContext {
            kind: publisher.kind.clone(),
            deployment_id: publisher.deployment_id.clone(),
            instance_id: publisher.instance_id.clone(),
            contract_id: Some(publisher.participant_id.clone()),
            contract_digest: None,
            session_status: "active".to_owned(),
        };
        if provider
            .revocation_time(&authorization_context)
            .map_err(|error| EventVerificationFailure::retryable(error.to_string()))?
            .is_some()
        {
            return Err(EventVerificationFailure::rejected(
                "authorization context is revoked",
            ));
        }
        Ok(result)
    }
}

impl RequestValidator for LocalAuthVerifier {
    fn validate<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        Box::pin(async move { self.verify_request(subject, payload, context).await })
    }

    fn validate_possession<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        Box::pin(async move {
            self.verify_request_inner(subject, payload, context, false)
                .await
        })
    }

    fn revalidate_current<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<bool, ServerError>> {
        Box::pin(async move {
            let (Some(provider), Some(digest), Some(route)) = (
                self.provider.as_ref(),
                context.authorization_context.as_deref(),
                context.required_permission.as_ref(),
            ) else {
                return Ok(false);
            };
            let Ok(permission) = route.permission_atom() else {
                return Ok(false);
            };
            Ok(provider.current_context_allows(digest, &permission))
        })
    }
}

/// Two-way durable event verification outcome used to choose redelivery.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum EventVerificationFailure {
    /// Authorization infrastructure is temporarily unavailable; redelivery is required.
    Retryable(String),
    /// The signed event is permanently invalid or unauthorized.
    Rejected(String),
}

impl EventVerificationFailure {
    pub(crate) fn retryable(message: impl Into<String>) -> Self {
        Self::Retryable(message.into())
    }

    pub(crate) fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(message.into())
    }

    /// Return the diagnostic message without changing its retry classification.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Retryable(message) | Self::Rejected(message) => message,
        }
    }
}

impl std::fmt::Display for EventVerificationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

fn required_event_header(
    headers: &async_nats::header::HeaderMap,
    name: &str,
) -> Result<String, EventVerificationFailure> {
    headers
        .get(name)
        .map(|value| value.as_str().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| EventVerificationFailure::rejected(format!("missing event {name} header")))
}

/// Canonical base64url SHA-256 payload digest used by request/event proofs.
#[doc = concat!("Trellis API operation `", stringify!(payload_hash_base64url), "`.")]
pub fn payload_hash_base64url(payload: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(payload))
}
