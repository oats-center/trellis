//! Generated-route verification through the connected runtime's shared authority owner.

use bytes::Bytes;
use futures_util::future::BoxFuture;
use trellis_protocol::{CatalogActionIdentity, SessionRequestProof};

use crate::client::AuthorizationOwner;
use crate::service::{RequestContext, RequestValidation, RequestValidator, ServerError};

pub use crate::client::AuthenticatedCaller;

/// Local request and publication verifier sharing the connected runtime's authority owner.
#[derive(Clone, Debug)]
pub struct LocalAuthVerifier {
    owner: AuthorizationOwner,
    transport: async_nats::Client,
    contract_id: String,
}

impl LocalAuthVerifier {
    /// Use the ordinary connected transport and its shared, pinned authority owner.
    pub fn new(
        owner: AuthorizationOwner,
        transport: async_nats::Client,
        api_id: impl Into<String>,
    ) -> Self {
        Self {
            owner,
            transport,
            contract_id: api_id.into(),
        }
    }

    pub(crate) fn api_id(&self) -> &str {
        &self.contract_id
    }

    pub(crate) async fn historical_status(
        &self,
        headers: Option<&async_nats::HeaderMap>,
    ) -> Result<trellis_protocol::VerifiedIssuerMetadata, EventVerificationFailure> {
        let headers = headers
            .ok_or_else(|| EventVerificationFailure::rejected("missing event proof headers"))?;
        let event: trellis_protocol::SessionEvent =
            serde_json::from_str(&required_event_header(headers, "Trellis-Session-Event")?)
                .map_err(|error| EventVerificationFailure::rejected(error.to_string()))?;
        self.owner
            .historical_status(&self.transport, &event.authority_digest)
            .await
            .map_err(|error| match error {
                crate::client::TrellisClientError::AuthorizationUnavailable(_) => {
                    EventVerificationFailure::retryable(error.to_string())
                }
                _ => EventVerificationFailure::rejected(error.to_string()),
            })
    }

    async fn verify_request_inner(
        &self,
        subject: &str,
        payload: &[u8],
        context: &RequestContext,
        possession_only: bool,
    ) -> Result<RequestValidation, ServerError> {
        let Some(request) = context.session_request.as_ref() else {
            return Ok(RequestValidation::denied());
        };
        // Header assertions cannot replace the actual publication destination.
        if request.subject != subject || request.reply_subject != context.reply_to {
            return Ok(RequestValidation::denied());
        }
        let Some(proof) = context
            .proof
            .as_deref()
            .and_then(|value| SessionRequestProof::parse(value).ok())
        else {
            return Ok(RequestValidation::denied());
        };
        let transport = context
            .transport
            .lease()
            .map_or(&self.transport, |lease| lease.nats());
        if context
            .required_permission
            .as_ref()
            .is_some_and(|route| route.api != request.api_id || route.name != request.action)
        {
            return Ok(RequestValidation::denied());
        }
        let verified = if possession_only {
            self.owner
                .verify_possession(transport, request, payload, &proof)
                .await
        } else {
            let Some(route) = context.required_permission.as_ref() else {
                return Ok(RequestValidation::denied());
            };
            self.owner
                .verify_request(transport, request, payload, &proof, &route.catalog_action())
                .await
        };
        let verified = match verified {
            Ok(verified) => verified,
            Err(error) => {
                tracing::debug!(subject, %error, "session opening rejected");
                return Ok(RequestValidation::denied());
            }
        };
        if possession_only && context.handler_authorizes {
            let Some(route) = context.required_permission.as_ref() else {
                return Ok(RequestValidation::denied());
            };
            let provider = match self.owner.own_authority() {
                Ok(provider) => provider,
                Err(_) => return Ok(RequestValidation::denied()),
            };
            let certificate = match self
                .owner
                .authorize_provider(transport, &provider, &route.api, &route.catalog_action())
                .await
            {
                Ok(certificate) => certificate,
                Err(_) => return Ok(RequestValidation::denied()),
            };
            if certificate.generation != request.api_generation {
                return Ok(RequestValidation::denied());
            }
        }
        self.owner
            .assert_current(verified.authority())
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        let result = RequestValidation {
            allowed: true,
            caller: Some(verified.caller().clone()),
            inbox_prefix: Some(verified.caller().inbox_prefix.clone()),
        };
        context.retain_verified_request(verified)?;
        Ok(result)
    }

