use async_nats::header::HeaderMap;
use async_nats::jetstream::{self, consumer, AckKind};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use bytes::Bytes;
use futures_util::stream::{self, BoxStream};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
#[cfg(feature = "device")]
use sha2::{Digest as _, Sha256};
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio::time::timeout;
#[cfg(feature = "device")]
use trellis_protocol::{DeviceEnrollmentSessionProofInput, SessionProofInput};

use crate::telemetry::instruments::{CounterFamily, DurationFamily};
use crate::telemetry::lifecycle::Observation;
use crate::telemetry::KeyValue;

/// Catalog participant kind for one authorization principal kind.
fn participant_kind_label(kind: &trellis_protocol::PrincipalKind) -> &'static str {
    match kind {
        trellis_protocol::PrincipalKind::User => "user",
        trellis_protocol::PrincipalKind::Service => "service",
        trellis_protocol::PrincipalKind::Device => "device",
    }
}

use super::events::{EVENT_ID_HEADER, EVENT_TIME_HEADER};
use crate::client::operations::OperationTransport;
use crate::client::proof::{new_request_id, now_iat_seconds};
use crate::client::transfer::{get_download_grant, DownloadTransferGrant};
use crate::client::transfer::{put_upload_grant, FileInfo, UploadTransferGrant};
use crate::client::{
    prepare_event, AuthorizationContextBundle, AuthorizationContextCache, AuthorizationOwner,
    AuthorizationRuntimeBinding, EventDescriptor, LiveDescriptor, PreparedTrellisEvent,
    RpcErrorPayload, SessionAuth, TrellisClientError,
};
use crate::generated::Codec as _;
use crate::service::{BootstrapBinding, CoreBootstrapBinding, ServiceResourceBindings};

const HEALTH_HEARTBEAT_INTERVAL_MS: u64 = 30_000;
const DEFAULT_EVENT_STREAM: &str = "trellis";

/// Internal service connection inputs; assignment and resources come from the server.
pub struct ServiceConnectWithContractOptions<'a> {
    /// Explicit provisioned broker endpoints; never discovered over HTTP.
    pub nats_servers: Vec<String>,
    /// Installation, account, and issuer pin established during provisioning.
    pub trust: crate::client::RuntimeAuthorizationTrust,
    pub participant_id: &'a str,
    pub provisioned_identity_seed_base64url: &'a str,
    pub name: Option<&'a str>,
    pub timeout_ms: u64,
}

/// Runtime and device-identity options for an activated device principal.
///
/// The type parameter `C` supplies the generated participant identity.
pub struct DeviceConnectOptions<'a, C> {
    #[cfg_attr(not(feature = "device"), allow(dead_code))]
    trellis_url: &'a str,
    nats_servers: Vec<String>,
    trust: crate::client::RuntimeAuthorizationTrust,
    participant_id: &'a str,
    identity_seed_base64url: &'a str,
    companion: Option<CompanionSession<'a>>,
    timeout_ms: u64,
    name: Option<&'a str>,
    contract_type: std::marker::PhantomData<C>,
}

impl<'a, C: crate::generated::ParticipantDescriptor> DeviceConnectOptions<'a, C> {
    /// Create device connection options using the participant identity from `C`.
    ///
    /// Runtime bootstrap generates fresh session keys internally; this constructor accepts only
    /// runtime location and the provisioned device seed.
    pub fn new(
        trellis_url: &'a str,
        identity_seed_base64url: &'a str,
        nats_servers: Vec<String>,
        trust: crate::client::RuntimeAuthorizationTrust,
    ) -> Self {
        Self {
            trellis_url,
            nats_servers,
            trust,
            participant_id: C::ID,
            identity_seed_base64url,
            companion: None,
            timeout_ms: crate::service::DEFAULT_TIMEOUT_MS,
            name: None,
            contract_type: std::marker::PhantomData,
        }
    }
}

impl<'a, C> DeviceConnectOptions<'a, C> {
    /// Set the request/connect timeout in milliseconds.
    pub const fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// Return the device public identity key bound to these options.
    pub fn public_identity_key(&self) -> Result<String, TrellisClientError> {
        Ok(SessionAuth::from_seed_base64url(self.identity_seed_base64url)?.session_key)
    }

    /// Attach optional display metadata without changing device identity.
    pub fn with_name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }

    /// Supply the companion's independently consented OAuth session.
    /// The signed device assignment must identify this exact grant.
    #[cfg(feature = "native-oauth")]
    pub fn with_companion(mut self, companion: UserConnectOptions<'a>) -> Self {
        self.companion = Some(companion);
        self
    }

    #[cfg(feature = "device")]
    pub(crate) fn activation_origin_digest(
        &self,
        activation_key_base64url: &str,
        nonce: &str,
        session_key_seed_base64url: &str,
    ) -> [u8; 32] {
        let mut digest = Sha256::new();
        for value in [
            self.trellis_url,
            self.participant_id,
            self.identity_seed_base64url,
            activation_key_base64url,
            nonce,
            session_key_seed_base64url,
        ] {
            digest.update(value.len().to_be_bytes());
            digest.update(value.as_bytes());
        }
        digest.finalize().into()
    }
}

/// Whether an event subscription uses a durable or ephemeral JetStream consumer.
///
/// There is deliberately no `Default` implementation: an implicit delivery
/// guarantee is easy to get wrong. Select the mode explicitly through
/// [`EventSubscribeOptions::durable`] or [`EventSubscribeOptions::ephemeral`],
/// or let a generated service API select the declared consumer binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventSubscriptionMode {
    /// Reuse a named durable consumer and retain delivery state across reconnects.
    Durable,
    /// Create an unnamed consumer that ends when the subscription is dropped.
    Ephemeral,
}

/// Initial delivery position for a descriptor-backed event subscription.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EventReplayPolicy {
    /// Deliver all retained events visible to the consumer.
    All,
    /// Deliver only events published after the consumer is created.
    #[default]
    New,
}

/// Options for descriptor-backed event subscriptions.
///
/// Construct these with [`EventSubscribeOptions::durable`] or
/// [`EventSubscribeOptions::ephemeral`]; there is intentionally no `Default`,
/// so a delivery guarantee is always chosen explicitly. A generated or service
/// API that already knows its declared consumer selects that consumer for you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSubscribeOptions {
    /// JetStream stream that owns the event consumer. Defaults to the Trellis event stream.
    pub stream: Option<String>,
    /// Durable or ephemeral consumer mode.
    pub mode: EventSubscriptionMode,
    /// Initial delivery position for a newly created consumer.
    pub replay: EventReplayPolicy,
    /// Optional durable name. Ignored for ephemeral subscriptions.
    pub durable_name: Option<String>,
}

impl EventSubscribeOptions {
    /// Durable consumption from the named Trellis-provisioned consumer.
    #[must_use]
    pub fn durable(durable_name: impl Into<String>) -> Self {
        Self {
            stream: None,
            mode: EventSubscriptionMode::Durable,
            replay: EventReplayPolicy::New,
            durable_name: Some(durable_name.into()),
        }
    }

    /// Explicit ephemeral consumption; delivery state is not retained.
    #[must_use]
    pub fn ephemeral() -> Self {
        Self {
            stream: None,
            mode: EventSubscriptionMode::Ephemeral,
            replay: EventReplayPolicy::New,
            durable_name: None,
        }
    }

    /// Select the JetStream stream that owns the event consumer.
    #[must_use]
    pub fn with_stream(mut self, stream: impl Into<String>) -> Self {
        self.stream = Some(stream.into());
        self
    }

    /// Select the initial delivery position for a newly created consumer.
    #[must_use]
    pub fn with_replay(mut self, replay: EventReplayPolicy) -> Self {
        self.replay = replay;
        self
    }
}

/// One descriptor-backed event message with explicit JetStream acknowledgement controls.
#[derive(Debug)]
pub struct EventMessage<T> {
    message: jetstream::Message,
    /// The generation lease that admitted this delivery. Retained on the delivery
    /// itself until it is dropped, so an un-acked message keeps its receiving
    /// generation alive even if the originating stream is dropped first.
    _lease: Option<crate::client::TransportLease>,
    _event: PhantomData<fn() -> T>,
}

impl<T> EventMessage<T> {
    /// Return the raw NATS headers delivered with the event message, if present.
    pub fn headers(&self) -> Option<&HeaderMap> {
        self.message.headers.as_ref()
    }

    /// Return the Trellis event id from the `Nats-Msg-Id` header, when present.
    pub fn event_id(&self) -> Option<&str> {
        self.headers()
            .and_then(|headers| headers.get(EVENT_ID_HEADER))
            .map(|value| value.as_str())
    }

    /// Return the Trellis event timestamp from the `Trellis-Event-Time` header, when present.
    pub fn event_time(&self) -> Option<&str> {
        self.headers()
            .and_then(|headers| headers.get(EVENT_TIME_HEADER))
            .map(|value| value.as_str())
    }

    /// Return original broker publication metadata, not caller-written event time.
    pub fn original_publication(
        &self,
    ) -> Result<trellis_protocol::OriginalEventPublication, TrellisClientError> {
        let info = self
            .message
            .info()
            .map_err(|error| TrellisClientError::EventSubscriptionProtocol(error.to_string()))?;
        let published_at = info
            .published
            .format(&Rfc3339)
            .map_err(|error| TrellisClientError::EventSubscriptionProtocol(error.to_string()))?;
        Ok(trellis_protocol::OriginalEventPublication {
            published_at,
            stream_sequence: trellis_protocol::U64s::new(info.stream_sequence),
        })
    }

