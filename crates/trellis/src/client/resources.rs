use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc};

use bytes::Bytes;
use futures_util::{stream::BoxStream, StreamExt};
use time::OffsetDateTime;

use crate::generated::Codec;

const STATE_API_ID: &str = "trellis.state@v1";

const RESOURCE_ENVELOPE_MAGIC: &[u8; 4] = b"TRKV";
const RESOURCE_ENVELOPE_FORMAT: u8 = 1;

type MigrationFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;
type Migration<T> = Arc<dyn Fn(serde_json::Value) -> MigrationFuture<T> + Send + Sync>;

/// Opaque storage revision returned by State and KV resources.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceRevision(u64);

impl ResourceRevision {
    #[doc(hidden)]
    pub const fn from_backend(value: u64) -> Self {
        Self(value)
    }

    #[doc(hidden)]
    pub const fn into_backend(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ResourceRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ResourceRevision")
            .field(&self.0)
            .finish()
    }
}

/// Failure while encoding, decoding, or migrating a typed resource value.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResourceCodecError {
    /// The generated current value could not be encoded.
    #[error("resource value encode failed: {message}")]
    Encode { message: String },
    /// The generated codec was configured with invalid metadata.
    #[error("invalid resource codec configuration: {0}")]
    InvalidConfiguration(String),
    /// Stored bytes are not the current Trellis resource envelope.
    #[error("invalid Trellis resource envelope: {message}")]
    InvalidEnvelope { message: String, raw: Bytes },
    /// The stored representation is neither current nor registered as accepted.
    #[error("unsupported resource representation version {version}")]
    UnsupportedVersion { version: u32, raw: Bytes },
    /// A generated codec rejected the stored or current value.
    #[error("resource representation version {version} failed validation: {message}")]
    Decode {
        version: u32,
        message: String,
        raw: Bytes,
    },
    /// An application migration failed.
    #[error("resource representation version {version} migration failed: {message}")]
    Migration {
        version: u32,
        message: String,
        raw: Bytes,
    },
}

/// Current generated codec and direct historical-to-current migrations for one resource.
pub struct ResourceCodec<T> {
    current_version: u32,
    migrations: BTreeMap<u32, Migration<T>>,
}

impl<T> Clone for ResourceCodec<T> {
    fn clone(&self) -> Self {
        Self {
            current_version: self.current_version,
            migrations: self.migrations.clone(),
        }
    }
}

impl<T> fmt::Debug for ResourceCodec<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceCodec")
            .field("current_version", &self.current_version)
            .field("migration_versions", &self.migrations.keys())
            .finish()
    }
}

