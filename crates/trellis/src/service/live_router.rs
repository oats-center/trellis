//! Router integration for live Live and Operation watch routes.
//!
//! A live-capable Live or Operation watch route answers one bounded opening
//! request with a signed offer and hands ownership of the reservation to the
//! connection's live session manager. It never enters an infinite reply loop
//! and never starts a domain source before the delivery-path challenge round
//! trip completes.
//!
use bytes::Bytes;
use futures_util::StreamExt;

use trellis_protocol::{
    derive_live_data_subject, derive_live_observe_wildcard_subject, ApiSurfaceKind, LiveErrorCode,
    LiveOfferLimits, LiveOpenKind, LiveSessionKind, PermissionAction, PermissionAtom,
    PermissionTarget, OPEN_RESERVATION_MS,
};

use crate::client::TrellisClient;
use crate::live::authority::{
    LiveAuthorityGuard, LiveAuthorityLost, LiveGuardRequirement, PinnedPeerIdentity,
};
use crate::live::deadlines::LiveDeadlines;
use crate::live::manager::LiveSessionManager;
use crate::live::provider_engine::{ProviderOpenRequest, ProviderSessionRecord, SourceItem};
use crate::service::error::{ServerError, ValidationIssue};
use crate::service::request_loop::LivePreparedResponse;
use crate::service::router::RequestContext;

/// One connection owner that can serve live reservations for its routes.
///
/// The router holds a strong client reference so the live provider path shares
/// the exact authenticated connection, provider cache, and signing identity of
/// the service that registered the route.
#[derive(Clone)]
pub struct LiveProviderOwner {
    client: std::sync::Arc<TrellisClient>,
}

impl LiveProviderOwner {
    /// Construct one live provider owner from a connected client.
    #[must_use]
    pub(crate) fn new(client: std::sync::Arc<TrellisClient>) -> Self {
        Self { client }
    }

    /// Construct one live provider owner from a connected client.
    ///
    /// Exposed to the runtime crate so built-in subsystems can adopt the
    /// normal authenticated provider connection they bootstrapped with.
    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    #[must_use]
    pub fn from_connected_client(client: std::sync::Arc<TrellisClient>) -> Self {
        Self { client }
    }

    /// Return the owning client.
    #[must_use]
    pub(crate) fn client(&self) -> &std::sync::Arc<TrellisClient> {
        &self.client
    }

    /// Return the authenticated NATS connection of the owning client.
    ///
    /// Exposed to the runtime crate so built-in subsystems serve their public
    /// routers over the exact connection they bootstrapped with.
    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    #[must_use]
    pub fn runtime_nats(&self) -> async_nats::Client {
        self.client.runtime_nats()
    }

    /// Return the connection's live manager.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has no live manager.
    pub(crate) fn manager(&self) -> Result<&std::sync::Arc<LiveSessionManager>, ServerError> {
        self.client.live_manager().ok_or_else(|| {
            ServerError::Nats("live session manager is unavailable for this connection".to_owned())
        })
    }
}

/// Inputs for one provider Live opening.
pub(crate) struct LiveOpenInputs {
    #[expect(dead_code, reason = "identity retained for live open diagnostics")]
    pub api_id: String,
    pub base_subject: String,
    #[expect(dead_code, reason = "identity retained for live open diagnostics")]
    pub provider_instance_id: String,
    pub provider_deployment_id: String,
}

/// The consumer's verified opening request for one provider session.
pub(crate) struct LiveOpenRequest {
    pub request: RequestContext,
    pub inputs: LiveOpenInputs,
    pub opening: LiveOpeningMeta,
    pub encoded_input: serde_json::Value,
    pub cancellation: crate::live::LiveCancellation,
}

