//! Runtime-local request/event verification backed by the validator cache.
//!
//! The runtime verifier checks v1 request and event proofs entirely against
//! in-process state: the shared [`AuthorizationProviderCache`] (verified contexts,
//! online issuer keys, and full revocation timestamps) plus a precompiled
//! route/permission index built once at startup from generated API descriptors.
//! Cache-hit verification performs no SQLite, HTTP, Auth RPC, or
//! NATS registry I/O; unknown context digests are resolved from the registry
//! through coalesced registry reads and expiry-bounded revocation watches.

use std::sync::Arc;

use base64::Engine;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use trellis_protocol::{AuthorizationEventPublisher, PermissionAtom, VerifiedAuthorizationContext};
use trellis_rs::client::{
    AuthorizationProviderCache, AuthorizationRegistryBinding, AuthorizationVerificationCore,
    EventVerificationInput, RequestVerificationInput, RuntimeAuthorizationTrust,
};
use trellis_rs::service::{
    RequestContext, RequestValidation, RequestValidator, ServerError, VerifiedCaller,
};

use super::AuthorizationStateError;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeAuthorizationRequestVerificationInput<'a> {
    pub(crate) subject: &'a str,
    pub(crate) payload: &'a [u8],
    pub(crate) session_key: &'a str,
    pub(crate) proof: &'a str,
    pub(crate) authorization_context: &'a str,
    pub(crate) iat: i64,
    pub(crate) request_id: &'a str,
    pub(crate) reply: Option<&'a str>,
    pub(crate) required_permissions: &'a [PermissionAtom],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeAuthorizationEventVerificationInput<'a> {
    pub(crate) subject: &'a str,
    pub(crate) descriptor_identity: &'a str,
    pub(crate) payload: &'a [u8],
    pub(crate) session_key: &'a str,
    pub(crate) proof: &'a str,
    pub(crate) authorization_context: &'a str,
    pub(crate) event_id: &'a str,
    pub(crate) event_time: &'a str,
}

pub(crate) struct VerifiedRuntimeEvent {
    pub(crate) publisher: AuthorizationEventPublisher,
    pub(crate) owner_contract_id: String,
    pub(crate) owner_event_name: String,
}

fn provider_error(error: trellis_rs::client::TrellisClientError) -> AuthorizationStateError {
    AuthorizationStateError::Storage(error.to_string())
}

/// Verified request material handed to Auth RPC handlers.
pub(crate) struct VerifiedRequest {
    pub(crate) caller: VerifiedCaller,
    pub(crate) context: VerifiedAuthorizationContext,
}

/// Runtime-local verifier shared by the Auth RPC provider and built-in routers.
#[derive(Clone)]
pub(crate) struct RuntimeAuthVerifier {
    source: Arc<AuthorizationProviderCache>,
    verification: AuthorizationVerificationCore,
}

impl std::fmt::Debug for RuntimeAuthVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAuthVerifier")
            .finish_non_exhaustive()
    }
}

impl RuntimeAuthVerifier {
    pub(crate) fn new(source: Arc<AuthorizationProviderCache>) -> Self {
        Self {
            source,
            verification: AuthorizationVerificationCore::new(),
        }
    }

    /// Require one additional exact permission from an already-cached current context.
    pub(crate) fn require_cached_permission(
        &self,
        context_digest: &str,
        permission: &PermissionAtom,
    ) -> Result<(), AuthorizationStateError> {
        self.require_healthy()?;
        if self
            .source
            .runtime_revocation_time(context_digest)
            .map_err(provider_error)?
            .is_some()
        {
            return Err(denied("request is not granted by the active authority"));
        }
        let context = self
            .source
            .runtime_lease_cached_context(context_digest)
            .map_err(provider_error)?
            .ok_or_else(|| denied("authorization context is not cached"))?;
        if context.allows(permission)
            && self
                .source
                .runtime_revocation_time(context_digest)
                .map_err(provider_error)?
                .is_none()
        {
            Ok(())
        } else {
            Err(denied("request is not granted by the active authority"))
        }
    }

