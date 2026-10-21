use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;

use serde_json::Value;
use trellis_protocol::{
    ApiSurfaceKind, PermissionAction, PermissionAtom, PermissionTarget, ProtocolError,
};

use super::error::ValidationIssue;
use super::operations::ServiceOperationProvider;
use super::request_loop::{HandlerResponse, ResponseStream};
use super::schema_validation::{OperationSchemas, PreparedOperationSchemas, PreparedSchema};
use super::{
    control_subject, HandlerResult, LiveDescriptor, OperationControlRequest, OperationDescriptor,
    OperationLiveEvent, OperationLiveWatch, OperationSignalAccepted, OperationSnapshot,
    OperationSnapshotFrame, RpcDescriptor, ServerError,
};

/// Low-level context for one verified live Live invocation.
///
/// Supplied only after the router verified the opening request and reserved the
/// session. The cancellation token is cloneable and exposes no authority
/// constructor to applications.
#[derive(Clone)]
pub struct LiveRequestContext {
    /// Request metadata, unchanged from ordinary handlers.
    pub request: RequestContext,
    /// Cancellation for this source scope.
    pub cancellation: crate::live::LiveCancellation,
}

/// The transport generation that accepted one request, when it arrived through a
/// generation-scoped provider ingress.
///
/// Equality is by generation identity so [`RequestContext`] stays comparable;
/// the lease pins the generation for session-scoped work accepted on it.
#[derive(Clone, Default)]
pub(crate) struct GenerationPin(pub(crate) Option<crate::client::TransportLease>);

impl std::fmt::Debug for GenerationPin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("GenerationPin")
            .field(
                &self
                    .lease()
                    .map(crate::client::TransportLease::generation_id),
            )
            .finish()
    }
}

impl GenerationPin {
    /// The pinned generation lease, when the request arrived through an ingress.
    pub(crate) fn lease(&self) -> Option<&crate::client::TransportLease> {
        self.0.as_ref()
    }
}

impl PartialEq for GenerationPin {
    fn eq(&self, other: &Self) -> bool {
        self.lease()
            .map(crate::client::TransportLease::generation_id)
            == other
                .lease()
                .map(crate::client::TransportLease::generation_id)
    }
}

impl Eq for GenerationPin {}

/// Pin the generation that accepted one live-capable request, or the current
/// generation when the request did not arrive through a generation-scoped
/// ingress (for example a built-in runtime router).
async fn accepted_generation(
    owner: &super::live_router::LiveProviderOwner,
    context: &RequestContext,
) -> Result<crate::client::TransportLease, ServerError> {
    if let Some(lease) = context.transport.lease() {
        return Ok(lease.clone());
    }
    owner
        .client()
        .acquire_transport(&[], &[], owner.client().transport_deadline())
        .await
        .map_err(|error| ServerError::Nats(error.to_string()))
}

/// Request metadata forwarded to mounted RPC handlers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestContext {
    /// Presented request transcript, untrusted until `verified_request` is available.
    pub session_request: Option<trellis_protocol::SessionRequest>,
    /// Shared verified opening retained across dispatch and reply publication.
    pub(crate) verified_opening: VerifiedOpening,
    /// Whether this handler invocation reclaimed previously accepted work.
    pub resuming: bool,
    /// Last durable operation progress supplied to a resumed handler.
    pub operation_progress: Option<serde_json::Value>,
    /// NATS subject that received the request.
    pub subject: String,
    /// Runtime session key from the authenticated request headers.
    pub session_key: Option<String>,
    /// Proof signature from the authenticated request headers.
    pub proof: Option<String>,
    /// Authorization-context digest bound by the v1 request proof.
    pub authorization_context: Option<String>,
    /// Proof issued-at timestamp from the authenticated request headers.
    pub iat: Option<i64>,
    /// Unique request id from the authenticated request headers.
    pub request_id: Option<String>,
    /// Capability requirements for this exact routed request.
    pub required_capabilities: Option<Vec<String>>,
    /// Exact permission surface required for this exact routed request.
    pub required_permission: Option<RoutePermission>,
    /// Set only from registered route metadata, never from a wire assertion.
    pub(crate) handler_authorizes: bool,
    /// NATS reply inbox used for request/reply responses.
    pub reply_to: Option<String>,
    /// Locally verified caller projection, when the request was authorized.
    pub caller: Option<super::local_validator::AuthenticatedCaller>,
    /// W3C trace context header propagated by the caller, if present.
    pub traceparent: Option<String>,
    /// W3C trace state header propagated by the caller, if present.
    pub tracestate: Option<String>,
    /// The generation that accepted this request through a provider ingress.
    /// Session-scoped work accepted on it pins that generation.
    pub(crate) transport: GenerationPin,
}

/// Retains one verified opening across immutable validator and dispatcher contexts.
#[derive(Clone, Debug, Default)]
pub(crate) struct VerifiedOpening(
    Arc<std::sync::RwLock<Option<crate::client::VerifiedAuthorizationRequest>>>,
);

impl PartialEq for VerifiedOpening {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || self.0.read().ok().and_then(|value| {
                value
                    .as_ref()
                    .map(|request| (*request.digest(), request.authorized_action().cloned()))
            }) == other.0.read().ok().and_then(|value| {
                value
                    .as_ref()
                    .map(|request| (*request.digest(), request.authorized_action().cloned()))
            })
    }
}
impl Eq for VerifiedOpening {}

