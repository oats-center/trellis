//! Runtime ABI used by Trellis-generated Rust crates.
//!
//! Application code should use generated owner SDK and participant facade APIs
//! rather than implementing these traits directly.

use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};

pub use trellis_protocol::model::{
    assert_model_binding, decode_typed_error, serde_base64, serde_f64, serde_i64, serde_u64, Bytes,
    Codec, CodecError, CursorPage, CursorPageInfo, CursorQuery, Int64, ModelPackage, Nullable,
    Number, SerializableErrorData, Timestamp, TimestampError, TrellisError, Uint64, Ulid,
    UlidError,
};

/// Generated-code ABI version supported by this runtime.
pub const ABI_VERSION: u32 = 2;

/// Fail compilation when generated source targets a different runtime ABI.
pub const fn assert_abi(version: u32) {
    assert!(version == ABI_VERSION, "generated Trellis ABI mismatch");
}

/// Failure while traversing a cursor-paginated RPC.
#[derive(Debug)]
pub enum PaginationError<E: std::fmt::Debug> {
    /// The RPC call failed.
    Call(crate::client::CallError<E>),
    /// The server returned a cursor already seen by this traversal.
    RepeatedCursor(String),
}

impl<E: std::fmt::Debug> std::fmt::Display for PaginationError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Call(error) => error.fmt(formatter),
            Self::RepeatedCursor(cursor) => {
                write!(formatter, "server repeated pagination cursor '{cursor}'")
            }
        }
    }
}

impl<E: std::fmt::Debug> std::error::Error for PaginationError<E> {}

impl<E: std::fmt::Debug> From<crate::client::CallError<E>> for PaginationError<E> {
    fn from(error: crate::client::CallError<E>) -> Self {
        Self::Call(error)
    }
}

/// Traverse cursor pages once, rejecting cursor loops before exposing their page.
///
/// Generated adapters return the page, next request, and next cursor from `fetch`.
pub fn cursor_pages<I, P, E, F, Fut>(
    input: I,
    initial_cursor: Option<String>,
    fetch: F,
) -> futures_util::stream::BoxStream<'static, Result<P, PaginationError<E>>>
where
    I: Send + 'static,
    P: Send + 'static,
    E: std::fmt::Debug + Send + 'static,
    F: FnMut(I) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(P, I, Option<String>), crate::client::CallError<E>>>
        + Send,
{
    use futures_util::StreamExt;
    let seen = initial_cursor
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    futures_util::stream::try_unfold(
        (input, fetch, seen, false),
        |(input, mut fetch, mut seen, done)| async move {
            if done {
                return Ok(None);
            }
            let (page, input, next) = fetch(input).await.map_err(PaginationError::Call)?;
            let done = next.is_none();
            if let Some(cursor) = next {
                if !seen.insert(cursor.clone()) {
                    return Err(PaginationError::RepeatedCursor(cursor));
                }
            }
            Ok(Some((page, (input, fetch, seen, done))))
        },
    )
    .boxed()
}

/// Flatten successful pages without dropping page errors or stopping at empty pages.
pub fn cursor_items<P, T, E, F>(
    pages: futures_util::stream::BoxStream<'static, Result<P, PaginationError<E>>>,
    mut items: F,
) -> futures_util::stream::BoxStream<'static, Result<T, PaginationError<E>>>
where
    P: Send + 'static,
    T: Send + 'static,
    E: std::fmt::Debug + Send + 'static,
    F: FnMut(P) -> Vec<T> + Send + 'static,
{
    use futures_util::{StreamExt, TryStreamExt};
    pages
        .map_ok(move |page| futures_util::stream::iter(items(page).into_iter().map(Ok)))
        .try_flatten()
        .boxed()
}

/// Immutable projection of optional surfaces installed with an authorization context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvailabilitySnapshot {
    usable: bool,
    actions: Arc<[CatalogActionProjection]>,
    capabilities: Arc<[trellis_protocol::CapabilityAuthority]>,
    state_owner: Option<String>,
    resources: Arc<crate::service::ServiceResourceBindings>,
    resource_generations: Arc<std::collections::BTreeMap<(ResourceKind, String), u64>>,
}

/// Advisory membership of an action in an authenticated accepted catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogActionProjection {
    pub(crate) api_id: String,
    pub(crate) action: trellis_protocol::CatalogActionIdentity,
}

impl AvailabilitySnapshot {
    pub(crate) fn is_usable(&self) -> bool {
        self.usable
    }