    /// Verify a v1 request proof against the exact routed permission.
    ///
    /// `reply` must be the actual NATS reply inbox (`message.reply`); the
    /// proof is bound to it. Verification happens only after full proof, time,
    /// revocation and exact permission checks succeed.
    pub(crate) async fn verify_request(
        &self,
        input: RuntimeAuthorizationRequestVerificationInput<'_>,
    ) -> Result<VerifiedRequest, AuthorizationStateError> {
        let observation = trellis_rs::telemetry::lifecycle::Observation::start(
            trellis_rs::telemetry::instruments::DurationFamily::AuthVerification,
            vec![trellis_rs::telemetry::KeyValue::new(
                "trellis.purpose",
                "request",
            )],
            "cancelled",
        );
        let result = self.verify_request_inner(input).await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => verification_outcome(error),
        };
        observation.finish(outcome);
        result
    }

    async fn verify_request_inner(
        &self,
        input: RuntimeAuthorizationRequestVerificationInput<'_>,
    ) -> Result<VerifiedRequest, AuthorizationStateError> {
        let RuntimeAuthorizationRequestVerificationInput {
            subject,
            payload,
            session_key,
            proof,
            authorization_context,
            iat,
            request_id,
            reply,
            required_permissions,
        } = input;
        let now = now_seconds()?;
        if proof.is_empty() || authorization_context.is_empty() {
            return Err(denied("request proof headers are missing"));
        }
        let reply = reply
            .filter(|reply| !reply.is_empty())
            .ok_or_else(|| denied("request reply is missing"))?;
        self.require_healthy()?;
        if self
            .source
            .runtime_revocation_time(authorization_context)
            .map_err(provider_error)?
            .is_some()
        {
            return Err(denied("request is not granted by the active authority"));
        }
        let context = self
            .source
            .resolve_admission_context(authorization_context, now)
            .await
            .map_err(provider_error)?;
        let mut policy = self.source.runtime_policy().map_err(provider_error)?;
        policy.now_unix_seconds = now;
        let verified = self
            .verification
            .verify_request(RequestVerificationInput {
                context: &context,
                context_digest: authorization_context,
                subject,
                payload,
                iat,
                request_id,
                reply_subject: Some(reply),
                proof,
                policy: &policy,
                required_permissions,
            })
            .map_err(|error| {
                denied(format!(
                    "request is not granted by the active authority: {error}"
                ))
            })?;
        if verified.caller().session_key != session_key {
            return Err(denied("session key does not match authorization context"));
        }
        let result = VerifiedRequest {
            caller: verified.caller().clone(),
            context: verified.context().clone(),
        };
        if self
            .source
            .runtime_revocation_time(authorization_context)
            .map_err(provider_error)?
            .is_some()
        {
            return Err(denied("request is not granted by the active authority"));
        }
        Ok(result)
    }

    ///
    /// Historical events must fall within the signed context window. Published
    /// context revocation invalidates every event bound to that context.
    pub(crate) async fn verify_event(
        &self,
        input: RuntimeAuthorizationEventVerificationInput<'_>,
    ) -> Result<VerifiedRuntimeEvent, trellis_rs::service::EventVerificationFailure> {
        use trellis_rs::service::EventVerificationFailure;

        let RuntimeAuthorizationEventVerificationInput {
            subject,
            descriptor_identity,
            payload,
            session_key,
            proof,
            authorization_context,
            event_id,
            event_time,
        } = input;

        let now = now_seconds()
            .map_err(|error| EventVerificationFailure::Retryable(error.to_string()))?;
        if proof.is_empty()
            || authorization_context.is_empty()
            || event_id.is_empty()
            || event_time.is_empty()
            || descriptor_identity.is_empty()
        {
            return Err(EventVerificationFailure::Rejected(
                "event proof headers are missing".into(),
            ));
        }
        if !self.source.runtime_healthy() {
            return Err(EventVerificationFailure::Retryable(
                "authorization validator is not ready".into(),
            ));
        }
        let revoked_at = self
            .source
            .runtime_revocation_time(authorization_context)
            .map_err(|error| EventVerificationFailure::Retryable(error.to_string()))?;
        let historical_time =
            time::OffsetDateTime::parse(event_time, &time::format_description::well_known::Rfc3339)
                .map_err(|_| EventVerificationFailure::Rejected("event time is invalid".into()))?
                .unix_timestamp();
        let context = self
            .source
            .runtime_resolve_event_context_for_verification(authorization_context, historical_time)
            .await?;
        if session_key
            != base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(context.session_key().as_bytes())
        {
            return Err(EventVerificationFailure::Rejected(
                "session key does not match authorization context".into(),
            ));
        }
        let descriptor = trellis_protocol::decode_event_descriptor_identity(descriptor_identity)
            .map_err(|error| EventVerificationFailure::Rejected(error.to_string()))?;
        let owner_contract_id = descriptor.api_id().to_owned();
        let owner_event_name = descriptor.event_name().to_owned();
        let mut policy = self
            .source
            .runtime_policy()
            .map_err(|error| EventVerificationFailure::Retryable(error.to_string()))?;
        policy.now_unix_seconds = now;
        let verified_event = self
            .verification
            .verify_event(EventVerificationInput {
                context: &context,
                context_digest: authorization_context,
                subject,
                descriptor_identity,
                payload,
                event_id,
                event_time,
                proof,
                policy: &policy,
                revoked_at,
            })
            .map_err(|error| {
                EventVerificationFailure::Rejected(format!(
                    "event is not granted by the active authority: {error}"
                ))
            })?;
        let publisher = verified_event.publisher().clone();
        if self
            .source
            .runtime_revocation_time(authorization_context)
            .map_err(|error| EventVerificationFailure::Retryable(error.to_string()))?
            .is_some()
        {
            return Err(EventVerificationFailure::Rejected(
                "event is not granted by the active authority".into(),
            ));
        }
        Ok(VerifiedRuntimeEvent {
            publisher,
            owner_contract_id,
            owner_event_name,
        })
    }

    fn require_healthy(&self) -> Result<(), AuthorizationStateError> {
        if self.source.runtime_healthy() {
            Ok(())
        } else {
            Err(denied("authorization validator is not ready"))
        }
    }
}