impl RequestContext {
    /// Admit this exact opening after the handler has checked its payload-bound resource rights.
    /// Only handler-authorized routes may call this, after—not before—their authorization check.
    /// Possession alone never permits response signing or a Live reservation.
    pub fn authorize_handler_request(&self) -> Result<(), ServerError> {
        let route = self
            .required_permission
            .as_ref()
            .filter(|_| self.handler_authorizes)
            .ok_or_else(|| ServerError::Nats("request is not a handler-authorized route".into()))?;
        let mut slot = self
            .verified_opening
            .0
            .write()
            .map_err(|_| ServerError::Nats("verified request unavailable".into()))?;
        let verified = slot
            .as_mut()
            .ok_or_else(|| ServerError::Nats("verified opening unavailable".into()))?;
        if verified.request().api_id != route.api {
            return Err(ServerError::Nats("handler route API mismatch".into()));
        }
        verified
            .authorize_handler(route.catalog_action())
            .map_err(|error| ServerError::Nats(error.to_string()))
    }
    /// Return the authenticated opening, never the caller's unverified header assertions.
    pub fn verified_request(&self) -> Option<crate::client::VerifiedAuthorizationRequest> {
        self.verified_opening.0.read().ok()?.as_ref().cloned()
    }

    /// Retain a production-verified opening for downstream controls and response signing.
    /// Additional permission checks on the same request preserve its original reply action.
    pub fn retain_verified_request(
        &self,
        request: crate::client::VerifiedAuthorizationRequest,
    ) -> Result<(), ServerError> {
        let mut slot = self
            .verified_opening
            .0
            .write()
            .map_err(|_| ServerError::Nats("verified request unavailable".into()))?;
        if slot
            .as_ref()
            .is_some_and(|current| current.digest() != request.digest())
        {
            return Err(ServerError::Nats(
                "verified request already installed".into(),
            ));
        }
        if slot.is_none() {
            *slot = Some(request);
        }
        Ok(())
    }
}

/// One exact API-surface permission required by a routed request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(RoutePermission), "`.")]
pub struct RoutePermission {
    /// Versioned generated API identity that owns the surface.
    pub api: String,
    #[doc = concat!("The `", stringify!(surface), "` value.")]
    pub surface: ApiSurfaceKind,
    #[doc = concat!("The `", stringify!(name), "` value.")]
    pub name: String,
    #[doc = concat!("The `", stringify!(action), "` value.")]
    pub action: PermissionAction,
    /// Operation signal name for a `Control` permission.
    pub signal: Option<String>,
}

impl RoutePermission {
    /// Exact generated catalog action; capabilities authorize the operation direction.
    pub fn catalog_action(&self) -> trellis_protocol::CatalogActionIdentity {
        trellis_protocol::CatalogActionIdentity {
            kind: self.surface,
            name: self.name.clone(),
            direction: self.action,
        }
    }
    /// Convert generated route metadata into its exact permission atom.
    pub fn permission_atom(&self) -> Result<PermissionAtom, ProtocolError> {
        let target = if self.action == PermissionAction::Control {
            PermissionTarget::operation_signal(
                self.api.clone(),
                self.name.clone(),
                self.signal
                    .clone()
                    .ok_or(ProtocolError::InvalidIdentifier {
                        field: "signal name",
                        reason: "control permission is missing its signal name",
                    })?,
            )?
        } else {
            PermissionTarget::api_surface(self.api.clone(), self.surface, self.name.clone())?
        };
        PermissionAtom::new(target, self.action)
    }
}

type BoxedHandler = Box<
    dyn Fn(RequestContext, Bytes) -> BoxFuture<'static, Result<HandlerResponse, ServerError>>
        + Send
        + Sync,
>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationStartEnvelope {
    invocation_id: String,
    input: Value,
    #[serde(default)]
    cancellation_requested: bool,
}

struct Route {
    handler: BoxedHandler,
    capabilities: RouteCapabilities,
    permission: RoutePermissionSpec,
    /// Bounded registered route token for metric labels.
    route_token: &'static str,
    /// Whether this registered surface is a unary request/reply RPC.
    unary_rpc: bool,
    /// Whether this route is a live observation opening or lifecycle control.
    live: bool,
}

/// Exact permission surface recorded at registration time for one route.
#[derive(Debug, Clone)]
enum RoutePermissionSpec {
    /// The handler performs exact payload-bound authorization after proof verification.
    Handler(String, String),
    /// One fixed surface for every request on the route.
    Static(String, ApiSurfaceKind, String, PermissionAction),
    /// Operation control routes resolve the action from the payload.
    OperationControl(String, String),
}