    /// Return the raw JSON payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.message.payload
    }

    /// Return the concrete NATS subject that delivered this event message.
    pub fn subject(&self) -> &str {
        self.message.subject.as_ref()
    }

    /// Decode the message payload as the descriptor's typed event payload.
    pub fn decode(&self) -> Result<T, TrellisClientError>
    where
        T: for<'de> Deserialize<'de>,
    {
        Ok(serde_json::from_slice(&self.message.payload)?)
    }

    /// Acknowledge successful handling of the message.
    pub async fn ack(&self) -> Result<(), TrellisClientError> {
        let result = self
            .message
            .ack()
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()));
        record_delivery_disposition("ack", &result);
        result
    }

    /// Negatively acknowledge the message so JetStream may redeliver it.
    pub async fn nak(&self) -> Result<(), TrellisClientError> {
        let result = self
            .message
            .ack_with(AckKind::Nak(None))
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()));
        record_delivery_disposition("nak", &result);
        result
    }

    pub(crate) async fn nak_after(&self, delay: Duration) -> Result<(), TrellisClientError> {
        let result = self
            .message
            .ack_with(AckKind::Nak(Some(delay)))
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()));
        record_delivery_disposition("nak", &result);
        result
    }

    pub(crate) async fn ack_progress(&self) -> Result<(), TrellisClientError> {
        self.message
            .ack_with(AckKind::Progress)
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))
    }

    pub(crate) fn delivery_count(&self) -> u64 {
        self.message
            .info()
            .ok()
            .and_then(|info| u64::try_from(info.delivered).ok())
            .unwrap_or(1)
    }

    pub(crate) fn delivery_proof(&self) -> Result<String, TrellisClientError> {
        self.message
            .reply
            .as_ref()
            .map(ToString::to_string)
            .ok_or_else(|| {
                TrellisClientError::EventSubscriptionProtocol(
                    "JetStream delivery has no acknowledgement subject".to_owned(),
                )
            })
    }

    pub(crate) fn stream_sequence(&self) -> Result<u64, TrellisClientError> {
        self.message
            .info()
            .map(|info| info.stream_sequence)
            .map_err(|error| TrellisClientError::EventSubscriptionProtocol(error.to_string()))
    }

    /// Terminate the message without successful acknowledgement or redelivery.
    pub async fn term(&self) -> Result<(), TrellisClientError> {
        let result = self
            .message
            .ack_with(AckKind::Term)
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()));
        record_delivery_disposition("term", &result);
        result
    }
}

/// Records one observed final delivery disposition handoff.
fn record_delivery_disposition(action: &'static str, result: &Result<(), TrellisClientError>) {
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::telemetry::instruments::add_counter(
        CounterFamily::DeliveryDispositions,
        1,
        &[
            KeyValue::new("trellis.family", "event"),
            KeyValue::new("trellis.action", action),
            KeyValue::new("trellis.outcome", outcome),
        ],
    );
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg(feature = "device")]
pub(crate) struct DeviceEnrollmentResponse {
    pub(crate) server_now: i64,
    pub(crate) state: String,
    pub(crate) activation: Option<DeviceBootstrapActivation>,
}

#[cfg(feature = "device")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceBootstrapActivation {
    pub(crate) state: String,
    pub(crate) activation_url: String,
    pub(crate) review_id: String,
    pub(crate) expires_at: i64,
    pub(crate) retry_after_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceBootstrapAuthorization {
    participant_id: String,
    participant_digest: String,
    resource_runtime: ServiceResourceBindings,
    companion: Option<CompanionBootstrapAuthorization>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompanionBootstrapAuthorization {
    request_id: String,
    #[cfg_attr(not(feature = "native-oauth"), allow(dead_code))]
    oauth_grant_id: String,
    #[cfg_attr(not(feature = "native-oauth"), allow(dead_code))]
    client_id: String,
    required: bool,
}

#[derive(Clone, Debug)]
struct HealthHeartbeatConfig {
    service_name: String,
    started_at: String,
    publish_interval_ms: u64,
}

struct NativeConnectOptions<'a> {
    nats_servers: Vec<String>,
    trust: crate::client::RuntimeAuthorizationTrust,
    participant_id: &'a str,
    identity_seed: &'a str,
    companion_session: Option<CompanionSession<'a>>,
    companion_descriptor: Option<crate::generated::CompanionDescriptor>,
    name: Option<&'a str>,
    timeout_ms: u64,
    health_heartbeat_interval_ms: u64,
    kind: trellis_protocol::PrincipalKind,
}

#[cfg(feature = "device")]
pub(crate) async fn fetch_device_activation<C: crate::generated::ParticipantDescriptor>(
    opts: &DeviceConnectOptions<'_, C>,
    session_auth: &SessionAuth,
    connection_id: &str,
    provisioning_secret: Option<&str>,
    challenge_digest: &str,
    confirmation_code: &str,
) -> Result<DeviceEnrollmentResponse, TrellisClientError> {
    let identity_auth = SessionAuth::from_seed_base64url(opts.identity_seed_base64url)?;
    let origin = super::canonical_trellis_origin(opts.trellis_url)?;
    let mut request = serde_json::json!({
        "requestId": new_request_id(),
        "iat": now_context_millis()?,
        "connectionId": connection_id,
        "identityKeyId": identity_auth.key_id(),
        "sessionKey": session_auth.session_key,
        "participantId": opts.participant_id,
        "identityPublicKey": identity_auth.session_key,
        "challengeDigest": challenge_digest,
        "confirmationCode": confirmation_code,
    });
    if let Some(provisioning_secret) = provisioning_secret {
        request["provisioningSecret"] = serde_json::Value::String(provisioning_secret.to_owned());
    }
    let mut session_request = request.clone();
    session_request["identityKeyId"] = session_auth.key_id().into();
    let session_input = SessionProofInput::device_enrollment(DeviceEnrollmentSessionProofInput {
        origin: origin.clone(),
        unsigned_request: session_request,
    })
    .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
    let input = SessionProofInput::device_enrollment(DeviceEnrollmentSessionProofInput {
        origin: origin.clone(),
        unsigned_request: request.clone(),
    })
    .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
    request["proof"] = serde_json::to_value(identity_auth.sign_session_proof(&input)?)?;
    request["sessionProof"] =
        serde_json::to_value(session_auth.sign_session_proof(&session_input)?)?;
    let mut builder = super::http_client_builder()?;
    if !opts.trust.root_certificates.is_empty() {
        let mut certificates = Vec::new();
        for path in &opts.trust.root_certificates {
            let pem = std::fs::read(path).map_err(|error| {
                TrellisClientError::Bootstrap(format!("read CA '{}': {error}", path.display()))
            })?;
            certificates.extend(
                reqwest::Certificate::from_pem_bundle(&pem)
                    .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?,
            );
        }
        builder = reqwest::Client::builder().tls_certs_only(certificates);
    }
    let response = builder
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(opts.timeout_ms))
        .build()
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?
        .post(format!("{origin}/auth/device/enroll"))
        .json(&request)
        .send()
        .await
        .map_err(|error| TrellisClientError::AuthorizationUnavailable(error.to_string()))?;
    if !response.status().is_success() {
        let error = super::decode_trellis_http_error(response).await;
        return Err(TrellisClientError::BootstrapHttp {
            status: error.status,
            code: error.code,
        });
    }
    Ok(serde_json::from_slice(
        &super::read_bounded_http_body(response, 64 * 1024).await?,
    )?)
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(feature = "device")]
fn now_context_millis() -> Result<i64, TrellisClientError> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?
            .as_millis(),
    )
    .map_err(|_| TrellisClientError::Bootstrap("context time overflow".into()))
}

fn build_health_heartbeat(
    config: &HealthHeartbeatConfig,
    authority: &trellis_protocol::UnsignedSessionAuthority,
) -> Result<crate::__types::trellis::HealthHeartbeatSample, TrellisClientError> {
    use crate::__types::trellis::*;
    let (kind, participant_id, instance_id) = match &authority.binding {
        trellis_protocol::SessionBinding::Service {
            participant_id,
            instance_id,
            ..
        } => (
            HealthHeartbeatSampleParticipantKind::Service,
            participant_id,
            instance_id,
        ),
        trellis_protocol::SessionBinding::Device {
            participant_id,
            instance_id,
            ..
        } => (
            HealthHeartbeatSampleParticipantKind::Device,
            participant_id,
            instance_id,
        ),
        _ => {
            return Err(TrellisClientError::Bootstrap(
                "health publisher is not provisioned".into(),
            ))
        }
    };
    let metadata = authority
        .extensions
        .get("trellis.nativeParticipant.v1")
        .filter(|metadata| metadata["participantId"].as_str() == Some(participant_id.as_str()))
        .ok_or_else(|| {
            TrellisClientError::Bootstrap("missing signed native participant metadata".into())
        })?;
    let digest = metadata["participantDigest"]
        .as_str()
        .ok_or_else(|| TrellisClientError::Bootstrap("missing signed participant digest".into()))?;
    Ok(HealthHeartbeatSample {
        sample: HealthHeartbeatSampleSample {
            id: ulid::Ulid::new().to_string(),
            time: now_rfc3339(),
            extra: Default::default(),
        },
        participant: HealthHeartbeatSampleParticipant {
            name: HealthHeartbeatSampleParticipantName(config.service_name.clone()),
            kind,
            instance_id: HealthHeartbeatSampleParticipantInstanceId(instance_id.clone()),
            contract_id: HealthHeartbeatSampleParticipantContractId(participant_id.clone()),
            contract_digest: HealthHeartbeatSampleParticipantContractDigest(digest.into()),
            started_at: config.started_at.clone(),
            publish_interval_ms: HealthHeartbeatSampleParticipantPublishIntervalMs(
                crate::__types::Int64(i64::try_from(config.publish_interval_ms).map_err(|_| {
                    TrellisClientError::Bootstrap("health interval overflow".into())
                })?),
            ),
            runtime: HealthHeartbeatSampleParticipantRuntime::Rust,
            runtime_version: None,
            version: None,
            info: None,
            extra: Default::default(),
        },
        reported_status: HealthHeartbeatSampleReportedStatus::Healthy,
        summary: None,
        checks: vec![HealthHeartbeatSampleChecksItem {
            name: HealthHeartbeatSampleChecksItemName("nats".into()),
            status: HealthHeartbeatSampleChecksItemStatus::Ok,
            latency_ms: HealthHeartbeatSampleChecksItemLatencyMs(crate::__types::Number(0.0)),
            error: None,
            info: None,
            summary: None,
            extra: Default::default(),
        }],
        extra: Default::default(),
    })
}