    /// Verify a live publication, or a historical publication with original broker chronology
    /// and fresh signed cutoff evidence. Publisher time is never historical broker evidence.
    pub(crate) async fn verify_event(
        &self,
        subject: &str,
        payload: &[u8],
        headers: Option<&async_nats::HeaderMap>,
        api_id: &str,
        action: &CatalogActionIdentity,
        historical: Option<(
            &trellis_protocol::OriginalEventPublication,
            &trellis_protocol::VerifiedIssuerMetadata,
        )>,
    ) -> Result<super::runtime_facade::ServiceEventPublisherContext, EventVerificationFailure> {
        let headers = headers
            .ok_or_else(|| EventVerificationFailure::rejected("missing event proof headers"))?;
        let event: trellis_protocol::SessionEvent =
            serde_json::from_str(&required_event_header(headers, "Trellis-Session-Event")?)
                .map_err(|error| EventVerificationFailure::rejected(error.to_string()))?;
        if headers
            .get("authorization-context")
            .is_some_and(|digest| digest.as_str() != event.authority_digest)
        {
            return Err(EventVerificationFailure::rejected(
                "event authority header mismatch",
            ));
        }
        if event.subject != subject || event.api_id != api_id || event.action != action.name {
            return Err(EventVerificationFailure::rejected(
                "event does not match registered descriptor and transport",
            ));
        }
        let proof =
            trellis_protocol::SessionEventProof::parse(required_event_header(headers, "proof")?)
                .map_err(|error| EventVerificationFailure::rejected(error.to_string()))?;
        if action.kind != trellis_protocol::ApiSurfaceKind::Event
            || action.direction != trellis_protocol::PermissionAction::Publish
        {
            return Err(EventVerificationFailure::rejected(
                "event requires a registered publish action",
            ));
        }
        let authority = if let Some((original_publication, status)) = historical {
            self.owner
                .verify_historical_event(
                    &self.transport,
                    crate::client::HistoricalEventVerificationInput {
                        event: &event,
                        payload,
                        proof: &proof,
                        action,
                        original_publication,
                        status,
                    },
                )
                .await
                .map(|verified| verified.authority().clone())
                .map_err(|error| match error {
                    crate::client::TrellisClientError::AuthorizationUnavailable(_) => {
                        EventVerificationFailure::retryable(error.to_string())
                    }
                    _ => EventVerificationFailure::rejected(error.to_string()),
                })?
        } else {
            let authority = self
                .owner
                .resolve_authority(&self.transport, &event.authority_digest)
                .await
                .map_err(|error| match error {
                    crate::client::TrellisClientError::AuthorizationUnavailable(_) => {
                        EventVerificationFailure::retryable(error.to_string())
                    }
                    _ => EventVerificationFailure::rejected(error.to_string()),
                })?;
            self.owner
                .authorize_action(&self.transport, &authority, api_id, action)
                .await
                .map_err(|error| match error {
                    crate::client::TrellisClientError::AuthorizationUnavailable(_) => {
                        EventVerificationFailure::retryable(error.to_string())
                    }
                    _ => EventVerificationFailure::rejected(error.to_string()),
                })?;
            let payload_sha256: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(payload).into();
            trellis_protocol::verify_session_event(
                trellis_protocol::SessionEventVerificationInput {
                    authority: &authority,
                    event: &event,
                    payload_sha256,
                    proof: &proof,
                    policy: &self.owner.policy(),
                    original_publication: None,
                    known_revoked: false,
                },
            )
            .map_err(|error| EventVerificationFailure::rejected(error.to_string()))?;
            self.owner
                .assert_current(&authority)
                .map_err(|error| match error {
                    crate::client::TrellisClientError::AuthorizationUnavailable(_) => {
                        EventVerificationFailure::retryable(error.to_string())
                    }
                    _ => EventVerificationFailure::rejected(error.to_string()),
                })?;
            authority.verified().as_ref().clone()
        };
        let authority = authority.authority();
        if headers
            .get("session-key")
            .is_some_and(|key| key.as_str() != authority.session_public_key)
        {
            return Err(EventVerificationFailure::rejected(
                "event session key header mismatch",
            ));
        }
        let (kind, deployment_id, instance_id, participant_id) = match &authority.binding {
            trellis_protocol::SessionBinding::Service {
                participant_id,
                deployment_id,
                instance_id,
            } => (
                "service",
                Some(deployment_id.clone()),
                Some(instance_id.clone()),
                Some(participant_id.clone()),
            ),
            trellis_protocol::SessionBinding::Device {
                participant_id,
                deployment_id,
                instance_id,
                ..
            } => (
                "device",
                Some(deployment_id.clone()),
                Some(instance_id.clone()),
                Some(participant_id.clone()),
            ),
            trellis_protocol::SessionBinding::Browser { .. }
            | trellis_protocol::SessionBinding::Native { .. } => ("user", None, None, None),
        };
        Ok(super::runtime_facade::ServiceEventPublisherContext {
            kind: kind.to_owned(),
            deployment_id,
            instance_id,
            contract_id: participant_id,
            contract_digest: None,
            session_status: if historical.is_some() {
                "historical"
            } else {
                "active"
            }
            .to_owned(),
        })
    }
}