    #[cfg(test)]
    pub(crate) fn new(
        actions: Vec<CatalogActionProjection>,
        resources: crate::service::ServiceResourceBindings,
    ) -> Self {
        Self::replacing(actions, Vec::new(), None, resources, &Self::default())
    }

    pub(crate) fn replacing(
        actions: Vec<CatalogActionProjection>,
        capabilities: Vec<trellis_protocol::CapabilityAuthority>,
        state_owner: Option<String>,
        resources: crate::service::ServiceResourceBindings,
        previous: &Self,
    ) -> Self {
        let mut generations = (*previous.resource_generations).clone();
        let next = generations.values().copied().max().unwrap_or(0) + 1;
        let candidate = Self {
            usable: true,
            actions: actions.into(),
            capabilities: capabilities.into(),
            state_owner,
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
                let unchanged = previous.has_installed_resource(kind, &name)
                    && candidate.same_binding(previous, kind, &name);
                if !unchanged {
                    generations.insert((kind, name), next);
                }
            }
        }
        candidate.with_resource_generations(generations)
    }

    pub(crate) fn suspended(previous: &Self) -> Self {
        Self::with_usability(previous, false)
    }

    pub(crate) fn with_usability(previous: &Self, usable: bool) -> Self {
        Self {
            usable,
            ..previous.clone()
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
            // State is principal-owned, not a native physical resource binding.
            // Its declared name is validated by the State provider at dispatch.
            ResourceKind::State => Vec::new(),
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
            ResourceKind::State => self.state_owner == previous.state_owner,
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

    /// Test projected whole-capability membership for an API action.
    #[doc(hidden)]
    pub fn allows_action(&self, action: OptionalAction) -> bool {
        self.usable
            && self.actions.iter().any(|allowed| {
                allowed.api_id == action.api_id
                    && allowed.action.kind == action.surface
                    && allowed.action.name == action.name
                    && allowed.action.direction == action.action
            })
    }

    /// Test a whole capability whose signed identity and meaning match its accepted catalog.
    #[doc(hidden)]
    pub fn allows_capability(&self, capability_id: &str) -> bool {
        self.usable
            && self
                .capabilities
                .iter()
                .any(|capability| capability.capability_id == capability_id)
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
        self.usable && self.has_installed_resource(kind, name)
    }

    fn has_installed_resource(&self, kind: ResourceKind, name: &str) -> bool {
        match kind {
            ResourceKind::State => self.state_owner.is_some(),
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

    /// Declare an optional live subscription.
    pub const fn live(api_id: &'static str, name: &'static str) -> Self {
        Self {
            api_id,
            surface: trellis_protocol::ApiSurfaceKind::Live,
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

/// Metadata emitted for one generated live action.
pub trait LiveDescriptor {
    /// Live subscription input type.
    type Input: Codec + Serialize + DeserializeOwned;
    /// Live event payload type.
    type Event: Codec + Serialize + DeserializeOwned;

    /// Qualified API identity owning this live.
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
    /// Canonical durable identity: `<API_ID>:operation:<action name>`.
    /// Independent of generated method aliases and transport-family versions.
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
    /// Whether the upload streams directly to the handler instead of being
    /// durably staged first. Implies [`Self::UPLOAD`].
    const STREAM: bool = false;
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
    const STREAM: bool = D::STREAM;
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
///
/// Actions automatically acquire a physical generation that covers their granted
/// transport requirements. Wider authorization is adopted without an application
/// refresh call; work already in flight retains its admitted generation while it
/// remains authorized.
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
        let route = D::SUBJECT;
        // The runtime transport layer acquires an admitted generation for this
        // exact route and inbox, waiting for automatic adoption when the
        // capability is granted but not yet admitted. A route current authority
        // does not grant falls through to the ordinary broker decision.
        let input = input.encode().map_err(|error| {
            crate::client::CallError::Protocol(crate::client::ProtocolError::new(error.to_string()))
        })?;
        let output = self
            .client
            .request_json_value_routed(
                D::API_ID,
                trellis_protocol::CatalogActionIdentity {
                    kind: trellis_protocol::ApiSurfaceKind::Rpc,
                    name: action_name(D::DESCRIPTOR_NAME).to_owned(),
                    direction: trellis_protocol::PermissionAction::Call,
                },
                route,
                &input,
                crate::telemetry::instruments::route_token(
                    crate::telemetry::instruments::RouteFamily::Rpc,
                    &format!("{}:{}", D::API_ID, D::KEY),
                ),
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

    /// Subscribe to one generated live descriptor.
    pub async fn live<D>(
        &self,
        input: &D::Input,
    ) -> Result<
        crate::live::subscription::LiveSubscription<D::Event>,
        crate::client::TrellisClientError,
    >
    where
        D: LiveDescriptor,
        D::Event: Codec + Send + 'static,
    {
        self.ensure_available(OptionalAction::live(
            D::API_ID,
            action_name(D::DESCRIPTOR_NAME),
        ))?;
        self.client.live::<D>(input).await
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
    pub fn state_handle<T, D: 'static>(
        &self,
        name: &str,
        codec: crate::client::ResourceCodec<T>,
    ) -> Option<crate::client::ConnectedStateHandle<T, D>>
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

    /// Construct an app State handle after the generated Register RPC succeeds.
    /// App authority is checked by the shared connection owner on every request.
    #[doc(hidden)]
    pub fn registered_state_handle<T, D: 'static>(
        &self,
        name: &str,
        codec: crate::client::ResourceCodec<T>,
    ) -> crate::client::ConnectedStateHandle<T, D>
    where
        T: Codec + Send + 'static,
    {
        let mut handle = crate::client::StateHandle::from_generated(
            name,
            codec,
            crate::client::BoundStateResourceClient::new(self.client.clone()),
            self.client.watch_availability(),
        );
        handle.registered_app = true;
        handle
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
        if !availability.has_resource(ResourceKind::Kv, name) {
            return Ok(None);
        }
        let Some(binding) = availability.kv_binding(name).cloned() else {
            return Ok(None);
        };
        // The logical handle owns no generation: each call acquires a suitable
        // generation, so a resource granted after connect becomes usable without
        // recreating the handle or an explicit refresh.
        crate::service::open_generated_kv(
            self.client.transport_generations(),
            &self
                .client
                .participant_id()
                .map_err(|error| crate::service::ServerError::Nats(error.to_string()))?,
            name,
            binding,
            codec,
            self.client.watch_availability(),
            self.client.timeout_ms(),
        )
        .map(Some)
    }

    /// Open a generated Store resource handle from its current installation binding.
    #[doc(hidden)]
    pub async fn store_handle(
        &self,
        name: &str,
    ) -> Result<Option<crate::service::StoreHandle>, crate::service::ServerError> {
        let availability = self.client.availability();
        if !availability.has_resource(ResourceKind::Store, name) {
            return Ok(None);
        }
        let Some(binding) = availability.store_binding(name).cloned() else {
            return Ok(None);
        };
        crate::service::open_generated_store(
            self.client.transport_generations(),
            &self
                .client
                .participant_id()
                .map_err(|error| crate::service::ServerError::Nats(error.to_string()))?,
            name,
            binding,
            self.client.watch_availability(),
            self.client.timeout_ms(),
        )
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

    /// Start or replay the Operation with a caller-selected ULID.
    /// Replay deduplication is deployment-scoped. Do not automatically retry an
    /// uncertain canonical start: another deployment can accept the same ID.
    ///
    /// Persist the chosen id before this network call to retain ownership even if
    /// the acceptance reply is lost. The invocation id is the durable Operation
    /// id; recover the same Operation with [`Self::control`].
    pub async fn start_with_invocation_id(
        &self,
        invocation_id: impl Into<String>,
        input: &D::Input,
    ) -> Result<OperationRef<'a, D>, crate::client::CallError<D::Error>> {
        self.client
            .ensure_available(OptionalAction::operation(
                D::API_ID,
                action_name(D::DESCRIPTOR_NAME),
            ))
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))?;
        self.inner
            .start_with_invocation_id(invocation_id, input)
            .await
            .map(|inner| OperationRef { inner })
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))
    }

    /// Admit or replay this Operation with cancellation durably requested.
    /// Invocation deduplication applies only within the accepting deployment;
    /// canonical routing does not guarantee cross-deployment cancellation of an
    /// uncertain earlier start.
    ///
    /// Use the reserved invocation id and original input to prevent a delayed
    /// normal start from entering business work. Requires Invoke and Cancel
    /// authority. Returns after admission, not cleanup; explicitly call `wait()`
    /// on the returned reference to observe the terminal result.
    pub async fn start_cancelled_with_invocation_id(
        &self,
        invocation_id: impl Into<String>,
        input: &D::Input,
    ) -> Result<OperationRef<'a, D>, crate::client::CallError<D::Error>> {
        self.client
            .ensure_available(OptionalAction::operation(
                D::API_ID,
                action_name(D::DESCRIPTOR_NAME),
            ))
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))?;
        self.inner
            .start_cancelled_with_invocation_id(invocation_id, input)
            .await
            .map(|inner| OperationRef { inner })
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))
    }

    /// Start or replay on the authenticated deployment persisted before the first request.
    ///
    /// Persist the deployment, invocation ULID, and identical input before sending.
    /// A lost canonical reply has no known owner; choosing one afterward is not recovery.
    /// No automatic retry, fallback, or cross-deployment deduplication is performed.
    pub async fn start_on_deployment_with_invocation_id(
        &self,
        deployment_id: &str,
        invocation_id: impl Into<String>,
        input: &D::Input,
    ) -> Result<OperationRef<'a, D>, crate::client::CallError<D::Error>> {
        self.client
            .ensure_available(OptionalAction::operation(
                D::API_ID,
                action_name(D::DESCRIPTOR_NAME),
            ))
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))?;
        self.inner
            .start_on_deployment_with_invocation_id(deployment_id, invocation_id, input)
            .await
            .map(|inner| OperationRef { inner })
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))
    }

    /// Admit cancellation on the deployment persisted before the first targeted start.
    ///
    /// Requires Invoke and Cancel; retries preserve durable monotone cancellation.
    /// The returned owner reference acknowledges admission, not completed cleanup.
    pub async fn start_cancelled_on_deployment_with_invocation_id(
        &self,
        deployment_id: &str,
        invocation_id: impl Into<String>,
        input: &D::Input,
    ) -> Result<OperationRef<'a, D>, crate::client::CallError<D::Error>> {
        self.client
            .ensure_available(OptionalAction::operation(
                D::API_ID,
                action_name(D::DESCRIPTOR_NAME),
            ))
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))?;
        self.inner
            .start_cancelled_on_deployment_with_invocation_id(deployment_id, invocation_id, input)
            .await
            .map(|inner| OperationRef { inner })
            .map_err(|error| crate::client::CallError::from_client(error, D::decode_error))
    }

    /// Restore typed control from a complete serialized Operation reference.
    /// Its deployment routes the durable workflow; receivers still authorize controls.
    pub fn control(
        &self,
        reference: crate::client::OperationRefData,
    ) -> Result<OperationRef<'a, D>, crate::client::TrellisClientError> {
        self.inner
            .control(reference)
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

    /// Complete durable reference for serialization and later owner-directed control.
    /// Restoring it does not grant authority or start another execution.
    pub fn reference(&self) -> &crate::client::OperationRefData {
        self.inner.reference()
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

    /// Observe durable lifecycle snapshots from the current runtime.
    pub async fn live(
        &self,
    ) -> Result<
        crate::live::subscription::LiveSubscription<
            crate::client::OperationEvent<D::Progress, D::Output, serde_json::Value>,
        >,
        crate::client::TrellisClientError,
    > {
        self.inner.live().await
    }

    /// Observe durable lifecycle snapshots plus declared live-only updates.
    pub async fn live_with_updates(
        &self,
    ) -> Result<
        crate::live::subscription::LiveSubscription<
            crate::client::OperationEvent<D::Progress, D::Output, D::Update>,
        >,
        crate::client::TrellisClientError,
    >
    where
        D::UpdateEvidence: crate::client::HasOperationUpdates,
    {
        self.inner.live_with_updates().await
    }

    /// Subscribe to declared live-only updates until the operation is terminal.
    pub async fn updates(
        &self,
    ) -> Result<
        impl futures_util::Stream<
                Item = Result<
                    crate::client::OperationUpdateEvent<D::Update>,
                    crate::client::TrellisClientError,
                >,
            > + Unpin,
        crate::client::TrellisClientError,
    >
    where
        D::UpdateEvidence: crate::client::HasOperationUpdates,
    {
        self.inner.updates().await
    }

    /// Upload the declared Operation transfer body through the accepted grant.
    pub async fn upload(
        &self,
        body: impl AsRef<[u8]>,
    ) -> Result<crate::client::FileInfo, crate::client::TrellisClientError> {
        self.upload_from(&mut body.as_ref()).await
    }

    /// Stream the declared Operation upload without collecting the body in memory.
    /// Success requires authenticated Transfer commit, not just source exhaustion.
    pub async fn upload_from<R>(
        &self,
        reader: &mut R,
    ) -> Result<crate::client::FileInfo, crate::client::TrellisClientError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + ?Sized,
    {
        if !D::UPLOAD {
            return Err(crate::client::TrellisClientError::OperationProtocol(
                "operation does not declare an upload transfer".to_owned(),
            ));
        }
        self.inner.transfer_from(reader).await
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

/// Kind of deployable principal descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParticipantKind {
    /// Long-running service participant.
    Service,
    /// Provisioned physical or embedded device participant.
    Device,
}

/// Public-client authorization flow requested by an application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppKind {
    /// Browser application using an origin-bound client.
    Browser,
    /// Native application using a durable DPoP key.
    Native,
}