async fn publish_prepared_event(
    nats: &async_nats::Client,
    auth: &SessionAuth,
    owner: &AuthorizationOwner,
    deadline: Instant,
    event: &PreparedTrellisEvent,
) -> Result<(), TrellisClientError> {
    let jetstream = jetstream::new(nats.clone());
    let identity = trellis_protocol::decode_event_descriptor_identity(event.descriptor_identity())?;
    trellis_protocol::validate_event_descriptor_subject(&identity, event.subject())?;
    let authority = owner.own_authority()?;
    owner
        .authorize_action(
            nats,
            &authority,
            identity.api_id(),
            &trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Event,
                name: identity.event_name().to_owned(),
                direction: trellis_protocol::PermissionAction::Publish,
            },
        )
        .await?;
    let api = authority
        .authority()
        .apis
        .iter()
        .find(|api| api.api_id == identity.api_id())
        .ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable("event API unavailable".into())
        })?;
    let publication = trellis_protocol::SessionEvent {
        authority_digest: authority.digest().to_owned(),
        api_id: identity.api_id().to_owned(),
        api_generation: api.generation,
        accepted_revision: api.accepted_revision,
        action: identity.event_name().to_owned(),
        subject: event.subject().to_owned(),
        event_id: event.event_id().to_owned(),
        event_time: event.event_time().to_owned(),
    };
    let mut headers = event.publish_headers();
    headers.insert("authorization-context", authority.digest());
    headers.insert("session-key", auth.session_key.as_str());
    headers.insert(
        "Trellis-Session-Event",
        serde_json::to_string(&publication)?,
    );
    headers.insert(
        "proof",
        auth.create_event_proof(&publication, event.payload())?
            .as_str(),
    );
    owner.assert_current(&authority)?;
    let publish_remaining = deadline.saturating_duration_since(Instant::now());
    if publish_remaining.is_zero() {
        return Err(TrellisClientError::Timeout);
    }
    let publish = async {
        jetstream
            .publish_with_headers(event.subject().to_string(), headers, event.payload_bytes())
            .await
    };
    let ack = timeout(publish_remaining, publish)
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    let ack_remaining = deadline.saturating_duration_since(Instant::now());
    if ack_remaining.is_zero() {
        return Err(TrellisClientError::Timeout);
    }
    timeout(ack_remaining, ack)
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    Ok(())
}

async fn publish_health_heartbeat(
    nats: &async_nats::Client,
    auth: &SessionAuth,
    owner: &AuthorizationOwner,
    deadline: Instant,
    config: &HealthHeartbeatConfig,
) -> Result<(), TrellisClientError> {
    let own = owner.own_authority()?;
    let sample = build_health_heartbeat(config, own.authority())?;
    let event = crate::client::prepare_event::<crate::runtime_health::events::Heartbeat>(&sample)
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
    if event.payload().len() > 65_536 {
        return Err(TrellisClientError::Bootstrap(
            "health sample exceeds 64 KiB".into(),
        ));
    }
    publish_prepared_event(nats, auth, owner, deadline, &event).await
}

fn spawn_health_heartbeat_task(
    manager: crate::client::TransportGenerationManager,
    auth: Arc<SessionAuth>,
    owner: AuthorizationOwner,
    timeout_ms: u64,
    config: HealthHeartbeatConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let subject = crate::runtime_health::events::Heartbeat::SUBJECT.to_owned();
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(config.publish_interval_ms));
        loop {
            interval.tick().await;
            // The heartbeat follows the current generation and never pins one
            // forever: each publish acquires a suitable generation and releases
            // it when the publish completes.
            let deadline = Instant::now() + std::time::Duration::from_millis(timeout_ms);
            let lease = match manager
                .acquire_for(std::slice::from_ref(&subject), &[], deadline)
                .await
            {
                Ok(lease) => lease,
                Err(error) => {
                    tracing::warn!(%error, "failed to acquire transport for health heartbeat");
                    continue;
                }
            };
            if let Err(error) =
                publish_health_heartbeat(lease.nats(), &auth, &owner, deadline, &config).await
            {
                tracing::warn!(%error, "failed to publish health heartbeat");
            }
        }
    })
}

/// Connected provider-cache handle returned by attach.
struct AuthorizationProviderHandle {
    provider: AuthorizationOwner,
    stop: tokio::sync::watch::Sender<()>,
    task: JoinHandle<()>,
}

/// Attach the connected NATS authorization registry to this client's provider
/// cache and wait for its complete snapshot before returning.
async fn attach_authorization_provider(
    authorization_contexts: Arc<AuthorizationContextCache>,
    generations: crate::client::TransportGenerationManager,
) -> Result<AuthorizationProviderHandle, TrellisClientError> {
    let provider = authorization_contexts.owner.clone();
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let watcher = provider.clone();
    let task = tokio::spawn(async move {
        let mut publications = generations.subscribe_publication();
        let mut own_changes = watcher.subscribe_changes();
        let mut initial_ready = Some(ready_tx);
        let mut retry_ms = 100u64;
        let mut active: Option<
            tokio_util::task::AbortOnDropHandle<Result<(), TrellisClientError>>,
        > = None;
        loop {
            if generations.terminal().is_some() {
                return;
            }
            // A known cutoff retires a session, not the logical client. Keep
            // denial sticky, but wait for an independently verified replacement
            // instead of reopening consumers for the same retired context.
            while matches!(
                watcher.staged_own_authority(),
                Err(TrellisClientError::Protocol(
                    trellis_protocol::ProtocolError::Authorization {
                        code: trellis_protocol::AuthorizationErrorCode::SessionRevoked,
                        ..
                    }
                ))
            ) {
                authorization_contexts.set_transport_ready(false);
                tokio::select! {
                    biased;
                    _ = stop_rx.changed() => return,
                    _ = generations.wait_terminal() => return,
                    changed = own_changes.recv() => {
                        if matches!(
                            changed,
                            Err(tokio::sync::broadcast::error::RecvError::Closed)
                        ) {
                            return;
                        }
                    }
                    _ = publications.changed() => {},
                }
            }
            // Consume publication changes before acquiring, never after it: a
            // change during acquisition must still cancel the old mirror.
            publications.borrow_and_update();
            let result = async {
                let lease = match generations.acquire_published() {
                    Ok(lease) => lease,
                    Err(error) => {
                        // The manager alone opens physical replacements. Pace
                        // its demand-driven recovery with this supervisor's retry
                        // rather than abandoning a temporarily absent carrier.
                        generations.authorization_promoted();
                        return Err(error);
                    }
                };
                let generation_id = lease.generation_id();
                let (snapshot_ready, snapshot) = tokio::sync::oneshot::channel();
                let candidate_owner = watcher.clone();
                let mut mirror =
                    tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                        let (_shutdown, shutdown) = tokio::sync::watch::channel(false);
                        candidate_owner
                            .run_revocations(lease.nats(), shutdown, snapshot_ready)
                            .await
                    }));
                tokio::pin!(snapshot);
                loop {
                    tokio::select! {
                        biased;
                        _ = stop_rx.changed() => return Ok(false),
                        _ = generations.wait_terminal() => return Ok(false),
                        _ = publications.changed() => return Ok(true),
                        result = &mut mirror => {
                            result.map_err(|_| {
                                TrellisClientError::AuthorizationUnavailable(
                                    "retirement mirror task interrupted".into(),
                                )
                            })??;
                            return Err(TrellisClientError::AuthorizationUnavailable(
                                "authorization retirement mirror ended".into(),
                            ));
                        }
                        result = async {
                            match active.as_mut() {
                                Some(previous) => previous.await,
                                None => std::future::pending().await,
                            }
                        } => {
                            active = None;
                            authorization_contexts.set_transport_ready(false);
                            tracing::warn!(
                                ?result,
                                "predecessor retirement mirror ended during preparation"
                            );
                        }
                        ready = &mut snapshot => {
                            ready.map_err(|error| {
                                TrellisClientError::AuthorizationUnavailable(error.to_string())
                            })?;
                            break;
                        }
                    }
                }
                // A complete snapshot does not authorize a retired own session.
                // Pin the exact carrier and installation through the final swap.
                let resumed = {
                    let transition = authorization_contexts.lock_own_transition()?;
                    generations.commit_published_if_current(&transition, generation_id, || {
                        if watcher.own_authority().is_err() {
                            return false;
                        }
                        authorization_contexts.set_transport_ready(true);
                        true
                    })?
                };
                if !resumed {
                    watcher.own_authority()?;
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "own authority is unavailable after retirement snapshot".into(),
                    ));
                }
                // Only a complete checked replacement retires the healthy mirror.
                active = Some(mirror);
                retry_ms = 100;
                if let Some(ready) = initial_ready.take() {
                    let _ = ready.send(());
                }
                loop {
                    tokio::select! {
                        biased;
                        _ = stop_rx.changed() => return Ok(false),
                        _ = generations.wait_terminal() => return Ok(false),
                        _ = publications.changed() => return Ok(true),
                        result = active.as_mut().expect("published retirement mirror") => {
                            active = None;
                            result.map_err(|_| {
                                TrellisClientError::AuthorizationUnavailable(
                                    "retirement mirror task interrupted".into(),
                                )
                            })??;
                            return Err(TrellisClientError::AuthorizationUnavailable(
                                "authorization retirement mirror ended".into(),
                            ));
                        }
                        changed = own_changes.recv() => {
                            if matches!(
                                changed,
                                Err(tokio::sync::broadcast::error::RecvError::Closed)
                            ) {
                                return Ok(false);
                            }
                            if watcher.own_authority().is_err() {
                                authorization_contexts.suspend();
                                authorization_contexts.request_coverage_reconciliation();
                            }
                        }
                    }
                }
            }
            .await;
            // Dropping the epoch above fences verification before retrying. The
            // owner keeps every authenticated cutoff across these replacements.
            authorization_contexts.set_transport_ready(false);
            match result {
                Ok(false) => return,
                Ok(true) => continue,
                Err(error) => {
                    tracing::warn!(%error, "authorization retirement mirror unavailable; retrying");
                }
            }
            let delay =
                Duration::from_millis(retry_ms + rand::random::<u64>() % (retry_ms / 2 + 1));
            retry_ms = (retry_ms * 2).min(5_000);
            tokio::select! {
                biased;
                _ = stop_rx.changed() => return,
                _ = generations.wait_terminal() => return,
                _ = publications.changed() => {},
                _ = tokio::time::sleep(delay) => {},
            }
        }
    });
    let ready = match tokio::time::timeout(std::time::Duration::from_secs(30), ready_rx).await {
        Ok(ready) => ready,
        Err(_) => {
            let _ = stop_tx.send(());
            task.abort();
            return Err(TrellisClientError::Bootstrap(
                "authorization provider did not become ready".into(),
            ));
        }
    };
    if let Err(error) = ready {
        let _ = stop_tx.send(());
        task.abort();
        return Err(TrellisClientError::AuthorizationUnavailable(
            error.to_string(),
        ));
    }
    Ok(AuthorizationProviderHandle {
        provider,
        stop: stop_tx,
        task,
    })
}

/// A device's companion OAuth session, which needs the `native-oauth` feature.
#[cfg(feature = "native-oauth")]
type CompanionSession<'a> = UserConnectOptions<'a>;
/// Without `native-oauth` a device has no companion session.
#[cfg(not(feature = "native-oauth"))]
enum CompanionSession<'a> {
    #[allow(dead_code)]
    Never(std::convert::Infallible, std::marker::PhantomData<&'a ()>),
}

