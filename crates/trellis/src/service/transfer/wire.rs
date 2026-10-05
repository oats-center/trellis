//! Runtime authentication around the canonical, transport-neutral Transfer protocol.

use async_nats::{HeaderMap, Message};
use bytes::Bytes;
use trellis_protocol::transfer::*;
use trellis_protocol::*;

use super::{ServerError, TransferIdentity};
use crate::{
    client::{AuthorizationProviderCache, SessionAuth, TrellisClient},
    live::authority::{LiveAuthorityGuard, LiveGuardRequirement},
};

pub(crate) fn error(value: impl std::fmt::Display) -> ServerError {
    ServerError::Nats(value.to_string())
}

pub(crate) fn header(headers: &HeaderMap, name: &str) -> Result<String, ServerError> {
    let mut values = headers.get_all(name);
    let value = values
        .next()
        .ok_or_else(|| error(format!("missing transfer header {name}")))?
        .to_string();
    if value.is_empty() || values.next().is_some() {
        return Err(error(format!("ambiguous transfer header {name}")));
    }
    Ok(value)
}

pub(crate) fn descriptor(
    message: &Message,
    id: &str,
    direction: TransferDirection,
) -> Result<TransferFrameDescriptor, ServerError> {
    let headers = message
        .headers
        .as_ref()
        .ok_or_else(|| error("transfer headers missing"))?;
    let sequence = U64s::new(
        parse_transfer_counter(&header(headers, TRANSFER_SEQUENCE_HEADER)?).map_err(error)?,
    );
    let kind = match header(headers, TRANSFER_CONTROL_HEADER)?.as_str() {
        "data" => TransferFrameKind::Data,
        "complete" => TransferFrameKind::Complete,
        "eof" => TransferFrameKind::Eof,
        "control" => TransferFrameKind::Control,
        "signal" => TransferFrameKind::Signal,
        _ => return Err(error("invalid transfer frame kind")),
    };
    let terminal = if kind == TransferFrameKind::Eof {
        Some(
            parse_transfer_terminal(header(headers, TRANSFER_TERMINAL_HEADER)?.as_bytes())
                .map_err(error)?,
        )
    } else {
        if headers.get_all(TRANSFER_TERMINAL_HEADER).next().is_some() {
            return Err(error("unexpected transfer terminal descriptor"));
        }
        None
    };
    let descriptor = TransferFrameDescriptor {
        transfer_id: id.into(),
        direction,
        sequence,
        kind,
        terminal,
    };
    descriptor.validate(&message.payload).map_err(error)?;
    Ok(descriptor)
}

pub(crate) fn frame_headers(
    descriptor: &TransferFrameDescriptor,
) -> Result<HeaderMap, ServerError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        TRANSFER_SEQUENCE_HEADER,
        descriptor.sequence.get().to_string().as_str(),
    );
    headers.insert(
        TRANSFER_CONTROL_HEADER,
        match descriptor.kind {
            TransferFrameKind::Data => "data",
            TransferFrameKind::Complete => "complete",
            TransferFrameKind::Eof => "eof",
            TransferFrameKind::Control => "control",
            TransferFrameKind::Signal => "signal",
        },
    );
    if let Some(terminal) = &descriptor.terminal {
        headers.insert(
            TRANSFER_TERMINAL_HEADER,
            serde_json::to_string(terminal).map_err(error)?.as_str(),
        );
    }
    Ok(headers)
}

pub(crate) fn caller_headers(
    client: &TrellisClient,
    subject: &str,
    signal: &str,
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
) -> Result<HeaderMap, ServerError> {
    let compact = transfer_frame_digest(descriptor, payload).map_err(error)?;
    let mut headers = client
        .signed_headers(subject, signal, &compact)
        .map_err(error)?;
    for (name, values) in frame_headers(descriptor)?.iter() {
        for value in values {
            headers.append(name.clone(), value.clone());
        }
    }
    Ok(headers)
}

/// Covered peer evidence with immutable logical identity and exact originating permission.
pub(crate) struct TransferPeer {
    pub(crate) guard: LiveAuthorityGuard,
    cache: AuthorizationProviderCache,
    expected: TransferIdentity,
    required: Vec<PermissionAtom>,
}

impl TransferPeer {
    pub(crate) async fn retain(
        cache: &AuthorizationProviderCache,
        digest: &str,
        expected: &TransferIdentity,
        permission: Option<PermissionAtom>,
    ) -> Result<Self, ServerError> {
        let requirement = permission.clone().map_or(
            LiveGuardRequirement::LocalProvider,
            LiveGuardRequirement::Observer,
        );
        let guard = LiveAuthorityGuard::retain(cache, digest, requirement)
            .await
            .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        if guard.identity().connection_id != expected.connection_id
            || guard.identity().session_key != expected.session_key
        {
            return Err(error("transfer peer identity mismatch"));
        }
        Ok(Self {
            guard,
            cache: cache.clone(),
            expected: expected.clone(),
            required: permission.into_iter().collect(),
        })
    }

