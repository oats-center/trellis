use async_nats::header::HeaderMap;
use async_nats::jetstream::{self, consumer, AckKind};
use async_nats::ConnectOptions;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use bytes::Bytes;
use futures_util::stream::{self, BoxStream};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::marker::PhantomData;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use trellis_protocol::{NativeBootstrapSessionProofInput, SessionProofInput};

use crate::telemetry::instruments::{CounterFamily, DurationFamily};
use crate::telemetry::lifecycle::Observation;
use crate::telemetry::KeyValue;

/// Catalog participant kind for one authorization principal kind.
fn participant_kind_label(kind: &trellis_protocol::AuthorizationPrincipalKind) -> &'static str {
    match kind {
        trellis_protocol::AuthorizationPrincipalKind::User => "user",
        trellis_protocol::AuthorizationPrincipalKind::Service => "service",
        trellis_protocol::AuthorizationPrincipalKind::Device => "device",
    }
}

use super::events::{EVENT_ID_HEADER, EVENT_TIME_HEADER};
use crate::client::authorization::is_retriable_authorization_code;
use crate::client::authorization::{AuthorizationTransportRotation, TransportRotationDisposition};
use crate::client::operations::OperationTransport;
use crate::client::proof::{base64url_decode, new_request_id, now_iat_seconds};
use crate::client::transfer::{get_download_grant, DownloadTransferGrant};
use crate::client::transfer::{put_upload_grant, FileInfo, UploadTransferGrant};
use crate::client::{
    prepare_event, AuthorizationContextBundle, AuthorizationContextCache,
    AuthorizationProviderCache, AuthorizationRuntimeBinding, EventDescriptor, LiveDescriptor,
    PreparedTrellisEvent, RpcErrorPayload, SessionAuth, TrellisClientError,
};
use crate::generated::Codec as _;
use crate::service::{BootstrapBinding, CoreBootstrapBinding, ServiceResourceBindings};

const HEALTH_HEARTBEAT_SUBJECT_PREFIX: &str = "health.v1.heartbeat";
const HEALTH_HEARTBEAT_INTERVAL_MS: u64 = 30_000;
const DEFAULT_EVENT_STREAM: &str = "trellis";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppliedNativeAuthorization {
    runtime: AuthorizationRuntimeBinding,
    context_digest: String,
    routing_jwt: String,
}

impl AppliedNativeAuthorization {
    pub(crate) fn from_cache(
        contexts: &AuthorizationContextCache,
    ) -> Result<Self, TrellisClientError> {
        let (runtime, context_digest, routing_jwt) = contexts.applied_transport()?;
        Ok(Self {
            runtime,
            context_digest,
            routing_jwt,
        })
    }

    fn record(
        &mut self,
        refreshed: Self,
        result: Result<(), TrellisClientError>,
    ) -> Result<(), TrellisClientError> {
        result?;
        *self = refreshed;
        Ok(())
    }

    /// Whether installing `refreshed` must rotate the physical NATS attachment.
    pub(crate) fn rotates_to(&self, refreshed: &Self) -> bool {
        let credentials_changed = self.context_digest != refreshed.context_digest
            || self.routing_jwt != refreshed.routing_jwt;
        credentials_changed || self.runtime.transports != refreshed.runtime.transports
    }
}

pub(crate) fn signed_headers(
    auth: &SessionAuth,
    context_digest: &str,
    subject: &str,
    reply: &str,
    payload: &[u8],
) -> Result<HeaderMap, TrellisClientError> {
    let iat = now_iat_seconds() as i64;
    let request_id = new_request_id();
    let proof =
        auth.create_request_proof(context_digest, subject, reply, payload, iat, &request_id)?;
    let mut headers = HeaderMap::new();
    headers.insert("authorization-context", context_digest);
    headers.insert("session-key", auth.session_key.as_str());
    headers.insert("proof", proof.as_str());
    headers.insert("iat", iat.to_string().as_str());
    headers.insert("request-id", request_id.as_str());
    Ok(headers)
}

/// Internal service connection inputs; assignment and resources come from the server.
pub struct ServiceConnectWithContractOptions<'a> {
    pub trellis_url: &'a str,
    pub participant_id: &'a str,
    pub participant_path: &'static str,
    pub package_evidence: crate::generated::PackageEvidence,
    pub provisioned_identity_seed_base64url: &'a str,
    pub name: Option<&'a str>,
    pub timeout_ms: u64,
}

/// Runtime and device-identity options for an activated device principal.
///
/// The type parameter `C` supplies the generated participant identity.
pub struct DeviceConnectOptions<'a, C> {
    trellis_url: &'a str,
    participant_id: &'a str,
    identity_seed_base64url: &'a str,
    companion_installation_seed_base64url: Option<&'a str>,
    timeout_ms: u64,
    name: Option<&'a str>,
    contract_type: std::marker::PhantomData<C>,
}