/// Connection options for an OAuth-authorized native application.
#[cfg(feature = "native-oauth")]
pub struct UserConnectOptions<'a> {
    nats_servers: Vec<String>,
    timeout_ms: u64,
    session: crate::auth::NativeOAuthSession,
    name: Option<&'a str>,
}

#[cfg(feature = "native-oauth")]
impl<'a> UserConnectOptions<'a> {
    /// Create user-authenticated connection options.
    pub fn new(
        nats_servers: Vec<String>,
        timeout_ms: u64,
        session: crate::auth::NativeOAuthSession,
    ) -> Self {
        Self {
            nats_servers,
            timeout_ms,
            session,
            name: None,
        }
    }

    /// Attach optional display metadata without changing login or connection identity.
    pub fn with_name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }
}

/// Internal authenticated Trellis transport.
pub struct TrellisClient {
    nats: async_nats::Client,
    inbox_prefix: String,
    authorization_provider: AuthorizationOwner,
    authorization_provider_stop: tokio::sync::watch::Sender<()>,
    authorization_provider_task: JoinHandle<()>,
    auth: Arc<SessionAuth>,
    timeout_ms: u64,
    service_bootstrap_binding: Option<CoreBootstrapBinding>,
    health_heartbeat_task: Option<JoinHandle<()>>,
    authorization_contexts: Option<Arc<AuthorizationContextCache>>,
    /// Automatic transport generations that own this logical connection's
    /// physical NATS attachments for finite request and publish work.
    generations: crate::client::TransportGenerationManager,
    authorization_context_refresh_task: Option<JoinHandle<()>>,
    oauth_admission_refresh_task: Option<JoinHandle<()>>,
    oauth_admission_refresh_requests: Option<
        tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<Result<(), TrellisClientError>>>,
    >,
    companion: Option<Arc<TrellisClient>>,
    /// Process-local connection state registration for telemetry gauges.
    connection: std::sync::Arc<crate::telemetry::lifecycle::ConnectionRegistration>,
    /// One live-observation manager for this authenticated connection owner.
    live: Option<std::sync::Arc<crate::live::manager::LiveSessionManager>>,
}

impl TrellisClient {
    pub(crate) fn descriptor_subject(&self, subject: &str) -> String {
        subject.to_string()
    }

    /// Return this connection's caller-owned inbox prefix.
    pub(crate) fn inbox_prefix(&self) -> &str {
        &self.inbox_prefix
    }

    pub(crate) fn nats(&self) -> async_nats::Client {
        self.nats.clone()
    }

    /// The generation manager that owns this logical connection's physical
    /// attachments. Bound resources acquire a suitable generation per call
    /// through it instead of pinning the initial attachment.
    pub(crate) fn transport_generations(&self) -> crate::client::TransportGenerationManager {
        self.generations.clone()
    }

    /// The default per-call transport deadline shared by acquisition and the
    /// exchange it guards.
    pub(crate) fn transport_deadline(&self) -> Instant {
        Instant::now() + Duration::from_millis(self.timeout_ms)
    }

    /// Acquire one admitted generation lease for physical work.
    ///
    /// Callers hold the lease through completion/error/drop; the deadline
    /// covers generation acquisition and the exchange it guards.
    pub(crate) async fn acquire_transport(
        &self,
        publish: &[String],
        subscribe: &[String],
        deadline: Instant,
    ) -> Result<crate::client::TransportLease, TrellisClientError> {
        self.generations
            .acquire_for(publish, subscribe, deadline)
            .await
    }

    /// Return the cloneable session signer for owned live controls.
    pub(crate) fn auth_handle(&self) -> std::sync::Arc<crate::client::SessionAuth> {
        self.auth.clone()
    }

    /// Return the live-observation manager for this connection owner.
    pub(crate) fn live_manager(
        &self,
    ) -> Option<&std::sync::Arc<crate::live::manager::LiveSessionManager>> {
        self.live.as_ref()
    }

    /// Return the retained provider cache that owns local verification state.
    pub(crate) fn authorization_owner(&self) -> &AuthorizationOwner {
        &self.authorization_provider
    }

