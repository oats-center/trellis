use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::oneshot;

use serde_json::Value;
use trellis_protocol::{
    ApiSurfaceKind, PermissionAction, PermissionAtom, PermissionTarget, ProtocolError,
};

use super::error::ValidationIssue;
use super::operations::ServiceOperationProvider;
use super::request_loop::{HandlerResponse, ResponseStream};
use super::schema_validation::validate_input_schema;
use super::{
    control_subject, FeedDescriptor, HandlerResult, OperationControlRequest, OperationDescriptor,
    OperationLiveEvent, OperationLiveWatch, OperationSignalAccepted, OperationSnapshot,
    OperationSnapshotFrame, RpcDescriptor, ServerError,
};

/// Request metadata forwarded to mounted RPC handlers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestContext {
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
    /// NATS reply inbox used for request/reply responses.
    pub reply_to: Option<String>,
    /// Locally verified caller projection, when the request was authorized.
    pub caller: Option<super::local_validator::VerifiedCaller>,
    /// W3C trace context header propagated by the caller, if present.
    pub traceparent: Option<String>,
    /// W3C trace state header propagated by the caller, if present.
    pub tracestate: Option<String>,
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
}

struct Route {
    handler: BoxedHandler,
    capabilities: RouteCapabilities,
    permission: RoutePermissionSpec,
}

/// Exact permission surface recorded at registration time for one route.
#[derive(Debug, Clone)]
enum RoutePermissionSpec {
    /// The handler performs exact payload-bound authorization after proof verification.
    Handler,
    /// One fixed surface for every request on the route.
    Static(String, ApiSurfaceKind, String, PermissionAction),
    /// Operation control routes resolve the action from the payload.
    OperationControl(String, String),
}

impl RoutePermissionSpec {
    fn for_payload(&self, payload: &[u8]) -> Option<RoutePermission> {
        match self {
            Self::Handler => None,
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

enum FeedCancellationState {
    Active {
        cancel: oneshot::Sender<()>,
        reply_to: String,
        principal_id: String,
        participant_id: String,
    },
}

type FeedCancellations = Arc<Mutex<HashMap<(String, String), FeedCancellationState>>>;

struct FeedCancellation {
    receiver: oneshot::Receiver<()>,
    key: (String, String),
    cancellations: FeedCancellations,
}

impl Future for FeedCancellation {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(context).map(|_| ())
    }
}

impl Drop for FeedCancellation {
    fn drop(&mut self) {
        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

/// An in-memory subject router for descriptor-backed RPC handlers.
#[derive(Default)]
pub struct Router {
    handlers: HashMap<String, Route>,
    feed_cancellations: FeedCancellations,
    provider_deployment_id: Option<String>,
    provider_instance_id: Option<String>,
    operation_recoveries: Vec<OperationRecovery>,
}

type OperationRecovery = Box<dyn Fn() -> BoxFuture<'static, Result<(), ServerError>> + Send + Sync>;

impl Router {
    fn route(&self, subject: &str) -> Option<&Route> {
        self.handlers.get(subject).or_else(|| {
            let (prefix, _) = subject.rsplit_once('.')?;
            self.handlers.get(&format!("{prefix}.*"))
        })
    }

    /// Create an empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind subsequently registered request routes to this provider deployment.
    pub fn set_provider_deployment_id(&mut self, deployment_id: impl Into<String>) {
        self.provider_deployment_id = Some(deployment_id.into());
    }

    /// Bind Feed control routes to this service instance.
    pub fn set_provider_instance_id(&mut self, instance_id: impl Into<String>) {
        self.provider_instance_id = Some(instance_id.into());
    }

    fn descriptor_subject(
        &self,
        family: &str,
        api_id: &str,
        action: &str,
        fallback: &str,
    ) -> String {
        let Some(deployment_id) = self.provider_deployment_id.as_deref() else {
            return fallback.to_owned();
        };
        let action = self.descriptor_name(action);
        let subject = match family {
            "rpc" => trellis_protocol::derive_bound_rpc_subject(api_id, deployment_id, &action),
            "operation" => {
                trellis_protocol::derive_bound_operation_subject(api_id, deployment_id, &action)
            }
            "feed" => trellis_protocol::derive_bound_feed_subject(api_id, deployment_id, &action),
            _ => unreachable!("only request route families are deployment-bound"),
        };
        subject.expect("generated route metadata must form a valid bound subject")
    }

    fn descriptor_capabilities(&self, capabilities: &[&str]) -> Vec<String> {
        capabilities
            .iter()
            .map(|capability| (*capability).to_string())
            .collect()
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
        self.handlers.insert(
            self.descriptor_subject("rpc", D::API_ID, D::KEY, D::SUBJECT),
            Route {
                capabilities: RouteCapabilities::Static(capabilities),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Rpc,
                    self.descriptor_name(D::KEY),
                    PermissionAction::Call,
                ),
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
            ),
            },
        );
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
        let subject = self.descriptor_subject("rpc", D::API_ID, D::KEY, D::SUBJECT);
        let route = self
            .handlers
            .get_mut(&subject)
            .expect("RPC must be registered before changing its authorization mode");
        route.permission = RoutePermissionSpec::Handler;
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
            self.descriptor_subject("rpc", D::API_ID, D::KEY, D::SUBJECT),
            Route {
                capabilities: RouteCapabilities::Static(capabilities),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Rpc,
                    self.descriptor_name(D::KEY),
                    PermissionAction::Call,
                ),
                handler: Box::new(|_, _| {
                    Box::pin(async {
                        Err(ServerError::Nats(
                            "routing-metadata-only handler cannot execute".to_owned(),
                        ))
                    })
                }),
            },
        );
    }

