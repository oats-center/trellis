//! Runtime ABI used by Trellis-generated Rust crates.
//!
//! Application code should use generated owner SDK and participant facade APIs
//! rather than implementing these traits directly.

use std::sync::Arc;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Generated-code ABI version supported by this runtime.
pub const ABI_VERSION: u32 = 1;

/// Fail compilation when generated source targets a different runtime ABI.
pub const fn assert_abi(version: u32) {
    assert!(version == ABI_VERSION, "generated Trellis ABI mismatch");
}

/// Immutable projection of optional surfaces installed with an authorization context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvailabilitySnapshot {
    usable: bool,
    permissions: Arc<[trellis_protocol::PermissionAtom]>,
    resources: Arc<crate::service::ServiceResourceBindings>,
    resource_generations: Arc<std::collections::BTreeMap<(ResourceKind, String), u64>>,
}

impl AvailabilitySnapshot {
    pub(crate) fn is_usable(&self) -> bool {
        self.usable
    }

    #[cfg(test)]
    pub(crate) fn new(
        permissions: Vec<trellis_protocol::PermissionAtom>,
        resources: crate::service::ServiceResourceBindings,
    ) -> Self {
        Self::replacing(permissions, resources, &Self::default())
    }

    pub(crate) fn replacing(
        permissions: Vec<trellis_protocol::PermissionAtom>,
        resources: crate::service::ServiceResourceBindings,
        previous: &Self,
    ) -> Self {
        let mut generations = (*previous.resource_generations).clone();
        let next = generations.values().copied().max().unwrap_or(0) + 1;
        let candidate = Self {
            usable: true,
            permissions: permissions.into(),
            resources: Arc::new(resources),
            resource_generations: Arc::default(),
        };
        for kind in [
            ResourceKind::State,
            ResourceKind::Kv,
            ResourceKind::Store,
            ResourceKind::Job,
            ResourceKind::Consumer,
        ] {
            let names = candidate.resource_names(kind);
            for name in names {
                let unchanged = previous.has_resource(kind, &name)
                    && candidate.same_binding(previous, kind, &name);
                if !unchanged {
                    generations.insert((kind, name), next);
                }
            }
        }
        candidate.with_resource_generations(generations)
    }

    pub(crate) fn suspended(previous: &Self) -> Self {
        Self {
            usable: false,
            permissions: previous.permissions.clone(),
            resources: previous.resources.clone(),
            resource_generations: previous.resource_generations.clone(),
        }
    }

    fn with_resource_generations(
        mut self,
        generations: std::collections::BTreeMap<(ResourceKind, String), u64>,
    ) -> Self {
        self.resource_generations = Arc::new(generations);
        self
    }

    fn resource_names(&self, kind: ResourceKind) -> Vec<String> {
        match kind {
            ResourceKind::State => self
                .permissions
                .iter()
                .filter_map(|permission| match permission.target() {
                    trellis_protocol::PermissionTarget::ParticipantResource {
                        resource: trellis_protocol::ParticipantResourceKind::State,
                        name,
                        ..
                    } => Some(name.clone()),
                    _ => None,
                })
                .collect(),
            ResourceKind::Kv => self.resources.kv.keys().cloned().collect(),
            ResourceKind::Store => self.resources.store.keys().cloned().collect(),
            ResourceKind::Job => self
                .resources
                .jobs
                .as_ref()
                .map_or_else(Vec::new, |jobs| jobs.queues.keys().cloned().collect()),
            ResourceKind::Consumer => self.resources.event_consumers.keys().cloned().collect(),
        }
    }

    fn same_binding(&self, previous: &Self, kind: ResourceKind, name: &str) -> bool {
        match kind {
            ResourceKind::State => true,
            ResourceKind::Kv => self.resources.kv.get(name) == previous.resources.kv.get(name),
            ResourceKind::Store => {
                self.resources.store.get(name) == previous.resources.store.get(name)
            }
            ResourceKind::Job => {
                self.resources
                    .jobs
                    .as_ref()
                    .and_then(|jobs| jobs.queues.get(name))
                    == previous
                        .resources
                        .jobs
                        .as_ref()
                        .and_then(|jobs| jobs.queues.get(name))
            }
            ResourceKind::Consumer => {
                self.resources.event_consumers.get(name)
                    == previous.resources.event_consumers.get(name)
            }
        }
    }

    /// Return the current installation generation for an available resource.
    #[doc(hidden)]
    pub fn resource_generation(&self, kind: ResourceKind, name: &str) -> Option<u64> {
        self.has_resource(kind, name).then(|| {
            self.resource_generations
                .get(&(kind, name.to_owned()))
                .copied()
                .unwrap_or(0)
        })
    }

    pub(crate) fn kv_binding(&self, name: &str) -> Option<&crate::service::KvResourceBinding> {
        self.resources.kv.get(name)
    }

    pub(crate) fn store_binding(
        &self,
        name: &str,
    ) -> Option<&crate::service::StoreResourceBinding> {
        self.resources.store.get(name)
    }

    pub(crate) fn consumer_binding(
        &self,
        name: &str,
    ) -> Option<&crate::service::EventConsumerResourceBinding> {
        self.resources.event_consumers.get(name)
    }

    /// Test exact installed authority for an API action.
    #[doc(hidden)]
    pub fn allows_action(&self, action: OptionalAction) -> bool {
        self.usable
            && self.permissions.iter().any(|permission| {
                permission.action() == action.action
                    && permission.target().as_api_surface()
                        == Some((action.api_id, action.surface, action.name))
            })
    }

    /// Reject an optional action absent from this installed snapshot.
    #[doc(hidden)]
    pub fn require_action(
        &self,
        optional_actions: &[OptionalAction],
        action: OptionalAction,
    ) -> Result<(), crate::client::TrellisClientError> {
        if !optional_actions.contains(&action) || self.allows_action(action) {
            return Ok(());
        }
        Err(crate::client::TrellisClientError::AuthorizationUnavailable(
            format!("{} {}", action.api_id, action.name),
        ))
    }

    /// Test whether bootstrap installed a participant resource binding.
    #[doc(hidden)]
    pub fn has_resource(&self, kind: ResourceKind, name: &str) -> bool {
        if !self.usable {
            return false;
        }
        match kind {
            ResourceKind::State => self.permissions.iter().any(|permission| {
                matches!(
                    permission.target(),
                    trellis_protocol::PermissionTarget::ParticipantResource {
                        resource: trellis_protocol::ParticipantResourceKind::State,
                        name: resource_name,
                        ..
                    } if resource_name == name
                )
            }),
            ResourceKind::Kv => self.resources.kv.contains_key(name),
            ResourceKind::Store => self.resources.store.contains_key(name),
            ResourceKind::Job => self
                .resources
                .jobs
                .as_ref()
                .is_some_and(|jobs| jobs.queues.contains_key(name)),
            ResourceKind::Consumer => self.resources.event_consumers.contains_key(name),
        }
    }