    /// Return this connection owner's pinned identity from its signed context.
    ///
    /// # Errors
    ///
    /// Returns an error when no installed context exists.
    pub(crate) fn own_pinned_identity(
        &self,
    ) -> Result<crate::live::authority::PinnedPeerIdentity, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "authorization context cache unavailable".to_owned(),
            )
        })?;
        contexts.pinned_identity()
    }

    /// Return the deployment id of this connection's installed signed context.
    pub fn runtime_deployment_id(&self) -> Result<String, TrellisClientError> {
        let bundle = self
            .authorization_contexts
            .as_ref()
            .ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context unavailable".into())
            })?
            .installed_signed_context()?;
        match &bundle.unsigned.binding {
            trellis_protocol::SessionBinding::Service { deployment_id, .. }
            | trellis_protocol::SessionBinding::Device { deployment_id, .. } => {
                Ok(deployment_id.clone())
            }
            _ => Err({
                TrellisClientError::Bootstrap(
                    "installed context carries no deployment identity".into(),
                )
            }),
        }
    }

    pub(crate) fn participant_id(&self) -> Result<String, TrellisClientError> {
        let authority = self
            .authorization_contexts
            .as_ref()
            .ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context unavailable".into())
            })?
            .installed_signed_context()?;
        match &authority.unsigned.binding {
            trellis_protocol::SessionBinding::Service { participant_id, .. }
            | trellis_protocol::SessionBinding::Device { participant_id, .. } => {
                Ok(participant_id.clone())
            }
            _ => Err(TrellisClientError::AuthorizationUnavailable(
                "runtime is not a provisioned participant".into(),
            )),
        }
    }

    /// Return the session auth helper used by this client.
    pub fn auth(&self) -> &SessionAuth {
        &self.auth
    }

    /// Return the request timeout configured for this client.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Return the resource binding supplied by service HTTP bootstrap, if this is a service client.
    pub fn service_bootstrap_binding(&self) -> Option<&CoreBootstrapBinding> {
        self.service_bootstrap_binding.as_ref()
    }

    pub(crate) fn companion(&self) -> Option<Arc<TrellisClient>> {
        self.companion.clone()
    }

    /// Connect using a provisioned service seed and server-owned assignment.
    pub async fn connect_service_with_contract(
        opts: ServiceConnectWithContractOptions<'_>,
    ) -> Result<Self, TrellisClientError> {
        Self::connect_service_with_contract_with_health_interval(opts, None).await
    }

    /// Connect a service with an explicitly selected health-publishing interval.
    pub(crate) async fn connect_service_with_contract_with_health_interval(
        opts: ServiceConnectWithContractOptions<'_>,
        health_heartbeat_interval_ms: Option<u64>,
    ) -> Result<Self, TrellisClientError> {
        let heartbeat_ms = health_heartbeat_interval_ms.unwrap_or(HEALTH_HEARTBEAT_INTERVAL_MS);
        Self::connect_native(NativeConnectOptions {
            nats_servers: opts.nats_servers,
            trust: opts.trust,
            participant_id: opts.participant_id,
            identity_seed: opts.provisioned_identity_seed_base64url,
            companion_session: None,
            companion_descriptor: None,
            name: opts.name,
            timeout_ms: opts.timeout_ms,
            health_heartbeat_interval_ms: heartbeat_ms,
            kind: trellis_protocol::PrincipalKind::Service,
        })
        .await
    }

    async fn connect_native(opts: NativeConnectOptions<'_>) -> Result<Self, TrellisClientError> {
        let observation = Observation::start(
            DurationFamily::Connect,
            vec![
                KeyValue::new("trellis.phase", "total"),
                KeyValue::new(
                    "trellis.participant.kind",
                    participant_kind_label(&opts.kind),
                ),
            ],
            "cancelled",
        );
        let result = Self::connect_native_inner(opts).await;
        if let Ok(client) = &result {
            client.connection.usable();
        }
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => Self::client_outcome(error),
        };
        observation.finish(outcome);
        result
    }

    async fn connect_native_inner(
        opts: NativeConnectOptions<'_>,
    ) -> Result<Self, TrellisClientError> {
        let NativeConnectOptions {
            nats_servers,
            trust,
            participant_id,
            identity_seed,
            companion_session,
            companion_descriptor,
            name,
            timeout_ms,
            health_heartbeat_interval_ms,
            kind,
        } = opts;
        if !(1_000..=600_000).contains(&health_heartbeat_interval_ms) {
            return Err(TrellisClientError::Bootstrap(
                "health heartbeat interval must be between 1000 and 600000 milliseconds".into(),
            ));
        }
        let identity = Arc::new(SessionAuth::from_seed_base64url(identity_seed)?);
        let (session_seed, _) = crate::auth::generate_session_keypair();
        let auth = SessionAuth::from_seed_base64url(&session_seed)?;
        // A device companion is never part of the native bootstrap request. The
        // server returns a durable companion assignment from an active device
        // delegation, and the companion then connects through the ordinary user
        // login path after this native connection is established.
        if companion_descriptor.is_none() && companion_session.is_some() {
            return Err(TrellisClientError::Bootstrap(
                "device descriptor has no companion".into(),
            ));
        }
        let runtime_id = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>());
        let now = now_iat_seconds() as i64;
        let admission = trellis_protocol::ProvisionedAdmission {
            format: trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1.into(),
            trellis_instance_id: trust.instance_id.clone(),
            audience_nats_account: trust.account.clone(),
            credential_id: identity.key_id(),
            runtime_id: runtime_id.clone(),
            ephemeral_nkey: auth.session_nkey()?,
            issued_at: now,
            expires_at: now + 60,
        };
        let owner =
            AuthorizationOwner::new(trust.clone(), runtime_id.clone(), auth.session_key.clone())?;
        let runtime = AuthorizationRuntimeBinding {
            runtime_id,
            inbox_prefix: trellis_protocol::admission_inbox_prefix(&auth.session_key)?,
            transports: crate::client::AuthorizationRuntimeTransports {
                native: Some(crate::client::AuthorizationNativeTransport {
                    nats_servers,
                    root_certificates: trust.root_certificates.clone(),
                }),
                websocket: None,
            },
        };
        let contexts = Arc::new(AuthorizationContextCache::new(
            owner,
            trust,
            runtime,
            &auth.session_key,
            super::authorization::AuthorizationCredential::Provisioned {
                admission,
                identity,
            },
        )?);
        let mut connected = Self::connect_context(
            auth,
            contexts.clone(),
            timeout_ms,
            participant_kind_label(&kind),
        )
        .await?;
        let authorization: ServiceBootstrapAuthorization =
            serde_json::from_value(contexts.service_bootstrap_authorization()?)?;
        if authorization.participant_id != participant_id {
            return Err(TrellisClientError::Bootstrap(
                "native resource evidence has the wrong participant".into(),
            ));
        }
        let context = contexts.installed_signed_context()?;
        match &context.unsigned.binding {
            trellis_protocol::SessionBinding::Service {
                participant_id: bound,
                ..
            } if kind == trellis_protocol::PrincipalKind::Service && bound == participant_id => (),
            trellis_protocol::SessionBinding::Device {
                participant_id: bound,
                ..
            } if kind == trellis_protocol::PrincipalKind::Device && bound == participant_id => (),
            _ => {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "provisioned binding mismatch".into(),
                ))
            }
        };
        let heartbeat = HealthHeartbeatConfig {
            service_name: name.unwrap_or(participant_id).to_owned(),
            started_at: now_rfc3339(),
            publish_interval_ms: health_heartbeat_interval_ms,
        };
        connected.service_bootstrap_binding = Some(CoreBootstrapBinding::new(
            BootstrapBinding {
                contract_id: authorization.participant_id,
                digest: authorization.participant_digest,
            },
            authorization.resource_runtime,
        ));
        if let Some(companion) = authorization.companion {
            if companion_descriptor.is_none_or(|descriptor| {
                descriptor.request_id != companion.request_id.as_str()
                    || descriptor.required != companion.required
            }) {
                return Err(TrellisClientError::Bootstrap(
                    "device bootstrap companion does not match its descriptor".into(),
                ));
            }
            #[cfg(feature = "native-oauth")]
            if let Some(companion_session) = companion_session {
                if companion_session
                    .session
                    .admission
                    .credential
                    .oauth_grant_id
                    != companion.oauth_grant_id
                    || companion_session.session.admission.credential.client_id
                        != companion.client_id
                    || companion_session.session.trust.instance_id
                        != context.unsigned.trellis_instance_id
                    || companion_session.session.trust.account
                        != context.unsigned.audience_nats_account
                {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "companion grant assignment mismatch".into(),
                    ));
                }
                match Self::connect_user(companion_session).await {
                    Ok(child) => connected.companion = Some(Arc::new(child)),
                    Err(error) if !companion.required => {
                        tracing::warn!(%error, "optional device companion could not connect");
                    }
                    Err(error) => return Err(error),
                }
            } else if companion.required {
                return Err(TrellisClientError::Bootstrap(
                    "device companion OAuth session is required".into(),
                ));
            }
            #[cfg(not(feature = "native-oauth"))]
            if companion.required || companion_session.is_some() {
                return Err(TrellisClientError::Bootstrap(
                    "a device companion needs the native-oauth feature".into(),
                ));
            }
        } else if companion_descriptor.is_some_and(|descriptor| descriptor.required) {
            return Err(TrellisClientError::Bootstrap(
                "required device companion is unavailable".into(),
            ));
        }
        connected.health_heartbeat_task = Some(spawn_health_heartbeat_task(
            connected.generations.clone(),
            connected.auth.clone(),
            connected.authorization_owner().clone(),
            timeout_ms,
            heartbeat,
        ));
        Ok(connected)
    }

    /// Connect an activated device using refreshed auth-owned connect info.
    pub async fn connect_device<C: crate::generated::ParticipantDescriptor>(
        opts: DeviceConnectOptions<'_, C>,
    ) -> Result<Self, TrellisClientError> {
        Self::connect_native(NativeConnectOptions {
            nats_servers: opts.nats_servers,
            trust: opts.trust,
            participant_id: opts.participant_id,
            identity_seed: opts.identity_seed_base64url,
            companion_session: opts.companion,
            companion_descriptor: C::COMPANION,
            name: opts.name,
            timeout_ms: opts.timeout_ms,
            health_heartbeat_interval_ms: HEALTH_HEARTBEAT_INTERVAL_MS,
            kind: trellis_protocol::PrincipalKind::Device,
        })
        .await
    }

    /// Connect with a verified native OAuth admission, then retrieve application authority.
    #[cfg(feature = "native-oauth")]
    pub async fn connect_user(opts: UserConnectOptions<'_>) -> Result<Self, TrellisClientError> {
        let observation = Observation::start(
            DurationFamily::Connect,
            vec![
                KeyValue::new("trellis.phase", "total"),
                KeyValue::new("trellis.participant.kind", "user"),
            ],
            "cancelled",
        );
        let result = Self::connect_user_inner(opts).await;
        if let Ok(client) = &result {
            client.connection.usable();
        }
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => Self::client_outcome(error),
        };
        observation.finish(outcome);
        result
    }

    #[cfg(feature = "native-oauth")]
    async fn connect_user_inner(opts: UserConnectOptions<'_>) -> Result<Self, TrellisClientError> {
        let mut session = opts.session;
        let auth = session.auth.clone();
        let owner = AuthorizationOwner::new(
            session.trust.clone(),
            session.runtime_id.clone(),
            auth.session_key.clone(),
        )?;
        let runtime = AuthorizationRuntimeBinding {
            runtime_id: session.runtime_id.clone(),
            inbox_prefix: trellis_protocol::admission_inbox_prefix(&auth.session_key)?,
            transports: crate::client::AuthorizationRuntimeTransports {
                native: Some(crate::client::AuthorizationNativeTransport {
                    nats_servers: opts.nats_servers,
                    root_certificates: session.trust.root_certificates.clone(),
                }),
                websocket: None,
            },
        };
        let authorization_contexts = AuthorizationContextCache::new(
            owner,
            session.trust.clone(),
            runtime,
            &auth.session_key,
            super::authorization::AuthorizationCredential::OAuth(session.admission.clone()),
        )?;
        let authorization_contexts = Arc::new(authorization_contexts);
        let mut connected = Self::connect_context(
            auth,
            authorization_contexts.clone(),
            opts.timeout_ms,
            "user",
        )
        .await?;
        let generations = connected.generations.clone();
        let (renew_tx, mut renew_rx) = tokio::sync::mpsc::channel::<
            tokio::sync::oneshot::Sender<Result<(), TrellisClientError>>,
        >(1);
        connected.oauth_admission_refresh_requests = Some(renew_tx);
        connected.oauth_admission_refresh_task = Some(tokio::spawn(async move {
            loop {
                let remaining = session
                    .admission
                    .credential
                    .expires_at
                    .saturating_sub(now_iat_seconds() as i64);
                let request = tokio::select! {
                    biased;
                    _ = generations.wait_terminal() => return,
                    request = renew_rx.recv() => match request {
                        Some(request) => Some(request),
                        None => return,
                    },
                    _ = tokio::time::sleep(Duration::from_secs((remaining / 2).max(1) as u64)) =>
                        None,
                };
                // Manual and timed renewals share this one custody owner;
                // never retry a potentially spent refresh token elsewhere.
                let result: Result<(), TrellisClientError> = async {
                    let trust = authorization_contexts.owner.current_trust()?;
                    session.refresh(trust).await.map_err(|_| {
                        TrellisClientError::AuthorizationUnavailable(
                            "native OAuth recovery required".into(),
                        )
                    })?;
                    authorization_contexts
                        .owner
                        .install_oauth_rotations(session.take_issuer_rotations())
                        .await?;
                    authorization_contexts
                        .replace_oauth_admission(&session.auth, session.admission.clone())?;
                    // HTTP authenticates rotation from the retained pin. Wake ordinary
                    // fresh admission even when no previous NATS attachment survives.
                    authorization_contexts.request_refresh();
                    generations.authorization_promoted();
                    Ok(())
                }
                .await;
                let failed = result.is_err();
                if let Some(request) = request {
                    let _ = request.send(result);
                }
                if failed {
                    generations.close();
                    tracing::warn!("native OAuth credential renewal requires recovery");
                    return;
                }
            }
        }));
        Ok(connected)
    }

    async fn connect_context(
        auth: SessionAuth,
        authorization_contexts: Arc<AuthorizationContextCache>,
        timeout_ms: u64,
        kind: &'static str,
    ) -> Result<Self, TrellisClientError> {
        let inbox_prefix = authorization_contexts.runtime_binding()?.inbox_prefix;
        let auth = Arc::new(auth);
        let live_slot = std::sync::Arc::new(std::sync::Mutex::new(
            None::<std::sync::Weak<crate::live::manager::LiveSessionManager>>,
        ));
        // The generation manager owns every physical attachment: the initial
        // one here, and any wider one automatic adoption opens later. The
        // baseline lease keeps the initial framework-loop attachment alive until
        // provider/live intake migrates to the current generation.
        let (generations, baseline_lease) = crate::client::TransportGenerationManager::connect(
            auth.clone(),
            authorization_contexts.clone(),
            timeout_ms,
            live_slot.clone(),
        )
        .await?;
        let nats = baseline_lease.nats().clone();
        // The baseline attachment is not pinned for the connection lifetime: its
        // framework-loop attachment migrates to the current generation through
        // the established attachment contract, so the initial generation is
        // reclaimed once superseded. Retain the baseline generation's immutable
        // identity for the live manager's initial owner-control attachment
        // before `drop` releases the bootstrap hold here.
        let baseline_generation_id = baseline_lease.generation_id();
        let baseline_generation = baseline_lease.weak_generation();
        drop(baseline_lease);

        let live = crate::live::manager::LiveSessionManager::new(
            nats.clone(),
            baseline_generation_id,
            baseline_generation,
            auth.clone(),
            authorization_contexts.owner.clone(),
            authorization_contexts.runtime_binding()?.runtime_id,
        );
        if let Ok(mut slot) = live_slot.lock() {
            *slot = Some(std::sync::Arc::downgrade(&live));
        }
        let provider =
            attach_authorization_provider(authorization_contexts.clone(), generations.clone())
                .await?;
        let authorization_context_refresh_task = Some(
            crate::client::authorization::spawn_authorization_context_refresh_task(
                crate::client::authorization::AuthorizationRefreshRuntime {
                    contexts: authorization_contexts.clone(),
                    auth: auth.clone(),
                    generations: generations.clone(),
                },
            ),
        );
        Ok(Self {
            nats,
            inbox_prefix,
            authorization_provider: provider.provider,
            authorization_provider_stop: provider.stop,
            authorization_provider_task: provider.task,
            auth,
            timeout_ms,
            service_bootstrap_binding: None,
            health_heartbeat_task: None,
            authorization_contexts: Some(authorization_contexts),
            connection: std::sync::Arc::new(
                crate::telemetry::lifecycle::ConnectionRegistration::start(kind),
            ),
            generations,
            authorization_context_refresh_task,
            oauth_admission_refresh_task: None,
            oauth_admission_refresh_requests: None,
            companion: None,
            live: Some(live),
        })
    }

    /// Return the underlying NATS client for runtime-owned serving.
    ///
    /// Exposed to the runtime crate so a built-in subsystem can serve its
    /// public router over the exact authenticated provider connection it
    /// bootstrapped with.
    #[must_use]
    pub(crate) fn provider_nats(&self) -> async_nats::Client {
        self.nats.clone()
    }

    /// Return the signed authorization context used by this connection.
    pub fn authorization_context(
        &self,
    ) -> Result<Option<AuthorizationContextBundle>, TrellisClientError> {
        self.authorization_contexts
            .as_ref()
            .map(|cache| cache.bundle())
            .transpose()
    }

    /// Refresh and verify the current authorization context immediately.
    /// Native OAuth connections first renew admission through their single
    /// credential-custody worker and wait for authenticated transport coverage.
    pub async fn refresh_authorization_context(
        &self,
    ) -> Result<AuthorizationContextBundle, TrellisClientError> {
        let result = self.refresh_authorization_context_inner().await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => Self::client_outcome(error),
        };
        crate::telemetry::instruments::add_counter(
            CounterFamily::AuthRefreshAttempts,
            1,
            &[
                KeyValue::new("trellis.participant.kind", "user"),
                KeyValue::new("trellis.outcome", outcome),
            ],
        );
        result
    }

    async fn refresh_authorization_context_inner(
        &self,
    ) -> Result<AuthorizationContextBundle, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        if let Some(renew) = &self.oauth_admission_refresh_requests {
            let mut publication = self.generations.subscribe_publication();
            let mut changes = contexts.owner.subscribe_changes();
            let (complete, completed) = tokio::sync::oneshot::channel();
            renew.send(complete).await.map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("native renewal stopped".into())
            })?;
            tokio::select! {
                biased;
                cause = self.generations.wait_terminal() => return Err(cause.client_error()),
                completed = completed => completed.map_err(|_| {
                    TrellisClientError::AuthorizationUnavailable("native renewal stopped".into())
                })??,
            }
            // A retired session needs a privately admitted replacement before
            // own-context renewal can use a published physical attachment.
            tokio::time::timeout(Duration::from_secs(10), async {
                while contexts.owner.staged_own_authority().is_err()
                    || self.generations.acquire_published().is_err()
                {
                    tokio::select! {
                        biased;
                        cause = self.generations.wait_terminal() =>
                            return Err(cause.client_error()),
                        _ = publication.changed() => {},
                        _ = changes.recv() => {},
                    }
                }
                Ok(())
            })
            .await
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "native authority replacement unavailable".into(),
                )
            })??;
        }
        crate::client::authorization::install_prepared_authorization(
            &crate::client::authorization::AuthorizationRefreshRuntime {
                contexts: contexts.clone(),
                auth: self.auth.clone(),
                generations: self.generations.clone(),
            },
            None,
        )
        .await?;
        let mut availability = contexts.watch_availability();
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                biased;
                cause = self.generations.wait_terminal() => Err(cause.client_error()),
                ready = availability.wait_for(|state| state.is_usable()) => ready.map(|_| ())
                    .map_err(|_| TrellisClientError::AuthorizationUnavailable("authorization stopped".into())),
            }
        }).await.map_err(|_| TrellisClientError::AuthorizationUnavailable(
            "authorization coverage unavailable".into(),
        ))??;
        contexts.bundle()
    }

    pub(crate) fn availability(&self) -> crate::generated::AvailabilitySnapshot {
        self.authorization_contexts
            .as_ref()
            .map_or_else(Default::default, |contexts| contexts.availability())
    }

    pub(crate) fn watch_availability(
        &self,
    ) -> tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot> {
        self.authorization_contexts
            .as_ref()
            .expect("connected clients always retain authorization context state")
            .watch_availability()
    }

    /// Outcome classification for one client transport result.
    fn client_outcome(error: &TrellisClientError) -> &'static str {
        match error {
            TrellisClientError::Timeout => "timeout",
            TrellisClientError::Nats(_)
            | TrellisClientError::NatsConnect(_)
            | TrellisClientError::NatsRequest(_)
            | TrellisClientError::ServiceUnavailable(_)
            | TrellisClientError::BootstrapHttp { .. }
            | TrellisClientError::AuthorizationUnavailable(_)
            | TrellisClientError::TransportUnavailable(_) => "unavailable",
            TrellisClientError::RpcError(_) => "declared_error",
            TrellisClientError::TransferCancelled => "cancelled",
            TrellisClientError::Base64(_)
            | TrellisClientError::InvalidSeedLen(_)
            | TrellisClientError::Json(_)
            | TrellisClientError::Codec(_)
            | TrellisClientError::Subject(_)
            | TrellisClientError::Bootstrap(_)
            | TrellisClientError::OperationProtocol(_)
            | TrellisClientError::TransferProtocol(_)
            | TrellisClientError::EventSubscriptionProtocol(_)
            | TrellisClientError::LiveProtocol(_) => "invalid",
            _ => "error",
        }
    }

    pub(crate) fn own_deployment_id(&self) -> Result<String, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        let context = contexts.installed_signed_context()?;
        match &context.unsigned.binding {
            trellis_protocol::SessionBinding::Service { deployment_id, .. }
            | trellis_protocol::SessionBinding::Device { deployment_id, .. } => {
                Ok(deployment_id.clone())
            }
            _ => Err(TrellisClientError::Bootstrap(
                "runtime has no deployment assignment".into(),
            )),
        }
    }

    pub(crate) fn own_connection_id(&self) -> Result<String, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        let context = contexts.installed_signed_context()?;
        Ok(context.unsigned.runtime_id.clone())
    }

    pub(crate) fn own_instance_id(&self) -> Result<String, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        let context = contexts.installed_signed_context()?;
        match &context.unsigned.binding {
            trellis_protocol::SessionBinding::Service { instance_id, .. }
            | trellis_protocol::SessionBinding::Device { instance_id, .. } => {
                Ok(instance_id.clone())
            }
            _ => Err(TrellisClientError::Bootstrap(
                "runtime has no instance assignment".into(),
            )),
        }
    }

    /// Call an explicit API action with exact-payload proof and a certified reply.
    /// Route metadata is not inferred from the supplied subject.
    pub async fn request_json_value(
        &self,
        api_id: &'static str,
        action: trellis_protocol::CatalogActionIdentity,
        subject: &str,
        body: &Value,
    ) -> Result<Value, TrellisClientError> {
        self.request_json_value_routed(
            api_id,
            action,
            subject,
            body,
            crate::telemetry::instruments::unknown_route(),
        )
        .await
    }

    /// Call one descriptor-backed subject with its bounded route token.
    pub(crate) async fn request_json_value_routed(
        &self,
        api_id: &'static str,
        action: trellis_protocol::CatalogActionIdentity,
        subject: &str,
        body: &Value,
        route: &'static str,
    ) -> Result<Value, TrellisClientError> {
        let observation = Observation::start(
            DurationFamily::RpcClient,
            vec![KeyValue::new("trellis.route", route)],
            "cancelled",
        );
        let result = self
            .request_certified_json(api_id, action, None, subject.to_owned(), body.clone())
            .await;
        observation.finish(match &result {
            Ok(_) => "ok",
            Err(error) => Self::client_outcome(error),
        });
        result
    }

    /// Publish one descriptor-backed event.
    pub async fn publish<D>(&self, event: &D::Event) -> Result<(), TrellisClientError>
    where
        D: EventDescriptor,
    {
        let subject = self.descriptor_subject(D::SUBJECT);
        let prepared = prepare_event::<D>(event)?.with_subject(subject);
        self.publish_prepared(&prepared).await
    }

    /// Publish an event that was already prepared, preserving its subject, payload, and message id.
    pub async fn publish_prepared(
        &self,
        event: &PreparedTrellisEvent,
    ) -> Result<(), TrellisClientError> {
        let event = event
            .clone()
            .with_subject(self.descriptor_subject(event.subject()));
        let route = crate::telemetry::instruments::route_token(
            crate::telemetry::instruments::RouteFamily::Event,
            event.descriptor_identity(),
        );
        let observation = Observation::start(
            DurationFamily::EventPublish,
            vec![
                KeyValue::new("trellis.route", route),
                KeyValue::new("trellis.delivery", "durable"),
            ],
            "cancelled",
        );
        // One deadline covers generation acquisition and the publish/ack.
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let result = timeout(deadline.saturating_duration_since(Instant::now()), async {
            let publish = [event.subject().to_owned()];
            let lease = self
                .generations
                .acquire_for(&publish, &[], deadline)
                .await?;
            publish_prepared_event(
                lease.nats(),
                &self.auth,
                self.authorization_owner(),
                deadline,
                &event,
            )
            .await
        })
        .await
        .unwrap_or(Err(TrellisClientError::Timeout));
        observation.finish(if result.is_ok() { "ok" } else { "error" });
        result
    }

    /// Subscribe to one descriptor-backed event subject with explicit subscription options.
    pub async fn subscribe_with_options<D>(
        &self,
        options: EventSubscribeOptions,
    ) -> Result<BoxStream<'static, Result<D::Event, TrellisClientError>>, TrellisClientError>
    where
        D: EventDescriptor,
        D::Event: Send + 'static,
    {
        if options.mode == EventSubscriptionMode::Ephemeral {
            return self.subscribe_live::<D>().await;
        }

        let messages = self.subscribe_messages::<D>(options).await?;
        let stream = stream::try_unfold(messages, |mut messages| async move {
            match messages.next().await {
                Some(Ok(event_message)) => {
                    let event = event_message.decode()?;
                    event_message.ack().await?;
                    Ok(Some((event, messages)))
                }
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        });

        Ok(Box::pin(stream) as BoxStream<'static, Result<D::Event, TrellisClientError>>)
    }

    async fn subscribe_live<D>(
        &self,
    ) -> Result<BoxStream<'static, Result<D::Event, TrellisClientError>>, TrellisClientError>
    where
        D: EventDescriptor,
        D::Event: Send + 'static,
    {
        // Pin one generation for the whole subscription lifetime, charging the
        // subscription setup against the same deadline as acquisition.
        let deadline = self.transport_deadline();
        let subscribe = [self.descriptor_subject(D::SUBSCRIBE_SUBJECT)];
        let lease = self.acquire_transport(&[], &subscribe, deadline).await?;
        let subscriber = timeout(
            deadline.saturating_duration_since(Instant::now()),
            lease.nats().subscribe(subscribe[0].clone()),
        )
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;

        timeout(
            deadline.saturating_duration_since(Instant::now()),
            lease.nats().flush(),
        )
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;

        let owner = self.authorization_owner().clone();
        let verifier =
            crate::service::LocalAuthVerifier::new(owner.clone(), lease.nats().clone(), D::API_ID);
        let action_name = D::KEY.split_once('.').map_or(D::KEY, |(_, name)| name);
        let publish_action = trellis_protocol::CatalogActionIdentity {
            kind: trellis_protocol::ApiSurfaceKind::Event,
            name: action_name.to_owned(),
            direction: trellis_protocol::PermissionAction::Publish,
        };
        let subscribe_action = trellis_protocol::CatalogActionIdentity {
            direction: trellis_protocol::PermissionAction::Subscribe,
            ..publish_action.clone()
        };
        let stream = stream::try_unfold(
            (lease, subscriber, owner, verifier),
            move |(lease, mut subscriber, owner, verifier)| {
                let publish_action = publish_action.clone();
                let subscribe_action = subscribe_action.clone();
                async move {
                    while let Some(message) = subscriber.next().await {
                        owner.own_authority()?;
                        if let Err(error) = verifier
                            .verify_event(
                                message.subject.as_ref(),
                                &message.payload,
                                message.headers.as_ref(),
                                D::API_ID,
                                &publish_action,
                                None,
                            )
                            .await
                        {
                            tracing::warn!(
                                subject = %message.subject,
                                error = %error.message(),
                                "Event auth validation failed"
                            );
                            continue;
                        }
                        let own = owner.own_authority()?;
                        owner
                            .authorize_action(lease.nats(), &own, D::API_ID, &subscribe_action)
                            .await?;
                        let value: Value = serde_json::from_slice(&message.payload)?;
                        let event = D::Event::decode(value)
                            .map_err(|error| TrellisClientError::Codec(error.to_string()))?;
                        return Ok(Some((event, (lease, subscriber, owner, verifier))));
                    }
                    Ok(None)
                }
            },
        );

        Ok(Box::pin(stream) as BoxStream<'static, Result<D::Event, TrellisClientError>>)
    }

    /// Subscribe to authenticated descriptor-backed event messages with explicit ack/nak/term
    /// control.
    /// Retained messages are verified at their original broker publication time.
    pub async fn subscribe_messages<D>(
        &self,
        options: EventSubscribeOptions,
    ) -> Result<
        BoxStream<'static, Result<EventMessage<D::Event>, TrellisClientError>>,
        TrellisClientError,
    >
    where
        D: EventDescriptor,
        D::Event: Send + 'static,
    {
        let messages = self
            .event_messages(
                options,
                Some(self.descriptor_subject(D::SUBSCRIBE_SUBJECT)),
                None,
            )
            .await?;
        let owner = self.authorization_owner().clone();
        Ok(messages
            .then(move |message| {
                let owner = owner.clone();
                async move {
                    let message = message?;
                    owner.own_authority()?;
                    let lease = message._lease.as_ref().ok_or_else(|| {
                        TrellisClientError::EventSubscriptionProtocol(
                            "event transport unavailable".into(),
                        )
                    })?;
                    let verifier = crate::service::LocalAuthVerifier::new(
                        owner.clone(),
                        lease.nats().clone(),
                        D::API_ID,
                    );
                    let action = trellis_protocol::CatalogActionIdentity {
                        kind: trellis_protocol::ApiSurfaceKind::Event,
                        name: D::KEY
                            .split_once('.')
                            .map_or(D::KEY, |(_, name)| name)
                            .to_owned(),
                        direction: trellis_protocol::PermissionAction::Publish,
                    };
                    let status = verifier
                        .historical_status(message.headers())
                        .await
                        .map_err(|error| {
                            TrellisClientError::EventSubscriptionProtocol(
                                error.message().to_owned(),
                            )
                        })?;
                    let publication = message.original_publication()?;
                    verifier
                        .verify_event(
                            message.subject(),
                            message.payload(),
                            message.headers(),
                            D::API_ID,
                            &action,
                            Some((&publication, &status)),
                        )
                        .await
                        .map_err(|error| {
                            TrellisClientError::EventSubscriptionProtocol(
                                error.message().to_owned(),
                            )
                        })?;
                    owner.own_authority()?;
                    Ok(message)
                }
            })
            .boxed())
    }

    pub(crate) async fn event_messages<T: Send + 'static>(
        &self,
        options: EventSubscribeOptions,
        filter_subject: Option<String>,
        max_messages: Option<usize>,
    ) -> Result<BoxStream<'static, Result<EventMessage<T>, TrellisClientError>>, TrellisClientError>
    {
        // Framework intake reacquires on each bounded batch. A received delivery
        // keeps its exact attachment until its handler and disposition finish.
        let lease = self
            .acquire_transport(&[], &[], self.transport_deadline())
            .await?;
        self.event_messages_with_transport(lease, options, filter_subject, max_messages)
            .await
    }

    pub(crate) async fn event_messages_with_transport<T: Send + 'static>(
        &self,
        lease: crate::client::TransportLease,
        options: EventSubscribeOptions,
        filter_subject: Option<String>,
        max_messages: Option<usize>,
    ) -> Result<BoxStream<'static, Result<EventMessage<T>, TrellisClientError>>, TrellisClientError>
    {
        let jetstream = jetstream::new(lease.nats().clone());
        let stream_name = options.stream.as_deref().unwrap_or(DEFAULT_EVENT_STREAM);
        if options.mode == EventSubscriptionMode::Durable && options.durable_name.is_none() {
            return Err(TrellisClientError::EventSubscriptionProtocol(
                "durable event subscriptions require a pre-provisioned durable name".to_string(),
            ));
        }

        let durable_name = if options.mode == EventSubscriptionMode::Durable {
            options.durable_name.as_deref()
        } else {
            None
        };
        let consumer = match durable_name {
            Some(name) => timeout(
                std::time::Duration::from_millis(self.timeout_ms),
                jetstream.get_consumer_from_stream(name, stream_name),
            )
            .await
            .map_err(|_| TrellisClientError::Timeout)?
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?,
            None => {
                let event_stream = timeout(
                    std::time::Duration::from_millis(self.timeout_ms),
                    jetstream.get_stream_no_info(stream_name),
                )
                .await
                .map_err(|_| TrellisClientError::Timeout)?
                .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
                let subject = filter_subject.ok_or_else(|| {
                    TrellisClientError::EventSubscriptionProtocol(
                        "ephemeral event subscriptions require a filter subject".to_owned(),
                    )
                })?;
                let config = event_consumer_config(&options, subject);
                timeout(
                    std::time::Duration::from_millis(self.timeout_ms),
                    event_stream.create_consumer(config),
                )
                .await
                .map_err(|_| TrellisClientError::Timeout)?
                .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?
            }
        };

        let messages: BoxStream<'static, Result<jetstream::Message, TrellisClientError>> =
            if let Some(max_messages) = max_messages {
                let batch = timeout(
                    Duration::from_millis(self.timeout_ms),
                    consumer
                        .batch()
                        .max_messages(max_messages)
                        .expires(Duration::from_secs(1))
                        .messages(),
                )
                .await
                .map_err(|_| TrellisClientError::Timeout)?
                .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
                Box::pin(batch.map(|message| {
                    message.map_err(|error| TrellisClientError::NatsRequest(error.to_string()))
                }))
            } else {
                let messages = timeout(
                    Duration::from_millis(self.timeout_ms),
                    consumer.stream().expires(Duration::from_secs(1)).messages(),
                )
                .await
                .map_err(|_| TrellisClientError::Timeout)?
                .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
                Box::pin(messages.map(|message| {
                    message.map_err(|error| TrellisClientError::NatsRequest(error.to_string()))
                }))
            };

        let stream = messages.map(move |message| {
            message.map(|message| EventMessage {
                message,
                _lease: Some(lease.clone()),
                _event: PhantomData,
            })
        });

        Ok(Box::pin(stream)
            as BoxStream<
                'static,
                Result<EventMessage<T>, TrellisClientError>,
            >)
    }

    /// Subscribe to one descriptor-backed live and decode event payloads.
    /// Subscribe to one generated Live through the connection's live manager.
    ///
    /// Returns a prepared, owned handle. The first `poll_next` installs the
    /// exact data subscription and activates the session; an uniterated handle
    /// expires without starting the provider's domain source.
    ///
    /// # Errors
    ///
    /// Returns a setup error for an invalid input, a missing live manager, an
    /// incompatible peer, or a lost/invalid offer.
    pub async fn live<D>(
        &self,
        input: &D::Input,
    ) -> Result<crate::live::subscription::LiveSubscription<D::Event>, TrellisClientError>
    where
        D: LiveDescriptor,
        D::Event: Send + 'static,
    {
        let encoded = input
            .encode()
            .map_err(|error| TrellisClientError::Codec(error.to_string()))?;
        let base_subject = D::SUBJECT.to_owned();
        // Pin one generation for the whole observation: the opening exchange and
        // the data/control pump all use this exact physical attachment.
        let deadline = self.transport_deadline();
        let lease = self
            .acquire_transport(
                std::slice::from_ref(&base_subject),
                &[format!("{}.>", self.inbox_prefix)],
                deadline,
            )
            .await?;
        let receive_max_payload_bytes = lease.nats().max_payload() as u64;
        let open_id = trellis_protocol::generate_nonce()
            .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
        let body = serde_json::json!({
            "format": trellis_protocol::LIVE_VERSION,
            "type": "open",
            "openId": open_id,
            "receiveMaxPayloadBytes": receive_max_payload_bytes,
            "input": encoded,
        });
        let action_name = D::KEY.split_once('.').map_or(D::KEY, |(_, action)| action);
        let action = trellis_protocol::CatalogActionIdentity {
            kind: trellis_protocol::ApiSurfaceKind::Live,
            name: action_name.to_owned(),
            direction: trellis_protocol::PermissionAction::Subscribe,
        };
        let open = crate::live::client_open::ClientOpen {
            kind: trellis_protocol::LiveSessionKind::Standalone,
            api_id: D::API_ID,
            base_subject: &base_subject,
            publish_subject: &base_subject,
            body: Bytes::from(serde_json::to_vec(&body)?),
            open_id,
            receive_max_payload_bytes,
            action,
        };
        let prepared = crate::live::client_open::open_client_session(
            self,
            &self.authorization_provider,
            open,
            lease,
            deadline,
        )
        .await?;
        crate::live::client_open::install_live_handle::<D>(self, prepared).await
    }

    /// Download the bytes exposed by a receive transfer grant.
    pub async fn download_transfer(
        &self,
        grant: &DownloadTransferGrant,
    ) -> Result<Vec<u8>, TrellisClientError> {
        get_download_grant(self, grant).await
    }

    /// Stream the bytes exposed by a receive transfer grant into `writer`.
    pub async fn download_transfer_into<W>(
        &self,
        grant: &DownloadTransferGrant,
        writer: &mut W,
    ) -> Result<FileInfo, TrellisClientError>
    where
        W: tokio::io::AsyncWrite + Unpin + Send + ?Sized,
    {
        crate::client::transfer::get_download_grant_into(self, grant, writer).await
    }

    /// Stream a receive transfer into `writer` with authenticated cancellation.
    pub async fn download_transfer_into_with_cancel<W>(
        &self,
        grant: &DownloadTransferGrant,
        writer: &mut W,
        cancellation: &crate::client::TransferCancellation,
    ) -> Result<FileInfo, TrellisClientError>
    where
        W: tokio::io::AsyncWrite + Unpin + Send + ?Sized,
    {
        crate::client::transfer::get_download_grant_into_with_cancel(
            self,
            grant,
            writer,
            Some(cancellation),
        )
        .await
    }
}