impl RoutePermissionSpec {
    fn for_payload(&self, payload: &[u8]) -> Option<RoutePermission> {
        match self {
            Self::Handler(api, name) => Some(RoutePermission {
                api: api.clone(),
                surface: ApiSurfaceKind::Rpc,
                name: name.clone(),
                action: PermissionAction::Call,
                signal: None,
            }),
            Self::Static(api, surface, name, action) => Some(RoutePermission {
                api: api.clone(),
                surface: *surface,
                name: name.clone(),
                action: *action,
                signal: None,
            }),
            Self::OperationControl(api, name) => {
                let request = serde_json::from_slice::<OperationControlRequest>(payload).ok()?;
                let action = match request.action.as_str() {
                    "get" | "wait" | "watch" => PermissionAction::Observe,
                    "cancel" => PermissionAction::Cancel,
                    "signal" => PermissionAction::Control,
                    _ => return None,
                };
                Some(RoutePermission {
                    api: api.clone(),
                    surface: ApiSurfaceKind::Operation,
                    name: name.clone(),
                    action,
                    signal: if action == PermissionAction::Control {
                        Some(request.signal?)
                    } else {
                        None
                    },
                })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum RouteCapabilities {
    Static(Vec<String>),
    OperationControl {
        observe: Vec<String>,
        cancel: Vec<String>,
        control: Vec<String>,
    },
}

impl RouteCapabilities {
    fn required_for_payload(&self, payload: &[u8]) -> Option<Vec<String>> {
        let capabilities = match self {
            Self::Static(capabilities) => capabilities,
            Self::OperationControl {
                observe,
                cancel,
                control,
            } => match serde_json::from_slice::<OperationControlRequest>(payload) {
                Ok(request) => match request.action.as_str() {
                    "get" | "wait" | "watch" => observe,
                    "cancel" => cancel,
                    "signal" => control,
                    _ => return Some(Vec::new()),
                },
                Err(_) => return Some(Vec::new()),
            },
        };

        Some(capabilities.to_vec())
    }
}

/// An in-memory subject router for descriptor-backed RPC handlers.
#[derive(Default)]
pub struct Router {
    handlers: HashMap<String, Arc<Route>>,
    provider_deployment_id: Option<String>,
    provider_instance_id: Option<String>,
    operation_recoveries: Vec<OperationRecovery>,
    /// Live owner for Live/Operation observation routes on this connection.
    ///
    /// Shared so a route registered before its owner is installed still
    /// resolves the owner at dispatch time.
    live_owner: Arc<std::sync::RwLock<Option<super::live_router::LiveProviderOwner>>>,
}

type OperationRecovery = Box<dyn Fn() -> BoxFuture<'static, Result<(), ServerError>> + Send + Sync>;

impl Router {
    fn route(&self, subject: &str) -> Option<&Route> {
        self.handlers
            .get(subject)
            .or_else(|| {
                let (prefix, _) = subject.rsplit_once('.')?;
                self.handlers.get(&format!("{prefix}.*"))
            })
            .map(Arc::as_ref)
    }

    /// Create an empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Exact registered entry and owner-control subjects for provider intake.
    pub fn subjects(&self) -> Vec<String> {
        let mut subjects: Vec<_> = self.handlers.keys().cloned().collect();
        subjects.sort();
        subjects
    }

    /// Bind Operation routes to their owning deployment before registering handlers.
    pub fn set_provider_deployment_id(&mut self, deployment_id: impl Into<String>) {
        self.provider_deployment_id = Some(deployment_id.into());
    }

    /// Bind Live control routes to this service instance.
    pub fn set_provider_instance_id(&mut self, instance_id: impl Into<String>) {
        self.provider_instance_id = Some(instance_id.into());
    }

    /// Bind Operation starts and controls to the installation's verified provider owner.
    pub fn bind_provider_owner(
        &mut self,
        owner: &trellis_protocol::VerifiedSessionAuthority,
    ) -> Result<(), ServerError> {
        let trellis_protocol::SessionBinding::Service {
            deployment_id,
            instance_id,
            ..
        } = &owner.authority().binding
        else {
            return Err(ServerError::Nats(
                "provider requires a verified service binding".into(),
            ));
        };
        let previous_deployment = self.provider_deployment_id.replace(deployment_id.clone());
        self.provider_instance_id = Some(instance_id.clone());
        let mut controls = Vec::new();
        let mut starts = Vec::new();
        let mut previous_starts = Vec::new();
        for (subject, route) in &self.handlers {
            if let RoutePermissionSpec::OperationControl(api, name) = &route.permission {
                let base =
                    trellis_protocol::derive_bound_operation_subject(api, deployment_id, name)
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                controls.push((subject.clone(), control_subject(&base)));
            }
            if let RoutePermissionSpec::Static(
                api,
                ApiSurfaceKind::Operation,
                name,
                PermissionAction::Invoke,
            ) = &route.permission
            {
                let bound =
                    trellis_protocol::derive_bound_operation_subject(api, deployment_id, name)
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                if let Some(previous) = &previous_deployment {
                    let previous =
                        trellis_protocol::derive_bound_operation_subject(api, previous, name)
                            .map_err(|error| ServerError::Nats(error.to_string()))?;
                    if subject == &previous {
                        previous_starts.push(subject.clone());
                        continue;
                    }
                }
                starts.push((bound, Arc::clone(route)));
            }
        }
        for previous in previous_starts {
            self.handlers.remove(&previous);
        }
        self.handlers.extend(starts);
        for (old, new) in controls {
            if let Some(route) = self.handlers.remove(&old) {
                self.handlers.insert(new, route);
            }
        }
        Ok(())
    }

    /// Bind live Live/Operation observation routes to one connected owner.
    pub fn set_live_owner(&mut self, owner: super::live_router::LiveProviderOwner) {
        if let Ok(mut live_owner) = self.live_owner.write() {
            *live_owner = Some(owner);
        }
    }

    /// Return whether this router contains any live Live or Operation route.
    #[must_use]
    pub fn serves_live_surface(&self) -> bool {
        self.handlers.values().any(|route| route.live)
    }

    /// Return whether a live provider owner is installed.
    #[must_use]
    pub fn has_live_owner(&self) -> bool {
        self.live_owner
            .read()
            .map(|owner| owner.is_some())
            .unwrap_or(false)
    }

    /// Fail when a live route is registered without a live provider owner.
    ///
    /// This is a construction/startup invariant: a router that serves a Live
    /// or Operation watch route must be given its connection's provider owner
    /// before it serves traffic. Missing ownership is an immediate
    /// configuration failure, never a dispatch-time fallback.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Nats`] when this router serves a live surface
    /// without an installed live provider owner.
    pub fn require_live_owner(&self) -> Result<(), ServerError> {
        if self.serves_live_surface() && !self.has_live_owner() {
            return Err(ServerError::Nats(
                "a router serving a Live or Operation watch route requires a live provider owner"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn operation_owner_subject(&self, api_id: &str, action: &str, canonical: &str) -> String {
        self.provider_deployment_id.as_deref().map_or_else(
            || canonical.to_owned(),
            |deployment| {
                trellis_protocol::derive_bound_operation_subject(
                    api_id,
                    deployment,
                    action
                        .split_once(":operation:")
                        .expect("generated canonical Operation identity")
                        .1,
                )
                .expect("generated operation metadata must form a valid owner subject")
            },
        )
    }

    fn descriptor_capabilities(&self, capabilities: &[&str]) -> Vec<String> {
        capabilities
            .iter()
            .map(|capability| (*capability).to_string())
            .collect()
    }

    /// Interns one bounded route token at registration time.
    ///
    /// Live surfaces share the request-route family: their open is a
    /// request/response RPC at the shared dispatch boundary, while the live's
    /// own lifetime is observed by the live instruments, not this token.
    fn intern_route(&self, api: &str, key: &str) -> &'static str {
        crate::telemetry::instruments::route_token(
            crate::telemetry::instruments::RouteFamily::Rpc,
            &format!("{api}:{key}"),
        )
    }

    /// Registered route token for one inbound request subject.
    pub(crate) fn route_token(&self, subject: &str) -> Option<&'static str> {
        self.route(subject).map(|route| route.route_token)
    }

    /// Whether one registered subject is a unary request/reply RPC surface.
    pub(crate) fn is_unary_rpc_route(&self, subject: &str) -> bool {
        self.route(subject).is_some_and(|route| route.unary_rpc)
    }

    /// Whether one registered subject is a live observation opening or
    /// lifecycle-control route.
    pub(crate) fn is_live_route(&self, subject: &str) -> bool {
        self.route(subject).is_some_and(|route| route.live)
    }

    fn descriptor_name(&self, name: &str) -> String {
        name.split_once('.')
            .map_or(name, |(_, action)| action)
            .to_owned()
    }

    /// Register one descriptor-backed handler.
    pub fn register_rpc<D, F, Fut>(&mut self, handler: F)
    where
        D: RpcDescriptor + 'static,
        F: Fn(RequestContext, D::Input) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult<D::Output>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let capabilities = self.descriptor_capabilities(D::CALLER_CAPABILITIES);
        self.handlers.insert(D::SUBJECT.to_owned(), Arc::new(Route { capabilities: RouteCapabilities::Static(capabilities),
        permission: RoutePermissionSpec::Static(
            D::API_ID.to_owned(),
            ApiSurfaceKind::Rpc,
            self.descriptor_name(D::KEY),
            PermissionAction::Call,
        ),
        route_token: self.intern_route(D::API_ID, D::KEY),
        unary_rpc: true,
        live: false,
        handler: Box::new(
        move |ctx, payload| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let input = decode_generated_input::<D::Input>(&payload)?;
                let output = handler(ctx, input).await?;
                let output = crate::generated::Codec::encode(&output)
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                Ok(HandlerResponse::Frames(vec![Bytes::from(serde_json::to_vec(
                    &output,
                )?)]))
            })
        },
                    ), }));
    }

    /// Mark a registered RPC as performing exact payload-bound authorization in its handler.
    ///
    /// This must only be used when one request can authorize through alternatives that cannot be
    /// represented by a single static API permission, such as owner Resource authority or an
    /// explicit API-call-plus-Admin fallback.
    pub fn rpc_handler_authorizes<D>(&mut self)
    where
        D: RpcDescriptor + 'static,
    {
        let subject = D::SUBJECT.to_owned();
        let route = self
            .handlers
            .get_mut(&subject)
            .expect("RPC must be registered before changing its authorization mode");
        Arc::get_mut(route)
            .expect("RPC routes have one registration owner")
            .permission = RoutePermissionSpec::Handler(
            D::API_ID.to_owned(),
            D::KEY
                .split_once('.')
                .map_or(D::KEY, |(_, action)| action)
                .to_owned(),
        );
    }

    /// Register one generated RPC descriptor for routing metadata only.
    ///
    /// This is used by runtimes whose handler dispatch predates the typed
    /// router but still must consume the router's exact permission metadata.
    pub fn register_rpc_metadata<D>(&mut self)
    where
        D: RpcDescriptor + 'static,
    {
        let capabilities = self.descriptor_capabilities(D::CALLER_CAPABILITIES);
        self.handlers.insert(
            D::SUBJECT.to_owned(),
            Arc::new(Route {
                capabilities: RouteCapabilities::Static(capabilities),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Rpc,
                    self.descriptor_name(D::KEY),
                    PermissionAction::Call,
                ),
                route_token: self.intern_route(D::API_ID, D::KEY),
                unary_rpc: true,
                live: false,
                handler: Box::new(|_, _| {
                    Box::pin(async {
                        Err(ServerError::Nats(
                            "routing-metadata-only handler cannot execute".to_owned(),
                        ))
                    })
                }),
            }),
        );
    }

    /// Register one generated operation descriptor for routing metadata only.
    pub fn register_operation_metadata<D>(&mut self)
    where
        D: OperationDescriptor + 'static,
    {
        let subject = D::SUBJECT.to_owned();
        let name = D::KEY
            .split_once(":operation:")
            .expect("generated canonical Operation identity")
            .1
            .to_owned();
        let metadata_handler = || {
            Box::new(
                |_, _| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
                    Box::pin(async {
                        Err(ServerError::Nats(
                            "routing-metadata-only handler cannot execute".to_owned(),
                        ))
                    })
                },
            ) as BoxedHandler
        };
        let operation_token = self.intern_route(D::API_ID, &name);
        self.handlers.insert(
            subject.clone(),
            Arc::new(Route {
                capabilities: RouteCapabilities::Static(
                    self.descriptor_capabilities(D::CALLER_CAPABILITIES),
                ),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Operation,
                    name.clone(),
                    PermissionAction::Invoke,
                ),
                route_token: operation_token,
                unary_rpc: false,
                live: true,
                handler: metadata_handler(),
            }),
        );
        if self.provider_deployment_id.is_some() {
            let bound = self.operation_owner_subject(D::API_ID, D::KEY, D::SUBJECT);
            self.handlers
                .insert(bound, Arc::clone(&self.handlers[D::SUBJECT]));
        }
        self.handlers.insert(
            control_subject(&self.operation_owner_subject(D::API_ID, D::KEY, D::SUBJECT)),
            Arc::new(Route {
                capabilities: RouteCapabilities::OperationControl {
                    observe: self.descriptor_capabilities(D::OBSERVE_CAPABILITIES),
                    cancel: self.descriptor_capabilities(D::CANCEL_CAPABILITIES),
                    control: self.descriptor_capabilities(D::CONTROL_CAPABILITIES),
                },
                permission: RoutePermissionSpec::OperationControl(D::API_ID.to_owned(), name),
                route_token: operation_token,
                unary_rpc: false,
                live: true,
                handler: metadata_handler(),
            }),
        );
    }

    /// Register one descriptor-backed live Live handler.
    ///
    /// The opening request is verified and the session reserved before the
    /// handler runs; handler output starts only after the delivery-path
    /// challenge is answered. The response is one signed offer, never an
    /// infinite reply loop.
    pub fn register_live<D, F, S>(&mut self, handler: F)
    where
        D: LiveDescriptor + 'static,
        D::Input: Send + 'static,
        F: Fn(LiveRequestContext, D::Input) -> S + Send + Sync + 'static,
        S: Stream<Item = Result<D::Event, ServerError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let live_owner = Arc::clone(&self.live_owner);
        let subject = D::SUBJECT.to_owned();
        let capabilities = self.descriptor_capabilities(D::SUBSCRIBE_CAPABILITIES);
        let handler_subject = subject.clone();
        self.handlers.insert(subject, Arc::new(Route { capabilities: RouteCapabilities::Static(capabilities),
        permission: RoutePermissionSpec::Static(
            D::API_ID.to_owned(),
            ApiSurfaceKind::Live,
            self.descriptor_name(D::KEY),
            PermissionAction::Subscribe,
        ),
        route_token: self.intern_route(D::API_ID, D::KEY),
        unary_rpc: false,
        live: true,
        handler: Box::new(
            move |ctx, payload| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
                let handler = Arc::clone(&handler);
                let live_owner = live_owner.clone();
                let handler_subject = handler_subject.clone();
                Box::pin(async move {
                    let owner = live_owner
                        .read()
                        .ok()
                        .and_then(|owner| owner.clone())
                        .ok_or_else(|| {
                            ServerError::Nats(format!(
                                "live route '{handler_subject}' has no live provider owner"
                            ))
                        })?;
                    let authority = owner.client().authorization_owner().own_authority().map_err(|error| ServerError::Nats(error.to_string()))?;
                    let trellis_protocol::SessionBinding::Service { .. } = &authority.authority().binding else {
                        return Err(ServerError::Nats("live provider requires a service binding".into()));
                    };
                    let opening = crate::service::live_router::parse_live_open::<D::Input>(
                        &payload,
                    )?;
                    let encoded_input = crate::generated::Codec::encode(&opening.input)
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                    let input = opening.input;
                    let meta = crate::service::live_router::LiveOpeningMeta {
                        open_id: opening.open_id.clone(),
                        receive_max_payload_bytes: opening.receive_max_payload_bytes,
                    };
                    let cancellation = crate::live::LiveCancellation::new();
                    let factory_cancellation = cancellation.clone();
                    let source_factory = {
                        let handler = Arc::clone(&handler);
                        let request = ctx.clone();
                        move || {
                            let stream = handler(
                                LiveRequestContext {
                                    request,
                                    cancellation: factory_cancellation.clone(),
                                },
                                input,
                            );
                            crate::service::live_router::source_from_handler::<D::Event, S>(
                                stream,
                            )
                        }
                    };
                    let lease = accepted_generation(&owner, &ctx).await?;
                    let reserved = crate::service::live_router::reserve_live::<D, _>(
                        owner.client(),
                        lease,
                        owner.manager()?,
                        &crate::service::live_router::LiveOpenRequest {
                            request: ctx,
                            inputs: crate::service::live_router::LiveOpenInputs {
                                base_subject: handler_subject,
                            },
                            opening: meta,
                            encoded_input,
                            cancellation,
                        },
                        source_factory,
                    )
                    .await?;
                    Ok(HandlerResponse::LivePrepared(Box::new(reserved.prepared)))
                })
            },
        ), }));
    }

    fn register_operation_routes<D, P>(&mut self, provider: Arc<P>)
    where
        D: OperationDescriptor + 'static,
        P: ServiceOperationProvider<D>,
    {
        let start = Arc::clone(&provider);
        let get = Arc::clone(&provider);
        let watch = Arc::clone(&provider);
        let cancel = Arc::clone(&provider);
        let signal = Arc::clone(&provider);
        let schemas = Arc::new(PreparedOperationSchemas::new::<D>());
        let start_schemas = Arc::clone(&schemas);
        let control_schemas = Arc::clone(&schemas);
        self.operation_recoveries.push(Box::new(move || {
            let provider = Arc::clone(&provider);
            let schemas = Arc::clone(&schemas);
            Box::pin(async move {
                schemas.get()?;
                provider.recover().await
            })
        }));
        let subject = D::SUBJECT.to_owned();
        let live_owner = Arc::clone(&self.live_owner);
        let caller_capabilities = self.descriptor_capabilities(D::CALLER_CAPABILITIES);
        let observe_capabilities = self.descriptor_capabilities(D::OBSERVE_CAPABILITIES);
        let cancel_capabilities = self.descriptor_capabilities(D::CANCEL_CAPABILITIES);
        let control_capabilities = self.descriptor_capabilities(D::CONTROL_CAPABILITIES);

        self.handlers.insert(subject.clone(), Arc::new(Route { capabilities: RouteCapabilities::Static(caller_capabilities),
        permission: RoutePermissionSpec::Static(
            D::API_ID.to_owned(),
            ApiSurfaceKind::Operation,
            D::KEY.split_once(":operation:").expect("generated canonical Operation identity").1.to_owned(),
            PermissionAction::Invoke,
        ),
        route_token: self.intern_route(D::API_ID, D::KEY),
        unary_rpc: false,
        live: false,
        handler: Box::new(
        move |ctx, payload| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
            let start = Arc::clone(&start);
            let schemas = Arc::clone(&start_schemas);
            Box::pin(async move {
                let schemas = schemas.get()?;
                let envelope: OperationStartEnvelope = serde_json::from_slice(&payload)?;
                envelope.invocation_id.parse::<ulid::Ulid>().map_err(|_| {
                    ServerError::Nats("operation invocation id must be a ULID".to_owned())
                })?;
                schemas.input.validate(&envelope.input)?;
                let input = serde_json::from_value(envelope.input)?;
                let output = start
                    .start_invocation(
                        ctx,
                        envelope.invocation_id,
                        input,
                        envelope.cancellation_requested.then_some(payload),
                    )
                    .await?;
                validate_operation_snapshot::<D>(&output.snapshot, schemas)?;
                Ok(HandlerResponse::Frames(vec![Bytes::from(
                    serde_json::to_vec(&output)?,
                )]))
            })
        },
                    ), }));

        if self.provider_deployment_id.is_some() {
            let bound = self.operation_owner_subject(D::API_ID, D::KEY, D::SUBJECT);
            self.handlers
                .insert(bound, Arc::clone(&self.handlers[D::SUBJECT]));
        }
        self.handlers.insert(control_subject(&self.operation_owner_subject(D::API_ID, D::KEY, D::SUBJECT)), Arc::new(Route { capabilities: RouteCapabilities::OperationControl {
            observe: observe_capabilities,
            cancel: cancel_capabilities,
            control: control_capabilities,
        },
        permission: RoutePermissionSpec::OperationControl(
            D::API_ID.to_owned(),
            D::KEY.split_once(":operation:").expect("generated canonical Operation identity").1.to_owned(),
        ),
        route_token: self.intern_route(D::API_ID, D::KEY),
        unary_rpc: false,
        live: true,
        handler: Box::new(
        move |ctx, payload| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
            let get = Arc::clone(&get);
            let watch = Arc::clone(&watch);
            let cancel = Arc::clone(&cancel);
            let signal = Arc::clone(&signal);
            let schemas = Arc::clone(&control_schemas);
            let live_owner = live_owner.clone();
            let subject = ctx.subject.strip_suffix(".control").unwrap_or(&ctx.subject).to_owned();
            Box::pin(async move {
                let schemas = schemas.get()?;
                let request = serde_json::from_slice::<OperationControlRequest>(&payload)
                    .map_err(ServerError::Json)?;
                tracing::debug!(
                    subject = %subject,
                    action = %request.action,
                    operation_id = %request.operation_id,
                    "operation control request"
                );
                match request.action.as_str() {
                    "get" => Ok(HandlerResponse::Frames(vec![snapshot_frame::<D>(
                        get.get(ctx, request.operation_id).await?,
                        schemas,
                    )?])),
                    "watch" => {
                        let include_updates = request.include_updates.unwrap_or(false);
                        if include_updates && schemas.update.is_none() {
                            return Err(ServerError::InvalidOperationControlAction {
                                subject: subject.clone(),
                                action: "watch:updates".to_string(),
                            });
                        }
                        let owner = live_owner
                            .read()
                            .ok()
                            .and_then(|owner| owner.clone())
                            .ok_or_else(|| {
                                ServerError::Nats(format!(
                                    "operation route '{subject}' has no live provider owner"
                                ))
                            })?;
                        let authority = owner.client().authorization_owner().own_authority().map_err(|error| ServerError::Nats(error.to_string()))?;
                        let trellis_protocol::SessionBinding::Service { .. } = &authority.authority().binding else {
                            return Err(ServerError::Nats("operation provider requires a service binding".into()));
                        };
                        let opening =
                            crate::service::live_router::parse_operation_watch_open(
                                &payload,
                            )?;
                        let include_updates = opening.include_updates;
                        let cancellation = crate::live::LiveCancellation::new();
                        let factory_watch = Arc::clone(&watch);
                        let factory_ctx = ctx.clone();
                        let factory_operation_id = opening.operation_id.clone();
                        let schemas = Arc::clone(schemas);
                        let source_factory = move || {
                            crate::service::live_router::source_from_watch_frames(
                                watch_response_stream::<D, D::Update>(
                                    factory_watch
                                        .watch(factory_ctx, factory_operation_id),
                                    include_updates,
                                    schemas,
                                ),
                            )
                        };
                        let lease = accepted_generation(&owner, &ctx).await?;
                        let reserved = crate::service::live_router::reserve_operation_watch::<
                            D,
                            _,
                        >(
                            owner.client(),
                            lease,
                            owner.manager()?,
                            &crate::service::live_router::OperationWatchOpenRequest {
                                request: ctx,
                                inputs: crate::service::live_router::OperationWatchOpenInputs {
                                    base_subject: subject,
                                },
                                opening,
                                cancellation,
                            },
                            source_factory,
                        )
                        .await?;
                        Ok(HandlerResponse::LivePrepared(Box::new(reserved.prepared)))
                    }
                    "cancel" if D::CANCELABLE => {
                        Ok(HandlerResponse::Frames(vec![snapshot_frame::<D>(
                            cancel.cancel(ctx, request.operation_id).await?,
                            schemas,
                        )?]))
                    }
                    "signal" => {
                        let signal_name = request.signal.ok_or_else(|| {
                            ServerError::InvalidOperationControlAction {
                                subject: subject.clone(),
                                action: "signal".to_string(),
                            }
                        })?;
                        let signal_schema = schemas.signals
                            .get(&signal_name)
                            .ok_or_else(|| ServerError::InvalidOperationControlAction {
                                subject: subject.clone(),
                                action: format!("signal:{signal_name}"),
                            })?;
                        let signal_value = request.input.as_ref().unwrap_or(&serde_json::Value::Null);
                        signal_schema.validate(signal_value)?;
                        Ok(HandlerResponse::Frames(vec![signal_frame::<D>(
                            signal.signal(ctx, request.operation_id, signal_name, request.input)
                                .await?,
                            schemas,
                        )?]))
                    }
                    action => Err(ServerError::InvalidOperationControlAction {
                        subject,
                        action: action.to_string(),
                    }),
                }
            })
        },
                    ), }));
    }

    /// Register one operation-backed provider.
    pub(crate) fn register_operation_provider<D, P>(&mut self, provider: P)
    where
        D: OperationDescriptor + 'static,
        P: ServiceOperationProvider<D>,
    {
        self.register_operation_routes::<D, P>(Arc::new(provider));
    }

    /// Register an operation business handler while the router owns lifecycle control.
    #[cfg(feature = "runtime-internals")]
    pub fn register_operation_handler<D, F, Fut, V>(
        &mut self,
        runtime: super::operations::OperationHandlerRuntime<V>,
        handler: F,
    ) where
        D: OperationDescriptor + 'static,
        D::Progress: serde::de::DeserializeOwned,
        D::Output: serde::de::DeserializeOwned,
        F: Fn(RequestContext, D::Input, super::OperationControl<D>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
        V: super::RequestValidator + Clone + 'static,
    {
        self.register_operation_provider::<D, _>(super::operations::RuntimeOperationProvider::new(
            runtime, handler,
        ));
    }

    #[doc(hidden)]
    pub async fn recover_operations(&self) -> Result<(), ServerError> {
        for recover in &self.operation_recoveries {
            recover().await?;
        }
        Ok(())
    }

    /// Dispatch one request to the registered handler for its subject.
    pub async fn handle_request(
        &self,
        subject: &str,
        payload: Bytes,
        context: RequestContext,
    ) -> Result<Bytes, ServerError> {
        let mut frames = self
            .handle_request_frames(subject, payload, context)
            .await?;
        let first = frames.drain(..).next().ok_or_else(|| {
            ServerError::Nats(format!("handler for '{subject}' returned no response"))
        })?;
        Ok(first)
    }

    /// Return declared capabilities required for the routed request payload.
    pub fn required_capabilities(
        &self,
        subject: &str,
        payload: &[u8],
    ) -> Result<Option<Vec<String>>, ServerError> {
        let route = self
            .route(subject)
            .ok_or_else(|| ServerError::MissingHandler(subject.to_string()))?;
        Ok(route.capabilities.required_for_payload(payload))
    }

    /// Return the exact permission surface required for the routed request payload.
    ///
    /// `Ok(None)` means no exact permission applies (unknown control action);
    /// the caller must deny rather than fall back to a weaker check.
    pub fn required_permission(
        &self,
        subject: &str,
        payload: &[u8],
    ) -> Result<Option<RoutePermission>, ServerError> {
        let route = self
            .route(subject)
            .ok_or_else(|| ServerError::MissingHandler(subject.to_string()))?;
        Ok(route.permission.for_payload(payload))
    }

    /// Whether the registered handler enforces payload-bound authority after possession verification.
    pub fn handler_authorizes(&self, subject: &str) -> bool {
        self.route(subject)
            .is_some_and(|route| matches!(route.permission, RoutePermissionSpec::Handler(..)))
    }

    pub(crate) async fn sign_reply(
        &self,
        context: &RequestContext,
        request_payload: &[u8],
        reply: &super::request_loop::OutboundReply,
    ) -> Result<async_nats::HeaderMap, ServerError> {
        let owner = self
            .live_owner
            .read()
            .ok()
            .and_then(|owner| owner.clone())
            .ok_or_else(|| ServerError::Nats("provider reply owner unavailable".into()))?;
        let action = self
            .required_permission(&context.subject, request_payload)?
            .map(|route| route.catalog_action())
            .ok_or_else(|| ServerError::Nats("reply has no registered provider action".into()))?;
        super::request_loop::sign_verified_reply(owner.client(), context, &action, reply).await
    }

    /// Dispatch one request to the registered handler for its subject.
    pub async fn handle_request_frames(
        &self,
        subject: &str,
        payload: Bytes,
        context: RequestContext,
    ) -> Result<Vec<Bytes>, ServerError> {
        match self
            .handle_request_response(subject, payload, context)
            .await?
        {
            HandlerResponse::Frames(frames) => Ok(frames),
            HandlerResponse::Error(payload) => Ok(vec![payload]),
            HandlerResponse::LivePrepared(_) => Err(ServerError::Nats(
                "a live response requires a live owner and cannot be collected".to_owned(),
            )),
        }
    }

    /// Dispatch one request to the registered handler for its subject.
    pub async fn handle_request_response(
        &self,
        subject: &str,
        payload: Bytes,
        context: RequestContext,
    ) -> Result<HandlerResponse, ServerError> {
        let route = self
            .route(subject)
            .ok_or_else(|| ServerError::MissingHandler(subject.to_string()))?;
        if route.live
            && !context
                .verified_request()
                .is_some_and(|verified| verified.authorized_action().is_some())
        {
            return Err(ServerError::RequestDenied {
                subject: subject.to_owned(),
                session_key: String::new(),
            });
        }
        (route.handler)(context, payload).await
    }
}