pub(crate) async fn start_read_only(
    config: &crate::RuntimeConfig,
    client: async_nats::Client,
    stop: crate::shutdown::StopHandle,
) -> Result<
    (
        RuntimeAuthVerifier,
        tokio::task::JoinHandle<Result<(), crate::supervisor::RuntimeError>>,
    ),
    crate::supervisor::RuntimeError,
> {
    let authorization = config
        .resolve_authorization()
        .map_err(crate::supervisor::RuntimeError::Config)?;
    let now = now_seconds()
        .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))?;
    config.http.as_ref().ok_or_else(|| {
        crate::supervisor::RuntimeError::Platform(
            "HTTP configuration is required for issuer resolution".into(),
        )
    })?;
    let policy = trellis_protocol::AuthorizationVerificationPolicy::new(
        now,
        authorization
            .allowed_clock_skew_seconds
            .try_into()
            .map_err(|_| crate::supervisor::RuntimeError::Platform("invalid clock skew".into()))?,
        authorization
            .context_lifetime_seconds
            .try_into()
            .map_err(|_| {
                crate::supervisor::RuntimeError::Platform("invalid context lifetime".into())
            })?,
        authorization.maximum_context_bytes,
        authorization.maximum_permissions,
    )
    .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))?;
    let trellis_origin = config.public_origin();
    let allow_insecure_origin = config.public_origin_allows_insecure();
    let cache = AuthorizationProviderCache::attach_runtime(
        client,
        &AuthorizationRegistryBinding::from_runtime_parts(authorization.context_bucket.clone()),
        RuntimeAuthorizationTrust {
            trellis_origin,
            allow_insecure_origin,
            issuer: None,
            policy,
        },
    )
    .await
    .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))?;
    let watcher = cache.clone();
    let watcher_stop = stop.clone();
    let join = tokio::spawn(async move {
        let (sender, receiver) = tokio::sync::watch::channel(());
        tokio::spawn(async move {
            watcher_stop.stopped().await;
            drop(sender);
        });
        watcher
            .run_runtime(receiver)
            .await
            .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), cache.wait_until_ready())
        .await
        .map_err(|_| {
            crate::supervisor::RuntimeError::Platform(
                "authorization provider cache did not become ready".to_owned(),
            )
        })?
        .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))?;
    let verifier = RuntimeAuthVerifier::new(Arc::new(cache));
    Ok((verifier, join))
}

pub(crate) async fn ensure_read_only(
    context: &crate::supervisor::RuntimeContext,
    stop: crate::shutdown::StopHandle,
) -> Result<
    Option<tokio::task::JoinHandle<Result<(), crate::supervisor::RuntimeError>>>,
    crate::supervisor::RuntimeError,