impl TrellisClient {
    pub(crate) fn abort_native_runtime(&self) {
        self.stop_background_tasks();
        self.generations.close();
    }

    pub(crate) async fn shutdown_native_runtime(&self) -> Result<(), TrellisClientError> {
        {
            let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
                TrellisClientError::Bootstrap("runtime provider authorization unavailable".into())
            })?;
            let transition = contexts.lock_own_transition()?;
            self.generations.close_runtime(&transition);
        }
        self.stop_background_tasks();
        self.generations.wait_stopped().await;
        Ok(())
    }

    fn stop_background_tasks(&self) {
        if let Some(task) = self.health_heartbeat_task.as_ref() {
            task.abort();
        }
        if let Some(task) = self.authorization_context_refresh_task.as_ref() {
            task.abort();
        }
        if let Some(task) = self.oauth_admission_refresh_task.as_ref() {
            task.abort();
        }
        if let Some(live) = self.live.as_ref() {
            live.stop();
        }
        let _ = self.authorization_provider_stop.send(());
        self.authorization_provider_task.abort();
    }
}

impl Drop for TrellisClient {
    fn drop(&mut self) {
        self.stop_background_tasks();
        // Stop automatic adoption and release every physical generation.
        self.generations.close();
    }
}

impl TrellisClient {
    async fn request_certified_json(
        &self,
        api_id: &'static str,
        action: trellis_protocol::CatalogActionIdentity,
        expected_deployment_id: Option<String>,
        subject: String,
        body: Value,
    ) -> Result<Value, TrellisClientError> {
        let deadline = self.transport_deadline();
        timeout(deadline.saturating_duration_since(Instant::now()), async {
            let lease = self
                .acquire_transport(
                    std::slice::from_ref(&subject),
                    &[format!("{}.>", self.inbox_prefix)],
                    deadline,
                )
                .await?;
            let own = self.authorization_owner().own_authority()?;
            self.authorization_owner()
                .authorize_action(lease.nats(), &own, api_id, &action)
                .await?;
            let api = own
                .authority()
                .apis
                .iter()
                .find(|api| api.api_id == api_id)
                .ok_or_else(|| {
                    TrellisClientError::AuthorizationUnavailable("operation API unavailable".into())
                })?;
            let reply = lease.nats().new_inbox();
            let request = trellis_protocol::SessionRequest {
                authority_digest: own.digest().to_owned(),
                api_id: api_id.to_owned(),
                api_generation: api.generation,
                accepted_revision: api.accepted_revision,
                action: action.name.clone(),
                subject: subject.clone(),
                reply_subject: Some(reply.clone()),
                request_id: new_request_id(),
                issued_at: now_iat_seconds() as i64,
            };
            let payload = serde_json::to_vec(&body)?;
            let (proof, digest) = self.auth.create_request_proof(&request, &payload)?;
            let mut headers = HeaderMap::new();
            headers.insert(
                "Trellis-Session-Request",
                serde_json::to_string(&request)?.as_str(),
            );
            headers.insert("authorization-context", request.authority_digest.as_str());
            headers.insert("iat", request.issued_at.to_string().as_str());
            headers.insert("request-id", request.request_id.as_str());
            headers.insert(
                "Trellis-Api-Generation",
                request.api_generation.get().to_string().as_str(),
            );
            headers.insert(
                "Trellis-Accepted-Revision",
                request.accepted_revision.get().to_string().as_str(),
            );
            headers.insert("proof", proof.as_str());
            headers.insert("session-key", self.auth.session_key.as_str());
            let mut trace_pairs = Vec::new();
            crate::telemetry::propagation::inject_context(
                &opentelemetry::Context::current(),
                &mut trace_pairs,
            );
            for (key, value) in trace_pairs {
                if matches!(key.as_str(), "traceparent" | "tracestate") {
                    headers.insert(key.as_str(), value.as_str());
                }
            }
            // A single send: invocation deduplication does not span deployments.
            let message = timeout(
                deadline.saturating_duration_since(Instant::now()),
                lease.nats().send_request(
                    subject,
                    async_nats::Request::new()
                        // The enclosing timeout owns the caller's configured deadline.
                        .timeout(None)
                        .inbox(reply)
                        .headers(headers)
                        .payload(Bytes::from(payload)),
                ),
            )
            .await
            .map_err(|_| TrellisClientError::Timeout)?
            .map_err(TrellisClientError::from)?;
            let headers = message.headers.as_ref().ok_or_else(|| {
                TrellisClientError::OperationProtocol("uncertified operation response".into())
            })?;
            let response: trellis_protocol::SessionResponse = serde_json::from_str(
                headers
                    .get("Trellis-Session-Response")
                    .ok_or_else(|| {
                        TrellisClientError::OperationProtocol(
                            "missing signed operation response".into(),
                        )
                    })?
                    .as_str(),
            )?;
            let proof = trellis_protocol::SessionResponseProof::parse(
                headers
                    .get("proof")
                    .ok_or_else(|| {
                        TrellisClientError::OperationProtocol(
                            "missing operation response proof".into(),
                        )
                    })?
                    .as_str(),
            )
            .map_err(|error| TrellisClientError::OperationProtocol(error.to_string()))?;
            let value: Value = serde_json::from_slice(&message.payload)?;
            // This field is untrusted until the certificate and exact payload proof
            // below establish that the accepting provider owns that deployment.
            let accepted_owner = if action.kind == trellis_protocol::ApiSurfaceKind::Operation
                && action.direction == trellis_protocol::PermissionAction::Invoke
                && value.get("kind").and_then(Value::as_str) == Some("accepted")
            {
                Some(
                    value
                        .pointer("/ref/deploymentId")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            TrellisClientError::OperationProtocol(
                                "accepted operation omitted owner deployment".into(),
                            )
                        })?,
                )
            } else {
                None
            };
            if expected_deployment_id
                .as_deref()
                .zip(accepted_owner)
                .is_some_and(|(expected, actual)| expected != actual)
            {
                return Err(TrellisClientError::OperationProtocol(
                    "operation owner mismatch".into(),
                ));
            }
            self.authorization_owner()
                .verify_response(
                    lease.nats(),
                    crate::client::ResponseVerificationInput {
                        pending_request: &request,
                        pending_request_digest: &digest,
                        action: &action,
                        expected_deployment_id: expected_deployment_id
                            .as_deref()
                            .or(accepted_owner),
                        received_subject: message.subject.as_str(),
                        response: &response,
                        payload: &message.payload,
                        proof: &proof,
                    },
                )
                .await?;
            self.authorization_owner()
                .assert_accepted_response_allowed(&own)?;
            if response.kind == trellis_protocol::SessionResponseKind::Error {
                return Err(TrellisClientError::RpcError(
                    RpcErrorPayload::from_json_slice(&message.payload)?,
                ));
            }
            Ok(value)
        })
        .await
        .map_err(|_| TrellisClientError::Timeout)?
    }
}