fn decode_generated_input<T>(payload: &[u8]) -> Result<T, ServerError>
where
    T: crate::generated::Codec,
{
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: String::new(),
                message: format!("Invalid JSON: {error}"),
            }]),
        })?;
    T::decode(value).map_err(|error| ServerError::Validation {
        issues: Box::new(vec![ValidationIssue {
            path: String::new(),
            message: error.to_string(),
        }]),
    })
}

fn validate_provider_value(
    key: &str,
    schema: &PreparedSchema,
    value: &serde_json::Value,
) -> Result<(), ServerError> {
    schema.validate(value).map_err(|error| {
        tracing::error!(surface = key, %error, "provider emitted an invalid contract payload");
        ServerError::Nats(format!(
            "provider output for `{key}` violated its contract schema"
        ))
    })
}

fn watch_response_stream<D, TUpdate>(
    events: OperationLiveWatch<D::Progress, TUpdate, D::Output>,
    include_updates: bool,
    schemas: Arc<OperationSchemas>,
) -> ResponseStream
where
    D: OperationDescriptor,
    TUpdate: serde::Serialize + 'static,
{
    let mut sent_initial_snapshot = false;
    let mut current_snapshot = None;
    Box::pin(
        events
            .map(move |event| match event {
                Ok(OperationLiveEvent::Snapshot(snapshot)) => {
                    let value = match serde_json::to_value(&snapshot) {
                        Ok(value) => value,
                        Err(error) => return Some(Err(error.into())),
                    };
                    current_snapshot = Some(value);
                    let index = usize::from(sent_initial_snapshot);
                    sent_initial_snapshot = true;
                    Some(operation_watch_frame::<D>(index, snapshot, &schemas))
                }
                Ok(OperationLiveEvent::Update(update)) if include_updates => {
                    Some(operation_update_frame(
                        update,
                        schemas.update.as_ref(),
                        current_snapshot.as_ref(),
                    ))
                }
                Ok(OperationLiveEvent::Update(_)) => None,
                Err(error) => Some(Err(error)),
            })
            .filter_map(futures_util::future::ready),
    )
}