impl<'a, C: crate::generated::ParticipantDescriptor> DeviceConnectOptions<'a, C> {
    /// Create device connection options using the participant identity from `C`.
    ///
    /// Runtime bootstrap generates fresh session keys internally; this constructor accepts only
    /// runtime location and the provisioned device seed.
    pub fn new(trellis_url: &'a str, identity_seed_base64url: &'a str) -> Self {
        Self {
            trellis_url,
            participant_id: C::ID,
            identity_seed_base64url,
            companion_installation_seed_base64url: None,
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

    /// Attach the root-derived installation seed for the descriptor's exact companion.
    pub fn with_companion_installation_seed(mut self, seed_base64url: &'a str) -> Self {
        self.companion_installation_seed_base64url = Some(seed_base64url);
        self
    }

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
pub(crate) struct DeviceEnrollmentResponse {
    pub(crate) server_now: i64,
    pub(crate) state: String,
    pub(crate) activation: Option<DeviceBootstrapActivation>,
}

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
    participant_id: String,
    login_session_id: String,
    required: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NatsConnectToken {
    format: &'static str,
    context_digest: String,
}

#[derive(Clone, Debug)]
struct HealthHeartbeatConfig {
    session_key: String,
    service_name: String,
    kind: HealthHeartbeatServiceKind,
    deployment_id: String,
    instance_id: String,
    contract_id: String,
    contract_digest: String,
    started_at: String,
    publish_interval_ms: u64,
}

struct NativeConnectOptions<'a> {
    trellis_url: &'a str,
    participant_id: &'a str,
    participant_path: &'static str,
    package_evidence: crate::generated::PackageEvidence,
    identity_seed: &'a str,
    companion_installation_seed: Option<&'a str>,
    companion_descriptor: Option<crate::generated::CompanionDescriptor>,
    name: Option<&'a str>,
    timeout_ms: u64,
    kind: trellis_protocol::AuthorizationPrincipalKind,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum HealthHeartbeatServiceKind {
    Service,
    Device,
}

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
        "packageEvidence": C::package_evidence(),
        "participantPath": C::PATH,
        "packageDigest": C::package_evidence().root_digest(),
    });
    if let Some(provisioning_secret) = provisioning_secret {
        request["provisioningSecret"] = serde_json::Value::String(provisioning_secret.to_owned());
    }
    match (C::COMPANION, opts.companion_installation_seed_base64url) {
        (Some(companion), Some(seed)) => {
            let installation = SessionAuth::from_seed_base64url(seed)?;
            let companion_kind = match companion.kind {
                crate::generated::ParticipantKind::App => "app",
                crate::generated::ParticipantKind::Agent => "agent",
                _ => unreachable!("validated companion kind"),
            };
            let claim = serde_json::json!({
                "participantId": companion.id,
                "kind": companion_kind,
                "installationPublicKey": installation.session_key,
            });
            let digest = trellis_protocol::digest_json(&serde_json::json!({
                "format": "trellis.device.user-companion.v1",
                "origin": origin,
                "enrollment": request,
                "claim": claim,
            }))
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
            let digest = base64url_decode(&digest)?;
            request["companion"] = serde_json::json!({
                "participantId": companion.id,
                "kind": companion_kind,
                "installationPublicKey": installation.session_key,
                "requestProof": installation.sign_bytes(&digest),
            });
        }
        (Some(_), None) => {
            return Err(TrellisClientError::Bootstrap(
                "device companion installation seed is required".to_owned(),
            ));
        }
        (None, Some(_)) => {
            return Err(TrellisClientError::Bootstrap(
                "device descriptor has no companion".to_owned(),
            ));
        }
        (None, None) => {}
    }
    let input = SessionProofInput::device_enrollment(NativeBootstrapSessionProofInput {
        origin: origin.clone(),
        unsigned_request: request.clone(),
    })
    .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
    request["proof"] = serde_json::to_value(identity_auth.sign_session_proof(&input)?)?;
    let response = reqwest::Client::builder()
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

fn now_context_millis() -> Result<i64, TrellisClientError> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?
            .as_millis(),
    )
    .map_err(|_| TrellisClientError::Bootstrap("context time overflow".into()))
}

fn health_subject_token(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value.as_bytes())
}

fn health_heartbeat_subject(config: &HealthHeartbeatConfig) -> String {
    let kind = match config.kind {
        HealthHeartbeatServiceKind::Service => "service",
        HealthHeartbeatServiceKind::Device => "device",
    };
    format!(
        "{HEALTH_HEARTBEAT_SUBJECT_PREFIX}.{kind}.{}.{}.{}.{}.{}",
        health_subject_token(&config.contract_id),
        health_subject_token(&config.contract_digest),
        health_subject_token(&config.deployment_id),
        health_subject_token(&config.instance_id),
        config.session_key,
    )
}

fn build_health_heartbeat(config: &HealthHeartbeatConfig) -> Value {
    serde_json::json!({
        "sample": {
            "id": ulid::Ulid::new().to_string(),
            "time": now_rfc3339(),
        },
        "participant": {
            "name": config.service_name,
            "kind": config.kind,
            "instanceId": config.instance_id,
            "contractId": config.contract_id,
            "contractDigest": config.contract_digest,
            "startedAt": config.started_at,
            "publishIntervalMs": config.publish_interval_ms,
            "runtime": "rust",
        },
        "reportedStatus": "healthy",
        "checks": [{
            "name": "nats",
            "status": "ok",
            "latencyMs": 0.0,
        }],
    })
}

fn signed_event_headers(
    auth: &SessionAuth,
    context_digest: &str,
    event: &PreparedTrellisEvent,
) -> Result<HeaderMap, TrellisClientError> {
    let mut headers = event.publish_headers();
    headers.insert("authorization-context", context_digest);
    headers.insert("session-key", auth.session_key.as_str());
    headers.insert(
        "proof",
        auth.create_event_proof(
            context_digest,
            event.descriptor_identity(),
            event.subject(),
            event.payload(),
            event.event_id(),
            event.event_time(),
        )?
        .as_str(),
    );
    Ok(headers)
}

async fn publish_prepared_event(
    nats: &async_nats::Client,
    auth: &SessionAuth,
    context_digest: &str,
    timeout_ms: u64,
    event: &PreparedTrellisEvent,
) -> Result<(), TrellisClientError> {
    let jetstream = jetstream::new(nats.clone());
    let headers = signed_event_headers(auth, context_digest, event)?;
    let publish = async {
        jetstream
            .publish_with_headers(event.subject().to_string(), headers, event.payload_bytes())
            .await
    };
    let ack = timeout(std::time::Duration::from_millis(timeout_ms), publish)
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    timeout(std::time::Duration::from_millis(timeout_ms), ack)
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    Ok(())
}

async fn publish_health_heartbeat(
    nats: &async_nats::Client,
    timeout_ms: u64,
    config: &HealthHeartbeatConfig,
) -> Result<(), TrellisClientError> {
    let payload = Bytes::from(serde_json::to_vec(&build_health_heartbeat(config))?);
    let jetstream = jetstream::new(nats.clone());
    let publish = jetstream.publish(health_heartbeat_subject(config), payload);
    let ack = timeout(std::time::Duration::from_millis(timeout_ms), publish)
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    timeout(std::time::Duration::from_millis(timeout_ms), ack)
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    Ok(())
}

fn spawn_health_heartbeat_task(
    nats: async_nats::Client,
    timeout_ms: u64,
    config: HealthHeartbeatConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(config.publish_interval_ms));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = publish_health_heartbeat(&nats, timeout_ms, &config).await {
                tracing::warn!(%error, "failed to publish health heartbeat");
            }
        }
    })
}

fn spawn_authorization_context_refresh_task(
    contexts: std::sync::Arc<AuthorizationContextCache>,
    auth: Arc<SessionAuth>,
    nats: async_nats::Client,
    applied_native_authorization: Arc<tokio::sync::Mutex<AppliedNativeAuthorization>>,
    provider: AuthorizationProviderCache,
    timeout_ms: u64,
    rotation: Arc<AuthorizationTransportRotation>,
    live: Arc<crate::live::manager::LiveSessionManager>,
) -> JoinHandle<()> {
    crate::client::authorization::spawn_authorization_context_refresh_task(
        contexts,
        auth,
        nats,
        applied_native_authorization,
        provider,
        timeout_ms,
        rotation,
        live,
    )
}