    /// Test whether a cached KV handle still names the exact current binding.
    #[doc(hidden)]
    pub fn has_kv_binding(&self, name: &str, binding: &crate::service::KvResourceBinding) -> bool {
        self.usable
            && self
                .resources
                .kv
                .get(name)
                .is_some_and(|current| current.bucket == binding.bucket)
    }

    /// Test whether a cached Store handle still names the exact physical binding.
    #[doc(hidden)]
    pub fn has_store_binding(
        &self,
        name: &str,
        binding: &crate::service::StoreResourceBinding,
    ) -> bool {
        self.usable
            && self
                .resources
                .store
                .get(name)
                .is_some_and(|current| current.name == binding.name)
    }
}

/// Generated optional action identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionalAction {
    api_id: &'static str,
    surface: trellis_protocol::ApiSurfaceKind,
    name: &'static str,
    action: trellis_protocol::PermissionAction,
}

impl OptionalAction {
    /// Declare an optional RPC call.
    pub const fn rpc(api_id: &'static str, name: &'static str) -> Self {
        Self {
            api_id,
            surface: trellis_protocol::ApiSurfaceKind::Rpc,
            name,
            action: trellis_protocol::PermissionAction::Call,
        }
    }

    /// Declare an optional Operation invocation.
    pub const fn operation(api_id: &'static str, name: &'static str) -> Self {
        Self {
            api_id,
            surface: trellis_protocol::ApiSurfaceKind::Operation,
            name,
            action: trellis_protocol::PermissionAction::Invoke,
        }
    }

    /// Declare an optional event publication.
    pub const fn publish_event(api_id: &'static str, name: &'static str) -> Self {
        Self {
            api_id,
            surface: trellis_protocol::ApiSurfaceKind::Event,
            name,
            action: trellis_protocol::PermissionAction::Publish,
        }
    }

    /// Declare an optional event subscription.
    pub const fn subscribe_event(api_id: &'static str, name: &'static str) -> Self {
        Self {
            api_id,
            surface: trellis_protocol::ApiSurfaceKind::Event,
            name,
            action: trellis_protocol::PermissionAction::Subscribe,
        }
    }

    /// Declare an optional feed subscription.
    pub const fn feed(api_id: &'static str, name: &'static str) -> Self {
        Self {
            api_id,
            surface: trellis_protocol::ApiSurfaceKind::Feed,
            name,
            action: trellis_protocol::PermissionAction::Subscribe,
        }
    }
}

/// Participant resource family used by generated availability projections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    /// Participant-local State resource.
    State,
    /// NATS KV resource.
    Kv,
    /// Object store resource.
    Store,
    /// Private job queue.
    Job,
    /// Durable event consumer.
    Consumer,
}

/// Failure while converting a generated wire value.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The JSON representation did not match the generated Rust type.
    #[error("invalid generated wire value: {0}")]
    Json(#[from] serde_json::Error),
}

/// Bidirectional JSON codec implemented by generated wire types.
pub trait Codec: Sized {
    /// Encode this value to its JSON wire representation.
    fn encode(&self) -> Result<serde_json::Value, CodecError>;

    /// Decode this value from its JSON wire representation.
    fn decode(value: serde_json::Value) -> Result<Self, CodecError>;
}

impl<T> Codec for T
where
    T: Serialize + DeserializeOwned,
{
    fn encode(&self) -> Result<serde_json::Value, CodecError> {
        Ok(serde_json::to_value(self)?)
    }

    fn decode(value: serde_json::Value) -> Result<Self, CodecError> {
        Ok(serde_json::from_value(value)?)
    }
}

/// Metadata emitted for one generated RPC action.
pub trait RpcDescriptor {
    /// Request payload type.
    type Input: Codec + Serialize;
    /// Success payload type.
    type Output: Codec + DeserializeOwned;
    /// Generated union of errors declared by this action.
    type Error: std::fmt::Debug;

    /// Qualified API identity owning this action.
    const API_ID: &'static str;
    /// Exact generated descriptor name.
    const DESCRIPTOR_NAME: &'static str;
    /// Concrete NATS subject.
    const SUBJECT: &'static str;
    /// Exact permission/action key.
    const KEY: &'static str;
    /// Capability requirements declared for callers.
    const CALLER_CAPABILITIES: &'static [&'static str];
    /// Whether the success value may carry a runtime-issued download grant.
    const DOWNLOAD: bool;

    /// Decode a matching generated error, or return `None` for an unknown error type.
    fn decode_error(value: serde_json::Value) -> Result<Option<Self::Error>, serde_json::Error>;
}

/// Metadata emitted for one generated event action.
pub trait EventDescriptor {
    /// Event payload type.
    type Event: Codec + Serialize + DeserializeOwned;

    /// Qualified API identity owning this event.
    const API_ID: &'static str;
    /// Exact generated descriptor name.
    const DESCRIPTOR_NAME: &'static str;
    /// Canonical publish subject template.
    const SUBJECT: &'static str;
    /// Exact permission/action key.
    const KEY: &'static str;
    /// Wildcard subscription subject.
    const SUBSCRIBE_SUBJECT: &'static str = Self::SUBJECT;
    /// Capability requirements declared for publishers.
    const PUBLISH_CAPABILITIES: &'static [&'static str];
    /// Whether dependencies may publish this event.
    const DELEGATED_PUBLISH: bool = false;
    /// Capability requirements declared for subscribers.
    const SUBSCRIBE_CAPABILITIES: &'static [&'static str];

    /// Return the canonical identity cryptographically bound to publications.
    fn descriptor_identity() -> Result<String, crate::client::SubjectError> {
        trellis_protocol::encode_event_descriptor_identity(
            Self::API_ID,
            action_name(Self::DESCRIPTOR_NAME),
            Self::SUBSCRIBE_SUBJECT
                .split('.')
                .filter(|token| *token == "*")
                .count(),
        )
        .map_err(|error| crate::client::SubjectError::InvalidTemplate(error.to_string()))
    }

    /// Resolve any typed subject placeholders from the event payload.
    fn publish_subject(event: &Self::Event) -> Result<String, crate::client::SubjectError> {
        let value = event
            .encode()
            .map_err(|error| crate::client::SubjectError::InvalidPayload(error.to_string()))?;
        crate::client::resolve_subject(Self::SUBJECT, &value)
    }
}

/// Metadata emitted for one generated feed action.
pub trait FeedDescriptor {
    /// Feed subscription input type.
    type Input: Codec + Serialize + DeserializeOwned;
    /// Feed event payload type.
    type Event: Codec + Serialize + DeserializeOwned;

    /// Qualified API identity owning this feed.
    const API_ID: &'static str;
    /// Exact generated descriptor name.
    const DESCRIPTOR_NAME: &'static str;
    /// Concrete NATS subject.
    const SUBJECT: &'static str;
    /// Exact permission/action key.
    const KEY: &'static str;
    /// Capability requirements declared for subscribers.
    const SUBSCRIBE_CAPABILITIES: &'static [&'static str];
}