impl<T> ResourceCodec<T>
where
    T: Codec + Send + 'static,
{
    /// Create a codec for a positive current representation version.
    #[doc(hidden)]
    pub fn new(current_version: u32) -> Result<Self, ResourceCodecError> {
        if current_version == 0 {
            return Err(ResourceCodecError::InvalidConfiguration(
                "current representation version must be positive".into(),
            ));
        }
        Ok(Self {
            current_version,
            migrations: BTreeMap::new(),
        })
    }

    /// Register one direct, fallible historical-to-current migration.
    #[doc(hidden)]
    pub fn with_migration<F, Fut, E>(mut self, version: u32, migration: F) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        E: fmt::Display,
    {
        assert!(
            version > 0 && version != self.current_version,
            "migration version must be positive and historical"
        );
        self.migrations.insert(
            version,
            Arc::new(move |value| {
                let future = migration(value);
                Box::pin(async move { future.await.map_err(|error| error.to_string()) })
            }),
        );
        self
    }

    /// Encode a current typed value in the new-version `TRKV` envelope.
    pub fn encode(&self, value: &T) -> Result<Bytes, ResourceCodecError> {
        let payload = self.encode_payload(value)?;
        let mut encoded = Vec::with_capacity(9 + payload.len());
        encoded.extend_from_slice(RESOURCE_ENVELOPE_MAGIC);
        encoded.push(RESOURCE_ENVELOPE_FORMAT);
        encoded.extend_from_slice(&self.current_version.to_be_bytes());
        encoded.extend_from_slice(&payload);
        Ok(encoded.into())
    }

    /// Decode and, when needed, migrate one envelope without modifying storage.
    pub async fn decode(&self, bytes: &[u8]) -> Result<T, ResourceCodecError> {
        if bytes.len() < 9 || &bytes[..4] != RESOURCE_ENVELOPE_MAGIC {
            return Err(ResourceCodecError::InvalidEnvelope {
                message: "missing TRKV magic or version fields".into(),
                raw: Bytes::copy_from_slice(bytes),
            });
        }
        if bytes[4] != RESOURCE_ENVELOPE_FORMAT {
            return Err(ResourceCodecError::InvalidEnvelope {
                message: format!("unsupported envelope format {}", bytes[4]),
                raw: Bytes::copy_from_slice(bytes),
            });
        }
        let version = u32::from_be_bytes(bytes[5..9].try_into().expect("fixed envelope slice"));
        if version == 0 {
            return Err(ResourceCodecError::InvalidEnvelope {
                message: "representation version must be positive".into(),
                raw: Bytes::copy_from_slice(bytes),
            });
        }
        self.decode_payload_with_raw(version, &bytes[9..], Bytes::copy_from_slice(bytes))
            .await
    }

    fn encode_payload(&self, value: &T) -> Result<Bytes, ResourceCodecError> {
        let json = value.encode().map_err(|error| ResourceCodecError::Encode {
            message: error.to_string(),
        })?;
        serde_json::to_vec(&json)
            .map(Bytes::from)
            .map_err(|error| ResourceCodecError::Encode {
                message: error.to_string(),
            })
    }

    async fn decode_payload(&self, version: u32, bytes: &[u8]) -> Result<T, ResourceCodecError> {
        self.decode_payload_with_raw(version, bytes, Bytes::copy_from_slice(bytes))
            .await
    }

    async fn decode_payload_with_raw(
        &self,
        version: u32,
        bytes: &[u8],
        raw: Bytes,
    ) -> Result<T, ResourceCodecError> {
        if version == 0 {
            return Err(ResourceCodecError::InvalidEnvelope {
                message: "representation version must be positive".into(),
                raw,
            });
        }
        let json = serde_json::from_slice(bytes).map_err(|error| ResourceCodecError::Decode {
            version,
            message: error.to_string(),
            raw: raw.clone(),
        })?;
        if version == self.current_version {
            return T::decode(json).map_err(|error| ResourceCodecError::Decode {
                version,
                message: error.to_string(),
                raw,
            });
        }
        let migration = self.migrations.get(&version).ok_or_else(|| {
            ResourceCodecError::UnsupportedVersion {
                version,
                raw: raw.clone(),
            }
        })?;
        let current = migration(json)
            .await
            .map_err(|error| ResourceCodecError::Migration {
                version,
                message: error,
                raw: raw.clone(),
            })?;
        let validated = current
            .encode()
            .map_err(|error| ResourceCodecError::Migration {
                version,
                message: error.to_string(),
                raw: raw.clone(),
            })?;
        T::decode(validated).map_err(|error| ResourceCodecError::Migration {
            version,
            message: error.to_string(),
            raw,
        })
    }
}

/// Raw stored State value exchanged with the generated State transport.
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct RawStateValue {
    pub value: Bytes,
    pub representation_version: u32,
    pub revision: ResourceRevision,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// State write mode enforced by the authoritative State backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum StateWriteMode {
    Create,
    Set,
    Replace(ResourceRevision),
}

/// Raw backend result for a State write or checked delete.
#[derive(Debug, thiserror::Error)]
#[doc(hidden)]
pub enum RawStateWriteError {
    #[error("state revision conflict")]
    Conflict(Option<RawStateValue>),
    #[error("state backend error: {0}")]
    Backend(String),
}