/// Generated application selection metadata, never installation evidence.
pub trait AppRequestDescriptor {
    /// Source-local request identity, not an OAuth client identifier.
    const ID: &'static str;
    /// Public-client flow kind.
    const KIND: AppKind;
    /// Whole capabilities necessary for the application workflow.
    const REQUIRED_CAPABILITIES: &'static [&'static str];
    /// Whole capabilities requested only when authorized and approved.
    const OPTIONAL_CAPABILITIES: &'static [&'static str];
}

/// Lexical application request associated with a device, not another principal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompanionDescriptor {
    /// Fully qualified source-local application request identity.
    pub request_id: &'static str,
    /// Public-client flow kind.
    pub kind: AppKind,
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

    /// Lexical application request, when declared by this device.
    const COMPANION: Option<CompanionDescriptor> = None;

    /// APIs completely implemented by this participant.
    const IMPLEMENTED_API_IDS: &'static [&'static str] = &[];

    /// APIs referenced by this participant's required or optional consumption selections.
    const CONSUMED_API_IDS: &'static [&'static str] = &[];

    /// `event:<Name>` descriptor names this participant explicitly declares as
    /// subscribe needs. A declared durable consumer is not included: consumer
    /// `Consume` and event `Subscribe` are independent authorities.
    const EVENT_SUBSCRIBE_NEEDS: &'static [&'static str] = &[];

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
                || companion
                    .request_id
                    .rsplit_once('.')
                    .map(|(parent, _)| parent)
                    != Some(Self::ID)
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

