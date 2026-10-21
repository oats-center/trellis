//! Runtime authentication around the canonical, transport-neutral Transfer protocol.

use async_nats::{HeaderMap, Message};
use bytes::Bytes;
use trellis_protocol::transfer::*;
use trellis_protocol::*;

use super::{ServerError, TransferIdentity};
use crate::{
    client::{AuthorizationOwner, SessionAuth, TrellisClient},
    live::authority::{LiveAction, LiveAuthorityGuard, LiveGuardRequirement, PinnedPeerIdentity},
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
    action: &LiveAction,
    subject: &str,
    signal: &str,
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
) -> Result<HeaderMap, ServerError> {
    let compact = transfer_frame_digest(descriptor, payload).map_err(error)?;
    let owner = client.authorization_owner();
    let authority = owner.own_authority().map_err(error)?;
    owner.assert_current(&authority).map_err(error)?;
    let api = authority
        .authority()
        .apis
        .iter()
        .find(|api| api.api_id == action.api_id)
        .ok_or_else(|| error("transfer origin API unavailable"))?;
    let request = SessionRequest {
        authority_digest: authority.digest().to_owned(),
        api_id: action.api_id.clone(),
        api_generation: api.generation,
        accepted_revision: api.accepted_revision,
        action: action.identity.name.clone(),
        subject: subject.to_owned(),
        reply_subject: Some(signal.to_owned()),
        request_id: crate::client::new_request_id(),
        issued_at: crate::client::now_iat_seconds() as i64,
    };
    let auth = client.auth_handle();
    let (proof, _) = auth
        .create_request_proof(&request, &compact)
        .map_err(error)?;
    let mut headers = HeaderMap::new();
    headers.insert("authorization-context", request.authority_digest.as_str());
    headers.insert("session-key", auth.session_key.as_str());
    headers.insert("proof", proof.as_str());
    headers.insert("request-id", request.request_id.as_str());
    headers.insert("iat", request.issued_at.to_string().as_str());
    headers.insert(
        "Trellis-Api-Generation",
        request.api_generation.get().to_string().as_str(),
    );
    headers.insert(
        "Trellis-Accepted-Revision",
        request.accepted_revision.get().to_string().as_str(),
    );
    for (name, values) in frame_headers(descriptor)?.iter() {
        for value in values {
            headers.append(name.clone(), value.clone());
        }
    }
    Ok(headers)
}

/// Retained peer evidence with immutable identity and authenticated originating action.
pub(crate) struct TransferPeer {
    pub(crate) guard: LiveAuthorityGuard,
    owner: AuthorizationOwner,
    transport: async_nats::Client,
    expected: TransferIdentity,
    action: LiveAction,
}

impl TransferPeer {
    pub(crate) async fn retain(
        owner: &AuthorizationOwner,
        transport: &async_nats::Client,
        digest: &str,
        expected: &TransferIdentity,
        action: &LiveAction,
        provider: bool,
    ) -> Result<Self, ServerError> {
        let authority = owner
            .resolve_authority(transport, digest)
            .await
            .map_err(error)?;
        let identity = PinnedPeerIdentity::from_authority(&authority);
        let requirement = if provider {
            LiveGuardRequirement::PeerProvider {
                expected: identity,
                action: action.clone(),
            }
        } else {
            LiveGuardRequirement::Observer(action.clone())
        };
        let guard = LiveAuthorityGuard::retain(owner, transport, digest, requirement)
            .await
            .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        if guard.identity().connection_id != expected.connection_id
            || guard.identity().session_key != expected.session_key
        {
            return Err(error("transfer peer identity mismatch"));
        }
        Ok(Self {
            guard,
            owner: owner.clone(),
            transport: transport.clone(),
            expected: expected.clone(),
            action: action.clone(),
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
        let subject_kind = match descriptor.kind {
            TransferFrameKind::Control => TransferSubjectKind::Control,
            TransferFrameKind::Data | TransferFrameKind::Complete
                if descriptor.direction == TransferDirection::Send =>
            {
                TransferSubjectKind::UploadData
            }
            _ => return Err(error("invalid caller transfer frame kind")),
        };
        let expected_subject = derive_transfer_subject(
            subject_kind,
            provider_id,
            &self.expected.connection_id,
            &descriptor.transfer_id,
        )
        .map_err(error)?;
        let expected_signal = derive_transfer_subject(
            TransferSubjectKind::Signal,
            provider_id,
            &self.expected.connection_id,
            &descriptor.transfer_id,
        )
        .map_err(error)?;
        if message.subject.as_str() != expected_subject || signal_subject != expected_signal {
            return Err(error("transfer caller subject mismatch"));
        }
        if message.reply.as_deref() != Some(signal_subject)
            || header(headers, "session-key")? != self.expected.session_key
        {
            return Err(error("transfer caller boundary mismatch"));
        }
        let digest = header(headers, "authorization-context")?;
        // Immutable retained material needs replacement validation only when
        // its signed content address changes. Current coverage, revocation,
        // expiry and issuer trust are still checked before and after the proof.
        let candidate = if self.guard.context_digest() == digest {
            self.guard
                .check_now()
                .map_err(|e| error(format!("transfer authority: {e:?}")))?;
            None
        } else {
            Some(
                self.guard
                    .prepare_replacement(&digest)
                    .await
                    .map_err(|e| error(format!("transfer authority: {e:?}")))?,
            )
        };
        let authority = self
            .owner
            .resolve_authority(&self.transport, &digest)
            .await
            .map_err(error)?;
        let api = authority
            .authority()
            .apis
            .iter()
            .find(|api| api.api_id == self.action.api_id)
            .ok_or_else(|| error("transfer origin API unavailable"))?;
        let compact = transfer_frame_digest(descriptor, &message.payload).map_err(error)?;
        let proof = SessionRequestProof::parse(header(headers, "proof")?).map_err(error)?;
        let request_id = header(headers, "request-id")?;
        let request = SessionRequest {
            authority_digest: digest,
            api_id: self.action.api_id.clone(),
            api_generation: api.generation,
            accepted_revision: api.accepted_revision,
            action: self.action.identity.name.clone(),
            subject: message.subject.to_string(),
            reply_subject: Some(signal_subject.to_owned()),
            issued_at: header(headers, "iat")?.parse().map_err(error)?,
            request_id,
        };
        let verified = self
            .owner
            .verify_request(
                &self.transport,
                &request,
                &compact,
                &proof,
                &self.action.identity,
            )
            .await
            .map_err(error)?;
        if verified.caller().runtime_id != self.expected.connection_id
            || verified.caller().session_public_key != self.expected.session_key
        {
            return Err(error("transfer caller identity mismatch"));
        }
        if let Some(candidate) = candidate {
            self.guard
                .commit_replacement(candidate)
                .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        }
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
        let candidate = if self.guard.context_digest() == digest {
            self.guard
                .check_now()
                .map_err(|e| error(format!("transfer authority: {e:?}")))?;
            None
        } else {
            Some(
                self.guard
                    .prepare_replacement(&digest)
                    .await
                    .map_err(|e| error(format!("transfer authority: {e:?}")))?,
            )
        };
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
        if let Some(candidate) = candidate {
            self.guard
                .commit_replacement(candidate)
                .map_err(|e| error(format!("transfer authority: {e:?}")))?;
        }
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