    /// Register one generated operation descriptor for routing metadata only.
    pub fn register_operation_metadata<D>(&mut self)
    where
        D: OperationDescriptor + 'static,
    {
        let subject = self.descriptor_subject("operation", D::API_ID, D::KEY, D::SUBJECT);
        let name = self.descriptor_name(D::KEY);
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
        self.handlers.insert(
            subject.clone(),
            Route {
                capabilities: RouteCapabilities::Static(
                    self.descriptor_capabilities(D::CALLER_CAPABILITIES),
                ),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Operation,
                    name.clone(),
                    PermissionAction::Invoke,
                ),
                handler: metadata_handler(),
            },
        );
        self.handlers.insert(
            control_subject(&subject),
            Route {
                capabilities: RouteCapabilities::OperationControl {
                    observe: self.descriptor_capabilities(D::OBSERVE_CAPABILITIES),
                    cancel: self.descriptor_capabilities(D::CANCEL_CAPABILITIES),
                    control: self.descriptor_capabilities(D::CONTROL_CAPABILITIES),
                },
                permission: RoutePermissionSpec::OperationControl(D::API_ID.to_owned(), name),
                handler: metadata_handler(),
            },
        );
    }

    /// Register one descriptor-backed feed handler.
    pub fn register_feed<D, F, S>(&mut self, handler: F)
    where
        D: FeedDescriptor + 'static,
        F: Fn(RequestContext, D::Input) -> S + Send + Sync + 'static,
        S: Stream<Item = Result<D::Event, ServerError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let cancellations = Arc::clone(&self.feed_cancellations);
        let subject = self.descriptor_subject("feed", D::API_ID, D::KEY, D::SUBJECT);
        let control_subject = trellis_protocol::derive_feed_control_subject(
            &subject,
            self.provider_instance_id
                .as_deref()
                .unwrap_or("unbound-instance"),
        );
        let handler_subject = subject.clone();
        let owner_instance_id = self
            .provider_instance_id
            .clone()
            .unwrap_or_else(|| "unbound-instance".to_owned());
        let capabilities = self.descriptor_capabilities(D::SUBSCRIBE_CAPABILITIES);
        self.handlers.insert(
            subject.clone(),
            Route {
                capabilities: RouteCapabilities::Static(capabilities.clone()),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Feed,
                    self.descriptor_name(D::KEY),
                    PermissionAction::Subscribe,
                ),
                handler: Box::new(
                move |ctx, payload| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
                    let handler = Arc::clone(&handler);
                    let cancellations = Arc::clone(&cancellations);
                    let handler_subject = handler_subject.clone();
                    let owner_instance_id = owner_instance_id.clone();
                    Box::pin(async move {
                        let input = decode_generated_input::<D::Input>(&payload)?;
                        let reply_to = ctx.reply_to.clone().ok_or_else(|| {
                            ServerError::Nats("feed request is missing a reply inbox".to_string())
                        })?;
                        let caller = ctx.caller.as_ref().ok_or_else(|| ServerError::RequestDenied {
                            subject: handler_subject.clone(),
                            session_key: ctx.session_key.clone().unwrap_or_default(),
                        })?;
                        let feed_id = ctx.request_id.clone().ok_or_else(|| ServerError::Nats(
                            "feed request is missing a request id".to_owned(),
                        ))?;
                        let key = (handler_subject.clone(), feed_id.clone());
                        let (cancel, receiver) = oneshot::channel();
                        let mut states = cancellations
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        match states.remove(&key) {
                            Some(FeedCancellationState::Active { cancel: previous, .. }) => {
                                let _ = previous.send(());
                                states.insert(key.clone(), FeedCancellationState::Active {
                                    cancel,
                                    reply_to: reply_to.clone(),
                                    principal_id: caller.principal_id.clone(),
                                    participant_id: caller.participant_id.clone(),
                                });
                            }
                            None => {
                                states.insert(key.clone(), FeedCancellationState::Active {
                                    cancel,
                                    reply_to: reply_to.clone(),
                                    principal_id: caller.principal_id.clone(),
                                    participant_id: caller.participant_id.clone(),
                                });
                            }
                        }
                        drop(states);
                        let cancellation = FeedCancellation {
                            receiver,
                            key,
                            cancellations,
                        };
                        Ok(HandlerResponse::FeedStream {
                            stream: feed_response_stream(handler(ctx, input).take_until(cancellation)),
                            control_subject: trellis_protocol::derive_feed_instance_control_subject(
                                &handler_subject,
                                &owner_instance_id,
                                &feed_id,
                            ),
                            feed_id,
                        })
                    })
                },
            ),
            },
        );
        let cancellation_subject = subject;
        let cancellations = Arc::clone(&self.feed_cancellations);
        let owner_instance_id = self
            .provider_instance_id
            .clone()
            .unwrap_or_else(|| "unbound-instance".to_owned());
        self.handlers.insert(
            control_subject,
            Route {
                capabilities: RouteCapabilities::Static(capabilities),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Feed,
                    self.descriptor_name(D::KEY),
                    PermissionAction::Subscribe,
                ),
                handler: Box::new(move |ctx, payload| {
                    let cancellations = Arc::clone(&cancellations);
                    let cancellation_subject = cancellation_subject.clone();
                    let owner_instance_id = owner_instance_id.clone();
                    Box::pin(async move {
                        let (feed_id, reply_to) =
                            feed_cancel_metadata(&payload).ok_or_else(|| {
                                ServerError::Nats("invalid feed cancellation payload".to_owned())
                            })?;
                        if ctx.reply_to.as_deref() != Some(reply_to.as_str()) {
                            return Err(ServerError::Nats(
                                "feed cancellation reply inbox does not match".to_owned(),
                            ));
                        }
                        if ctx.subject
                            != trellis_protocol::derive_feed_instance_control_subject(
                                &cancellation_subject,
                                &owner_instance_id,
                                &feed_id,
                            )
                        {
                            return Err(ServerError::RequestDenied {
                                subject: ctx.subject,
                                session_key: ctx.session_key.unwrap_or_default(),
                            });
                        }
                        let key = (cancellation_subject, feed_id);
                        let mut states = cancellations
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let allowed = states.get(&key).is_some_and(|state| {
                            match (state, ctx.caller.as_ref()) {
                                (
                                    FeedCancellationState::Active {
                                        reply_to: active_reply,
                                        principal_id,
                                        participant_id,
                                        ..
                                    },
                                    Some(caller),
                                ) => {
                                    active_reply == &reply_to
                                        && principal_id == &caller.principal_id
                                        && participant_id == &caller.participant_id
                                }
                                _ => false,
                            }
                        });
                        if !allowed {
                            return Err(ServerError::RequestDenied {
                                subject: ctx.subject,
                                session_key: ctx.session_key.unwrap_or_default(),
                            });
                        }
                        if let Some(FeedCancellationState::Active { cancel, .. }) =
                            states.remove(&key)
                        {
                            let _ = cancel.send(());
                        }
                        Ok(HandlerResponse::Frames(Vec::new()))
                    })
                }),
            },
        );
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
        self.operation_recoveries
            .push(Box::new(move || provider.recover()));
        let update_schema_json = D::UPDATE_SCHEMA_JSON;
        let subject = self.descriptor_subject("operation", D::API_ID, D::KEY, D::SUBJECT);
        let handler_subject = subject.clone();
        let caller_capabilities = self.descriptor_capabilities(D::CALLER_CAPABILITIES);
        let observe_capabilities = self.descriptor_capabilities(D::OBSERVE_CAPABILITIES);
        let cancel_capabilities = self.descriptor_capabilities(D::CANCEL_CAPABILITIES);
        let control_capabilities = self.descriptor_capabilities(D::CONTROL_CAPABILITIES);

        self.handlers.insert(
            subject.clone(),
            Route {
                capabilities: RouteCapabilities::Static(caller_capabilities),
                permission: RoutePermissionSpec::Static(
                    D::API_ID.to_owned(),
                    ApiSurfaceKind::Operation,
                    self.descriptor_name(D::KEY),
                    PermissionAction::Invoke,
                ),
                handler: Box::new(
                move |ctx, payload| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
                    let start = Arc::clone(&start);
                    Box::pin(async move {
                        let envelope: OperationStartEnvelope = serde_json::from_slice(&payload)?;
                        envelope.invocation_id.parse::<ulid::Ulid>().map_err(|_| {
                            ServerError::Nats("operation invocation id must be a ULID".to_owned())
                        })?;
                        let input = parse_validated_input::<D::Input>(
                            &serde_json::to_vec(&envelope.input)?,
                            D::INPUT_SCHEMA_JSON,
                        )?;
                        let output = start
                            .start_invocation(ctx, envelope.invocation_id, input)
                            .await?;
                        validate_operation_snapshot::<D>(&output.snapshot)?;
                        Ok(HandlerResponse::Frames(vec![Bytes::from(
                            serde_json::to_vec(&output)?,
                        )]))
                    })
                },
            ),
            },
        );

        self.handlers.insert(
            control_subject(&subject),
            Route {
                capabilities: RouteCapabilities::OperationControl {
                    observe: observe_capabilities,
                    cancel: cancel_capabilities,
                    control: control_capabilities,
                },
                permission: RoutePermissionSpec::OperationControl(
                    D::API_ID.to_owned(),
                    self.descriptor_name(D::KEY),
                ),
                handler: Box::new(
                move |ctx, payload| -> BoxFuture<'static, Result<HandlerResponse, ServerError>> {
                    let get = Arc::clone(&get);
                    let watch = Arc::clone(&watch);
                    let cancel = Arc::clone(&cancel);
                    let signal = Arc::clone(&signal);
                    let subject = handler_subject.clone();
                    let request = serde_json::from_slice::<OperationControlRequest>(&payload)
                        .map_err(ServerError::Json);
                    Box::pin(async move {
                        let request = request?;
                        tracing::debug!(
                            subject = %subject,
                            action = %request.action,
                            operation_id = %request.operation_id,
                            "operation control request"
                        );
                        let frames = match request.action.as_str() {
                            "get" => HandlerResponse::Frames(vec![snapshot_frame::<D>(
                                get.get(ctx, request.operation_id).await?,
                            )?]),
                            "watch" => {
                                let include_updates = request.include_updates.unwrap_or(false);
                                if include_updates && update_schema_json.is_none() {
                                    return Err(ServerError::InvalidOperationControlAction {
                                        subject: subject.clone(),
                                        action: "watch:updates".to_string(),
                                    });
                                }
                                HandlerResponse::Stream(watch_response_stream::<D, D::Update>(
                                    watch.watch(ctx, request.operation_id),
                                    include_updates,
                                    update_schema_json,
                                ))
                            }
                            "cancel" if D::CANCELABLE => {
                                HandlerResponse::Frames(vec![snapshot_frame::<D>(
                                    cancel.cancel(ctx, request.operation_id).await?,
                                )?])
                            }
                            "signal" => {
                                let signal_name = request.signal.ok_or_else(|| {
                                    ServerError::InvalidOperationControlAction {
                                        subject: subject.clone(),
                                        action: "signal".to_string(),
                                    }
                                })?;
                                let signal_schemas: serde_json::Value =
                                    serde_json::from_str(D::SIGNAL_INPUT_SCHEMAS_JSON)
                                        .map_err(|e| ServerError::Nats(
                                            format!("failed to parse signal schemas: {e}")
                                        ))?;
                                let signal_schema = signal_schemas
                                    .get(&signal_name)
                                    .ok_or_else(|| ServerError::InvalidOperationControlAction {
                                        subject: subject.clone(),
                                        action: format!("signal:{signal_name}"),
                                    })?;
                                let signal_value = request.input.as_ref().unwrap_or(&serde_json::Value::Null);
                                let signal_schema_str = serde_json::to_string(signal_schema)
                                    .map_err(|e| ServerError::Nats(
                                        format!("failed to serialize signal schema: {e}")
                                    ))?;
                                validate_input_schema(&signal_schema_str, signal_value)?;
                                HandlerResponse::Frames(vec![signal_frame::<D>(
                                    signal.signal(ctx, request.operation_id, signal_name, request.input)
                                        .await?,
                                )?])
                            }
                            action => {
                                return Err(ServerError::InvalidOperationControlAction {
                                    subject,
                                    action: action.to_string(),
                                })
                            }
                        };
                        Ok(frames)
                    })
                },
            ),
            },
        );
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
            HandlerResponse::Stream(mut stream) => {
                let mut frames = Vec::new();
                while let Some(frame) = stream.next().await {
                    frames.push(frame?);
                }
                Ok(frames)
            }
            HandlerResponse::FeedStream { mut stream, .. } => {
                let mut frames = Vec::new();
                while let Some(frame) = stream.next().await {
                    frames.push(frame?);
                }
                Ok(frames)
            }
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
        (route.handler)(context, payload).await
    }
}