fn operation_update_frame<TUpdate>(
    update: crate::client::OperationUpdateEvent<TUpdate>,
    update_schema: Option<&PreparedSchema>,
    snapshot: Option<&serde_json::Value>,
) -> Result<Bytes, ServerError>
where
    TUpdate: serde::Serialize,
{
    let update_value = serde_json::to_value(update.update)?;
    let schema = update_schema.ok_or_else(|| {
        ServerError::Nats("operation update stream is missing its declared schema".to_string())
    })?;
    schema.validate(&update_value)?;
    let snapshot = snapshot.ok_or_else(|| {
        ServerError::Nats("operation update preceded its initial snapshot".into())
    })?;
    Ok(Bytes::from(serde_json::to_vec(&serde_json::json!({
        "kind": "event",
        "sequence": update.sequence,
        "event": {
            "type": "update",
            "operationId": update.operation_id,
            "sequence": update.sequence,
            "timestamp": update.timestamp,
            "update": update_value,
            "snapshot": snapshot,
        }
    }))?))
}

fn snapshot_frame<D>(
    snapshot: OperationSnapshot<D::Progress, D::Output>,
    schemas: &OperationSchemas,
) -> Result<Bytes, ServerError>
where
    D: OperationDescriptor,
{
    validate_operation_snapshot::<D>(&snapshot, schemas)?;
    Ok(Bytes::from(serde_json::to_vec(&OperationSnapshotFrame {
        kind: "snapshot".to_string(),
        snapshot,
    })?))
}