impl RequestValidator for LocalAuthVerifier {
    fn validate<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        Box::pin(self.verify_request_inner(subject, payload, context, false))
    }

    fn validate_possession<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        Box::pin(self.verify_request_inner(subject, payload, context, true))
    }

    fn revalidate_current<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<bool, ServerError>> {
        Box::pin(async move {
            let Some(verified) = context.verified_request() else {
                return Ok(false);
            };
            if self.owner.assert_current(verified.authority()).is_err() {
                return Ok(false);
            }
            let Some(action) = verified.authorized_action() else {
                return Ok(false);
            };
            let transport = context
                .transport
                .lease()
                .map_or(&self.transport, |lease| lease.nats());
            let provider = match self.owner.own_authority() {
                Ok(provider) => provider,
                Err(_) => return Ok(false),
            };
            if self
                .owner
                .authorize_provider(transport, &provider, &verified.request().api_id, action)
                .await
                .is_err()
            {
                return Ok(false);
            }
            if verified.handler_authorized() {
                return Ok(self.owner.assert_current(verified.authority()).is_ok());
            }
            Ok(self
                .owner
                .authorize_action(
                    transport,
                    verified.authority(),
                    &verified.request().api_id,
                    action,
                )
                .await
                .is_ok())
        })
    }
}

/// Durable event verification outcome used to choose redelivery.
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
    headers: &async_nats::HeaderMap,
    name: &str,
) -> Result<String, EventVerificationFailure> {
    let mut values = headers.get_all(name);
    let value = values
        .next()
        .filter(|value| !value.as_str().is_empty())
        .ok_or_else(|| {
            EventVerificationFailure::rejected(format!("missing event {name} header"))
        })?;
    if values.next().is_some() {
        return Err(EventVerificationFailure::rejected(format!(
            "duplicate event {name} header"
        )));
    }
    Ok(value.as_str().to_owned())
}

/// Canonical base64url SHA-256 payload digest used by request/event proofs.
pub fn payload_hash_base64url(payload: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(payload))
}