/// Parse and validate one Live opening body.
///
/// # Errors
///
/// Returns a validation error for a malformed envelope, an unsupported
/// protocol, or an invalid native input codec value.
pub(crate) fn parse_live_open<TInput>(payload: &[u8]) -> Result<LiveOpening<TInput>, ServerError>
where
    TInput: crate::generated::Codec,
{
    trellis_protocol::validate_open_body(payload).map_err(|error| ServerError::Validation {
        issues: Box::new(vec![ValidationIssue {
            path: String::new(),
            message: error.to_string(),
        }]),
    })?;
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: String::new(),
                message: format!("Invalid JSON: {error}"),
            }]),
        })?;
    if value.get("format").and_then(|format| format.as_str())
        != Some(trellis_protocol::LIVE_VERSION)
    {
        return Err(ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/format".to_owned(),
                message: "unsupported live protocol version".to_owned(),
            }]),
        });
    }
    if value.get("type").and_then(|kind| kind.as_str()) != Some("open") {
        return Err(ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/type".to_owned(),
                message: "live opening must carry the open discriminator".to_owned(),
            }]),
        });
    }
    let open_id = value
        .get("openId")
        .and_then(|open_id| open_id.as_str())
        .ok_or_else(|| ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/openId".to_owned(),
                message: "live opening omitted its open id".to_owned(),
            }]),
        })?;
    trellis_protocol::parse_nonce(open_id, ["openId"]).map_err(|error| {
        ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/openId".to_owned(),
                message: error.to_string(),
            }]),
        }
    })?;
    let receive_max_payload_bytes = value
        .get("receiveMaxPayloadBytes")
        .and_then(|limit| limit.as_u64())
        .ok_or_else(|| ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/receiveMaxPayloadBytes".to_owned(),
                message: "live opening omitted its receive payload limit".to_owned(),
            }]),
        })?;
    let input_value = value
        .get("input")
        .cloned()
        .ok_or_else(|| ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/input".to_owned(),
                message: "live opening omitted its native input".to_owned(),
            }]),
        })?;
    let input = TInput::decode(input_value).map_err(|error| ServerError::Validation {
        issues: Box::new(vec![ValidationIssue {
            path: "/input".to_owned(),
            message: error.to_string(),
        }]),
    })?;
    Ok(LiveOpening {
        open_id: open_id.to_owned(),
        receive_max_payload_bytes,
        input,
    })
}

/// One parsed Live opening.
pub(crate) struct LiveOpening<TInput> {
    pub open_id: String,
    pub receive_max_payload_bytes: u64,
    pub input: TInput,
}

/// Protocol metadata of one parsed Live opening, independent of the native
/// input so the decoded input can move into the delayed source factory.
pub(crate) struct LiveOpeningMeta {
    pub open_id: String,
    pub receive_max_payload_bytes: u64,
}

/// Inputs for one provider Operation watch opening.
pub(crate) struct OperationWatchOpenInputs {
    pub base_subject: String,
    #[expect(dead_code)]
    pub provider_instance_id: String,
    pub provider_deployment_id: String,
}

/// The consumer's verified Operation watch opening for one provider session.
pub(crate) struct OperationWatchOpenRequest {
    pub request: RequestContext,
    pub inputs: OperationWatchOpenInputs,
    pub opening: OperationWatchOpening,
    pub cancellation: crate::live::LiveCancellation,
}

/// Parse and validate one Operation watch opening body.
///
/// # Errors
///
/// Returns a validation error for a malformed envelope, an unsupported
/// protocol, or an invalid observation nonce.
pub(crate) fn parse_operation_watch_open(
    payload: &[u8],
) -> Result<OperationWatchOpening, ServerError> {
    trellis_protocol::validate_open_body(payload).map_err(|error| ServerError::Validation {
        issues: Box::new(vec![ValidationIssue {
            path: String::new(),
            message: error.to_string(),
        }]),
    })?;
    let open: trellis_protocol::OperationWatchOpen =
        serde_json::from_slice(payload).map_err(|error| ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: String::new(),
                message: format!("Invalid JSON: {error}"),
            }]),
        })?;
    if open.observation.format != trellis_protocol::LIVE_VERSION {
        return Err(ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/observation/format".to_owned(),
                message: "unsupported live protocol version".to_owned(),
            }]),
        });
    }
    if open.observation.kind != LiveOpenKind::Open {
        return Err(ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/observation/type".to_owned(),
                message: "operation watch opening must carry the open discriminator".to_owned(),
            }]),
        });
    }
    trellis_protocol::parse_nonce(&open.observation.open_id, ["openId"]).map_err(|error| {
        ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/observation/openId".to_owned(),
                message: error.to_string(),
            }]),
        }
    })?;
    if open.operation_id.is_empty() {
        return Err(ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: "/operationId".to_owned(),
                message: "operation watch omitted its durable operation id".to_owned(),
            }]),
        });
    }
    Ok(OperationWatchOpening {
        open_id: open.observation.open_id,
        receive_max_payload_bytes: open.observation.receive_max_payload_bytes,
        operation_id: open.operation_id,
        include_updates: open.include_updates.unwrap_or(false),
    })
}