impl OperationTransport for TrellisClient {
    async fn request_json_value(
        &self,
        api_id: &'static str,
        action: trellis_protocol::CatalogActionIdentity,
        expected_deployment_id: Option<String>,
        subject: String,
        body: Value,
    ) -> Result<Value, TrellisClientError> {
        self.request_certified_json(api_id, action, expected_deployment_id, subject, body)
            .await
    }

    async fn put_upload_transfer(
        &self,
        grant: UploadTransferGrant,
        body: Vec<u8>,
    ) -> Result<FileInfo, TrellisClientError> {
        put_upload_grant(self, &grant, body).await
    }

    async fn put_upload_transfer_from<'a, R>(
        &'a self,
        grant: UploadTransferGrant,
        reader: &'a mut R,
    ) -> Result<FileInfo, TrellisClientError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + ?Sized + 'a,
    {
        crate::client::transfer::put_upload_grant_from(self, &grant, reader).await
    }

    async fn put_upload_transfer_from_with_cancel<'a, R>(
        &'a self,
        grant: UploadTransferGrant,
        reader: &'a mut R,
        cancellation: &'a crate::client::TransferCancellation,
    ) -> Result<FileInfo, TrellisClientError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + ?Sized + 'a,
    {
        crate::client::transfer::put_upload_grant_from_with_cancel(
            self,
            &grant,
            reader,
            Some(cancellation),
        )
        .await
    }
}

fn event_consumer_config(
    options: &EventSubscribeOptions,
    filter_subject: String,
) -> consumer::pull::Config {
    consumer::pull::Config {
        durable_name: match options.mode {
            EventSubscriptionMode::Durable => options.durable_name.clone(),
            EventSubscriptionMode::Ephemeral => None,
        },
        deliver_policy: match options.replay {
            EventReplayPolicy::All => consumer::DeliverPolicy::All,
            EventReplayPolicy::New => consumer::DeliverPolicy::New,
        },
        ack_policy: consumer::AckPolicy::Explicit,
        filter_subject,
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "connection_mirror_tests.rs"]
mod mirror_tests;