    pub(crate) async fn verify_caller(
        &self,
        message: &Message,
        descriptor: &TransferFrameDescriptor,
        provider_id: &str,
        signal_subject: &str,
    ) -> Result<(), ServerError> {
        let _charge = super::telemetry::Charge::new(
            message.payload.len(),
            descriptor.direction,
            "provider-rx",
        );
        let headers = message
            .headers
            .as_ref()
            .ok_or_else(|| error("transfer headers missing"))?;
        if message.reply.as_deref() != Some(signal_subject)
            || header(headers, "session-key")? != self.expected.session_key
        {
            return Err(error("transfer caller boundary mismatch"));
        }
        let digest = header(headers, "authorization-context")?;
        let candidate = self
            .guard
            .prepare_replacement(&digest)
            .await
            .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        let policy = self.cache.policy().map_err(error)?;
        let context = self
            .cache
            .resolve_context(&digest, policy.now_unix_seconds)
            .await
            .map_err(error)?;
        let compact = transfer_frame_digest(descriptor, &message.payload).map_err(error)?;
        let proof = AuthorizationRequestProof::parse(header(headers, "proof")?).map_err(error)?;
        let request_id = header(headers, "request-id")?;
        verify_transfer_authorization_request(
            AuthorizationRequestVerificationInput {
                context: &context,
                subject: message.subject.as_str(),
                reply_subject: Some(signal_subject),
                raw_payload: &compact,
                iat: header(headers, "iat")?.parse().map_err(error)?,
                request_id: &request_id,
                proof: &proof,
                policy: &policy,
                required_permissions: &self.required,
            },
            provider_id,
            &self.expected.connection_id,
            &descriptor.transfer_id,
        )
        .map_err(error)?;
        self.guard
            .commit_replacement(candidate)
            .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        self.guard
            .check_now()
            .map_err(|e| error(format!("transfer authority: {e:?}")))
    }

    pub(crate) async fn verify_provider(
        &self,
        message: &Message,
        descriptor: &TransferFrameDescriptor,
    ) -> Result<(), ServerError> {
        let _charge =
            super::telemetry::Charge::new(message.payload.len(), descriptor.direction, "client-rx");
        let headers = message
            .headers
            .as_ref()
            .ok_or_else(|| error("transfer headers missing"))?;
        if message.reply.is_some() || header(headers, "session-key")? != self.expected.session_key {
            return Err(error("transfer provider identity mismatch"));
        }
        let digest = header(headers, "authorization-context")?;
        let candidate = self
            .guard
            .prepare_replacement(&digest)
            .await
            .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        let proof =
            TransferServerProof::parse(header(headers, TRANSFER_PROOF_HEADER)?).map_err(error)?;
        verify_transfer_server_proof_encoded(
            &proof,
            &digest,
            message.subject.as_str(),
            descriptor,
            &message.payload,
            &self.expected.session_key,
        )
        .map_err(error)?;
        self.guard
            .commit_replacement(candidate)
            .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        self.guard
            .check_now()
            .map_err(|e| error(format!("transfer authority: {e:?}")))
    }
}

pub(crate) async fn publish_provider(
    nats: &async_nats::Client,
    auth: &SessionAuth,
    local: &LiveAuthorityGuard,
    subject: &str,
    descriptor: &TransferFrameDescriptor,
    payload: Bytes,
) -> Result<(), ServerError> {
    local
        .reconcile()
        .await
        .map_err(|e| error(format!("transfer local authority: {e:?}")))?;
    let digest = local.context_digest();
    let proof = sign_transfer_server_proof(
        &digest,
        subject,
        descriptor,
        &payload,
        auth.live_signing_key(),
    )
    .map_err(error)?;
    let mut headers = frame_headers(descriptor)?;
    headers.insert("authorization-context", digest.as_str());
    headers.insert("session-key", auth.session_key.as_str());
    headers.insert(TRANSFER_PROOF_HEADER, proof.as_str());
    nats.publish_with_headers(subject.to_owned(), headers, payload)
        .await
        .map_err(error)
}

pub(crate) async fn publish_signal(
    nats: &async_nats::Client,
    client: &TrellisClient,
    local: &LiveAuthorityGuard,
    subject: &str,
    direction: TransferDirection,
    signal: TransferSignal,
) -> Result<(), ServerError> {
    let descriptor = TransferFrameDescriptor {
        transfer_id: signal.transfer_id().into(),
        direction,
        sequence: U64s::new(0),
        kind: TransferFrameKind::Signal,
        terminal: None,
    };
    publish_provider(
        nats,
        client.auth(),
        local,
        subject,
        &descriptor,
        Bytes::from(serde_json::to_vec(&signal).map_err(error)?),
    )
    .await
}

pub(crate) async fn authority_failure(local: &TransferPeer, peer: &TransferPeer) -> ServerError {
    loop {
        let deadline = local
            .guard
            .expires_at_seconds()
            .min(peer.guard.expires_at_seconds());
        let remaining = deadline.saturating_sub(time::OffsetDateTime::now_utc().unix_timestamp());
        tokio::time::sleep(std::time::Duration::from_secs(remaining.max(0) as u64)).await;
        if let Err(failure) = local.guard.reconcile().await {
            return error(format!("transfer local authority expired: {failure:?}"));
        }
        if let Err(failure) = peer.guard.check_now() {
            return error(format!("transfer peer authority expired: {failure:?}"));
        }
    }
}