/// Metadata emitted for one complete generated Operation action.
pub trait OperationDescriptor: Send + Sync + 'static {
    /// Invocation payload type.
    type Input: Codec + Serialize + DeserializeOwned + Send + 'static;
    /// Terminal success payload type.
    type Output: Codec + Serialize + DeserializeOwned + Send + 'static;
    /// Durable progress payload, or `serde_json::Value` when none is declared.
    type Progress: Codec + Serialize + DeserializeOwned + Send + 'static;
    /// Live-only update payload, or `serde_json::Value` when none is declared.
    type Update: Codec + Serialize + DeserializeOwned + Send + 'static;
    /// Compile-time evidence for whether live updates are declared.
    type UpdateEvidence: crate::client::OperationUpdateEvidence;
    /// Generated union of errors declared by this Operation.
    type Error: OperationDeclaredError;

    /// Qualified API identity owning the whole Operation lifecycle.
    const API_ID: &'static str;
    /// Exact generated descriptor name.
    const DESCRIPTOR_NAME: &'static str;
    /// Concrete Operation start subject.
    const SUBJECT: &'static str;
    /// Exact whole-Operation permission/action key.
    const KEY: &'static str;
    /// Capability requirements declared for callers.
    const CALLER_CAPABILITIES: &'static [&'static str];
    /// Qualified declared error names.
    const ERRORS: &'static [&'static str];
    /// Exact declared signal names.
    const SIGNALS: &'static [&'static str];
    /// JSON Schemas keyed by exact declared signal name.
    const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{}";
    /// Whether the Operation accepts an upload transfer.
    const UPLOAD: bool;
    /// Whether a typed progress payload is declared.
    const HAS_PROGRESS: bool;
    /// JSON Schema for live-only updates when declared.
    const UPDATE_SCHEMA_JSON: Option<&'static str>;

    /// Decode a matching declared error, or return `None` for an unknown type.
    fn decode_error(value: serde_json::Value) -> Result<Option<Self::Error>, serde_json::Error>;
}

/// One typed signal belonging to a generated Operation descriptor.
pub trait OperationSignal: Send + Sync + 'static {
    /// Operation that declares this signal.
    type Operation: OperationDescriptor;
    /// Signal payload type.
    type Input: Codec + Serialize + DeserializeOwned + Send + 'static;

    /// Exact signal name from the Operation declaration.
    const NAME: &'static str;
}

/// Error union behavior required by generated Operation clients and providers.
pub trait OperationDeclaredError: std::fmt::Debug + Send + 'static {
    /// Serializable declared-error fields carried by this variant.
    fn data(&self) -> &SerializableErrorData;
}

impl OperationDeclaredError for std::convert::Infallible {
    fn data(&self) -> &SerializableErrorData {
        match *self {}
    }
}

/// Projection of a generated Operation into the current runtime operation transport.
#[doc(hidden)]
pub struct OperationAdapter<D>(std::marker::PhantomData<D>);

impl<D> crate::client::OperationDescriptor for OperationAdapter<D>
where
    D: OperationDescriptor,
{
    type Input = D::Input;
    type Progress = D::Progress;
    type Output = D::Output;
    type Update = D::Update;
    type UpdateEvidence = D::UpdateEvidence;
    type Error = DeclaredOperationFailure<D::Error>;

    const API_ID: &'static str = D::API_ID;
    const KEY: &'static str = D::KEY;
    const SUBJECT: &'static str = D::SUBJECT;
    const CALLER_CAPABILITIES: &'static [&'static str] = D::CALLER_CAPABILITIES;
    const OBSERVE_CAPABILITIES: &'static [&'static str] = D::CALLER_CAPABILITIES;
    const CANCEL_CAPABILITIES: &'static [&'static str] = D::CALLER_CAPABILITIES;
    const CONTROL_CAPABILITIES: &'static [&'static str] = D::CALLER_CAPABILITIES;
    const CANCELABLE: bool = true;
    const UPLOAD: bool = D::UPLOAD;
    const UPDATE_SCHEMA_JSON: Option<&'static str> = D::UPDATE_SCHEMA_JSON;
    const ERRORS: &'static [&'static str] = D::ERRORS;
    const INPUT_SCHEMA_JSON: &'static str = "{}";
    const PROGRESS_SCHEMA_JSON: Option<&'static str> =
        if D::HAS_PROGRESS { Some("{}") } else { None };
    const OUTPUT_SCHEMA_JSON: &'static str = "{}";
    const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = D::SIGNAL_INPUT_SCHEMAS_JSON;
}

/// Provider-side wrapper preserving a generated declared Operation error.
#[doc(hidden)]
#[derive(Debug)]
pub struct DeclaredOperationFailure<E>(E);

impl<E> DeclaredOperationFailure<E> {
    /// Wrap a generated declared error for the existing provider runtime.
    pub fn new(error: E) -> Self {
        Self(error)
    }

    /// Recover the generated error.
    pub fn into_inner(self) -> E {
        self.0
    }
}

impl<E> crate::service::OperationFailureLike for DeclaredOperationFailure<E>
where
    E: OperationDeclaredError,
{
    fn error_type(&self) -> &str {
        &self.0.data().error_type
    }

    fn message(&self) -> String {
        self.0.data().message.clone()
    }

    fn fields(&self) -> serde_json::Map<String, serde_json::Value> {
        self.0.data().extra.clone()
    }
}

/// Opaque authenticated transport used by generated clients.
#[derive(Clone)]
pub struct Client {
    client: Arc<crate::client::TrellisClient>,
    optional_actions: &'static [OptionalAction],
}

impl Client {
    pub(crate) fn from_client(client: Arc<crate::client::TrellisClient>) -> Self {
        Self {
            client,
            optional_actions: &[],
        }
    }

    /// Attach participant-specific optional action identities.
    #[doc(hidden)]
    pub fn with_optional_actions(mut self, optional_actions: &'static [OptionalAction]) -> Self {
        self.optional_actions = optional_actions;
        self
    }

    fn ensure_available(
        &self,
        action: OptionalAction,
    ) -> Result<(), crate::client::TrellisClientError> {
        self.client
            .availability()
            .require_action(self.optional_actions, action)
    }

    /// Connect using a durable user login session.
    pub async fn connect_user(
        options: crate::client::UserConnectOptions<'_>,
    ) -> Result<Self, crate::client::TrellisClientError> {
        Ok(Self::from_client(Arc::new(
            crate::client::TrellisClient::connect_user(options).await?,
        )))
    }

    /// Connect an activated generated device participant.
    pub async fn connect_device<P>(
        options: crate::client::DeviceConnectOptions<'_, P>,
    ) -> Result<Self, crate::client::TrellisClientError>
    where
        P: ParticipantDescriptor,
    {
        Ok(Self::from_client(Arc::new(
            crate::client::TrellisClient::connect_device(options).await?,
        )))
    }