/// Connected provider-cache handle returned by attach.
struct AuthorizationProviderHandle {
    provider: AuthorizationProviderCache,
    stop: tokio::sync::watch::Sender<()>,
    task: JoinHandle<()>,
}

/// Attach the connected NATS authorization registry to this client's provider
/// cache and wait for its complete snapshot before returning.
async fn attach_authorization_provider(
    nats: async_nats::Client,
    authorization_contexts: Arc<AuthorizationContextCache>,
) -> Result<AuthorizationProviderHandle, TrellisClientError> {
    let registry_binding = match authorization_contexts.bundle() {
        Ok(bundle) => bundle.authorization_registry.clone(),
        Err(error) => return Err(error),
    };
    let provider = match AuthorizationProviderCache::attach(
        nats.clone(),
        &registry_binding,
        authorization_contexts.clone(),
    )
    .await
    {
        Ok(provider) => provider,
        Err(error) => {
            let _ = nats.drain().await;
            return Err(error);
        }
    };
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(());
    let watcher = provider.clone();
    let task_stop = stop_tx.clone();
    let task_nats = nats.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = watcher.run(stop_rx).await {
            tracing::warn!(%error, "authorization provider watch stopped");
            let _ = task_stop.send(());
            let _ = task_nats.drain().await;
        }
    });
    let ready = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        provider.wait_ready(stop_tx.subscribe()),
    )
    .await
    {
        Ok(ready) => ready,
        Err(_) => {
            let _ = stop_tx.send(());
            task.abort();
            let _ = nats.drain().await;
            return Err(TrellisClientError::Bootstrap(
                "authorization provider did not become ready".into(),
            ));
        }
    };
    if let Err(error) = ready {
        let _ = stop_tx.send(());
        task.abort();
        let _ = nats.drain().await;
        return Err(error);
    }
    Ok(AuthorizationProviderHandle {
        provider,
        stop: stop_tx,
        task,
    })
}

async fn connect_authorized_nats(
    auth: Arc<SessionAuth>,
    authorization_contexts: Arc<AuthorizationContextCache>,
    timeout_ms: u64,
    refresh_before_connect: bool,
    live_slot: std::sync::Arc<
        std::sync::Mutex<Option<std::sync::Weak<crate::live::manager::LiveSessionManager>>>,
    >,
    rotation: Arc<AuthorizationTransportRotation>,
) -> Result<async_nats::Client, TrellisClientError> {
    if refresh_before_connect {
        authorization_contexts.refresh(&auth).await?;
    }
    let runtime = authorization_contexts.runtime_binding()?;
    let key_pair = Arc::new(auth.nkey_pair()?);
    let session_nkey = key_pair.public_key();
    let contexts = authorization_contexts.clone();
    let event_contexts = contexts.clone();
    let next_rotation = rotation.clone();
    let options = ConnectOptions::with_auth_callback(move |nonce| {
        let contexts = contexts.clone();
        let key_pair = key_pair.clone();
        let session_nkey = session_nkey.clone();
        async move {
            let (routing_jwt, context_digest) = contexts
                .transport_credentials()
                .map_err(async_nats::AuthError::new)?;
            let mut credentials = async_nats::Auth::new();
            credentials.nkey = Some(session_nkey);
            credentials.jwt = Some(routing_jwt);
            credentials.signature =
                Some(key_pair.sign(&nonce).map_err(async_nats::AuthError::new)?);
            credentials.token = Some(
                serde_json::to_string(&NatsConnectToken {
                    format: "trellis.nats-connect-token.v1",
                    context_digest,
                })
                .map_err(async_nats::AuthError::new)?,
            );
            Ok(credentials)
        }
    })
    .connection_timeout(Duration::from_millis(timeout_ms))
    .event_callback(move |event| {
        let contexts = event_contexts.clone();
        let live_slot = live_slot.clone();
        let rotation = next_rotation.clone();
        async move {
            if matches!(event, async_nats::Event::Disconnected) {
                if rotation.observe_disconnect() == TransportRotationDisposition::Planned {
                    // The physical attachment is rotating to install refreshed
                    // routing credentials. The logical connection and its
                    // application authorization are preserved.
                    contexts.request_coverage_reconciliation();
                    tracing::debug!("planned authorization transport rotation disconnecting");
                } else {
                    contexts.suspend();
                    contexts.request_coverage_reconciliation();
                    if let Ok(slot) = live_slot.lock() {
                        if let Some(live) = slot.as_ref().and_then(std::sync::Weak::upgrade) {
                            live.suspend();
                        }
                    }
                    tracing::info!(
                        context_digest = contexts.retained_context_digest().ok(),
                        "suspended authorization installation after NATS disconnect"
                    );
                }
            }
            if matches!(event, async_nats::Event::Connected) {
                if rotation.observe_connected() == TransportRotationDisposition::Planned {
                    contexts.request_coverage_reconciliation();
                    tracing::debug!("planned authorization transport rotation reconnected");
                } else {
                    contexts.request_coverage_reconciliation();
                    if let Ok(slot) = live_slot.lock() {
                        if let Some(live) = slot.as_ref().and_then(std::sync::Weak::upgrade) {
                            live.resume();
                        }
                    }
                }
            }
            if matches!(
                event,
                async_nats::Event::ServerError(async_nats::ServerError::AuthorizationViolation)
            ) {
                // A broker rejection is always a real authorization problem,
                // even when a planned rotation happens to be in flight.
                rotation.cancel();
                contexts.suspend();
                contexts.request_refresh();
                tracing::info!(
                    "requested authorization refresh after broker authentication rejection"
                );
            }
            tracing::debug!(event = ?event, "observed NATS client event");
        }
    })
    .custom_inbox_prefix(runtime.inbox_prefix);
    let native = runtime.transports.native.ok_or_else(|| {
        TrellisClientError::AuthorizationUnavailable(
            "bootstrap did not offer a native NATS transport".into(),
        )
    })?;
    options
        .connect(native.nats_servers)
        .await
        .map_err(|error| TrellisClientError::NatsConnect(error.to_string()))
}