/// Generated State transport used by typed State handles.
#[doc(hidden)]
pub trait StateResourceClient: Clone + fmt::Debug + Send + Sync + 'static {
    fn get(
        &self,
        resource_name: &str,
    ) -> impl Future<Output = Result<Option<RawStateValue>, String>> + Send;

    fn put(
        &self,
        resource_name: &str,
        value: Bytes,
        representation_version: u32,
        mode: StateWriteMode,
    ) -> impl Future<Output = Result<RawStateValue, RawStateWriteError>> + Send;

    fn delete(
        &self,
        resource_name: &str,
        revision: Option<ResourceRevision>,
    ) -> impl Future<Output = Result<(), RawStateWriteError>> + Send;
}

#[derive(Clone)]
#[doc(hidden)]
pub struct BoundStateResourceClient {
    client: Arc<super::TrellisClient>,
}

impl fmt::Debug for BoundStateResourceClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundStateResourceClient")
            .finish_non_exhaustive()
    }
}

impl BoundStateResourceClient {
    pub(crate) fn new(client: Arc<super::TrellisClient>) -> Self {
        Self { client }
    }

    async fn call(
        &self,
        action: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, super::TrellisClientError> {
        let subject = self.client.bound_api_subject("rpc", STATE_API_ID, action)?;
        self.client.request_json_value(&subject, &body).await
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireStoredState {
    #[serde(with = "crate::generated::serde_base64")]
    value: Vec<u8>,
    representation_version: u32,
    revision: String,
    created_at: String,
    updated_at: String,
}

impl TryFrom<WireStoredState> for RawStateValue {
    type Error = String;

    fn try_from(value: WireStoredState) -> Result<Self, Self::Error> {
        use time::format_description::well_known::Rfc3339;
        let revision = value
            .revision
            .parse()
            .map_err(|error| format!("invalid State revision: {error}"))?;
        Ok(Self {
            value: value.value.into(),
            representation_version: value.representation_version,
            revision: ResourceRevision::from_backend(revision),
            created_at: OffsetDateTime::parse(&value.created_at, &Rfc3339)
                .map_err(|error| error.to_string())?,
            updated_at: OffsetDateTime::parse(&value.updated_at, &Rfc3339)
                .map_err(|error| error.to_string())?,
        })
    }
}

fn wire_state(value: serde_json::Value) -> Result<RawStateValue, String> {
    serde_json::from_value::<WireStoredState>(value)
        .map_err(|error| error.to_string())?
        .try_into()
}

fn state_conflict(
    error: &super::TrellisClientError,
) -> Option<Result<Option<RawStateValue>, String>> {
    let super::TrellisClientError::RpcError(payload) = error else {
        return None;
    };
    if payload.error_type() != Some("trellis.state@v1::Conflict") {
        return None;
    }
    Some(
        match payload
            .value()
            .and_then(|value| value.get("current"))
            .cloned()
        {
            Some(serde_json::Value::Null) | None => Ok(None),
            Some(value) => wire_state(value).map(Some),
        },
    )
}

impl StateResourceClient for BoundStateResourceClient {
    async fn get(&self, resource_name: &str) -> Result<Option<RawStateValue>, String> {
        let response = self
            .call("Get", serde_json::json!({ "resourceName": resource_name }))
            .await
            .map_err(|error| error.to_string())?;
        response
            .get("entry")
            .cloned()
            .filter(|value| !value.is_null())
            .map(wire_state)
            .transpose()
    }

    async fn put(
        &self,
        resource_name: &str,
        value: Bytes,
        representation_version: u32,
        mode: StateWriteMode,
    ) -> Result<RawStateValue, RawStateWriteError> {
        use base64::Engine as _;
        let (mode, revision) = match mode {
            StateWriteMode::Create => ("create", None),
            StateWriteMode::Set => ("set", None),
            StateWriteMode::Replace(revision) => {
                ("replace", Some(revision.into_backend().to_string()))
            }
        };
        let body = serde_json::json!({
            "resourceName": resource_name,
            "value": base64::engine::general_purpose::STANDARD.encode(value),
            "representationVersion": representation_version,
            "mode": mode,
            "revision": revision,
        });
        match self.call("Put", body).await {
            Ok(response) => response
                .get("entry")
                .cloned()
                .ok_or_else(|| RawStateWriteError::Backend("State Put omitted entry".into()))
                .and_then(|value| wire_state(value).map_err(RawStateWriteError::Backend)),
            Err(error) => match state_conflict(&error) {
                Some(Ok(current)) => Err(RawStateWriteError::Conflict(current)),
                Some(Err(error)) => Err(RawStateWriteError::Backend(error)),
                None => Err(RawStateWriteError::Backend(error.to_string())),
            },
        }
    }

    async fn delete(
        &self,
        resource_name: &str,
        revision: Option<ResourceRevision>,
    ) -> Result<(), RawStateWriteError> {
        let body = serde_json::json!({ "resourceName": resource_name, "revision": revision.map(|value| value.into_backend().to_string()) });
        match self.call("Delete", body).await {
            Ok(_) => Ok(()),
            Err(error) => match state_conflict(&error) {
                Some(Ok(current)) => Err(RawStateWriteError::Conflict(current)),
                Some(Err(error)) => Err(RawStateWriteError::Backend(error)),
                None => Err(RawStateWriteError::Backend(error.to_string())),
            },
        }
    }
}

#[doc(hidden)]
pub type ConnectedStateHandle<T> = StateHandle<T, BoundStateResourceClient>;

/// Generated metadata for one declared durable event consumer.
pub trait ConsumerDescriptor: Send + Sync + 'static {
    /// Contract-local resource name.
    const NAME: &'static str;
    /// Exact API and event identities selected by this consumer.
    const EVENTS: &'static [(&'static str, &'static str)];
}

/// Connected durable consumer fenced by its installed resource generation.
#[derive(Clone)]
pub struct ConsumerHandle<D> {
    binding: crate::service::EventConsumerResourceBinding,
    nats: async_nats::Client,
    client: crate::generated::Client,
    availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    generation: u64,
    marker: std::marker::PhantomData<D>,
}

impl<D: ConsumerDescriptor> fmt::Debug for ConsumerHandle<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConsumerHandle")
            .field("resource_name", &D::NAME)
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

impl<D: ConsumerDescriptor> ConsumerHandle<D> {
    pub(crate) fn from_generated(
        binding: crate::service::EventConsumerResourceBinding,
        nats: async_nats::Client,
        client: crate::generated::Client,
        availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    ) -> Self {
        let generation = availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::Consumer, D::NAME)
            .unwrap_or(0);
        Self {
            binding,
            nats,
            client,
            availability,
            generation,
            marker: std::marker::PhantomData,
        }
    }

    /// Return the exact installed durable consumer binding.
    pub fn binding(&self) -> &crate::service::EventConsumerResourceBinding {
        &self.binding
    }

    /// Subscribe to one event selected by this declared durable consumer.
    pub async fn subscribe<E>(
        &self,
    ) -> Result<
        BoxStream<'static, Result<E::Event, super::TrellisClientError>>,
        super::TrellisClientError,
    >
    where
        E: crate::generated::EventDescriptor,
        E::Event: Send + 'static,
    {
        self.ensure_current().map_err(|error| {
            super::TrellisClientError::AuthorizationUnavailable(error.to_string())
        })?;
        let event_name = E::DESCRIPTOR_NAME
            .split_once('.')
            .map_or(E::DESCRIPTOR_NAME, |(_, name)| name);
        if !D::EVENTS.contains(&(E::API_ID, event_name)) {
            return Err(super::TrellisClientError::AuthorizationUnavailable(
                format!(
                    "consumer {} does not select {}.{}",
                    D::NAME,
                    E::API_ID,
                    event_name
                ),
            ));
        }
        self.client
            .subscribe::<E>(crate::client::EventSubscribeOptions {
                stream: Some(self.binding.stream.clone()),
                mode: crate::client::EventSubscriptionMode::Durable,
                replay: crate::client::EventReplayPolicy::New,
                durable_name: Some(self.binding.consumer_name.clone()),
            })
            .await
    }

    /// Open the pre-provisioned durable consumer and stream its messages.
    pub async fn messages(
        &self,
    ) -> Result<
        BoxStream<
            'static,
            Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
        >,
        crate::service::ServerError,
    > {
        self.ensure_current()?;
        let stream = async_nats::jetstream::new(self.nats.clone())
            .get_stream(&self.binding.stream)
            .await
            .map_err(|error| crate::service::ServerError::Nats(error.to_string()))?;
        let consumer = stream
            .get_consumer::<async_nats::jetstream::consumer::pull::Config>(
                &self.binding.consumer_name,
            )
            .await
            .map_err(|error| crate::service::ServerError::Nats(error.to_string()))?;
        consumer
            .messages()
            .await
            .map(|messages| {
                messages
                    .map(|message| {
                        message.map_err(|error| {
                            Box::new(error) as Box<dyn std::error::Error + Send + Sync>
                        })
                    })
                    .boxed()
            })
            .map_err(|error| crate::service::ServerError::Nats(error.to_string()))
    }

    fn ensure_current(&self) -> Result<(), crate::service::ServerError> {
        if self
            .availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::Consumer, D::NAME)
            == Some(self.generation)
        {
            Ok(())
        } else {
            Err(crate::service::ServerError::ResourceUnavailable {
                resource_kind: "consumer".into(),
                resource_name: D::NAME.into(),
            })
        }
    }
}