    /// Return the independently authenticated companion client, when available.
    pub fn companion(&self) -> Option<Self> {
        self.client
            .companion()
            .map(|inner| Self::from_client(inner).with_optional_actions(self.optional_actions))
    }

    /// Invoke one generated RPC descriptor.
    pub async fn call<D>(
        &self,
        input: &D::Input,
    ) -> Result<D::Output, crate::client::CallError<D::Error>>
    where
        D: RpcDescriptor,
    {
        self.ensure_available(OptionalAction::rpc(
            D::API_ID,
            action_name(D::DESCRIPTOR_NAME),
        ))
        .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))?;
        let input = input.encode().map_err(|error| {
            crate::client::CallError::Protocol(crate::client::ProtocolError::new(error.to_string()))
        })?;
        let output = self
            .client
            .request_json_value(
                &self
                    .client
                    .bound_key_subject("rpc", D::API_ID, D::KEY)
                    .map_err(|error| {
                        crate::client::CallError::from_client(error, D::decode_error)
                    })?,
                &input,
            )
            .await
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))?;
        D::Output::decode(output).map_err(|error| {
            crate::client::CallError::Protocol(crate::client::ProtocolError::new(error.to_string()))
        })
    }

    /// Publish one generated event descriptor.
    pub async fn publish<D>(
        &self,
        event: &D::Event,
    ) -> Result<(), crate::client::TrellisClientError>
    where
        D: EventDescriptor,
    {
        self.ensure_available(OptionalAction::publish_event(
            D::API_ID,
            action_name(D::DESCRIPTOR_NAME),
        ))?;
        self.client.publish::<D>(event).await
    }

    /// Subscribe to one generated event descriptor.
    pub async fn subscribe<D>(
        &self,
        options: crate::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<D::Event, crate::client::TrellisClientError>,
        >,
        crate::client::TrellisClientError,
    >
    where
        D: EventDescriptor,
        D::Event: Send + 'static,
    {
        self.ensure_available(OptionalAction::subscribe_event(
            D::API_ID,
            action_name(D::DESCRIPTOR_NAME),
        ))?;
        self.client.subscribe_with_options::<D>(options).await
    }

    /// Subscribe to one generated feed descriptor.
    pub async fn feed<D>(
        &self,
        input: &D::Input,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<D::Event, crate::client::TrellisClientError>,
        >,
        crate::client::TrellisClientError,
    >
    where
        D: FeedDescriptor,
        D::Event: Send + 'static,
    {
        self.ensure_available(OptionalAction::feed(
            D::API_ID,
            action_name(D::DESCRIPTOR_NAME),
        ))?;
        self.client.feed::<D>(input).await
    }

    /// Create a typed facade for one complete generated Operation.
    pub fn operation<D>(&self) -> Operation<'_, D>
    where
        D: OperationDescriptor,
    {
        Operation {
            inner: crate::client::OperationInvoker::new(self.client.as_ref()),
            client: self,
        }
    }

    /// Refresh this client's authorization context.
    pub async fn refresh_authorization_context(
        &self,
    ) -> Result<(), crate::client::TrellisClientError> {
        self.client.refresh_authorization_context().await.map(drop)
    }

    /// Return the current immutable availability snapshot.
    pub fn availability(&self) -> AvailabilitySnapshot {
        self.client.availability()
    }

    /// Watch subsequent atomic availability replacements.
    pub fn watch_availability(&self) -> tokio::sync::watch::Receiver<AvailabilitySnapshot> {
        self.client.watch_availability()
    }

    /// Construct a generated typed State resource handle.
    #[doc(hidden)]
    pub fn state_handle<T>(
        &self,
        name: &str,
        codec: crate::client::ResourceCodec<T>,
    ) -> Option<crate::client::ConnectedStateHandle<T>>
    where
        T: Codec + Send + 'static,
    {
        self.client
            .availability()
            .has_resource(ResourceKind::State, name)
            .then(|| {
                crate::client::StateHandle::from_generated(
                    name,
                    codec,
                    crate::client::BoundStateResourceClient::new(self.client.clone()),
                    self.client.watch_availability(),
                )
            })
    }

    /// Open a generated typed KV resource handle from its current installation binding.
    #[doc(hidden)]
    pub async fn kv_handle<T>(
        &self,
        name: &str,
        codec: crate::client::ResourceCodec<T>,
    ) -> Result<Option<crate::service::KvHandle<T>>, crate::service::ServerError>
    where
        T: Codec + Send + 'static,
    {
        let availability = self.client.availability();
        let Some(binding) = availability.kv_binding(name).cloned() else {
            return Ok(None);
        };
        crate::service::open_generated_kv(
            &self.client.nats(),
            &self
                .client
                .participant_id()
                .map_err(|error| crate::service::ServerError::Nats(error.to_string()))?,
            name,
            binding,
            codec,
            self.client.watch_availability(),
        )
        .await
        .map(Some)
    }

    /// Open a generated Store resource handle from its current installation binding.
    #[doc(hidden)]
    pub async fn store_handle(
        &self,
        name: &str,
    ) -> Result<Option<crate::service::StoreHandle>, crate::service::ServerError> {
        let availability = self.client.availability();
        let Some(binding) = availability.store_binding(name).cloned() else {
            return Ok(None);
        };
        crate::service::open_generated_store(
            &self.client.nats(),
            &self
                .client
                .participant_id()
                .map_err(|error| crate::service::ServerError::Nats(error.to_string()))?,
            name,
            binding,
            self.client.watch_availability(),
        )
        .await
        .map(Some)
    }

    /// Construct a generated durable Consumer handle from its installed binding.
    #[doc(hidden)]
    pub fn consumer_handle<D>(&self) -> Option<crate::client::ConsumerHandle<D>>
    where
        D: crate::client::ConsumerDescriptor,
    {
        let availability = self.client.availability();
        let binding = availability.consumer_binding(D::NAME)?.clone();
        Some(crate::client::ConsumerHandle::from_generated(
            binding,
            self.client.nats().clone(),
            self.clone(),
            self.client.watch_availability(),
        ))
    }

    /// Download a generated receive-transfer grant.
    pub async fn download_transfer(
        &self,
        grant: &crate::client::DownloadTransferGrant,
    ) -> Result<Vec<u8>, crate::client::TrellisClientError> {
        self.client.download_transfer(grant).await
    }

    /// Stream a generated receive-transfer grant into a writer.
    pub async fn download_transfer_into<W>(
        &self,
        grant: &crate::client::DownloadTransferGrant,
        writer: &mut W,
    ) -> Result<crate::client::FileInfo, crate::client::TrellisClientError>
    where
        W: tokio::io::AsyncWrite + Unpin + Send + ?Sized,
    {
        self.client.download_transfer_into(grant, writer).await
    }

    /// Stream a generated receive-transfer grant with authenticated cancellation.
    pub async fn download_transfer_into_with_cancel<W>(
        &self,
        grant: &crate::client::DownloadTransferGrant,
        writer: &mut W,
        cancellation: &crate::client::TransferCancellation,
    ) -> Result<crate::client::FileInfo, crate::client::TrellisClientError>
    where
        W: tokio::io::AsyncWrite + Unpin + Send + ?Sized,
    {
        self.client
            .download_transfer_into_with_cancel(grant, writer, cancellation)
            .await
    }

    pub(crate) fn inner(&self) -> &Arc<crate::client::TrellisClient> {
        &self.client
    }

    pub(crate) async fn request_api_value(
        &self,
        api_id: &str,
        action: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, crate::client::TrellisClientError> {
        let subject = self.client.bound_api_subject("rpc", api_id, action)?;
        self.client.request_json_value(&subject, &input).await
    }
}

/// Typed client entrypoint for one generated Operation.
pub struct Operation<'a, D>
where
    D: OperationDescriptor,
{
    inner: crate::client::OperationInvoker<'a, crate::client::TrellisClient, OperationAdapter<D>>,
    client: &'a Client,
}

impl<D> std::fmt::Debug for Operation<'_, D>
where
    D: OperationDescriptor,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Operation")
            .field("api_id", &D::API_ID)
            .field("descriptor_name", &D::DESCRIPTOR_NAME)
            .finish_non_exhaustive()
    }
}