pub(crate) async fn apply_native_runtime_refresh(
    nats: &async_nats::Client,
    previous: &AuthorizationRuntimeBinding,
    refreshed: &AuthorizationRuntimeBinding,
    credentials_changed: bool,
    timeout_ms: u64,
) -> Result<(), TrellisClientError> {
    if previous.connection_id != refreshed.connection_id
        || previous.login_session_id != refreshed.login_session_id
        || previous.participant_id != refreshed.participant_id
        || previous.inbox_prefix != refreshed.inbox_prefix
    {
        return Err(TrellisClientError::Bootstrap(
            "refreshed native runtime changed stable session binding".into(),
        ));
    }
    if !credentials_changed && previous.transports == refreshed.transports {
        return Ok(());
    }
    let native = refreshed.transports.native.as_ref().ok_or_else(|| {
        TrellisClientError::AuthorizationUnavailable(
            "bootstrap did not offer a native NATS transport".into(),
        )
    })?;
    nats.set_server_pool(native.nats_servers.as_slice())
        .await
        .map_err(|error| TrellisClientError::NatsConnect(error.to_string()))?;
    let connects = nats.statistics().connects.load(Ordering::Relaxed);
    nats.force_reconnect()
        .await
        .map_err(|error| TrellisClientError::NatsConnect(error.to_string()))?;
    timeout(Duration::from_millis(timeout_ms), async {
        while nats.statistics().connects.load(Ordering::Relaxed) <= connects {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        nats.flush().await
    })
    .await
    .map_err(|_| TrellisClientError::Timeout)?
    .map_err(|error| TrellisClientError::NatsConnect(error.to_string()))
}

pub(crate) async fn apply_native_authorization_refresh(
    nats: &async_nats::Client,
    applied: &mut AppliedNativeAuthorization,
    refreshed: AppliedNativeAuthorization,
    timeout_ms: u64,
    rotation: &AuthorizationTransportRotation,
    contexts: &AuthorizationContextCache,
    live: Option<&crate::live::manager::LiveSessionManager>,
    planned: bool,
) -> Result<(), TrellisClientError> {
    let credentials_changed = applied.context_digest != refreshed.context_digest
        || applied.routing_jwt != refreshed.routing_jwt;
    let rotates = applied.rotates_to(&refreshed);
    // A planned rotation means the attachment was healthy and Trellis itself is
    // replacing it solely to install new credentials. When the transport is
    // already down, this is ordinary recovery: the refreshed credential is
    // still applied and a reconnect forced, but the reconnect must stay a real
    // logical transition so the connection returns to connected and Live
    // resumes rather than being captured as maintenance.
    let planned = rotates && planned;
    if planned && !rotation.begin() {
        return Err(TrellisClientError::Bootstrap(
            "authorization transport rotation is already active".into(),
        ));
    }
    let result = apply_native_runtime_refresh(
        nats,
        &applied.runtime,
        &refreshed.runtime,
        credentials_changed,
        timeout_ms,
    )
    .await;
    if planned && !rotation.is_active() {
        // A broker rejection already cancelled the planned rotation and took
        // the fail-closed authorization path.
        return applied.record(refreshed, result);
    }
    if planned {
        let reconnected = match &result {
            Ok(()) => {
                rotation
                    .wait_reconnected(Duration::from_millis(timeout_ms))
                    .await
            }
            Err(_) => false,
        };
        if !reconnected {
            // The maintenance attempt did not complete: fall back to ordinary
            // transport-loss semantics rather than leaving a logically
            // connected attachment that is not actually usable.
            if rotation.escalate() {
                contexts.suspend();
                contexts.request_coverage_reconciliation();
                if let Some(live) = live {
                    live.suspend();
                }
                return Err(TrellisClientError::Timeout);
            }
            return applied.record(refreshed, result);
        }
    }
    applied.record(refreshed, result)
}

/// Connection options for a user/session-key principal.
pub struct UserConnectOptions<'a> {
    trellis_url: &'a str,
    timeout_ms: u64,
    credentials: UserSessionCredentials<'a>,
    participant_id: &'a str,
    name: Option<&'a str>,
}

/// Secret session credential for a user connection.
pub struct UserSessionCredentials<'a> {
    /// Durable user-only login issued by the proof-bound final bind.
    pub login_session_id: &'a str,
    /// Base64url-encoded Ed25519 session key seed.
    pub session_key_seed_base64url: &'a str,
}

impl<'a> UserConnectOptions<'a> {
    /// Create user-authenticated connection options.
    pub fn new(
        trellis_url: &'a str,
        timeout_ms: u64,
        credentials: UserSessionCredentials<'a>,
        participant_id: &'a str,
    ) -> Self {
        Self {
            trellis_url,
            timeout_ms,
            credentials,
            participant_id,
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
    authorization_provider: AuthorizationProviderCache,
    authorization_provider_stop: tokio::sync::watch::Sender<()>,
    authorization_provider_task: JoinHandle<()>,
    auth: Arc<SessionAuth>,
    timeout_ms: u64,
    service_bootstrap_binding: Option<CoreBootstrapBinding>,
    health_heartbeat_task: Option<JoinHandle<()>>,
    authorization_contexts: Option<Arc<AuthorizationContextCache>>,
    applied_native_authorization: Arc<tokio::sync::Mutex<AppliedNativeAuthorization>>,
    /// Classification of a planned physical authorization-credential rotation.
    authorization_transport_rotation: Arc<AuthorizationTransportRotation>,
    authorization_context_refresh_task: Option<JoinHandle<()>>,
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

    pub(crate) fn bound_api_subject(
        &self,
        family: &str,
        api_id: &str,
        action: &str,
    ) -> Result<String, TrellisClientError> {
        let deployment_id = self
            .authorization_contexts
            .as_ref()
            .ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context unavailable".into())
            })?
            .provider_deployment_id(api_id)?;
        let subject = match family {
            "rpc" => trellis_protocol::derive_bound_rpc_subject(api_id, &deployment_id, action),
            "operation" => {
                trellis_protocol::derive_bound_operation_subject(api_id, &deployment_id, action)
            }
            "live" => trellis_protocol::derive_bound_live_subject(api_id, &deployment_id, action),
            _ => unreachable!("only request route families are deployment-bound"),
        };
        subject.map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }

    /// Derive a bound request subject from a generated action key. The generated
    /// key carries the API short-name prefix that bound routes do not repeat, so
    /// it is normalized exactly like `Router::descriptor_subject` does.
    pub(crate) fn bound_key_subject(
        &self,
        family: &str,
        api_id: &str,
        key: &str,
    ) -> Result<String, TrellisClientError> {
        self.bound_api_subject(family, api_id, descriptor_action(key))
    }

    pub(crate) fn nats(&self) -> async_nats::Client {
        self.nats.clone()
    }

    pub(crate) fn inbox_prefix(&self) -> &str {
        &self.inbox_prefix
    }

    /// Return the cloneable session signer for owned live controls.
    pub(crate) fn auth_handle(&self) -> std::sync::Arc<crate::client::SessionAuth> {
        self.auth.clone()
    }