fn feed_cancel_metadata(payload: &[u8]) -> Option<(String, String)> {
    let value = serde_json::from_slice::<Value>(payload).ok()?;
    let object = value.as_object()?;
    if object.len() != 2 {
        return None;
    }
    let reply = object
        .get("_trellisFeedCancel")
        .and_then(Value::as_str)
        .map(ToString::to_string)?;
    let feed_id = object.get("feedId").and_then(Value::as_str)?.to_owned();
    Some((feed_id, reply))
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

fn parse_validated_input<T>(payload: &[u8], schema_json: &str) -> Result<T, ServerError>
where
    T: serde::de::DeserializeOwned,
{
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| ServerError::Validation {
            issues: Box::new(vec![ValidationIssue {
                path: String::new(),
                message: format!("Invalid JSON: {error}"),
            }]),
        })?;
    validate_input_schema(schema_json, &value)?;
    serde_json::from_value(value).map_err(ServerError::from)
}

fn feed_response_stream<TEvent>(
    events: impl Stream<Item = Result<TEvent, ServerError>> + Send + 'static,
) -> ResponseStream
where
    TEvent: crate::generated::Codec + 'static,
{
    Box::pin(events.map(|event| {
        event.and_then(|event| {
            let event = crate::generated::Codec::encode(&event)
                .map_err(|error| ServerError::Nats(error.to_string()))?;
            Ok(Bytes::from(serde_json::to_vec(&event)?))
        })
    }))
}