impl<'a, D> Operation<'a, D>
where
    D: OperationDescriptor,
{
    /// Start the Operation through the existing authenticated transport.
    pub async fn start(
        &self,
        input: &D::Input,
    ) -> Result<OperationRef<'a, D>, crate::client::CallError<D::Error>> {
        self.client
            .ensure_available(OptionalAction::operation(
                D::API_ID,
                action_name(D::DESCRIPTOR_NAME),
            ))
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))?;
        self.inner
            .start(input)
            .await
            .map(|inner| OperationRef { inner })
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))
    }

    /// Open typed control for an existing durable Operation id without starting it.
    pub fn control(
        &self,
        operation_id: impl Into<String>,
    ) -> Result<OperationRef<'a, D>, crate::client::TrellisClientError> {
        self.inner
            .control(operation_id)
            .map(|inner| OperationRef { inner })
    }
}

fn action_name(descriptor_name: &'static str) -> &'static str {
    descriptor_name
        .split_once('.')
        .map_or(descriptor_name, |(_, name)| name)
}

/// Typed reference to one accepted generated Operation.
pub struct OperationRef<'a, D>
where
    D: OperationDescriptor,
{
    inner: crate::client::OperationRef<'a, crate::client::TrellisClient, OperationAdapter<D>>,
}

impl<D> std::fmt::Debug for OperationRef<'_, D>
where
    D: OperationDescriptor,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperationRef")
            .field("id", &self.inner.id())
            .field("api_id", &D::API_ID)
            .field("descriptor_name", &D::DESCRIPTOR_NAME)
            .finish_non_exhaustive()
    }
}

impl<'a, D> OperationRef<'a, D>
where
    D: OperationDescriptor,
{
    /// Durable Operation id.
    pub fn id(&self) -> &str {
        self.inner.id()
    }

    /// Read the current durable snapshot.
    pub async fn get(
        &self,
    ) -> Result<
        crate::client::OperationSnapshot<D::Progress, D::Output>,
        crate::client::TrellisClientError,
    > {
        self.inner.get().await
    }

    /// Wait using the current runtime's real terminal-result path.
    pub async fn wait(
        &self,
    ) -> Result<
        crate::client::OperationSnapshot<D::Progress, D::Output>,
        crate::client::TrellisClientError,
    > {
        self.inner.wait().await
    }

    /// Request cancellation through the whole-Operation control path.
    pub async fn cancel(
        &self,
    ) -> Result<
        crate::client::OperationSnapshot<D::Progress, D::Output>,
        crate::client::TrellisClientError,
    > {
        self.inner.cancel().await
    }

    /// Send one typed signal declared by this Operation.
    pub async fn signal<S>(
        &self,
        input: &S::Input,
    ) -> Result<
        crate::client::OperationSignalAccepted<D::Progress, D::Output>,
        crate::client::TrellisClientError,
    >
    where
        S: OperationSignal<Operation = D>,
    {
        let input = input
            .encode()
            .map_err(|error| crate::client::TrellisClientError::Codec(error.to_string()))?;
        self.inner
            .signal_encoded(S::NAME.to_owned(), Some(input))
            .await
    }

    /// Watch durable lifecycle snapshots from the current runtime.
    pub async fn watch(
        &self,
    ) -> Result<
        futures_util::stream::BoxStream<
            'a,
            Result<
                crate::client::OperationEvent<D::Progress, D::Output, serde_json::Value>,
                crate::client::TrellisClientError,
            >,
        >,
        crate::client::TrellisClientError,
    > {
        self.inner.watch().await
    }

    /// Upload the declared Operation transfer body through the accepted grant.
    pub async fn upload(
        &self,
        body: impl AsRef<[u8]>,
    ) -> Result<crate::client::FileInfo, crate::client::TrellisClientError> {
        if !D::UPLOAD {
            return Err(crate::client::TrellisClientError::OperationProtocol(
                "operation does not declare an upload transfer".to_owned(),
            ));
        }
        self.inner.transfer(body).await
    }
}

/// Descriptor implemented by each generated API module.
pub trait ApiDescriptor {
    /// Qualified API identity.
    const ID: &'static str;

    /// Validate local descriptor structure without deriving authority.
    fn validate() -> Result<(), DescriptorError> {
        validate_identity(Self::ID)
    }
}

/// Kind of generated application participant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParticipantKind {
    /// Long-running service participant.
    Service,
    /// Provisioned physical or embedded device participant.
    Device,
    /// User-facing application participant.
    App,
    /// User-authorized agent participant.
    Agent,
}

/// Exact lexical child installed alongside a device participant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompanionDescriptor {
    /// Fully qualified child participant identity.
    pub id: &'static str,
    /// Child participant kind, restricted to app or agent.
    pub kind: ParticipantKind,
    /// Whether device readiness requires the child connection.
    pub required: bool,
}

/// Descriptor implemented by each generated participant module.
pub trait ParticipantDescriptor {
    /// Qualified participant identity.
    const ID: &'static str;
    /// Lexical participant path within the generated root package.
    const PATH: &'static str;
    /// Participant kind.
    const KIND: ParticipantKind;

    /// Exact lexical app or agent companion, when declared by this device.
    const COMPANION: Option<CompanionDescriptor> = None;