    /// Return the cloneable authorization-context owner for live controls.
    pub(crate) fn authorization_contexts_handle(
        &self,
    ) -> Result<std::sync::Arc<crate::client::AuthorizationContextCache>, TrellisClientError> {
        self.authorization_contexts.clone().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "authorization context cache unavailable".to_owned(),
            )
        })
    }

    /// Return the live-observation manager for this connection owner.
    pub(crate) fn live_manager(
        &self,
    ) -> Option<&std::sync::Arc<crate::live::manager::LiveSessionManager>> {
        self.live.as_ref()
    }

    /// Return the retained provider cache that owns local verification state.
    pub(crate) fn authorization_provider(&self) -> &AuthorizationProviderCache {
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
            .bundle()?;
        let context = trellis_protocol::parse_authorization_context(&bundle.context)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        context.unsigned.deployment_id.ok_or_else(|| {
            TrellisClientError::Bootstrap("installed context carries no deployment identity".into())
        })
    }

    pub(crate) fn participant_id(&self) -> Result<String, TrellisClientError> {
        self.authorization_contexts
            .as_ref()
            .ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context unavailable".into())
            })?
            .runtime_binding()
            .map(|binding| binding.participant_id)
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
        Self::connect_native(NativeConnectOptions {
            trellis_url: opts.trellis_url,
            participant_id: opts.participant_id,
            participant_path: opts.participant_path,
            package_evidence: opts.package_evidence,
            identity_seed: opts.provisioned_identity_seed_base64url,
            companion_installation_seed: None,
            companion_descriptor: None,
            name: opts.name,
            timeout_ms: opts.timeout_ms,
            kind: trellis_protocol::AuthorizationPrincipalKind::Service,
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
            trellis_url,
            participant_id,
            participant_path,
            package_evidence,
            identity_seed,
            companion_installation_seed,
            companion_descriptor,
            name,
            timeout_ms,
            kind,
        } = opts;
        let identity = Arc::new(SessionAuth::from_seed_base64url(identity_seed)?);
        let (session_seed, _) = crate::auth::generate_session_keypair();
        let auth = SessionAuth::from_seed_base64url(&session_seed)?;
        let companion_credential = match (companion_descriptor, companion_installation_seed) {
            (Some(descriptor), Some(seed)) => {
                Some(super::authorization::NativeCompanionCredential {
                    participant_id: descriptor.id,
                    installation: Arc::new(SessionAuth::from_seed_base64url(seed)?),
                })
            }
            (Some(_), None) => {
                return Err(TrellisClientError::Bootstrap(
                    "device companion installation seed is required".into(),
                ))
            }
            (None, Some(_)) => {
                return Err(TrellisClientError::Bootstrap(
                    "device descriptor has no companion".into(),
                ))
            }
            (None, None) => None,
        };
        let contexts = Arc::new(AuthorizationContextCache::new(
            trellis_url,
            participant_id.to_owned(),
            ulid::Ulid::new().to_string(),
            auth.session_key.clone(),
            super::authorization::AuthorizationCredential::Native {
                kind,
                identity,
                package_evidence,
                participant_path,
                companion: companion_credential,
            },
            name.map(str::to_owned),
        )?);
        let mut retry_delay = Duration::from_millis(100);
        // The pending window is bounded by the connect budget: a service whose
        // resources never materialize fails as retryable-unavailable instead of
        // retrying forever or hiding behind a credential error.
        tokio::time::timeout(Duration::from_millis(timeout_ms), async {
            loop {
                match contexts.refresh(&auth).await {
                    Err(TrellisClientError::BootstrapHttp { code, .. })
                        if is_retriable_authorization_code(&code) =>
                    {
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                    }
                    result => {
                        result?;
                        break;
                    }
                }
            }
            Ok::<(), TrellisClientError>(())
        })
        .await
        .map_err(|_| {
            TrellisClientError::AuthorizationUnavailable(
                "native bootstrap resource materialization exceeded the connect budget".to_owned(),
            )
        })??;
        let authorization: ServiceBootstrapAuthorization =
            serde_json::from_value(contexts.state_snapshot()?.authorization.ok_or_else(|| {
                TrellisClientError::Bootstrap("native bootstrap omitted resource evidence".into())
            })?)?;
        if authorization.participant_id != participant_id {
            return Err(TrellisClientError::Bootstrap(
                "native resource evidence has the wrong participant".into(),
            ));
        }
        let context = trellis_protocol::parse_authorization_context(&contexts.bundle()?.context)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let heartbeat = HealthHeartbeatConfig {
            session_key: auth.session_key.clone(),
            service_name: name.unwrap_or(participant_id).to_owned(),
            kind: match kind {
                trellis_protocol::AuthorizationPrincipalKind::Service => {
                    HealthHeartbeatServiceKind::Service
                }
                trellis_protocol::AuthorizationPrincipalKind::Device => {
                    HealthHeartbeatServiceKind::Device
                }
                trellis_protocol::AuthorizationPrincipalKind::User => {
                    return Err(TrellisClientError::Bootstrap(
                        "native connection requires a service or device credential".into(),
                    ))
                }
            },
            deployment_id: context.unsigned.deployment_id.ok_or_else(|| {
                TrellisClientError::Bootstrap(
                    "native bootstrap omitted deployment assignment".into(),
                )
            })?,
            instance_id: context.unsigned.instance_id.ok_or_else(|| {
                TrellisClientError::Bootstrap("native bootstrap omitted instance assignment".into())
            })?,
            contract_id: participant_id.to_owned(),
            contract_digest: authorization.participant_digest.clone(),
            started_at: now_rfc3339(),
            publish_interval_ms: HEALTH_HEARTBEAT_INTERVAL_MS,
        };
        let mut connected =
            Self::connect_context(auth, contexts, timeout_ms, participant_kind_label(&kind))
                .await?;
        connected.service_bootstrap_binding = Some(CoreBootstrapBinding::new(
            BootstrapBinding {
                contract_id: authorization.participant_id,
                digest: authorization.participant_digest,
            },
            authorization.resource_runtime,
        ));
        if let Some(companion) = authorization.companion {
            let seed = companion_installation_seed.ok_or_else(|| {
                TrellisClientError::Bootstrap(
                    "device bootstrap returned a companion without its installation seed".into(),
                )
            })?;
            match Self::connect_user(UserConnectOptions::new(
                trellis_url,
                timeout_ms,
                UserSessionCredentials {
                    login_session_id: &companion.login_session_id,
                    session_key_seed_base64url: seed,
                },
                &companion.participant_id,
            ))
            .await
            {
                Ok(child) => connected.companion = Some(Arc::new(child)),
                Err(error) if !companion.required => {
                    tracing::warn!(%error, "optional device companion could not connect");
                }
                Err(error) => return Err(error),
            }
        }
        if let Err(error) = publish_health_heartbeat(&connected.nats, timeout_ms, &heartbeat).await
        {
            tracing::warn!(%error, "failed to publish initial health heartbeat");
        }
        connected.health_heartbeat_task = Some(spawn_health_heartbeat_task(
            connected.nats.clone(),
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
            trellis_url: opts.trellis_url,
            participant_id: opts.participant_id,
            participant_path: C::PATH,
            package_evidence: C::package_evidence(),
            identity_seed: opts.identity_seed_base64url,
            companion_installation_seed: opts.companion_installation_seed_base64url,
            companion_descriptor: C::COMPANION,
            name: opts.name,
            timeout_ms: opts.timeout_ms,
            kind: trellis_protocol::AuthorizationPrincipalKind::Device,
        })
        .await
    }

    /// Issue fresh connection authority from a durable user login before connecting.
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

    async fn connect_user_inner(opts: UserConnectOptions<'_>) -> Result<Self, TrellisClientError> {
        let installation = Arc::new(SessionAuth::from_seed_base64url(
            opts.credentials.session_key_seed_base64url,
        )?);
        let auth = connection_runtime_auth()?;
        let authorization_contexts = AuthorizationContextCache::new(
            opts.trellis_url,
            opts.participant_id.to_owned(),
            ulid::Ulid::new().to_string(),
            auth.session_key.clone(),
            super::authorization::AuthorizationCredential::User {
                login_session_id: opts.credentials.login_session_id.to_owned(),
                installation,
            },
            opts.name.map(str::to_owned),
        )?;
        let authorization_contexts = Arc::new(authorization_contexts);
        authorization_contexts.refresh(&auth).await?;
        Self::connect_context(auth, authorization_contexts, opts.timeout_ms, "user").await
    }

    async fn connect_context(
        auth: SessionAuth,
        authorization_contexts: Arc<AuthorizationContextCache>,
        timeout_ms: u64,
        kind: &'static str,
    ) -> Result<Self, TrellisClientError> {
        let inbox_prefix = authorization_contexts.runtime_binding()?.inbox_prefix;
        let applied_native_authorization =
            AppliedNativeAuthorization::from_cache(&authorization_contexts)?;
        let auth = Arc::new(auth);
        let live_slot = std::sync::Arc::new(std::sync::Mutex::new(
            None::<std::sync::Weak<crate::live::manager::LiveSessionManager>>,
        ));
        let authorization_transport_rotation = Arc::new(AuthorizationTransportRotation::new());
        let nats = connect_authorized_nats(
            auth.clone(),
            authorization_contexts.clone(),
            timeout_ms,
            false,
            live_slot.clone(),
            authorization_transport_rotation.clone(),
        )
        .await?;

        let live = crate::live::manager::LiveSessionManager::new(
            nats.clone(),
            auth.clone(),
            authorization_contexts.clone(),
            authorization_contexts.runtime_binding()?.connection_id,
        );
        if let Ok(mut slot) = live_slot.lock() {
            *slot = Some(std::sync::Arc::downgrade(&live));
        }
        let provider =
            attach_authorization_provider(nats.clone(), authorization_contexts.clone()).await?;
        let applied_native_authorization =
            Arc::new(tokio::sync::Mutex::new(applied_native_authorization));
        let authorization_context_refresh_task = Some(spawn_authorization_context_refresh_task(
            authorization_contexts.clone(),
            auth.clone(),
            nats.clone(),
            applied_native_authorization.clone(),
            provider.provider.clone(),
            timeout_ms,
            authorization_transport_rotation.clone(),
            live.clone(),
        ));
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
            applied_native_authorization,
            authorization_transport_rotation,
            authorization_context_refresh_task,
            companion: None,
            live: Some(live),
        })
    }

    /// Return the underlying NATS client for runtime-owned serving.
    ///
    /// Exposed to the runtime crate so a built-in subsystem can serve its
    /// public router over the exact authenticated provider connection it
    /// bootstrapped with.
    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    #[must_use]
    pub fn runtime_nats(&self) -> async_nats::Client {
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

    /// Return the in-process provider cache used by local verification.
    pub(crate) fn authorization_context_cache(
        &self,
    ) -> Result<AuthorizationProviderCache, TrellisClientError> {
        Ok(self.authorization_provider.clone())
    }

    /// Refresh and verify the current authorization context immediately.
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
        let mut applied = self.applied_native_authorization.lock().await;
        crate::client::authorization::install_prepared_authorization(
            contexts,
            &self.auth,
            &self.nats,
            &mut applied,
            &self.authorization_provider,
            &self.authorization_transport_rotation,
            self.live.as_deref(),
            self.timeout_ms,
        )
        .await?;
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

    /// One request transport attempt with a caller span and catalog metrics.
    ///
    /// `route` is the bounded registered token supplied by the typed caller;
    /// raw callers pass `_unknown`. The caller span covers the whole logical
    /// call from the typed facade, and each transport attempt records one
    /// attempt counter sample.
    pub(crate) async fn request_routed(
        &self,
        subject: &str,
        payload: Bytes,
        route: &'static str,
    ) -> Result<async_nats::Message, TrellisClientError> {
        use crate::telemetry::instruments::{CounterFamily, DurationFamily};
        use crate::telemetry::lifecycle::Observation;
        use crate::telemetry::propagation;
        use tracing::Instrument as _;

        let attributes = vec![crate::telemetry::KeyValue::new("trellis.route", route)];
        let observation = Observation::start(DurationFamily::RpcClient, attributes, "cancelled");
        let span = tracing::info_span!(
            "trellis.rpc.client",
            "trellis.route" = route,
            "otel.kind" = "client",
        );
        let result = async {
            // Create the exact reply inbox before signing so the proof binds the
            // reply subject the response arrives on.
            let nats = self.nats();
            let reply = nats.new_inbox();
            let mut headers = self.signed_headers(subject, &reply, &payload)?;
            // Untrusted diagnostic metadata only; never part of the proof.
            let mut trace_pairs = Vec::new();
            propagation::inject_context(&opentelemetry::Context::current(), &mut trace_pairs);
            for (key, value) in trace_pairs {
                match key.as_str() {
                    "traceparent" => {
                        headers.insert("traceparent", value.as_str());
                    }
                    "tracestate" => {
                        headers.insert("tracestate", value.as_str());
                    }
                    _ => {}
                }
            }
            let request = async_nats::Request::new()
                .inbox(reply)
                .headers(headers)
                .payload(payload);

            let future = nats.send_request(subject.to_string(), request);
            let message = timeout(std::time::Duration::from_millis(self.timeout_ms), future)
                .await
                .map_err(|_| TrellisClientError::Timeout)?
                .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
            Ok(message)
        }
        .instrument(span.clone())
        .await;

        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => Self::client_outcome(error),
        };
        span.record("trellis.outcome", outcome);
        observation.finish(outcome);
        crate::telemetry::instruments::add_counter(
            CounterFamily::RpcClientAttempts,
            1,
            &[
                crate::telemetry::KeyValue::new("trellis.route", route),
                crate::telemetry::KeyValue::new("trellis.outcome", outcome),
            ],
        );
        result
    }

    /// Outcome classification for one client transport result.
    fn client_outcome(error: &TrellisClientError) -> &'static str {
        match error {
            TrellisClientError::Timeout => "timeout",
            TrellisClientError::Nats(_)
            | TrellisClientError::NatsConnect(_)
            | TrellisClientError::NatsRequest(_)
            | TrellisClientError::BootstrapHttp { .. }
            | TrellisClientError::AuthorizationUnavailable(_) => "unavailable",
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

    pub(crate) fn signed_headers(
        &self,
        subject: &str,
        reply: &str,
        payload: &[u8],
    ) -> Result<HeaderMap, TrellisClientError> {
        let context_digest = self.authorization_context_digest()?;
        signed_headers(&self.auth, &context_digest, subject, reply, payload)
    }

    pub(crate) fn authorization_context_digest(&self) -> Result<String, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        contexts.context_digest()
    }

    pub(crate) fn own_deployment_id(&self) -> Result<String, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        let context = trellis_protocol::parse_authorization_context(&contexts.bundle()?.context)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        context.unsigned.deployment_id.ok_or_else(|| {
            TrellisClientError::Bootstrap(
                "authorization context omitted deployment assignment".into(),
            )
        })
    }

    pub(crate) fn own_connection_id(&self) -> Result<String, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        let context = trellis_protocol::parse_authorization_context(&contexts.bundle()?.context)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        Ok(context.unsigned.connection_id)
    }

    pub(crate) fn own_instance_id(&self) -> Result<String, TrellisClientError> {
        let contexts = self.authorization_contexts.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context unavailable".into())
        })?;
        let context = trellis_protocol::parse_authorization_context(&contexts.bundle()?.context)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        context.unsigned.instance_id.ok_or_else(|| {
            TrellisClientError::Bootstrap(
                "authorization context omitted instance assignment".into(),
            )
        })
    }

    /// One JSON request with a bounded registered route token.
    async fn request_json_routed(
        &self,
        subject: &str,
        body: Value,
        route: &'static str,
    ) -> Result<Value, TrellisClientError> {
        let payload = Bytes::from(serde_json::to_vec(&body)?);
        let message = self.request_routed(subject, payload, route).await?;

        decode_json_message(message)
    }

    /// Call a raw subject with a JSON value payload.
    pub async fn request_json_value(
        &self,
        subject: &str,
        body: &Value,
    ) -> Result<Value, TrellisClientError> {
        self.request_json_routed(
            subject,
            body.clone(),
            crate::telemetry::instruments::unknown_route(),
        )
        .await
    }

    /// Call one descriptor-backed subject with its bounded route token.
    pub(crate) async fn request_json_value_routed(
        &self,
        subject: &str,
        body: &Value,
        route: &'static str,
    ) -> Result<Value, TrellisClientError> {
        self.request_json_routed(subject, body.clone(), route).await
    }

    /// Publish one descriptor-backed event.
    pub async fn publish<D>(&self, event: &D::Event) -> Result<(), TrellisClientError>
    where
        D: EventDescriptor,
    {
        let prepared = prepare_event::<D>(event)?.with_subject(self.descriptor_subject(D::SUBJECT));
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
        let result = async {
            let context_digest = self.authorization_context_digest()?;
            publish_prepared_event(
                &self.nats(),
                &self.auth,
                &context_digest,
                self.timeout_ms,
                &event,
            )
            .await
        }
        .await;
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
        let subscriber = timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            self.nats()
                .subscribe(self.descriptor_subject(D::SUBSCRIBE_SUBJECT)),
        )
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;

        let stream = stream::try_unfold(subscriber, |mut subscriber| async move {
            match subscriber.next().await {
                Some(message) => {
                    let value: Value = serde_json::from_slice(&message.payload)?;
                    let event = D::Event::decode(value)
                        .map_err(|error| TrellisClientError::Codec(error.to_string()))?;
                    Ok(Some((event, subscriber)))
                }
                None => Ok(None),
            }
        });

        Ok(Box::pin(stream) as BoxStream<'static, Result<D::Event, TrellisClientError>>)
    }

    /// Subscribe to descriptor-backed event messages with explicit ack/nak/term control.
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
        self.event_messages(
            options,
            Some(self.descriptor_subject(D::SUBSCRIBE_SUBJECT)),
            None,
        )
        .await
    }

    pub(crate) async fn event_messages<T: Send + 'static>(
        &self,
        options: EventSubscribeOptions,
        filter_subject: Option<String>,
        max_messages: Option<usize>,
    ) -> Result<BoxStream<'static, Result<EventMessage<T>, TrellisClientError>>, TrellisClientError>
    {
        let jetstream = jetstream::new(self.nats());
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

        let mut request = consumer.stream().expires(std::time::Duration::from_secs(1));
        if let Some(max_messages) = max_messages {
            request = request.max_messages_per_batch(max_messages);
        }
        let messages = timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            request.messages(),
        )
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;

        let stream = stream::try_unfold(messages, |mut messages| async move {
            match messages.next().await {
                Some(Ok(message)) => {
                    let event_message = EventMessage {
                        message,
                        _event: PhantomData,
                    };
                    Ok(Some((event_message, messages)))
                }
                Some(Err(error)) => Err(TrellisClientError::NatsRequest(error.to_string())),
                None => Ok(None),
            }
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
        let base_subject = self.bound_key_subject("live", D::API_ID, D::KEY)?;
        let open_id = trellis_protocol::generate_nonce()
            .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
        let body = serde_json::json!({
            "format": trellis_protocol::LIVE_VERSION,
            "type": "open",
            "openId": open_id,
            "receiveMaxPayloadBytes": self.nats.max_payload() as u64,
            "input": encoded,
        });
        let action_name = D::KEY.split_once('.').map_or(D::KEY, |(_, action)| action);
        let permission = trellis_protocol::PermissionAtom::new(
            trellis_protocol::PermissionTarget::api_surface(
                D::API_ID,
                trellis_protocol::ApiSurfaceKind::Live,
                action_name.to_owned(),
            )
            .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?,
            trellis_protocol::PermissionAction::Subscribe,
        )
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
        let open = crate::live::client_open::ClientOpen {
            kind: trellis_protocol::LiveSessionKind::Standalone,
            api_id: D::API_ID,
            base_subject: &base_subject,
            publish_subject: &base_subject,
            body: Bytes::from(serde_json::to_vec(&body)?),
            open_id,
            receive_max_payload_bytes: self.nats.max_payload() as u64,
            permission,
        };
        let prepared =
            crate::live::client_open::open_client_session(self, &self.authorization_provider, open)
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
        let observation = Observation::start(
            DurationFamily::Transfer,
            vec![KeyValue::new("trellis.direction", "download")],
            "cancelled",
        );
        let result = crate::client::transfer::get_download_grant_into_with_cancel(
            self,
            grant,
            writer,
            Some(cancellation),
        )
        .await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(TrellisClientError::TransferCancelled) => "cancelled",
            Err(error) => Self::client_outcome(error),
        };
        observation.finish(outcome);
        if let Ok(info) = &result {
            crate::telemetry::instruments::add_counter(
                CounterFamily::TransferWireBytes,
                info.size,
                &[KeyValue::new("trellis.direction", "download")],
            );
        }
        result
    }
}