fn validate_provider_value(
    key: &str,
    schema_json: &str,
    value: &serde_json::Value,
) -> Result<(), ServerError> {
    validate_input_schema(schema_json, value).map_err(|error| {
        tracing::error!(surface = key, %error, "provider emitted an invalid contract payload");
        ServerError::Nats(format!(
            "provider output for `{key}` violated its contract schema"
        ))
    })
}

fn watch_response_stream<D, TUpdate>(
    events: OperationLiveWatch<D::Progress, TUpdate, D::Output>,
    include_updates: bool,
    update_schema_json: Option<&'static str>,
) -> ResponseStream
where
    D: OperationDescriptor,
    TUpdate: serde::Serialize + 'static,
{
    let mut sent_initial_snapshot = false;
    Box::pin(
        events
            .map(move |event| match event {
                Ok(OperationLiveEvent::Snapshot(snapshot)) => {
                    let index = usize::from(sent_initial_snapshot);
                    sent_initial_snapshot = true;
                    Some(operation_watch_frame::<D>(index, snapshot))
                }
                Ok(OperationLiveEvent::Update(update)) if include_updates => {
                    Some(operation_update_frame(update, update_schema_json))
                }
                Ok(OperationLiveEvent::Update(_)) => None,
                Err(error) => Some(Err(error)),
            })
            .filter_map(futures_util::future::ready),
    )
}