    /// APIs completely implemented by this participant.
    const IMPLEMENTED_API_IDS: &'static [&'static str] = &[];

    /// Exact package evidence embedded by generation.
    fn package_evidence() -> PackageEvidence;

    /// Validate local descriptor structure without deriving authority.
    fn validate() -> Result<(), DescriptorError> {
        validate_identity(Self::ID)?;
        let evidence = Self::package_evidence();
        evidence.validate()?;
        if Self::PATH.is_empty()
            || Self::ID.strip_prefix(evidence.root_package()) != Some(&format!(".{}", Self::PATH))
        {
            return Err(DescriptorError::ParticipantPath);
        }
        if let Some(companion) = Self::COMPANION {
            if Self::KIND != ParticipantKind::Device
                || !matches!(
                    companion.kind,
                    ParticipantKind::App | ParticipantKind::Agent
                )
                || companion.id.rsplit_once('.').map(|(parent, _)| parent) != Some(Self::ID)
            {
                return Err(DescriptorError::ParticipantPath);
            }
        }
        Ok(())
    }
}

/// Local structural failure in a generated descriptor.
#[derive(Debug, thiserror::Error)]
pub enum DescriptorError {
    /// A required generated identity was empty.
    #[error("generated descriptor identity must not be empty")]
    EmptyId,
    /// Generated source evidence was incomplete or not canonically ordered.
    #[error("generated package evidence is incomplete or unsorted")]
    PackageEvidence,
    /// The lexical participant path does not match its qualified identity.
    #[error("generated participant path does not match its package and identity")]
    ParticipantPath,
}

fn validate_identity(id: &str) -> Result<(), DescriptorError> {
    if id.is_empty() {
        return Err(DescriptorError::EmptyId);
    }
    Ok(())
}

/// One immutable source package embedded in generated evidence.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct PackageSourceEvidence {
    name: &'static str,
    version: &'static str,
    digest: &'static str,
    source: &'static str,
}

impl PackageSourceEvidence {
    /// Construct one source-package entry from generated constants.
    #[doc(hidden)]
    pub const fn from_generated(
        name: &'static str,
        version: &'static str,
        digest: &'static str,
        source: &'static str,
    ) -> Self {
        Self {
            name,
            version,
            digest,
            source,
        }
    }

    /// Package identity.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Exact package version.
    pub const fn version(&self) -> &'static str {
        self.version
    }

    /// Semantic package digest.
    pub const fn digest(&self) -> &'static str {
        self.digest
    }

    /// Presentation-preserving canonical source.
    pub const fn source(&self) -> &'static str {
        self.source
    }
}

/// Opaque generated package evidence presented to Trellis runtime boundaries.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageEvidence {
    root_package: &'static str,
    root_digest: &'static str,
    packages: &'static [PackageSourceEvidence],
}

impl PackageEvidence {
    /// Construct evidence from generated source-package constants.
    #[doc(hidden)]
    pub const fn from_generated(
        root_package: &'static str,
        root_digest: &'static str,
        packages: &'static [PackageSourceEvidence],
    ) -> Self {
        Self {
            root_package,
            root_digest,
            packages,
        }
    }

    /// Root package identity.
    pub const fn root_package(&self) -> &'static str {
        self.root_package
    }

    /// Root semantic package digest.
    pub const fn root_digest(&self) -> &'static str {
        self.root_digest
    }

    /// Complete source-package dependency closure in package identity order.
    pub const fn packages(&self) -> &'static [PackageSourceEvidence] {
        self.packages
    }

    /// Validate only the local shape and ordering of generated source evidence.
    pub fn validate(&self) -> Result<(), DescriptorError> {
        if self.root_package.is_empty() || self.root_digest.is_empty() {
            return Err(DescriptorError::PackageEvidence);
        }
        let mut previous = None;
        let mut root_found = false;
        for package in self.packages {
            if package.name.is_empty()
                || package.version.is_empty()
                || package.digest.is_empty()
                || package.source.is_empty()
                || previous.is_some_and(|name| name >= package.name)
            {
                return Err(DescriptorError::PackageEvidence);
            }
            previous = Some(package.name);
            if package.name == self.root_package {
                if package.digest != self.root_digest {
                    return Err(DescriptorError::PackageEvidence);
                }
                root_found = true;
            }
        }
        if !root_found {
            return Err(DescriptorError::PackageEvidence);
        }
        Ok(())
    }
}

/// RFC 3339 timestamp normalized to canonical UTC by generated SDKs.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Timestamp(String);

impl Timestamp {
    /// Parse an RFC 3339 timestamp with up to nanosecond precision and normalize it.
    pub fn parse(value: &str) -> Result<Self, TimestampError> {
        use time::format_description::well_known::Rfc3339;

        if !matches!(value.as_bytes().get(10), Some(b'T' | b't')) {
            return Err(TimestampError::Separator);
        }
        if value.get(17..19) == Some("60") {
            return Err(TimestampError::LeapSecond);
        }
        if value
            .get(19..)
            .and_then(|tail| tail.strip_prefix('.'))
            .is_some_and(|fraction| fraction.bytes().take_while(u8::is_ascii_digit).count() > 9)
        {
            return Err(TimestampError::Precision);
        }
        let parsed = time::OffsetDateTime::parse(value, &Rfc3339)?;
        Ok(Self(
            parsed.to_offset(time::UtcOffset::UTC).format(&Rfc3339)?,
        ))
    }

    /// Borrow the canonical wire string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::str::FromStr for Timestamp {
    type Err = TimestampError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Invalid generated timestamp.
#[derive(Debug, thiserror::Error)]
pub enum TimestampError {
    /// The value is not RFC 3339.
    #[error("invalid RFC 3339 timestamp: {0}")]
    Parse(#[from] time::error::Parse),
    /// A parsed timestamp could not be rendered.
    #[error("could not format RFC 3339 timestamp: {0}")]
    Format(#[from] time::error::Format),
    /// A fractional second exceeds nanosecond precision.
    #[error("timestamp exceeds nanosecond precision")]
    Precision,
    /// A non-RFC 3339 date/time separator was used.
    #[error("timestamp requires an RFC 3339 date/time separator")]
    Separator,
    /// Leap seconds cannot be represented without losing precision.
    #[error("leap seconds are not supported in timestamps")]
    LeapSecond,
}

/// Validated uppercase canonical ULID used by generated SDKs.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Ulid(ulid::Ulid);

impl Ulid {
    /// Parse an uppercase canonical ULID.
    pub fn parse(value: &str) -> Result<Self, UlidError> {
        let parsed = value.parse::<ulid::Ulid>().map_err(UlidError::Invalid)?;
        if parsed.to_string() != value {
            return Err(UlidError::NonCanonical);
        }
        Ok(Self(parsed))
    }
}

impl std::fmt::Display for Ulid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for Ulid {
    type Err = UlidError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for Ulid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Ulid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Invalid or non-canonical generated ULID.
#[derive(Debug, thiserror::Error)]
pub enum UlidError {
    /// The value is not a ULID.
    #[error("invalid ULID: {0}")]
    Invalid(ulid::DecodeError),
    /// The value does not use uppercase canonical spelling.
    #[error("ULID must use uppercase canonical spelling")]
    NonCanonical,
}

/// Standard serializable fields carried by generated declared errors.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializableErrorData {
    /// Error instance identifier.
    pub id: String,
    /// Contract error discriminator.
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-facing error message.
    pub message: String,
    /// Optional structured context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Map<String, serde_json::Value>>,
    /// Optional distributed trace identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Additive fields from newer runtimes or declared error data.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Contract implemented by generated typed errors.
pub trait TrellisError: std::error::Error + Serialize + DeserializeOwned {
    /// Stable contract error discriminator.
    const TYPE: &'static str;
}

/// Decode a generated typed error only when its discriminator matches.
pub fn decode_typed_error<E>(value: serde_json::Value) -> Result<Option<E>, serde_json::Error>
where
    E: TrellisError,
{
    if value.get("type").and_then(serde_json::Value::as_str) != Some(E::TYPE) {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some)
}

/// Serde adapter for decimal-string `i64` wire values.
pub mod serde_i64 {
    use serde::{de::Error as _, Deserialize as _, Deserializer, Serializer};

    /// Serialize an `i64` as a decimal JSON string.
    pub fn serialize<S>(value: &i64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    /// Deserialize an `i64` from a decimal JSON string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let digits = value.strip_prefix('-').unwrap_or(&value);
        if digits.is_empty()
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
            || value == "-0"
        {
            return Err(D::Error::custom("expected canonical i64 decimal string"));
        }
        value.parse().map_err(D::Error::custom)
    }
}

/// Serde adapter for decimal-string `u64` wire values.
pub mod serde_u64 {
    use serde::{de::Error as _, Deserialize as _, Deserializer, Serializer};

    /// Serialize a `u64` as a decimal JSON string.
    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    /// Deserialize a `u64` from a decimal JSON string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(D::Error::custom("expected canonical u64 decimal string"));
        }
        value.parse().map_err(D::Error::custom)
    }
}

/// Serde adapter rejecting non-finite `f64` values at both codec boundaries.
pub mod serde_f64 {
    use serde::{de::Error as _, ser::Error as _, Deserialize as _, Deserializer, Serializer};