#[cfg(test)]
mod tests {
    use std::fmt;

    use serde::{Deserialize, Serialize};

    #[tokio::test]
    async fn cursor_traversal_continues_through_empty_pages_and_stops_at_terminal_page() {
        use futures_util::TryStreamExt;
        let pages = super::cursor_pages(0, None, |step| async move {
            let (items, next) = match step {
                0 => (vec![], Some("first".into())),
                1 => (vec![1, 2], Some("second".into())),
                2 => (vec![3], None),
                _ => panic!("fetched beyond terminal page"),
            };
            Ok::<_, crate::client::CallError<std::convert::Infallible>>((items, step + 1, next))
        });
        assert_eq!(
            super::cursor_items(pages, |items| items)
                .try_collect::<Vec<_>>()
                .await
                .unwrap(),
            [1, 2, 3]
        );
    }

    #[tokio::test]
    async fn cursor_traversal_rejects_repeated_initial_cursor_and_preserves_fetch_errors() {
        use futures_util::StreamExt;
        let mut pages = super::cursor_pages((), Some("initial".into()), |()| async {
            Ok::<_, crate::client::CallError<std::convert::Infallible>>((
                vec![99],
                (),
                Some("initial".into()),
            ))
        });
        assert!(
            matches!(pages.next().await, Some(Err(super::PaginationError::RepeatedCursor(cursor))) if cursor == "initial")
        );
        assert!(pages.next().await.is_none());

        let pages = super::cursor_pages((), None, |()| async {
            Err::<(Vec<u8>, (), Option<String>), crate::client::CallError<std::convert::Infallible>>(
                crate::client::CallError::Timeout,
            )
        });
        let mut items = super::cursor_items(pages, |items| items);
        assert!(matches!(
            items.next().await,
            Some(Err(super::PaginationError::Call(
                crate::client::CallError::Timeout
            )))
        ));
        assert!(items.next().await.is_none());
    }

    use super::{Codec as _, Timestamp, TrellisError, Ulid};

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

    #[test]
    fn typed_error_decoder_rejects_foreign_discriminators() {
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
}