/// Typed current State value and its opaque storage revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateValue<T> {
    /// Decoded current in-memory representation.
    pub value: T,
    /// Revision to pass to `replace` or checked `delete`.
    pub revision: ResourceRevision,
}

/// Typed State read failure.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum StateReadError {
    /// This cached handle no longer names an available State resource.
    #[error("State resource binding is no longer available")]
    Unavailable,
    /// Stored bytes could not be decoded or migrated.
    #[error(transparent)]
    Codec(#[from] ResourceCodecError),
    /// The State transport failed.
    #[error("state backend error: {0}")]
    Backend(String),
}

/// Typed State write failure, including the authoritative current value on conflict.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum StateWriteError<T> {
    /// This cached handle no longer names an available State resource.
    #[error("State resource binding is no longer available")]
    Unavailable,
    /// The expected revision or create-if-absent condition did not hold.
    #[error("state revision conflict")]
    Conflict { current: Option<StateValue<T>> },
    /// Encoding, decoding, or migration failed.
    #[error(transparent)]
    Codec(#[from] ResourceCodecError),
    /// The State transport failed.
    #[error("state backend error: {0}")]
    Backend(String),
}

/// Typed handle for one generated single-value State resource.
#[derive(Debug)]
pub struct StateHandle<T, C> {
    resource_name: Arc<str>,
    codec: ResourceCodec<T>,
    client: C,
    availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    generation: u64,
}