fn signal_frame<D>(
    accepted: OperationSignalAccepted<D::Progress, D::Output>,
    schemas: &OperationSchemas,
) -> Result<Bytes, ServerError>
where
    D: OperationDescriptor,
{
    validate_operation_snapshot::<D>(&accepted.snapshot, schemas)?;
    Ok(Bytes::from(serde_json::to_vec(&accepted)?))
}

fn operation_watch_frame<D>(
    index: usize,
    snapshot: OperationSnapshot<D::Progress, D::Output>,
    schemas: &OperationSchemas,
) -> Result<Bytes, ServerError>
where
    D: OperationDescriptor,
{
    if index == 0 {
        return snapshot_frame::<D>(snapshot, schemas);
    }

    validate_operation_snapshot::<D>(&snapshot, schemas)?;

    let event_type = match snapshot.state {
        super::OperationState::Pending => "accepted",
        super::OperationState::Running if snapshot.transfer.is_some() => "transfer",
        super::OperationState::Running if snapshot.progress.is_some() => "progress",
        super::OperationState::Running => "started",
        super::OperationState::Completed => "completed",
        super::OperationState::Failed => "failed",
        super::OperationState::Cancelled => "cancelled",
    };

    let mut event = serde_json::json!({
        "type": event_type,
        "snapshot": snapshot,
    });
    if let Some(progress) = event
        .get("snapshot")
        .and_then(|value| value.get("progress"))
        .cloned()
    {
        event["progress"] = progress;
    }
    if let Some(transfer) = event
        .get("snapshot")
        .and_then(|value| value.get("transfer"))
        .cloned()
    {
        event["transfer"] = transfer;
    }

    Ok(Bytes::from(serde_json::to_vec(&serde_json::json!({
        "kind": "event",
        "sequence": index,
        "event": event,
    }))?))
}

fn validate_operation_snapshot<D>(
    snapshot: &OperationSnapshot<D::Progress, D::Output>,
    schemas: &OperationSchemas,
) -> Result<(), ServerError>
where
    D: OperationDescriptor,
{
    if let (Some(schema), Some(progress)) = (&schemas.progress, &snapshot.progress) {
        validate_provider_value(D::KEY, schema, &serde_json::to_value(progress)?)?;
    }
    if let Some(output) = &snapshot.output {
        validate_provider_value(D::KEY, &schemas.output, &serde_json::to_value(output)?)?;
    }
    Ok(())
}