/// One parsed Operation watch opening.
pub(crate) struct OperationWatchOpening {
    pub open_id: String,
    pub receive_max_payload_bytes: u64,
    pub operation_id: String,
    pub include_updates: bool,
}

/// Adapt one generated Live handler stream into the engine's source items.
///
/// Each encoded event is emitted as one application value and a normal stream
/// completion becomes the engine's explicit `End` item.
pub(crate) fn source_from_handler<TEvent, S>(
    stream: S,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<SourceItem, String>> + Send>>
where
    TEvent: crate::generated::Codec + 'static,
    S: futures_util::Stream<Item = Result<TEvent, ServerError>> + Send + 'static,
{
    Box::pin(stream.map(|item| {
        match item {
            Ok(event) => crate::generated::Codec::encode(&event)
                .map(SourceItem::Value)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        }
    }))
}

/// Adapt one Operation watch frame stream into the engine's source items.
///
/// Each snapshot or update JSON object is emitted as one application value and
/// a normal stream completion becomes the engine's explicit `End` item.
pub(crate) fn source_from_watch_frames<S>(
    stream: S,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<SourceItem, String>> + Send>>
where
    S: futures_util::Stream<Item = Result<Bytes, ServerError>> + Send + 'static,
{
    Box::pin(stream.map(|item| {
        match item {
            Ok(frame) => serde_json::from_slice(&frame)
                .map(SourceItem::Value)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        }
    }))
}

/// One reserved provider Live open ready to return its offer.
pub(crate) struct ReservedLive {
    pub prepared: LivePreparedResponse,
}

/// One reserved provider Operation watch open ready to return its offer.
pub(crate) struct ReservedOperationWatch {
    pub prepared: LivePreparedResponse,
}

/// Reserve one Live session, publish its activation challenge task, and build
/// the signed offer reply.
///
/// # Errors
///
/// Returns a setup error when the manager is unavailable, the transport epoch
/// changed, admission is exhausted, or the offer cannot be authenticated.
pub(crate) async fn reserve_live<D, F>(
    client: &TrellisClient,
    manager: &std::sync::Arc<LiveSessionManager>,
    request: &LiveOpenRequest,
    source_factory: F,
) -> Result<ReservedLive, ServerError>
where
    D: crate::generated::LiveDescriptor,
    F: FnOnce() -> std::pin::Pin<
            Box<dyn futures_util::Stream<Item = Result<SourceItem, String>> + Send>,
        > + Send
        + 'static,
{
    let context = &request.request;
    let inputs = &request.inputs;
    let opening = &request.opening;
    let encoded_input = request.encoded_input.clone();
    let cancellation = request.cancellation.clone();
    manager.is_available().map_err(|unavailable| {
        ServerError::Nats(format!("live manager unavailable: {unavailable:?}"))
    })?;
    let caller = context
        .caller
        .as_ref()
        .ok_or_else(|| ServerError::RequestDenied {
            subject: context.subject.clone(),
            session_key: context.session_key.clone().unwrap_or_default(),
        })?;
    let request_id = context
        .request_id
        .clone()
        .ok_or_else(|| ServerError::Nats("live request is missing a request id".to_owned()))?;
    let own_context_digest = client
        .authorization_context_digest()
        .map_err(|error| ServerError::Nats(error.to_string()))?;

    let consumer = PinnedPeerIdentity {
        connection_id: caller.connection_id.clone(),
        session_key: caller.session_key.clone(),
        principal_id: caller.principal_id.clone(),
        participant_id: caller.participant_id.clone(),
        deployment_id: caller.deployment_id.clone(),
        instance_id: caller.instance_id.clone(),
    };
    let action_name = D::KEY.split_once('.').map_or(D::KEY, |(_, action)| action);
    let observer_permission = PermissionAtom::new(
        PermissionTarget::api_surface(D::API_ID, ApiSurfaceKind::Live, action_name.to_owned())
            .map_err(|error| ServerError::Nats(error.to_string()))?,
        PermissionAction::Subscribe,
    )
    .map_err(|error| ServerError::Nats(error.to_string()))?;
    let own_guard = LiveAuthorityGuard::retain(
        client.authorization_provider(),
        &own_context_digest,
        LiveGuardRequirement::LocalProvider,
    )
    .await
    .map_err(|lost| ServerError::Nats(format!("provider authority unavailable: {lost:?}")))?;
    let caller_guard = LiveAuthorityGuard::retain(
        client.authorization_provider(),
        &caller.context_digest,
        LiveGuardRequirement::Observer(observer_permission),
    )
    .await
    .map_err(|lost| ServerError::Nats(format!("caller authority unavailable: {lost:?}")))?;
    let negotiated = trellis_protocol::negotiate_max_data_body_bytes(
        opening.receive_max_payload_bytes,
        client.nats().max_payload() as u64,
    )
    .map_err(|error| ServerError::Validation {
        issues: Box::new(vec![ValidationIssue {
            path: "/receiveMaxPayloadBytes".to_owned(),
            message: error.to_string(),
        }]),
    })?;
    let canonical_open_hash =
        trellis_protocol::logical_open_hash(&trellis_protocol::LogicalOpenIdentity {
            kind: LiveSessionKind::Standalone,
            base_subject: inputs.base_subject.clone(),
            open_id: opening.open_id.clone(),
            consumer_connection_id: consumer.connection_id.clone(),
            consumer_session_key: trellis_protocol::encode_subject_token(&consumer.session_key),
            consumer_principal_id: consumer.principal_id.clone(),
            consumer_participant_id: consumer.participant_id.clone(),
            receive_max_payload_bytes: opening.receive_max_payload_bytes,
            live_input: Some(encoded_input),
            operation_id: None,
            include_updates: None,
        })
        .map_err(|error| ServerError::Nats(error.to_string()))?;
    let now_ms = crate::client::now_iat_seconds() * 1_000;
    let open_request = ProviderOpenRequest {
        kind: LiveSessionKind::Standalone,
        base_subject: inputs.base_subject.clone(),
        open_id: opening.open_id.clone(),
        consumer: consumer.clone(),
        consumer_max_payload_bytes: opening.receive_max_payload_bytes,
        canonical_open_hash,
    };
    let (session, permit) = ProviderSessionRecord::reserve(
        manager,
        &open_request,
        &inputs.provider_deployment_id,
        negotiated,
        now_ms,
    )
    .await
    .map_err(|code| ServerError::Nats(format!("live reservation rejected: {code:?}")))?;
    let limits = LiveOfferLimits {
        max_data_body_bytes: negotiated,
        window_frames: trellis_protocol::WINDOW_FRAMES,
        window_bytes: trellis_protocol::WINDOW_BYTES,
        reservation_ms: OPEN_RESERVATION_MS,
        heartbeat_interval_ms: trellis_protocol::HEARTBEAT_INTERVAL_MS,
        peer_inactivity_ms: trellis_protocol::PEER_INACTIVITY_MS,
        consumer_stall_ms: trellis_protocol::CONSUMER_STALL_MS,
    };
    let record = std::sync::Arc::new(ProviderSessionRecord {
        session: std::sync::Arc::clone(&session),
        manager: std::sync::Arc::downgrade(manager),
        permit: std::sync::Mutex::new(Some(permit)),
        own_guard,
        caller_guard: std::sync::Arc::new(caller_guard),
        source_factory: std::sync::Mutex::new(Some(Box::new(source_factory))),
        cleanup: std::sync::Mutex::new(Vec::new()),
        terminal: std::sync::Mutex::new(None),
        cancellation: cancellation.clone(),
        source_started: std::sync::atomic::AtomicBool::new(false),
        max_data_body_bytes: negotiated,
        deadlines: std::sync::Mutex::new(LiveDeadlines::reserved(tokio::time::Instant::now())),
        deadline_notify: tokio::sync::Notify::new(),
        finished: std::sync::atomic::AtomicBool::new(false),
        telemetry: std::sync::Mutex::new(crate::live::telemetry::LiveTelemetryOwner::new_prepared(
            session.kind,
            crate::live::telemetry::LiveSide::Provider,
        )),
        output_lane: tokio::sync::Mutex::new(()),
        source_task: std::sync::Mutex::new(None),
        cleanup_driver_started: std::sync::atomic::AtomicBool::new(false),
        cleanup_result: std::sync::Mutex::new(None),
        control_receipt: std::sync::Mutex::new(None),
        pending_close_ack: std::sync::Mutex::new(None),
        end_sent: std::sync::atomic::AtomicBool::new(false),
    });
    manager.insert_provider_session(session.session_id.clone(), std::sync::Arc::clone(&record));
    let offer = record
        .offer_body(
            &request_id,
            &client
                .own_pinned_identity()
                .map_err(|error| ServerError::Nats(error.to_string()))?,
            &consumer,
            limits,
            negotiated,
        )
        .map_err(|code| ServerError::Nats(format!("offer build failed: {code:?}")))?;
    // Authenticate the exact offer bytes with the provider's live proof.
    let headers = crate::service::live_router::sign_live_offer(
        client.auth(),
        &own_context_digest,
        &context.reply_to.clone().unwrap_or_default(),
        &offer,
    )
    .map_err(|error| ServerError::Nats(error.to_string()))?;
    // Install the owner-control subscription before the offer is published so
    // an immediate activation cannot be lost.
    spawn_session_drivers(client.nats(), std::sync::Arc::clone(&record), negotiated)
        .await
        .map_err(|code| ServerError::Nats(format!("live control subscription failed: {code:?}")))?;
    Ok(ReservedLive {
        prepared: LivePreparedResponse {
            offer,
            headers,
            manager: std::sync::Arc::clone(manager),
            request_id,
        },
    })
}

/// Reserve one Operation watch session, publish its activation challenge task,
/// and build the signed offer reply.
///
/// # Errors
///
/// Returns a setup error when the manager is unavailable, the transport epoch
/// changed, admission is exhausted, or the offer cannot be authenticated.
pub(crate) async fn reserve_operation_watch<D, F>(
    client: &TrellisClient,
    manager: &std::sync::Arc<LiveSessionManager>,
    request: &OperationWatchOpenRequest,
    source_factory: F,
) -> Result<ReservedOperationWatch, ServerError>
where
    D: super::operations::OperationDescriptor,
    F: FnOnce() -> std::pin::Pin<
            Box<dyn futures_util::Stream<Item = Result<SourceItem, String>> + Send>,
        > + Send
        + 'static,
{
    let context = &request.request;
    let inputs = &request.inputs;
    let opening = &request.opening;
    let cancellation = request.cancellation.clone();
    manager.is_available().map_err(|unavailable| {
        ServerError::Nats(format!("live manager unavailable: {unavailable:?}"))
    })?;
    let caller = context
        .caller
        .as_ref()
        .ok_or_else(|| ServerError::RequestDenied {
            subject: context.subject.clone(),
            session_key: context.session_key.clone().unwrap_or_default(),
        })?;
    let request_id = context.request_id.clone().ok_or_else(|| {
        ServerError::Nats("operation watch request is missing a request id".to_owned())
    })?;
    let own_context_digest = client
        .authorization_context_digest()
        .map_err(|error| ServerError::Nats(error.to_string()))?;

    let consumer = PinnedPeerIdentity {
        connection_id: caller.connection_id.clone(),
        session_key: caller.session_key.clone(),
        principal_id: caller.principal_id.clone(),
        participant_id: caller.participant_id.clone(),
        deployment_id: caller.deployment_id.clone(),
        instance_id: caller.instance_id.clone(),
    };
    let action_name = D::KEY.split_once('.').map_or(D::KEY, |(_, action)| action);
    let observer_permission = PermissionAtom::new(
        PermissionTarget::api_surface(D::API_ID, ApiSurfaceKind::Operation, action_name.to_owned())
            .map_err(|error| ServerError::Nats(error.to_string()))?,
        PermissionAction::Observe,
    )
    .map_err(|error| ServerError::Nats(error.to_string()))?;
    let own_guard = LiveAuthorityGuard::retain(
        client.authorization_provider(),
        &own_context_digest,
        LiveGuardRequirement::LocalProvider,
    )
    .await
    .map_err(|lost| ServerError::Nats(format!("provider authority unavailable: {lost:?}")))?;
    let caller_guard = LiveAuthorityGuard::retain(
        client.authorization_provider(),
        &caller.context_digest,
        LiveGuardRequirement::Observer(observer_permission),
    )
    .await
    .map_err(|lost| ServerError::Nats(format!("caller authority unavailable: {lost:?}")))?;
    let negotiated = trellis_protocol::negotiate_max_data_body_bytes(
        opening.receive_max_payload_bytes,
        client.nats().max_payload() as u64,
    )
    .map_err(|error| ServerError::Validation {
        issues: Box::new(vec![ValidationIssue {
            path: "/observation/receiveMaxPayloadBytes".to_owned(),
            message: error.to_string(),
        }]),
    })?;
    let canonical_open_hash =
        trellis_protocol::logical_open_hash(&trellis_protocol::LogicalOpenIdentity {
            kind: LiveSessionKind::Operation,
            base_subject: inputs.base_subject.clone(),
            open_id: opening.open_id.clone(),
            consumer_connection_id: consumer.connection_id.clone(),
            consumer_session_key: trellis_protocol::encode_subject_token(&consumer.session_key),
            consumer_principal_id: consumer.principal_id.clone(),
            consumer_participant_id: consumer.participant_id.clone(),
            receive_max_payload_bytes: opening.receive_max_payload_bytes,
            live_input: None,
            operation_id: Some(opening.operation_id.clone()),
            include_updates: Some(opening.include_updates),
        })
        .map_err(|error| ServerError::Nats(error.to_string()))?;
    let now_ms = crate::client::now_iat_seconds() * 1_000;
    let open_request = ProviderOpenRequest {
        kind: LiveSessionKind::Operation,
        base_subject: inputs.base_subject.clone(),
        open_id: opening.open_id.clone(),
        consumer: consumer.clone(),
        consumer_max_payload_bytes: opening.receive_max_payload_bytes,
        canonical_open_hash,
    };
    let (session, permit) = ProviderSessionRecord::reserve(
        manager,
        &open_request,
        &inputs.provider_deployment_id,
        negotiated,
        now_ms,
    )
    .await
    .map_err(|code| ServerError::Nats(format!("live reservation rejected: {code:?}")))?;
    let limits = LiveOfferLimits {
        max_data_body_bytes: negotiated,
        window_frames: trellis_protocol::WINDOW_FRAMES,
        window_bytes: trellis_protocol::WINDOW_BYTES,
        reservation_ms: OPEN_RESERVATION_MS,
        heartbeat_interval_ms: trellis_protocol::HEARTBEAT_INTERVAL_MS,
        peer_inactivity_ms: trellis_protocol::PEER_INACTIVITY_MS,
        consumer_stall_ms: trellis_protocol::CONSUMER_STALL_MS,
    };
    let record = std::sync::Arc::new(ProviderSessionRecord {
        session: std::sync::Arc::clone(&session),
        manager: std::sync::Arc::downgrade(manager),
        permit: std::sync::Mutex::new(Some(permit)),
        own_guard,
        caller_guard: std::sync::Arc::new(caller_guard),
        source_factory: std::sync::Mutex::new(Some(Box::new(source_factory))),
        cleanup: std::sync::Mutex::new(Vec::new()),
        terminal: std::sync::Mutex::new(None),
        cancellation: cancellation.clone(),
        source_started: std::sync::atomic::AtomicBool::new(false),
        max_data_body_bytes: negotiated,
        deadlines: std::sync::Mutex::new(LiveDeadlines::reserved(tokio::time::Instant::now())),
        deadline_notify: tokio::sync::Notify::new(),
        finished: std::sync::atomic::AtomicBool::new(false),
        telemetry: std::sync::Mutex::new(crate::live::telemetry::LiveTelemetryOwner::new_prepared(
            session.kind,
            crate::live::telemetry::LiveSide::Provider,
        )),
        output_lane: tokio::sync::Mutex::new(()),
        source_task: std::sync::Mutex::new(None),
        cleanup_driver_started: std::sync::atomic::AtomicBool::new(false),
        cleanup_result: std::sync::Mutex::new(None),
        control_receipt: std::sync::Mutex::new(None),
        pending_close_ack: std::sync::Mutex::new(None),
        end_sent: std::sync::atomic::AtomicBool::new(false),
    });
    manager.insert_provider_session(session.session_id.clone(), std::sync::Arc::clone(&record));
    let offer = record
        .offer_body(
            &request_id,
            &client
                .own_pinned_identity()
                .map_err(|error| ServerError::Nats(error.to_string()))?,
            &consumer,
            limits,
            negotiated,
        )
        .map_err(|code| ServerError::Nats(format!("offer build failed: {code:?}")))?;
    // Authenticate the exact offer bytes with the provider's live proof.
    let headers = crate::service::live_router::sign_live_offer(
        client.auth(),
        &own_context_digest,
        &context.reply_to.clone().unwrap_or_default(),
        &offer,
    )
    .map_err(|error| ServerError::Nats(error.to_string()))?;
    // Install the owner-control subscription before the offer is published so
    // an immediate activation cannot be lost.
    spawn_session_drivers(client.nats(), std::sync::Arc::clone(&record), negotiated)
        .await
        .map_err(|code| ServerError::Nats(format!("live control subscription failed: {code:?}")))?;
    Ok(ReservedOperationWatch {
        prepared: LivePreparedResponse {
            offer,
            headers,
            manager: std::sync::Arc::clone(manager),
            request_id,
        },
    })
}

/// Spawn the per-session monotonic timer that owns live deadline policy.
///
/// Owner-control messages are drained by the route dispatcher; this task only
/// evaluates the session's deadline owner, so it has no fixed polling period
/// and no relation to the cleanup grace constant.
pub(crate) async fn spawn_session_drivers(
    nats: async_nats::Client,
    record: std::sync::Arc<ProviderSessionRecord>,
    _max_data_body_bytes: u64,
) -> Result<(), LiveErrorCode> {
    nats.flush()
        .await
        .map_err(|_| LiveErrorCode::Disconnected)?;
    tokio::spawn(async move {
        let mut own_changes = record.own_guard.subscribe_changes();
        let mut caller_changes = record.caller_guard.subscribe_changes();
        loop {
            let next = earliest_deadline(record.next_deadline(), record.guard_deadline());
            tokio::select! {
                () = async {
                    match next {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {}
                () = record.deadline_notify.notified() => continue,
                () = record.cancellation.cancelled() => return,
                // Quiet sessions fence on revocation/coverage/epoch changes and
                // on guard expiry without waiting for the next frame.
                _ = own_changes.recv() => {}
                _ = caller_changes.recv() => {}
            }
            if let Some(lost) = record.reconcile_authority().await {
                record.commit_end(crate::live::manager::authority_end(&lost));
                record.begin_close(tokio::time::Instant::now());
                record.spawn_close_driver(nats.clone());
                return;
            }
            if record.evaluate_deadlines(&nats).await {
                return;
            }
        }
    });
    Ok(())
}

/// Return the earlier of two optional monotonic deadlines.
fn earliest_deadline(
    first: Option<tokio::time::Instant>,
    second: Option<tokio::time::Instant>,
) -> Option<tokio::time::Instant> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

/// Sign one provider-origin message with the provider's live proof.
///
/// # Errors
///
/// Returns an error when the proof cannot be built.
pub(crate) fn sign_live_offer(
    auth: &crate::client::SessionAuth,
    context_digest: &str,
    subject: &str,
    body: &[u8],
) -> Result<async_nats::HeaderMap, trellis_protocol::ProtocolError> {
    let proof = trellis_protocol::sign_live_server_proof(
        context_digest,
        subject,
        body,
        auth.live_signing_key(),
    )?;
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("authorization-context", context_digest);
    headers.insert("session-key", auth.session_key.as_str());
    headers.insert("trellis-live-proof", proof.as_str());
    Ok(headers)
}

/// Return the exact owner-control wildcard for one Live route.
///
/// # Errors
///
/// Returns an error for an invalid subject or connection id.
#[expect(dead_code, reason = "subject helper for live route diagnostics")]
pub(crate) fn owner_control_wildcard(
    base_subject: &str,
    provider_connection_id: &str,
) -> Result<String, trellis_protocol::ProtocolError> {
    derive_live_observe_wildcard_subject(base_subject, provider_connection_id)
}

/// Return the exact data subject for one session.
///
/// # Errors
///
/// Returns an error for an invalid connection id or session id.
#[expect(dead_code, reason = "subject helper for live route diagnostics")]
pub(crate) fn data_subject(
    provider_connection_id: &str,
    consumer_connection_id: &str,
    session_id: &str,
) -> Result<String, trellis_protocol::ProtocolError> {
    derive_live_data_subject(provider_connection_id, consumer_connection_id, session_id)
}

/// Map one authority loss into a setup rejection.
#[must_use]
#[expect(dead_code, reason = "authority mapping for live route diagnostics")]
pub(crate) fn authority_rejection(lost: &LiveAuthorityLost) -> ServerError {
    ServerError::Nats(format!("live authority unavailable: {lost:?}"))
}

/// Map one wire error into a setup rejection.
#[must_use]
#[expect(dead_code, reason = "wire mapping for live route diagnostics")]
pub(crate) fn wire_rejection(code: LiveErrorCode) -> ServerError {
    ServerError::Nats(format!("live open rejected: {code:?}"))
}