> {
    if context.platform_verifier.get().is_some() {
        return Ok(None);
    }
    let (verifier, join) =
        start_read_only(&context.config, context.trellis_nats.clone(), stop).await?;
    context.platform_verifier.set(verifier).map_err(|_| {
        crate::supervisor::RuntimeError::Platform(
            "runtime-local auth verifier was already installed".to_owned(),
        )
    })?;
    Ok(Some(join))
}

impl RequestValidator for RuntimeAuthVerifier {
    fn validate<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        Box::pin(async move {
            let proof = context
                .proof
                .clone()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ServerError::MissingProof {
                    subject: subject.to_string(),
                })?;
            let session_key = context
                .session_key
                .clone()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ServerError::MissingSessionKey {
                    subject: subject.to_string(),
                })?;
            let authorization_context = context.authorization_context.clone().ok_or_else(|| {
                ServerError::MissingAuthorizationContext {
                    subject: subject.to_string(),
                }
            })?;
            let iat = context.iat.ok_or_else(|| {
                ServerError::Nats(format!("authenticated request for '{subject}' has no iat"))
            })?;
            let request_id = context.request_id.clone().ok_or_else(|| {
                ServerError::Nats(format!(
                    "authenticated request for '{subject}' has no request-id"
                ))
            })?;
            let required_permission = match context.required_permission.as_ref() {
                Some(permission) => match permission.permission_atom() {
                    Ok(permission) => Some(permission),
                    Err(error) => {
                        tracing::debug!(subject, %error, "invalid generated route permission");
                        return Ok(RequestValidation::denied());
                    }
                },
                None => None,
            };
            let verified = match self
                .verify_request(RuntimeAuthorizationRequestVerificationInput {
                    subject,
                    payload,
                    session_key: &session_key,
                    proof: &proof,
                    authorization_context: &authorization_context,
                    iat,
                    request_id: &request_id,
                    reply: context.reply_to.as_deref(),
                    required_permissions: required_permission.as_slice(),
                })
                .await
            {
                Ok(verified) => verified,
                Err(error) => {
                    tracing::debug!(subject, %error, "local request verification denied");
                    return Ok(RequestValidation::denied());
                }
            };
            Ok(RequestValidation {
                allowed: true,
                caller: Some(verified.caller),
                inbox_prefix: Some(verified.context.inbox_prefix().to_owned()),
            })
        })
    }

    fn revalidate_current<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<bool, ServerError>> {
        Box::pin(async move {
            let Some(digest) = context.authorization_context.as_deref() else {
                return Ok(false);
            };
            let Some(route) = context.required_permission.as_ref() else {
                return Ok(false);
            };
            let Ok(permission) = route.permission_atom() else {
                return Ok(false);
            };
            Ok(self.require_cached_permission(digest, &permission).is_ok())
        })
    }
}

/// Fail-closed validator for built-in routers in platform-less runtime modes.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DenyAllValidator;

impl RequestValidator for DenyAllValidator {
    fn validate<'a>(
        &'a self,
        _subject: &'a str,
        _payload: &'a Bytes,
        _context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        Box::pin(async move { Ok(RequestValidation::denied()) })
    }

    fn revalidate_current<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<bool, ServerError>> {
        Box::pin(async { Ok(false) })
    }
}

fn now_seconds() -> Result<i64, AuthorizationStateError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?
        .as_secs()
        .try_into()
        .map_err(|_| AuthorizationStateError::Storage("current time exceeds i64".to_owned()))
}

fn denied(message: impl Into<String>) -> AuthorizationStateError {
    AuthorizationStateError::InvalidRecord(message.into())
}