impl<T, C: Clone> Clone for StateHandle<T, C> {
    fn clone(&self) -> Self {
        Self {
            resource_name: Arc::clone(&self.resource_name),
            codec: self.codec.clone(),
            client: self.client.clone(),
            availability: self.availability.clone(),
            generation: self.generation,
        }
    }
}

impl<T, C> StateHandle<T, C>
where
    T: Codec + Send + 'static,
    C: StateResourceClient,
{
    /// Construct a handle from generated resource metadata and transport.
    #[doc(hidden)]
    pub fn from_generated(
        resource_name: impl Into<Arc<str>>,
        codec: ResourceCodec<T>,
        client: C,
        availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    ) -> Self {
        let resource_name = resource_name.into();
        let generation = availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::State, &resource_name)
            .unwrap_or(0);
        Self {
            resource_name,
            codec,
            client,
            availability,
            generation,
        }
    }

    /// Read the current value, migrating only in memory when needed.
    pub async fn get(&self) -> Result<Option<StateValue<T>>, StateReadError> {
        self.ensure_current()?;
        match self
            .client
            .get(&self.resource_name)
            .await
            .map_err(StateReadError::Backend)?
        {
            Some(raw) => self.project(raw).await.map(Some).map_err(Into::into),
            None => Ok(None),
        }
    }

    /// Create the value only when no live value exists.
    pub async fn create(&self, value: &T) -> Result<StateValue<T>, StateWriteError<T>> {
        self.write(value, StateWriteMode::Create).await
    }

    /// Unconditionally replace the current value.
    pub async fn set(&self, value: &T) -> Result<StateValue<T>, StateWriteError<T>> {
        self.write(value, StateWriteMode::Set).await
    }

    /// Replace the value only when `revision` remains current.
    pub async fn replace(
        &self,
        revision: ResourceRevision,
        value: &T,
    ) -> Result<StateValue<T>, StateWriteError<T>> {
        self.write(value, StateWriteMode::Replace(revision)).await
    }

    /// Delete unconditionally or only at the supplied revision.
    pub async fn delete(
        &self,
        revision: Option<ResourceRevision>,
    ) -> Result<(), StateWriteError<T>> {
        self.ensure_current()
            .map_err(|_| StateWriteError::Unavailable)?;
        match self.client.delete(&self.resource_name, revision).await {
            Ok(()) => Ok(()),
            Err(RawStateWriteError::Conflict(current)) => Err(StateWriteError::Conflict {
                current: self.project_optional(current).await?,
            }),
            Err(RawStateWriteError::Backend(error)) => Err(StateWriteError::Backend(error)),
        }
    }

    async fn write(
        &self,
        value: &T,
        mode: StateWriteMode,
    ) -> Result<StateValue<T>, StateWriteError<T>> {
        self.ensure_current()
            .map_err(|_| StateWriteError::Unavailable)?;
        let encoded = self.codec.encode_payload(value)?;
        match self
            .client
            .put(
                &self.resource_name,
                encoded,
                self.codec.current_version,
                mode,
            )
            .await
        {
            Ok(raw) => self.project(raw).await.map_err(StateWriteError::Codec),
            Err(RawStateWriteError::Conflict(current)) => Err(StateWriteError::Conflict {
                current: self.project_optional(current).await?,
            }),
            Err(RawStateWriteError::Backend(error)) => Err(StateWriteError::Backend(error)),
        }
    }

    async fn project(&self, raw: RawStateValue) -> Result<StateValue<T>, ResourceCodecError> {
        Ok(StateValue {
            value: self
                .codec
                .decode_payload(raw.representation_version, &raw.value)
                .await?,
            revision: raw.revision,
        })
    }

    async fn project_optional(
        &self,
        raw: Option<RawStateValue>,
    ) -> Result<Option<StateValue<T>>, ResourceCodecError> {
        match raw {
            Some(raw) => self.project(raw).await.map(Some),
            None => Ok(None),
        }
    }

    fn ensure_current(&self) -> Result<(), StateReadError> {
        if self
            .availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::State, &self.resource_name)
            == Some(self.generation)
        {
            Ok(())
        } else {
            Err(StateReadError::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Current {
        value: u64,
    }

    #[tokio::test]
    async fn resource_codec_requires_envelope_and_migrates_without_reencoding_storage() {
        let codec = ResourceCodec::<Current>::new(2)
            .unwrap()
            .with_migration(1, |old| async move {
                let value = old
                    .get("old")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("missing old value")?;
                Ok::<_, &'static str>(Current { value })
            });
        let mut historical = b"TRKV\x01\x00\x00\x00\x01".to_vec();
        historical.extend_from_slice(br#"{"old":7}"#);
        let original = historical.clone();

        assert_eq!(
            codec.decode(&historical).await.unwrap(),
            Current { value: 7 }
        );
        assert_eq!(historical, original);
        assert!(matches!(
            codec.decode(br#"{"value":7}"#).await,
            Err(ResourceCodecError::InvalidEnvelope { raw, .. })
                if raw == Bytes::from_static(br#"{"value":7}"#)
        ));
        let unsupported = ResourceCodec::<Current>::new(2)
            .unwrap()
            .decode(&historical)
            .await
            .unwrap_err();
        assert!(matches!(
            unsupported,
            ResourceCodecError::UnsupportedVersion { version: 1, raw }
                if raw.as_ref() == original
        ));
        assert_eq!(
            &codec.encode(&Current { value: 8 }).unwrap()[..9],
            b"TRKV\x01\x00\x00\x00\x02"
        );
    }
}