impl Drop for TrellisClient {
    fn drop(&mut self) {
        if let Some(task) = self.health_heartbeat_task.take() {
            task.abort();
        }
        if let Some(task) = self.authorization_context_refresh_task.take() {
            task.abort();
        }
        if let Some(live) = self.live.as_ref() {
            live.stop();
        }
        let _ = self.authorization_provider_stop.send(());
        self.authorization_provider_task.abort();
    }
}

impl OperationTransport for TrellisClient {
    fn operation_subject(
        &self,
        api_id: &str,
        operation: &str,
        subject: &str,
    ) -> Result<String, TrellisClientError> {
        if api_id.is_empty() {
            return Ok(subject.to_owned());
        }
        self.bound_key_subject("operation", api_id, operation)
    }

    async fn request_json_value(
        &self,
        subject: String,
        body: Value,
    ) -> Result<Value, TrellisClientError> {
        TrellisClient::request_json_value(self, &subject, &body).await
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
        expected_size: Option<u64>,
    ) -> Result<FileInfo, TrellisClientError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + ?Sized + 'a,
    {
        crate::client::transfer::put_upload_grant_from(self, &grant, reader, expected_size).await
    }

    async fn put_upload_transfer_from_with_cancel<'a, R>(
        &'a self,
        grant: UploadTransferGrant,
        reader: &'a mut R,
        expected_size: Option<u64>,
        cancellation: &'a crate::client::TransferCancellation,
    ) -> Result<FileInfo, TrellisClientError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + ?Sized + 'a,
    {
        crate::client::transfer::put_upload_grant_from_with_cancel(
            self,
            &grant,
            reader,
            expected_size,
            Some(cancellation),
        )
        .await
    }
}