/// Bounded verification outcome for one authorization error.
fn verification_outcome(error: &AuthorizationStateError) -> &'static str {
    match error {
        AuthorizationStateError::NotAuthorized
        | AuthorizationStateError::PrincipalInactive
        | AuthorizationStateError::DeploymentInactive
        | AuthorizationStateError::InstanceInactive
        | AuthorizationStateError::DeviceInactive
        | AuthorizationStateError::ParticipantMissing
        | AuthorizationStateError::IdentityMissing => "denied",
        AuthorizationStateError::SessionRevoked
        | AuthorizationStateError::AuthorityRevoked
        | AuthorizationStateError::PortalPolicyChanged => "revoked",
        AuthorizationStateError::SessionExpired
        | AuthorizationStateError::AuthorityExpired
        | AuthorizationStateError::DelegationExpired => "expired",
        AuthorizationStateError::RequiredDependencyUnavailable(_)
        | AuthorizationStateError::RequiredResourceUnavailable(_)
        | AuthorizationStateError::MaterializationStale
        | AuthorizationStateError::AuthorityPending
        | AuthorizationStateError::ContextLifetimeUnavailable => "unavailable",
        AuthorizationStateError::InvalidRecord(_)
        | AuthorizationStateError::WrongPrincipalKind
        | AuthorizationStateError::ParticipantDigestMismatch
        | AuthorizationStateError::NeedsDigestMismatch
        | AuthorizationStateError::SessionMissing
        | AuthorizationStateError::ContextSnapshotChanged => "invalid",
        _ => "error",
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use serde_json::Map;
    use sha2::{Digest as _, Sha256};
    use trellis_protocol::{
        sign_authorization_context, sign_authorization_event, verify_authorization_context,
        AuthorizationContextPurpose, AuthorizationIssuerKey, AuthorizationIssuerState,
        AuthorizationPrincipalKind, AuthorizationVerificationPolicy, GrantOwnerKind, GrantSet,
        UnsignedAuthorizationContext, AUTHORIZATION_CONTEXT_FORMAT_V1,
    };

    use super::{AuthorizationVerificationCore, EventVerificationInput};

    #[test]
    fn verified_context_without_target_event_publish_authority_is_rejected() {
        let issuer_key = SigningKey::from_bytes(&[2; 32]);
        let session_key = SigningKey::from_bytes(&[3; 32]);
        let issuer = AuthorizationIssuerKey {
            key_id: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(issuer_key.verifying_key().as_bytes())),
            public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(issuer_key.verifying_key().as_bytes()),
            state: AuthorizationIssuerState::Active,
        };
        let signed = sign_authorization_context(
            UnsignedAuthorizationContext {
                format: AUTHORIZATION_CONTEXT_FORMAT_V1.to_owned(),
                issuer_key_id: issuer.key_id.clone(),
                principal_id: "01JY0000000000000000000001".to_owned(),
                principal_kind: AuthorizationPrincipalKind::User,
                participant_id: "console".to_owned(),
                owner_kind: GrantOwnerKind::User,
                owner_id: "01JY0000000000000000000001".to_owned(),
                grant_revision: 1,
                identity_key_id: None,
                login_session_id: Some("01JY0000000000000000000002".to_owned()),
                connection_id: "01JY0000000000000000000003".to_owned(),
                session_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(session_key.verifying_key().as_bytes()),
                deployment_id: None,
                instance_id: None,
                inbox_prefix: "_INBOX.test".to_owned(),
                issued_at: 1_100,
                not_before: 1_100,
                expires_at: 1_300,
                grants: GrantSet::new(vec![]),
                platform_privileges: vec![],
                extensions: Map::new(),
                critical: vec![],
            },
            &issuer_key,
        )
        .expect("sign context");
        let policy = AuthorizationVerificationPolicy::new(1_100, 30, 300, 16_384, 16)
            .expect("verification policy");
        let context = verify_authorization_context(
            &issuer,
            &signed,
            &policy,
            AuthorizationContextPurpose::Live,
        )
        .expect("verify context");
        type Event = trellis_runtime_apis::apis::trellis_auth_v1::events::ConnectionsOpened;
        let subject = <Event as trellis_rs::client::EventDescriptor>::SUBJECT;
        let descriptor_identity =
            <Event as trellis_rs::client::EventDescriptor>::descriptor_identity()
                .expect("event descriptor identity");
        let proof = sign_authorization_event(
            context.context_digest(),
            &descriptor_identity,
            subject,
            b"{}",
            "01JY0000000000000000000004",
            "1970-01-01T00:18:20Z",
            &session_key,
        )
        .expect("sign event");

        let result = AuthorizationVerificationCore::new().verify_event(EventVerificationInput {
            context: &context,
            context_digest: context.context_digest(),
            subject,
            descriptor_identity: &descriptor_identity,
            payload: b"{}",
            event_id: "01JY0000000000000000000004",
            event_time: "1970-01-01T00:18:20Z",
            proof: proof.as_str(),
            policy: &policy,
            revoked_at: None,
        });

        assert!(result.is_err());
    }
}