fn operation_update_frame<TUpdate>(
    update: crate::client::OperationUpdateEvent<TUpdate>,
    update_schema_json: Option<&str>,
) -> Result<Bytes, ServerError>
where
    TUpdate: serde::Serialize,
{
    let update_value = serde_json::to_value(update.update)?;
    let schema_json = update_schema_json.ok_or_else(|| {
        ServerError::Nats("operation update stream is missing its declared schema".to_string())
    })?;
    validate_input_schema(schema_json, &update_value)?;
    Ok(Bytes::from(serde_json::to_vec(&serde_json::json!({
        "kind": "event",
        "sequence": update.sequence,
        "event": {
            "type": "update",
            "operationId": update.operation_id,
            "sequence": update.sequence,
            "timestamp": update.timestamp,
            "update": update_value,
        }
    }))?))
}

fn snapshot_frame<D>(
    snapshot: OperationSnapshot<D::Progress, D::Output>,
) -> Result<Bytes, ServerError>
where
    D: OperationDescriptor,
{
    validate_operation_snapshot::<D>(&snapshot)?;
    Ok(Bytes::from(serde_json::to_vec(&OperationSnapshotFrame {
        kind: "snapshot".to_string(),
        snapshot,
    })?))
}

fn signal_frame<D>(
    accepted: OperationSignalAccepted<D::Progress, D::Output>,
) -> Result<Bytes, ServerError>
where
    D: OperationDescriptor,
{
    validate_operation_snapshot::<D>(&accepted.snapshot)?;
    Ok(Bytes::from(serde_json::to_vec(&accepted)?))
}