fn decode_json_message(message: async_nats::Message) -> Result<Value, TrellisClientError> {
    if let Some(headers) = &message.headers {
        if headers
            .get("status")
            .is_some_and(|status| status.as_str() == "error")
        {
            return Err(TrellisClientError::RpcError(
                RpcErrorPayload::from_json_slice(&message.payload)?,
            ));
        }
    }

    Ok(serde_json::from_slice(&message.payload)?)
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

fn descriptor_action(action: &str) -> &str {
    action.split_once('.').map_or(action, |(_, action)| action)
}

fn connection_runtime_auth() -> Result<SessionAuth, TrellisClientError> {
    let (context_seed, _) = crate::auth::generate_session_keypair();
    SessionAuth::from_seed_base64url(&context_seed)
}

#[cfg(test)]
mod tests {
    use super::AppliedNativeAuthorization;
    use crate::client::{
        AuthorizationNativeTransport, AuthorizationRuntimeBinding, AuthorizationRuntimeTransports,
        TrellisClientError,
    };

    #[test]
    fn each_user_connection_gets_an_independent_runtime_key() {
        let first = super::connection_runtime_auth().expect("runtime auth");
        let second = super::connection_runtime_auth().expect("runtime auth");
        assert_ne!(first.session_key, second.session_key);
    }

    #[test]
    fn event_subscribe_options_require_an_explicit_delivery_mode() {
        let durable = super::EventSubscribeOptions::durable("orders");
        assert_eq!(durable.mode, super::EventSubscriptionMode::Durable);
        assert_eq!(durable.durable_name.as_deref(), Some("orders"));
        let ephemeral = super::EventSubscribeOptions::ephemeral();
        assert_eq!(ephemeral.mode, super::EventSubscriptionMode::Ephemeral);
        assert!(ephemeral.durable_name.is_none());
        assert_eq!(
            durable.with_replay(super::EventReplayPolicy::All).replay,
            super::EventReplayPolicy::All,
        );
    }

    #[test]
    fn generated_action_keys_normalize_like_bound_routes() {
        assert_eq!(super::descriptor_action("auth.Sessions.Me"), "Sessions.Me");
        assert_eq!(
            super::descriptor_action("core.Resources.Destroy"),
            "Resources.Destroy"
        );
        assert_eq!(super::descriptor_action("Ping"), "Ping");
        // Internal SDK descriptors carry the same generated-key form as the
        // routes they call, including for actions that already contain a dot.
        assert_eq!(
            super::descriptor_action("events.Consumers.ReportDelivery"),
            "Consumers.ReportDelivery"
        );
        assert_eq!(
            super::descriptor_action("events.DeadLetters.Inspect"),
            "DeadLetters.Inspect"
        );
    }

    fn authorization(
        server: &str,
        context_digest: &str,
        routing_jwt: &str,
    ) -> AppliedNativeAuthorization {
        AppliedNativeAuthorization {
            runtime: AuthorizationRuntimeBinding {
                connection_id: "connection".to_owned(),
                login_session_id: None,
                participant_id: "participant".to_owned(),
                inbox_prefix: "_INBOX.connection".to_owned(),
                transports: AuthorizationRuntimeTransports {
                    native: Some(AuthorizationNativeTransport {
                        nats_servers: vec![server.to_owned()],
                    }),
                    websocket: None,
                },
            },
            context_digest: context_digest.to_owned(),
            routing_jwt: routing_jwt.to_owned(),
        }
    }

    #[test]
    fn failed_native_transport_refresh_remains_pending() {
        let mut applied = authorization("nats://old", "old-context", "old-jwt");
        let refreshed = authorization("nats://new", "new-context", "new-jwt");

        assert!(matches!(
            applied.record(refreshed.clone(), Err(TrellisClientError::Timeout)),
            Err(TrellisClientError::Timeout)
        ));
        assert_ne!(applied, refreshed);

        assert!(applied.record(refreshed.clone(), Ok(())).is_ok());
        assert_eq!(applied, refreshed);
    }
}
