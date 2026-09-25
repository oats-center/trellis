use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, Method};
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use url::Url;

use super::bootstrap::{device_bootstrap, device_enroll, service_bootstrap};
use super::browser::{
    bind_flow, complete_admin_account, console_index, console_page, decide_approval,
    get_account_flow, get_flow, get_portal_flow, local_login, oidc_callback, portal_asset,
    portal_index, portal_page, register_local, start_account_flow_oidc, start_auth, start_oidc,
    web_fallback,
};
use super::security::{canonical_origin, security_headers};
use super::well_known::{issuer_key, refresh_context};
use super::{
    AccountRepository, AuthEphemeralRepository, AuthHttpOptions, AuthHttpState,
    AuthorityEvidenceRepository, AuthorizationStateError, ContextRepository, DeploymentRepository,
    OutboxRepository, PortalRepository, ProvisioningRepository, SessionRepository, WebSource,
    EMBEDDED_WEB_ASSETS, MAX_AUTH_REQUEST_BODY_BYTES,
};
use crate::config::WebSourceConfig;
use crate::platform::auth::context::AuthorizationContextRepository;
use crate::platform::auth::GrantRepository;

const LOCAL_LOGIN_ROUTE: &str = "/auth/login/local";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RouteMethod {
    Get,
    Post,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RouteHandler {
    StartAuth,
    ServiceBootstrap,
    DeviceBootstrap,
    DeviceEnroll,
    ContextRefresh,
    IssuerKey,
    GetFlow,
    GetPortalFlow,
    LocalLogin,
    RegisterLocal,
    GetAccountFlow,
    CompleteFirstAdmin,
    StartOidc,
    StartAccountFlowOidc,
    OidcCallback,
    DecideApproval,
    BindFlow,
    PortalIndex,
    PortalPage,
    PortalAsset,
    ConsoleIndex,
    ConsolePage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RouteDefinition {
    method: RouteMethod,
    path: &'static str,
    handler: RouteHandler,
}

impl RouteDefinition {
    fn is_static(self) -> bool {
        matches!(
            self.handler,
            RouteHandler::PortalIndex
                | RouteHandler::PortalPage
                | RouteHandler::PortalAsset
                | RouteHandler::ConsoleIndex
                | RouteHandler::ConsolePage
        )
    }
}

const ROUTES: &[RouteDefinition] = &[
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/auth/keys/{key_id}",
        handler: RouteHandler::IssuerKey,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/requests",
        handler: RouteHandler::StartAuth,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/bootstrap/service",
        handler: RouteHandler::ServiceBootstrap,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/bootstrap/device",
        handler: RouteHandler::DeviceBootstrap,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/device/enroll",
        handler: RouteHandler::DeviceEnroll,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/context/refresh",
        handler: RouteHandler::ContextRefresh,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/auth/flow/{flow_id}",
        handler: RouteHandler::GetFlow,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/flow/{flow_id}/portal",
        handler: RouteHandler::GetPortalFlow,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: LOCAL_LOGIN_ROUTE,
        handler: RouteHandler::LocalLogin,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/flow/{flow_id}/register/local",
        handler: RouteHandler::RegisterLocal,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/auth/account-flow/{flow_token}",
        handler: RouteHandler::GetAccountFlow,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/account-flow/{flow_token}/local-password",
        handler: RouteHandler::CompleteFirstAdmin,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/auth/login/{provider_id}",
        handler: RouteHandler::StartOidc,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/auth/account-flow/{flow_token}/login/{provider_id}",
        handler: RouteHandler::StartAccountFlowOidc,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/auth/callback/{provider_id}",
        handler: RouteHandler::OidcCallback,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/flow/{flow_id}/approval",
        handler: RouteHandler::DecideApproval,
    },
    RouteDefinition {
        method: RouteMethod::Post,
        path: "/auth/flow/{flow_id}/bind",
        handler: RouteHandler::BindFlow,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/login",
        handler: RouteHandler::PortalIndex,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/login/{*path}",
        handler: RouteHandler::PortalPage,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/assets/login/{*path}",
        handler: RouteHandler::PortalAsset,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/console",
        handler: RouteHandler::ConsoleIndex,
    },
    RouteDefinition {
        method: RouteMethod::Get,
        path: "/console/{*path}",
        handler: RouteHandler::ConsolePage,
    },
];

fn add_route<R, E>(
    routes: Router<AuthHttpState<R, E>>,
    route: RouteDefinition,
) -> Router<AuthHttpState<R, E>>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + AuthorizationContextRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone + Send + Sync + 'static,
{
    match (route.method, route.handler) {
        (RouteMethod::Get, RouteHandler::IssuerKey) => {
            routes.route(route.path, get(issuer_key::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::StartAuth) => {
            routes.route(route.path, post(start_auth::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::ServiceBootstrap) => {
            routes.route(route.path, post(service_bootstrap::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::DeviceBootstrap) => {
            routes.route(route.path, post(device_bootstrap::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::DeviceEnroll) => {
            routes.route(route.path, post(device_enroll::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::ContextRefresh) => {
            routes.route(route.path, post(refresh_context::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::GetFlow) => {
            routes.route(route.path, get(get_flow::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::GetPortalFlow) => {
            routes.route(route.path, post(get_portal_flow::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::LocalLogin) => {
            routes.route(route.path, post(local_login::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::RegisterLocal) => {
            routes.route(route.path, post(register_local::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::GetAccountFlow) => {
            routes.route(route.path, get(get_account_flow::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::CompleteFirstAdmin) => {
            routes.route(route.path, post(complete_admin_account::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::StartOidc) => {
            routes.route(route.path, get(start_oidc::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::StartAccountFlowOidc) => {
            routes.route(route.path, get(start_account_flow_oidc::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::OidcCallback) => {
            routes.route(route.path, get(oidc_callback::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::DecideApproval) => {
            routes.route(route.path, post(decide_approval::<R, E>))
        }
        (RouteMethod::Post, RouteHandler::BindFlow) => {
            routes.route(route.path, post(bind_flow::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::PortalIndex) => {
            routes.route(route.path, get(portal_index::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::PortalPage) => {
            routes.route(route.path, get(portal_page::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::PortalAsset) => {
            routes.route(route.path, get(portal_asset::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::ConsoleIndex) => {
            routes.route(route.path, get(console_index::<R, E>))
        }
        (RouteMethod::Get, RouteHandler::ConsolePage) => {
            routes.route(route.path, get(console_page::<R, E>))
        }
        _ => unreachable!("auth route inventory method and handler disagree"),
    }
}

pub(crate) fn router<R, E>(
    options: AuthHttpOptions<R, E>,
) -> Result<Router, AuthorizationStateError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + AuthorizationContextRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone + Send + Sync + 'static,
{
    fn resolve_source(
        source: Option<WebSourceConfig>,
        name: &str,
    ) -> Result<Option<WebSource>, AuthorizationStateError> {
        source
            .map(|source| match source {
                WebSourceConfig::Directory(path) => Ok(WebSource::Directory(path)),
                WebSourceConfig::Proxy(value) => {
                    let _ = rustls::crypto::ring::default_provider().install_default();
                    let url = Url::parse(&value).map_err(|_| {
                        AuthorizationStateError::InvalidRecord(format!(
                            "{name} proxy URL is invalid: {value}"
                        ))
                    })?;
                    if !matches!(url.scheme(), "http" | "https")
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.query().is_some()
                        || url.fragment().is_some()
                    {
                        return Err(AuthorizationStateError::InvalidRecord(format!(
                            "{name} proxy URL must be an HTTP(S) base URL without credentials, query, or fragment"
                        )));
                    }
                    let proxy: Router =
                        axum_reverse_proxy::ReverseProxy::new("/", url.as_str()).into();
                    Ok(WebSource::Proxy(proxy))
                }
            })
            .transpose()
    }

    let web_source =
        resolve_source(options.web_source, "default web source")?.unwrap_or(WebSource::Embedded);
    let portal_source = resolve_source(options.portal_source, "login Portal")?
        .unwrap_or_else(|| web_source.clone());
    let console_source_is_override = options.console_source.is_some();
    let console_source =
        resolve_source(options.console_source, "Console")?.unwrap_or_else(|| web_source.clone());
    for (name, source, entry) in [
        ("default web source", &web_source, "200.html"),
        ("login Portal", &portal_source, "200.html"),
        (
            "Console",
            &console_source,
            if console_source_is_override {
                "index.html"
            } else {
                "200.html"
            },
        ),
    ] {
        match source {
            WebSource::Directory(directory) if !directory.join(entry).is_file() => {
                return Err(AuthorizationStateError::InvalidRecord(format!(
                    "{name} directory '{}' must contain {entry}",
                    directory.display()
                )));
            }
            WebSource::Embedded if matches!(EMBEDDED_WEB_ASSETS, []) => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "embedded web assets are unavailable".to_owned(),
                ));
            }
            _ => {}
        }
    }
    let public_url = Url::parse(&options.public_origin).map_err(|_| {
        AuthorizationStateError::InvalidRecord("HTTP public origin is invalid".to_owned())
    })?;
    let use_hsts = public_url.scheme() == "https";
    let allowed_origins = if options.allowed_origins.is_empty() {
        vec![options.public_origin.clone()]
    } else {
        options.allowed_origins
    };
    let allowed_redirect_origins = allowed_origins
        .iter()
        .filter(|origin| origin.as_str() != "*")
        .chain(std::iter::once(&options.public_origin))
        .map(|origin| {
            canonical_origin(origin).map_err(|_| {
                AuthorizationStateError::InvalidRecord(format!("HTTP origin is invalid: {origin}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cors = if allowed_origins.iter().any(|origin| origin == "*") {
        CorsLayer::new()
            .allow_origin(AllowOrigin::any())
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers(AllowHeaders::mirror_request())
    } else {
        let origins = allowed_origins
            .iter()
            .map(|origin| {
                HeaderValue::from_str(origin).map_err(|_| {
                    AuthorizationStateError::InvalidRecord(format!(
                        "HTTP origin is invalid: {origin}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers(AllowHeaders::mirror_request())
            .allow_credentials(true)
    };
    let content_security_policy = super::security::content_security_policy(
        &options.websocket_nats_servers,
    )
    .map_err(|error| {
        AuthorizationStateError::InvalidRecord(format!(
            "WebSocket NATS URL is invalid for Content Security Policy: {error}"
        ))
    })?;
    let state = AuthHttpState {
        nats: options.nats,
        service: options.service,
        ephemeral: options.ephemeral,
        issuer: options.issuer,
        authorization_contexts: options.authorization_contexts,
        public_origin: options.public_origin,
        allowed_redirect_origins,
        native_nats_servers: options.native_nats_servers,
        websocket_nats_servers: options.websocket_nats_servers,
        oidc_providers: options.oidc_providers,
        proof_policy: trellis_protocol::SessionProofPolicy::default(),
        web_source,
        portal_source,
        console_source,
        console_source_is_override,
        browser_flow_ttl_ms: options.browser_flow_ttl_ms,
    };
    let mut api_routes: Router<AuthHttpState<R, E>> = Router::new();
    let mut static_routes: Router<AuthHttpState<R, E>> = Router::new();
    for route in ROUTES {
        if route.is_static() {
            static_routes = add_route::<R, E>(static_routes, *route);
        } else {
            api_routes = add_route::<R, E>(api_routes, *route);
        }
    }
    if options.rate_limit_max > 0 {
        let mut builder = GovernorConfigBuilder::default();
        builder
            .period(Duration::from_millis(
                (options.rate_limit_window_ms / u64::from(options.rate_limit_max)).max(1),
            ))
            .burst_size(options.rate_limit_max);
        let config = builder.finish().ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("HTTP rate limit is invalid".to_owned())
        })?;
        api_routes = api_routes.layer(GovernorLayer::new(Arc::new(config)));
    }
    let mut routes = api_routes
        .merge(static_routes)
        .fallback(web_fallback::<R, E>)
        .layer(RequestBodyLimitLayer::new(MAX_AUTH_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn(super::telemetry::observe_http))
        .layer(cors)
        .layer(middleware::from_fn_with_state(
            content_security_policy,
            security_headers,
        ))
        .with_state(state);
    if use_hsts {
        routes = routes.layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ));
    }
    Ok(routes)
}