fn operation_watch_frame<D>(
    index: usize,
    snapshot: OperationSnapshot<D::Progress, D::Output>,
) -> Result<Bytes, ServerError>
where
    D: OperationDescriptor,
{
    if index == 0 {
        return snapshot_frame::<D>(snapshot);
    }

    validate_operation_snapshot::<D>(&snapshot)?;

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
) -> Result<(), ServerError>
where
    D: OperationDescriptor,
{
    if let (Some(schema), Some(progress)) = (D::PROGRESS_SCHEMA_JSON, &snapshot.progress) {
        validate_provider_value(D::KEY, schema, &serde_json::to_value(progress)?)?;
    }
    if let Some(output) = &snapshot.output {
        validate_provider_value(
            D::KEY,
            D::OUTPUT_SCHEMA_JSON,
            &serde_json::to_value(output)?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{stream, StreamExt};

    use super::*;

    struct TestOperation;

    impl OperationDescriptor for TestOperation {
        type Input = Value;
        type Progress = Value;
        type Output = Value;
        type Update = Value;
        type UpdateEvidence = crate::client::NoOperationUpdates;
        type Error = super::super::OperationFailure;

        const API_ID: &'static str = "test@v1";
        const KEY: &'static str = "Test.Operation";
        const SUBJECT: &'static str = "operations.v1.Test.Operation";
        const CANCELABLE: bool = true;
        const INPUT_SCHEMA_JSON: &'static str = r#"{"type":"object"}"#;
        const PROGRESS_SCHEMA_JSON: Option<&'static str> = None;
        const OUTPUT_SCHEMA_JSON: &'static str = r#"{"type":"object"}"#;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = None;
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = r#"{"resume":{"type":"object"}}"#;
    }

    struct DefaultControlProvider;

    #[test]
    fn bound_routes_use_canonical_action_name_without_legacy_api_prefix() {
        let mut router = Router::new();
        router.set_provider_deployment_id("provider");
        let subject = trellis_protocol::derive_bound_operation_subject(
            TestOperation::API_ID,
            "provider",
            "Operation",
        )
        .expect("bound subject");
        assert_eq!(
            router.descriptor_subject(
                "operation",
                TestOperation::API_ID,
                TestOperation::KEY,
                TestOperation::SUBJECT,
            ),
            subject,
        );
    }

    impl ServiceOperationProvider<TestOperation> for DefaultControlProvider {
        fn start(
            &self,
            _context: RequestContext,
            _input: Value,
        ) -> BoxFuture<'static, Result<super::super::AcceptedOperation<Value, Value>, ServerError>>
        {
            unreachable!("start is not exercised")
        }

        fn get(
            &self,
            context: RequestContext,
            operation_id: String,
        ) -> BoxFuture<'static, Result<OperationSnapshot<Value, Value>, ServerError>> {
            self.wait(context, operation_id)
        }

        fn wait(
            &self,
            _context: RequestContext,
            _operation_id: String,
        ) -> BoxFuture<'static, Result<OperationSnapshot<Value, Value>, ServerError>> {
            Box::pin(async {
                Ok(OperationSnapshot {
                    revision: 2,
                    state: super::super::OperationState::Completed,
                    output: Some(serde_json::json!({})),
                    ..Default::default()
                })
            })
        }
    }

    #[tokio::test]
    async fn provider_defaults_watch_terminal_and_reject_controls() {
        let mut router = Router::new();
        router.register_operation_provider::<TestOperation, _>(DefaultControlProvider);
        let control = super::super::control_subject(TestOperation::SUBJECT);
        let context = || RequestContext {
            subject: control.clone(),
            ..Default::default()
        };

        let response = router
            .handle_request_response(
                &control,
                Bytes::from_static(br#"{"action":"watch","operationId":"op-1"}"#),
                context(),
            )
            .await
            .expect("open default watch");
        let HandlerResponse::Stream(mut stream) = response else {
            panic!("watch should stream");
        };
        let frame = stream.next().await.expect("terminal frame").expect("frame");
        assert_eq!(
            serde_json::from_slice::<Value>(&frame).unwrap()["snapshot"]["state"],
            "completed"
        );
        assert!(stream.next().await.is_none());

        for payload in [
            br#"{"action":"cancel","operationId":"op-1"}"#.as_slice(),
            br#"{"action":"signal","operationId":"op-1","signal":"resume","input":{}}"#.as_slice(),
        ] {
            assert!(matches!(
                router
                    .handle_request_response(&control, Bytes::copy_from_slice(payload), context())
                    .await,
                Err(ServerError::InvalidOperationControlAction { .. })
            ));
        }
    }

    struct TestFeed;

    impl FeedDescriptor for TestFeed {
        type Input = Value;
        type Event = Value;

        const API_ID: &'static str = "test@v1";
        const DESCRIPTOR_NAME: &'static str = "feed.Live";
        const KEY: &'static str = "Test.Live";
        const SUBJECT: &'static str = "feeds.v1.Test.Live";
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &[];
    }

    #[tokio::test]
    async fn feed_cancel_frame_stops_the_active_response_stream() {
        let mut router = Router::new();
        router.set_provider_instance_id("provider-instance");
        router.register_feed::<TestFeed, _, _>(|_, _| {
            stream::pending::<Result<Value, ServerError>>()
        });
        let reply_to = "_INBOX.test.feed";
        let caller = crate::service::VerifiedCaller {
            session_key: "caller-session".to_owned(),
            inbox_prefix: "_INBOX.test".to_owned(),
            context_digest: "context".to_owned(),
            connection_id: "connection".to_owned(),
            login_session_id: Some("login".to_owned()),
            principal_id: "creator".to_owned(),
            principal_kind: trellis_protocol::AuthorizationPrincipalKind::User,
            participant_id: "caller-participant".to_owned(),
            platform_privileges: Vec::new(),
            deployment_id: None,
            instance_id: None,
        };
        let context = || RequestContext {
            subject: TestFeed::SUBJECT.to_string(),
            reply_to: Some(reply_to.to_string()),
            session_key: Some(caller.session_key.clone()),
            request_id: Some("feed-id".to_owned()),
            caller: Some(caller.clone()),
            ..Default::default()
        };
        let response = router
            .handle_request_response(TestFeed::SUBJECT, Bytes::from_static(b"{}"), context())
            .await
            .expect("open feed");
        let HandlerResponse::FeedStream {
            mut stream,
            control_subject,
            ..
        } = response
        else {
            panic!("feed registration should return a response stream");
        };

        let second_reply = "_INBOX.test.feed.second";
        let mut second_context = context();
        second_context.reply_to = Some(second_reply.to_owned());
        second_context.request_id = Some("feed-id-2".to_owned());
        let second_response = router
            .handle_request_response(TestFeed::SUBJECT, Bytes::from_static(b"{}"), second_context)
            .await
            .expect("open second feed");
        let HandlerResponse::FeedStream {
            stream: mut second_stream,
            control_subject: second_control_subject,
            ..
        } = second_response
        else {
            panic!("second feed registration should return a response stream");
        };

        let forged = router
            .handle_request_response(
                &control_subject,
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({
                        "_trellisFeedCancel": reply_to,
                        "feedId": "feed-id-2",
                    }))
                    .expect("serialize forged cancellation"),
                ),
                RequestContext {
                    subject: control_subject.clone(),
                    reply_to: Some(reply_to.to_string()),
                    session_key: Some(caller.session_key.clone()),
                    caller: Some(caller.clone()),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(forged, Err(ServerError::RequestDenied { .. })));
        let mut intruder = caller.clone();
        intruder.principal_id = "different-principal".to_owned();
        let denied = router
            .handle_request_response(
                &control_subject,
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({
                        "_trellisFeedCancel": reply_to,
                        "feedId": "feed-id",
                    }))
                    .expect("serialize unauthorized cancellation"),
                ),
                RequestContext {
                    subject: control_subject.clone(),
                    reply_to: Some(reply_to.to_string()),
                    session_key: Some(intruder.session_key.clone()),
                    caller: Some(intruder),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(denied, Err(ServerError::RequestDenied { .. })));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), stream.next())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), second_stream.next())
                .await
                .is_err()
        );

        let mut reconnected_caller = caller.clone();
        reconnected_caller.session_key = "reconnected-session".to_owned();
        router
            .handle_request_response(
                &control_subject,
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({
                        "_trellisFeedCancel": reply_to,
                        "feedId": "feed-id",
                    }))
                    .expect("serialize cancellation"),
                ),
                RequestContext {
                    subject: control_subject.clone(),
                    reply_to: Some(reply_to.to_string()),
                    session_key: Some(reconnected_caller.session_key.clone()),
                    caller: Some(reconnected_caller),
                    ..Default::default()
                },
            )
            .await
            .expect("cancel feed");

        assert!(
            tokio::time::timeout(Duration::from_millis(100), stream.next())
                .await
                .expect("feed should stop promptly")
                .is_none()
        );

        router
            .handle_request_response(
                &second_control_subject,
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({
                        "_trellisFeedCancel": second_reply,
                        "feedId": "feed-id-2",
                    }))
                    .expect("serialize second cancellation"),
                ),
                RequestContext {
                    subject: second_control_subject.clone(),
                    reply_to: Some(second_reply.to_owned()),
                    session_key: Some(caller.session_key.clone()),
                    caller: Some(caller),
                    ..Default::default()
                },
            )
            .await
            .expect("cancel second feed");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), second_stream.next())
                .await
                .expect("second feed should stop promptly")
                .is_none()
        );
    }
}