    /// Serialize a finite JSON number.
    pub fn serialize<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if !value.is_finite() {
            return Err(S::Error::custom("expected finite f64"));
        }
        serializer.serialize_f64(*value)
    }

    /// Deserialize a finite JSON number.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        if !value.is_finite() {
            return Err(D::Error::custom("expected finite f64"));
        }
        Ok(value)
    }
}

/// Serde adapter for padded standard-base64 byte vectors.
pub mod serde_base64 {
    use base64::Engine as _;
    use serde::{de::Error as _, Deserialize as _, Deserializer, Serializer};

    /// Serialize bytes as padded standard base64.
    pub fn serialize<S>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(value))
    }

    /// Deserialize bytes from canonical padded standard base64.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&value)
            .map_err(D::Error::custom)?;
        if base64::engine::general_purpose::STANDARD.encode(&decoded) != value {
            return Err(D::Error::custom(
                "expected canonical padded standard base64",
            ));
        }
        Ok(decoded)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use serde::{Deserialize, Serialize};

    use super::{
        ApiDescriptor, AvailabilitySnapshot, Codec as _, DeclaredOperationFailure,
        OperationAdapter, OperationDeclaredError, OperationDescriptor, OperationSignal,
        OptionalAction, PackageEvidence, PackageSourceEvidence, ResourceKind,
        SerializableErrorData, Timestamp, TrellisError, Ulid,
    };

    #[test]
    fn resource_generations_fence_removed_and_replaced_handles() {
        let binding = crate::service::KvResourceBinding {
            bucket: "KV_A".into(),
            history: 2,
            max_value_bytes: None,
            ttl_ms: 0,
        };
        let resources = crate::service::ServiceResourceBindings {
            kv: [("records".into(), binding.clone())].into(),
            ..Default::default()
        };
        let first = AvailabilitySnapshot::replacing(vec![], resources.clone(), &Default::default());
        let generation = first
            .resource_generation(ResourceKind::Kv, "records")
            .unwrap();

        let unchanged = AvailabilitySnapshot::replacing(vec![], resources, &first);
        assert_eq!(
            unchanged.resource_generation(ResourceKind::Kv, "records"),
            Some(generation)
        );

        let removed = AvailabilitySnapshot::replacing(vec![], Default::default(), &unchanged);
        let readded = AvailabilitySnapshot::replacing(
            vec![],
            crate::service::ServiceResourceBindings {
                kv: [("records".into(), binding.clone())].into(),
                ..Default::default()
            },
            &removed,
        );
        assert_ne!(
            readded.resource_generation(ResourceKind::Kv, "records"),
            Some(generation)
        );

        let replaced = AvailabilitySnapshot::replacing(
            vec![],
            crate::service::ServiceResourceBindings {
                kv: [(
                    "records".into(),
                    crate::service::KvResourceBinding {
                        bucket: "KV_B".into(),
                        ..binding
                    },
                )]
                .into(),
                ..Default::default()
            },
            &readded,
        );
        assert_ne!(
            replaced.resource_generation(ResourceKind::Kv, "records"),
            readded.resource_generation(ResourceKind::Kv, "records")
        );
    }

    #[tokio::test]
    async fn availability_snapshot_replacement_wakes_watchers() {
        let initial = AvailabilitySnapshot::default();
        let (sender, mut receiver) = tokio::sync::watch::channel(initial.clone());
        let action = OptionalAction::rpc("fixture.Primary@v1", "Fetch");
        let replacement = AvailabilitySnapshot::new(
            vec![trellis_protocol::PermissionAtom::new(
                trellis_protocol::PermissionTarget::api_surface(
                    "fixture.Primary@v1",
                    trellis_protocol::ApiSurfaceKind::Rpc,
                    "Fetch",
                )
                .unwrap(),
                trellis_protocol::PermissionAction::Call,
            )
            .unwrap()],
            Default::default(),
        );

        assert!(!initial.allows_action(action));
        sender.send_replace(replacement.clone());
        receiver.changed().await.unwrap();
        assert_eq!(*receiver.borrow(), replacement);
        assert!(receiver.borrow().allows_action(action));
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct WireScalars {
        #[serde(with = "super::serde_i64")]
        signed: i64,
        #[serde(with = "super::serde_u64")]
        unsigned: u64,
        #[serde(with = "super::serde_base64")]
        bytes: Vec<u8>,
        #[serde(with = "super::serde_f64")]
        number: f64,
    }

    #[test]
    fn generated_scalar_adapters_use_exact_wire_forms() {
        let value = WireScalars {
            signed: i64::MIN,
            unsigned: u64::MAX,
            bytes: vec![0, 1, 254, 255],
            number: 1.25,
        };
        let encoded = value.encode().unwrap();

        assert_eq!(
            encoded,
            serde_json::json!({
                "signed": "-9223372036854775808",
                "unsigned": "18446744073709551615",
                "bytes": "AAH+/w==",
                "number": 1.25
            })
        );
        assert_eq!(WireScalars::decode(encoded).unwrap(), value);
        assert!(WireScalars::decode(serde_json::json!({
            "signed": 1,
            "unsigned": "01",
            "bytes": "AAE",
            "number": 1.25
        }))
        .is_err());
        assert!(serde_json::from_value::<WireScalars>(serde_json::json!({
            "signed": "+1",
            "unsigned": "+1",
            "bytes": "AA==",
            "number": 1.25
        }))
        .is_err());
        assert!(serde_json::to_value(WireScalars {
            signed: 0,
            unsigned: 0,
            bytes: Vec::new(),
            number: f64::NAN,
        })
        .is_err());
        assert_eq!(
            Timestamp::parse("2026-09-10T12:34:56.123456789Z")
                .unwrap()
                .as_str(),
            "2026-09-10T12:34:56.123456789Z"
        );
        for (input, canonical) in [
            ("2026-09-10T12:34:56.000Z", "2026-09-10T12:34:56Z"),
            ("2026-09-10T12:34:56.010Z", "2026-09-10T12:34:56.01Z"),
            ("2026-09-10T12:34:56.100Z", "2026-09-10T12:34:56.1Z"),
            ("2026-09-10T12:34:56.120Z", "2026-09-10T12:34:56.12Z"),
            ("2026-09-10T12:34:56.123Z", "2026-09-10T12:34:56.123Z"),
            ("2026-09-10T12:34:56.500Z", "2026-09-10T12:34:56.5Z"),
            ("2026-09-10T12:34:56.999Z", "2026-09-10T12:34:56.999Z"),
            ("2026-09-10T14:34:56.120+02:00", "2026-09-10T12:34:56.12Z"),
            ("2026-09-10T12:34:56.120+00:00", "2026-09-10T12:34:56.12Z"),
            ("2026-09-10t12:34:56.120z", "2026-09-10T12:34:56.12Z"),
        ] {
            let timestamp = Timestamp::parse(input).unwrap();
            assert_eq!(timestamp.as_str(), canonical);
            assert_eq!(serde_json::to_value(&timestamp).unwrap(), canonical);
        }
        for second in 0..60 {
            for millis in [0, 10, 100, 120, 123, 500, 999] {
                let input = format!("2026-09-10T12:34:{second:02}.{millis:03}Z");
                let fraction = format!(".{millis:03}");
                let fraction = if millis == 0 {
                    ""
                } else {
                    fraction.trim_end_matches('0')
                };
                let expected = format!("2026-09-10T12:34:{second:02}{fraction}Z");
                assert_eq!(Timestamp::parse(&input).unwrap().as_str(), expected);
            }
        }
        assert!(Timestamp::parse("2026-09-10T12:34:56.1234567890Z").is_err());
        assert!(Timestamp::parse("2026-09-10.12:34:56.1234567891Z").is_err());
        assert!(Timestamp::parse("2016-12-31T23:59:60Z").is_err());
        assert!(Timestamp::parse("2026-02-30T12:34:56Z").is_err());
        assert_eq!(
            Ulid::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .unwrap()
                .to_string(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
    }

    struct ExampleApi;

    impl ApiDescriptor for ExampleApi {
        const ID: &'static str = "example/Orders@v1";
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct ExampleError {
        message: String,
    }

    impl fmt::Display for ExampleError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.message)
        }
    }

    impl std::error::Error for ExampleError {}

    impl TrellisError for ExampleError {
        const TYPE: &'static str = "ExampleError";
    }

    #[derive(Debug)]
    struct ExampleOperationError(SerializableErrorData);

    impl OperationDeclaredError for ExampleOperationError {
        fn data(&self) -> &SerializableErrorData {
            &self.0
        }
    }

    struct ExampleOperation;

    impl OperationDescriptor for ExampleOperation {
        type Input = String;
        type Output = bool;
        type Progress = u32;
        type Update = serde_json::Value;
        type UpdateEvidence = crate::client::NoOperationUpdates;
        type Error = ExampleOperationError;

        const API_ID: &'static str = "example/Orders@v1";
        const DESCRIPTOR_NAME: &'static str = "operation.Process";
        const SUBJECT: &'static str = "operation.v1.Orders.Process";
        const KEY: &'static str = "Orders.Process";
        const CALLER_CAPABILITIES: &'static [&'static str] = &["orders.process"];
        const ERRORS: &'static [&'static str] = &["example/Orders@v1.Rejected"];
        const SIGNALS: &'static [&'static str] = &["Resume"];
        const UPLOAD: bool = true;
        const HAS_PROGRESS: bool = true;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = None;

        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            serde_json::from_value(value).map(|data| Some(ExampleOperationError(data)))
        }
    }

    struct Resume;

    impl OperationSignal for Resume {
        type Operation = ExampleOperation;
        type Input = String;

        const NAME: &'static str = "Resume";
    }

    #[test]
    fn generated_descriptors_validate_only_local_structure() {
        ExampleApi::validate().unwrap();
        const PACKAGES: &[PackageSourceEvidence] = &[PackageSourceEvidence::from_generated(
            "example",
            "1.0.0",
            "digest",
            "package \"example\";\n",
        )];
        let evidence = PackageEvidence::from_generated("example", "digest", PACKAGES);
        evidence.validate().unwrap();
        assert_eq!(evidence.root_package(), "example");
        assert_eq!(evidence.root_digest(), "digest");
        assert_eq!(evidence.packages()[0].name(), "example");
        assert_eq!(
            super::decode_typed_error::<ExampleError>(serde_json::json!({
                "type": "ExampleError",
                "message": "typed"
            }))
            .unwrap(),
            Some(ExampleError {
                message: "typed".into()
            })
        );
        assert!(
            super::decode_typed_error::<ExampleError>(serde_json::json!({
                "type": "OtherError",
                "message": "unknown"
            }))
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn generated_operation_descriptor_projects_into_current_runtime() {
        type Runtime = OperationAdapter<ExampleOperation>;

        assert_eq!(
            <Runtime as crate::client::OperationDescriptor>::KEY,
            ExampleOperation::KEY
        );
        assert_eq!(
            <Runtime as crate::client::OperationDescriptor>::SUBJECT,
            ExampleOperation::SUBJECT
        );
        assert_eq!(
            <Runtime as crate::client::OperationDescriptor>::PROGRESS_SCHEMA_JSON,
            Some("{}")
        );
        assert_eq!(
            <<Resume as OperationSignal>::Operation as OperationDescriptor>::SIGNALS,
            &[<Resume as OperationSignal>::NAME]
        );

        let failure = DeclaredOperationFailure::new(ExampleOperationError(SerializableErrorData {
            id: "error-1".into(),
            error_type: "example/Orders@v1.Rejected".into(),
            message: "rejected".into(),
            context: None,
            trace_id: None,
            extra: serde_json::Map::new(),
        }));
        assert_eq!(
            crate::service::OperationFailureLike::error_type(&failure),
            "example/Orders@v1.Rejected"
        );
        assert_eq!(
            crate::service::OperationFailureLike::message(&failure),
            "rejected"
        );
    }
}
