//! Platform subsystem scaffold.

use std::path::Path;
use std::sync::Arc;

/// Rust-owned authorization state and materialization.
pub mod auth;
pub mod auth_callout;
mod auth_operation;
mod auth_post_commit;
pub mod bootstrap;
mod builtin_live_provider;
mod live_provider;
mod state;

use auth::{
    portal_policy_reconciliation, AuthService, AuthServiceConfig, AuthorityEvidenceRepository,
    DeploymentProfileCreation, DeploymentProfileRecord, DeploymentProfileState, DeploymentRecord,
    DeploymentRepository, FirstAdminAuthorityTarget, IdempotencyResultRecord, LoginPortalMutation,
    LoginPortalRecord, LoginSettingsRecord, ParticipantBindingRecord, PortalRepository,
    PrincipalKind, PrincipalRecord, PrincipalState, ResourceBindingEvidence, ResourceBindingState,
    ResourceProviderIdentity, RuntimeInstanceRecord, RuntimeInstanceState,
    SqliteAuthorizationStore,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::Value;
use trellis_rs::client::SessionAuth;
use trellis_rs::service::Router;

use crate::shutdown::StopHandle;
use crate::supervisor::{NatsEndpointOverride, RuntimeContext, RuntimeError, SubsystemHandle};
use crate::{ResolvedRuntimeNatsConfig, RuntimeConfig, SubsystemName};
use auth::rpc::{AuthRpcProcessor, AuthRpcRuntime};
use auth_callout::{AuthCallout, CalloutKeys};
use auth_operation::AuthOperationRuntime;
use auth_post_commit::{AuthEventPublisher, AuthPostCommitRuntime};
pub(crate) use builtin_live_provider::{
    connect_builtin_live_provider, BuiltinLiveProviderConnectOptions,
};
pub(crate) use live_provider::{
    await_live_owner, ensure_live_provider_resources, ensure_live_provider_seed,
    load_live_provider_seed, LiveProviderRole, LiveProviderSlots,
};

/// Browser-flow records are retained for one day, so a configured pending-auth
/// TTL beyond that would advertise flows after their record has expired.
const BROWSER_FLOW_TTL_MAX_MS: i64 = 86_400_000;

/// Resolve the configured browser-flow TTL, bounded by the positive check and
/// the browser-flow record retention window.
fn browser_flow_ttl_ms(config: &RuntimeConfig) -> Result<i64, RuntimeError> {
    let Some(ttl) = config
        .platform
        .as_ref()
        .and_then(|platform| platform.ttl_ms.as_ref())
        .and_then(|ttl| ttl.pending_auth)
    else {
        return Ok(15 * 60_000);
    };
    match i64::try_from(ttl) {
        Ok(ttl) if ttl > 0 && ttl <= BROWSER_FLOW_TTL_MAX_MS => Ok(ttl),
        _ => Err(RuntimeError::Platform(format!(
            "platform.ttl_ms.pending_auth must be a positive millisecond value no greater than {BROWSER_FLOW_TTL_MAX_MS}"
        ))),
    }
}

pub(crate) async fn start(context: &RuntimeContext) -> Result<SubsystemHandle, RuntimeError> {
    let _owner = context.owner(crate::ownership::OwnerGroup::Platform)?;
    let auth_store = SqliteAuthorizationStore::open(context.stores.platform()?)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| RuntimeError::Platform("current time exceeds i64 milliseconds".to_owned()))?;
    let authorization_config = context
        .config
        .resolve_authorization()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let cli = auth::cli_participant_binding(now)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    auth_store
        .put_participant_binding(cli.clone())
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let cli_revision = auth_store
        .get_installed_participant_record(cli.participant_id.clone(), None)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .ok_or_else(|| RuntimeError::Platform("CLI participant installation missing".to_owned()))?
        .0;
    let console = auth::console_participant_binding(now)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    auth_store
        .put_participant_binding(console.clone())
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let console_revision = auth_store
        .get_installed_participant_record(console.participant_id.clone(), None)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .ok_or_else(|| {
            RuntimeError::Platform("Console participant installation missing".to_owned())
        })?
        .0;
    let portal = auth::portal_participant_binding(now)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    auth_store
        .put_participant_binding(portal.clone())
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    auth_store
        .ensure_admin_capability_group(now)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let auth_participant = auth::auth_runtime_participant_binding(now)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    auth_store
        .put_participant_binding(auth_participant.clone())
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    for (deployment_id, display_name, participant) in [
        (
            "dep_trellis_events_runtime",
            "Trellis Events Runtime",
            auth::events_runtime_participant_binding(now)
                .map_err(|error| RuntimeError::Platform(error.to_string()))?,
        ),
        (
            "dep_trellis_health_runtime",
            "Trellis Health Runtime",
            auth::health_runtime_participant_binding(now)
                .map_err(|error| RuntimeError::Platform(error.to_string()))?,
        ),
        (
            "dep_trellis_jobs_runtime",
            "Trellis Jobs Runtime",
            auth::jobs_runtime_participant_binding(now)
                .map_err(|error| RuntimeError::Platform(error.to_string()))?,
        ),
    ] {
        auth_store
            .put_participant_binding(participant.clone())
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        ensure_builtin_provider_deployment(
            &auth_store,
            deployment_id,
            display_name,
            &participant,
            now,
        )
        .await?;
    }
    ensure_builtin_portal(&auth_store, now).await?;
    let nats = context
        .config
        .resolve_nats_runtime_with(context.nats_override.as_ref().map(|o| o.servers.as_str()))
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let callout = context
        .config
        .resolve_nats_auth_callout()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let user_jwt_ttl_ms = auth_callout::resolve_user_jwt_ttl_ms(
        context
            .config
            .platform
            .as_ref()
            .and_then(|platform| platform.ttl_ms.as_ref())
            .and_then(|ttl| ttl.nats_jwt),
    )
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let connection_max_age = auth_callout::connection_presence_max_age(user_jwt_ttl_ms)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let ephemeral =
        auth::NatsAuthEphemeralRepository::ensure(context.trellis_nats.clone(), connection_max_age)
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let public_origin = context.config.public_origin();
    let allow_insecure_origin = context.config.public_origin_allows_insecure();
    let authorization_contexts = auth::AuthorizationContextService::start(
        Arc::new(auth_store.clone()),
        context.trellis_nats.clone(),
        authorization_config.clone(),
        public_origin.clone(),
        allow_insecure_origin,
        now / 1_000,
    )
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let issuer = auth::NatsBootstrapIssuer::from_files(
        &callout.issuer_signing_seed_file,
        &nats.auth_creds_path,
        authorization_config.maximum_bootstrap_jwt_lifetime_seconds,
    )
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let auth_nats = connect_nats(&nats.servers, &nats.auth_creds_path).await?;
    let system_nats = connect_nats(&nats.servers, &nats.system_creds_path).await?;
    let rpc_nats = context.trellis_nats.clone();
    let post_commit_nats = context.trellis_nats.clone();
    let http_nats = post_commit_nats.clone();
    let post_commit_system_nats = system_nats.clone();
    let auth_service = AuthService::new(auth_store.clone(), AuthServiceConfig::default())
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let (portal_reconciliation, portal_reconciliation_worker) =
        portal_policy_reconciliation(auth_service.clone());
    portal_reconciliation_worker
        .reconcile_startup()
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let callout_runtime = AuthCallout::start(
        auth_nats,
        system_nats,
        ephemeral.clone(),
        auth_store.clone(),
        authorization_contexts.clone(),
        CalloutKeys::from_files(
            &callout.issuer_signing_seed_file,
            &callout.target_signing_seed_file,
            &callout.xkey_seed_file,
            &nats.auth_creds_path,
            &nats.trellis_creds_path,
        )
        .map_err(|error| RuntimeError::Platform(error.to_string()))?,
        user_jwt_ttl_ms,
    )
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let (auth_event_session, auth_operation_session, event_identity_key_id, event_connection_id) =
        ensure_auth_event_session(&auth_service, &auth_participant, now).await?;
    // Built-in live providers need normal authenticated identities before any
    // live-capable router registers. The platform owner is the only writer, so
    // every reserved role is provisioned here under its fixed deployment.
    for role in LiveProviderRole::all() {
        let provider = ensure_live_provider_seed(&auth_service, &context.config, role, now).await?;
        ensure_live_provider_resources(&auth_service, role, &provider).await?;
    }
    let event_context = authorization_contexts
        .issue(
            auth::context::AuthorizationContextIssueRequest {
                connection: auth::IssuanceConnection {
                    credential: auth::IssuanceCredential::Native(event_identity_key_id.clone()),
                    connection_id: event_connection_id.clone(),
                    session_public_key: auth_event_session.session_key.clone(),
                },
                request_id: ulid::Ulid::new().to_string(),
                request_digest: trellis_protocol::digest_json(&serde_json::json!({
                    "purpose": "auth.event_session.context",
                    "sessionKey": auth_event_session.session_key,
                }))
                .map_err(|error| RuntimeError::Platform(error.to_string()))?,
            },
            now / 1_000,
        )
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let event_context = trellis_protocol::parse_authorization_context(&event_context.context)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let event_context_digest = event_context
        .digest()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    if let Some(digest_path) = context.config.event_context_digest_file.as_ref() {
        std::fs::write(digest_path, format!("{event_context_digest}\n")).map_err(|error| {
            RuntimeError::Platform(format!(
                "failed to write '{}': {error}",
                digest_path.display()
            ))
        })?;
    }

    let (native_nats_servers, websocket_nats_servers) =
        advertised_endpoints(&context.config, &nats, context.nats_override.as_ref());
    let (stop, mut validator_join, verifier) =
        start_validator_cache(context, &authorization_contexts).await?;
    let state = state::StateRuntime::start(
        context.trellis_nats.clone(),
        auth_store.clone(),
        verifier.clone(),
    )
    .await?;
    let auth_operation = AuthOperationRuntime::new(
        context.trellis_nats.clone(),
        auth_operation_session,
        auth_service.clone(),
        verifier.clone(),
        context.live_providers.receiver(LiveProviderRole::Platform),
    )
    .await?;
    let mut auth_rpc_routes = Router::new();
    auth_rpc_routes.set_provider_deployment_id("dep_trellis_auth_runtime");
    trellis_runtime_apis::apis::trellis_auth_v1::register_rpc_metadata(&mut auth_rpc_routes);
    trellis_runtime_apis::apis::trellis_core_v1::register_rpc_metadata(&mut auth_rpc_routes);
    let auth_rpc = AuthRpcRuntime::start(AuthRpcProcessor {
        client: rpc_nats,
        service: auth_service.clone(),
        ephemeral: ephemeral.clone(),
        public_origin: public_origin.clone(),
        verifier: verifier.clone(),
        routes: Arc::new(auth_rpc_routes),
        portal_reconciliation: portal_reconciliation.clone(),
    })
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let auth_post_commit = AuthPostCommitRuntime::new(
        auth_service.repository().clone(),
        ephemeral.clone(),
        post_commit_nats,
        post_commit_system_nats,
        AuthEventPublisher::new(
            auth_event_session,
            event_identity_key_id,
            event_connection_id,
            event_context_digest,
        ),
        authorization_contexts.clone(),
    );
    ensure_first_admin(
        &auth_service,
        &[
            FirstAdminAuthorityTarget {
                participant_id: cli.participant_id,
                installed_revision: cli_revision,
            },
            FirstAdminAuthorityTarget {
                participant_id: console.participant_id,
                installed_revision: console_revision,
            },
        ],
        &public_origin,
        context.reset_admin,
        now,
    )
    .await?;
    // Startup provisioning enqueues post-commit resource reconciliation for the
    // reserved provider deployments. That reconciliation can rewrite resource
    // evidence and the grant binding (and therefore the issuance snapshot token)
    // after a provider has issued its context, so a provider that bootstraps
    // while it is still in flight is denied at callout admission. Drain ready
    // startup effects before any router serves or any provider connects.
    // ponytail: fixed 30s convergence window, matching the validator-cache
    // startup wait; widen if a real startup reconcile exceeds it.
    let reconcile_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match auth_post_commit.dispatch_ready().await {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                stop.stop();
                validator_join.abort();
                return Err(RuntimeError::Platform(format!(
                    "auth startup reconciliation: {error}"
                )));
            }
        }
        if std::time::Instant::now() >= reconcile_deadline {
            stop.stop();
            validator_join.abort();
            return Err(RuntimeError::Platform(
                "auth startup reconciliation did not settle before live provider bootstrap"
                    .to_owned(),
            ));
        }
    }
    let http = context.config.http.as_ref();
    let browser_flow_ttl_ms = match browser_flow_ttl_ms(&context.config) {
        Ok(ttl) => ttl,
        Err(error) => {
            stop.stop();
            validator_join.abort();
            return Err(error);
        }
    };
    let oidc_providers =
        match auth::discover_oidc_providers(context.config.oauth.as_ref(), &public_origin).await {
            Ok(providers) => providers,
            Err(error) => {
                stop.stop();
                validator_join.abort();
                return Err(RuntimeError::Platform(error.to_string()));
            }
        };
    let router = match auth::auth_http_router(auth::AuthHttpOptions {
        nats: http_nats,
        service: auth_service,
        ephemeral,
        issuer,
        authorization_contexts: authorization_contexts.clone(),
        public_origin,
        allowed_origins: http
            .and_then(|http| http.origins.clone())
            .unwrap_or_default(),
        native_nats_servers,
        websocket_nats_servers,
        oidc_providers,
        rate_limit_max: http.and_then(|http| http.rate_limit_max).unwrap_or(100),
        rate_limit_window_ms: http
            .and_then(|http| http.rate_limit_window_ms)
            .unwrap_or(60_000),
        browser_flow_ttl_ms,
        web_source: http.and_then(|http| http.web_source.clone()),
        portal_source: http.and_then(|http| http.portal_source.clone()),
        console_source: http.and_then(|http| http.console_source.clone()),
    }) {
        Ok(router) => router,
        Err(error) => {
            stop.stop();
            validator_join.abort();
            return Err(RuntimeError::Platform(error.to_string()));
        }
    };
    if let Err(error) = context.register_http_router(router) {
        stop.stop();
        validator_join.abort();
        return Err(error);
    }
    let task_stop = stop.clone();
    let sampler_store = auth_store.clone();
    let sampler_stop = stop.clone();
    // Telemetry samplers own their tasks and never own business lifetime.
    let samplers = crate::telemetry::snapshots::SamplerOwner::start(vec![Box::pin(async move {
        let _ = crate::telemetry::snapshots::run_auth_sampler(sampler_store, sampler_stop).await;
    })]);
    let join = tokio::spawn(async move {
        let samplers = samplers;
        let result = tokio::select! {
            result = portal_reconciliation_worker.run(task_stop.clone()) => {
                result.map_err(|error| RuntimeError::Platform(error.to_string()))
            }
            result = callout_runtime.run(task_stop.clone()) => result,
            result = auth_rpc.run(task_stop.clone()) => result,
            result = auth_operation.run(task_stop.clone()) => result,
            result = auth_post_commit.run(task_stop.clone()) => result,
            result = state.run(task_stop.clone()) => result,
            result = authorization_contexts.clone().run_janitor(task_stop.clone()) => result,
            result = &mut validator_join => {
                match result {
                    Ok(result) => result,
                    Err(error) => Err(RuntimeError::Platform(format!(
                        "authorization validator cache task failed: {error}"
                    ))),
                }
            },
        };
        // Telemetry samplers own their tasks and never own business lifetime.
        samplers.stop().await;
        result
    });

    Ok(SubsystemHandle {
        name: SubsystemName::Platform,
        stop,
        join,
    })
}

async fn connect_nats(
    servers: &str,
    credentials: &Path,
) -> Result<async_nats::Client, RuntimeError> {
    async_nats::ConnectOptions::new()
        .subscription_capacity(256)
        .credentials_file(credentials)
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))?
        .connect(servers)
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))
}

async fn start_validator_cache(
    context: &RuntimeContext,
    authorization_contexts: &auth::AuthorizationContextService,
) -> Result<
    (
        StopHandle,
        tokio::task::JoinHandle<Result<(), RuntimeError>>,
        auth::verifier::RuntimeAuthVerifier,
    ),
    RuntimeError,
> {
    let stop = StopHandle::new();
    let validator_stop = stop.clone();
    let validator_contexts = authorization_contexts.clone();
    let mut validator_join =
        tokio::spawn(async move { validator_contexts.run_validator_cache(validator_stop).await });
    tokio::select! {
        result = authorization_contexts.wait_for_validator_cache() => {
            if let Err(error) = result {
                stop.stop();
                validator_join.abort();
                return Err(error);
            }
        }
        result = &mut validator_join => {
            return match result {
                Ok(result) => result.and_then(|_| Err(RuntimeError::Platform(
                    "authorization validator cache exited during startup".to_owned(),
                ))),
                Err(error) => Err(RuntimeError::Platform(format!(
                    "authorization validator cache task failed: {error}"
                ))),
            };
        }
    }
    let verifier = auth::verifier::RuntimeAuthVerifier::new(Arc::new(
        authorization_contexts.validator_cache(),
    ));
    if context.platform_verifier.set(verifier.clone()).is_err() {
        stop.stop();
        validator_join.abort();
        return Err(RuntimeError::Platform(
            "runtime-local auth verifier was already installed".to_owned(),
        ));
    }
    Ok((stop, validator_join, verifier))
}

async fn ensure_first_admin(
    service: &AuthService<SqliteAuthorizationStore>,
    authority_targets: &[FirstAdminAuthorityTarget],
    public_origin: &str,
    reset_admin: bool,
    now: i64,
) -> Result<(), RuntimeError> {
    let bootstrap = if reset_admin {
        service
            .rotate_admin_account_flow(public_origin, authority_targets, now)
            .await
    } else {
        service
            .ensure_admin_account_flow(public_origin, authority_targets, now)
            .await
    }
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;

    if let Some(bootstrap) = bootstrap {
        if let Some(bootstrap_url) = bootstrap.bootstrap_url {
            tracing::warn!(
                adminAccountUrl = %bootstrap_url,
                expiresAt = bootstrap.expires_at,
                "administrator account setup or recovery requested"
            );
        } else {
            tracing::warn!(
                status = "pending",
                flowIdHash = %bootstrap.flow_id_hash,
                expiresAt = bootstrap.expires_at,
                "first administrator bootstrap already pending"
            );
        }
    }
    Ok(())
}

async fn ensure_builtin_provider_deployment(
    repository: &SqliteAuthorizationStore,
    deployment_id: &str,
    display_name: &str,
    participant: &ParticipantBindingRecord,
    now: i64,
) -> Result<(), RuntimeError> {
    if repository
        .get_deployment_profile(deployment_id)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .is_none()
    {
        let principal = PrincipalRecord {
            principal_id: deployment_id.to_owned(),
            kind: PrincipalKind::Service,
            state: PrincipalState::Active,
            created_at: now,
            updated_at: now,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        auth::validate_principal(&principal)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let request_digest = trellis_protocol::digest_json(&serde_json::json!({
            "deploymentId": deployment_id,
            "participantId": participant.participant_id,
        }))
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        repository
            .create_deployment_profile(DeploymentProfileCreation {
                principal,
                profile: DeploymentProfileRecord {
                    deployment_id: deployment_id.to_owned(),
                    kind: PrincipalKind::Service,
                    display_name: display_name.to_owned(),
                    participant_id: Some(participant.participant_id.clone()),
                    portal_id: None,
                    review_mode: None,
                    requires_device_delegation: false,
                    expires_at: None,
                    state: DeploymentProfileState::Active,
                    created_at: now,
                    updated_at: now,
                    version: 1,
                },
                idempotency: IdempotencyResultRecord {
                    scope_key: request_digest.clone(),
                    purpose: "builtin.provider.start".to_owned(),
                    signer_id: "system:startup".to_owned(),
                    request_id: deployment_id.to_owned(),
                    request_digest,
                    result: serde_json::json!({ "deploymentId": deployment_id }),
                    created_at: now,
                    expires_at: auth::MAX_PROTOCOL_INTEGER as i64,
                },
                actions: Vec::new(),
            })
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    }
    repository
        .put_deployment_evidence(DeploymentRecord {
            deployment_id: deployment_id.to_owned(),
            participant_id: participant.participant_id.clone(),
            participant_kind: participant.participant_kind,
            active: true,
            expires_at: None,
        })
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    Ok(())
}

async fn ensure_auth_event_session(
    service: &AuthService<SqliteAuthorizationStore>,
    participant: &ParticipantBindingRecord,
    now: i64,
) -> Result<(SessionAuth, SessionAuth, String, String), RuntimeError> {
    const DEPLOYMENT_ID: &str = "dep_trellis_auth_runtime";

    if service
        .repository()
        .get_deployment_profile(DEPLOYMENT_ID)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .is_none()
    {
        let principal = PrincipalRecord {
            principal_id: DEPLOYMENT_ID.to_owned(),
            kind: PrincipalKind::Service,
            state: PrincipalState::Active,
            created_at: now,
            updated_at: now,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        auth::validate_principal(&principal)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let request_digest = trellis_protocol::digest_json(&serde_json::json!({
            "deploymentId": DEPLOYMENT_ID,
            "participantId": participant.participant_id,
        }))
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        service
            .repository()
            .create_deployment_profile(DeploymentProfileCreation {
                principal,
                profile: DeploymentProfileRecord {
                    deployment_id: DEPLOYMENT_ID.to_owned(),
                    kind: PrincipalKind::Service,
                    display_name: "Trellis Auth Runtime".to_owned(),
                    participant_id: Some(participant.participant_id.clone()),
                    portal_id: None,
                    review_mode: None,
                    requires_device_delegation: false,
                    expires_at: None,
                    state: DeploymentProfileState::Active,
                    created_at: now,
                    updated_at: now,
                    version: 1,
                },
                idempotency: IdempotencyResultRecord {
                    scope_key: request_digest.clone(),
                    purpose: "auth.event_deployment.start".to_owned(),
                    signer_id: "system:startup".to_owned(),
                    request_id: "builtin-v1".to_owned(),
                    request_digest,
                    result: serde_json::json!({ "deploymentId": DEPLOYMENT_ID }),
                    created_at: now,
                    expires_at: auth::MAX_PROTOCOL_INTEGER as i64,
                },
                actions: Vec::new(),
            })
            .await
            .map_err(|error| {
                RuntimeError::Platform(format!("create Auth event deployment: {error}"))
            })?;
    }
    service
        .repository()
        .put_deployment_evidence(DeploymentRecord {
            deployment_id: DEPLOYMENT_ID.to_owned(),
            participant_id: participant.participant_id.clone(),
            participant_kind: participant.participant_kind,
            active: true,
            expires_at: None,
        })
        .await
        .map_err(|error| RuntimeError::Platform(format!("store Auth deployment: {error}")))?;
    let installed = service
        .repository()
        .get_installed_participant(participant.participant_id.clone(), None)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let installed_revision = installed
        .get("participant")
        .and_then(|value| value.get("revision"))
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            RuntimeError::Platform("installed Auth participant revision is missing".to_owned())
        })?;
    let resources = [
        (
            "browserFlows",
            "trellis_auth_browser_flows",
            86_400_000,
            65_536,
        ),
        ("oauthStates", "trellis_auth_oauth_states", 900_000, 16_384),
        ("connections", "trellis_auth_connections", 120_000, 16_384),
    ]
    .into_iter()
    .map(
        |(local_name, bucket, ttl_ms, max_value_bytes)| ResourceBindingEvidence {
            resource_kind: "kv".to_owned(),
            local_name: local_name.to_owned(),
            binding_id: format!("binding:{DEPLOYMENT_ID}:kv:{local_name}"),
            owner_participant_id: participant.participant_id.clone(),
            provider_identity: ResourceProviderIdentity::Kv {
                bucket: bucket.to_owned(),
            },
            actual: Some(auth::resources::ResourceActual::Kv {
                history: 1,
                ttl_ms,
                max_value_bytes: Some(max_value_bytes),
            }),
            state: ResourceBindingState::Available,
            materialized_at: now,
            error: None,
        },
    )
    .collect::<Vec<_>>();
    service
        .repository()
        .replace_resource_bindings(
            auth::GrantOwnerKind::Deployment,
            DEPLOYMENT_ID.to_owned(),
            participant.participant_id.clone(),
            installed_revision,
            resources.clone(),
        )
        .await
        .map_err(|error| RuntimeError::Platform(format!("bind Auth resources: {error}")))?;
    let current = service
        .repository()
        .get_grant_binding(
            auth::GrantOwnerKind::Deployment,
            DEPLOYMENT_ID.to_owned(),
            participant.participant_id.clone(),
        )
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let expected_revision = current.as_ref().map_or(0, |binding| binding.revision);
    if current.as_ref().is_none_or(|binding| {
        binding.installed_revision != installed_revision
            || binding.state != auth::GrantBindingState::Active
    }) {
        let grants = participant
            .resolve()
            .map_err(|error| RuntimeError::Platform(error.to_string()))?
            .select_grants(&[])
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let digest = trellis_protocol::digest_json(&serde_json::json!({ "ownerId": DEPLOYMENT_ID, "participantId": participant.participant_id, "installedRevision": installed_revision, "grants": grants })).map_err(|error| RuntimeError::Platform(error.to_string()))?;
        service
            .repository()
            .set_grant_binding(
                auth::GrantBindingReplacement {
                    owner_kind: auth::GrantOwnerKind::Deployment,
                    owner_id: DEPLOYMENT_ID.to_owned(),
                    participant_id: participant.participant_id.clone(),
                    installed_revision,
                    grants: grants.clone(),
                    approval_mode: auth::ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: auth::policy::participant_resource_commitments(participant)
                        .map_err(|error| RuntimeError::Platform(error.to_string()))?,
                    delegation_ceiling: auth::DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(grants),
                        platform_privileges: Vec::new(),
                    },
                    approval_decision_digest: digest.clone(),
                    companion_approved: false,
                    platform_privileges: Vec::new(),
                    expected_revision,
                    expected_current_installed_revision: None,
                    state: auth::GrantBindingState::Active,
                    expires_at: None,
                    provenance: None,
                },
                IdempotencyResultRecord {
                    scope_key: digest.clone(),
                    purpose: "auth.event_binding.start".to_owned(),
                    signer_id: DEPLOYMENT_ID.to_owned(),
                    request_id: digest.clone(),
                    request_digest: digest,
                    result: Value::Null,
                    created_at: now,
                    expires_at: auth::MAX_PROTOCOL_INTEGER as i64,
                },
            )
            .await
            .map_err(|error| RuntimeError::Platform(format!("bind Auth deployment: {error}")))?;
    }
    service
        .repository()
        .replace_resource_bindings(
            auth::GrantOwnerKind::Deployment,
            DEPLOYMENT_ID.to_owned(),
            participant.participant_id.clone(),
            installed_revision,
            resources,
        )
        .await
        .map_err(|error| RuntimeError::Platform(format!("bind Auth resources: {error}")))?;
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let seed = URL_SAFE_NO_PAD.encode(seed);
    let event_auth = SessionAuth::from_seed_base64url(&seed)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let identity_key_id =
        auth::validate_ed25519_public_key("identityPublicKey", &event_auth.session_key)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let instance_id = ulid::Ulid::new().to_string();
    service
        .repository()
        .install_runtime_identity(
            RuntimeInstanceRecord {
                instance_id: instance_id.clone(),
                deployment_id: DEPLOYMENT_ID.to_owned(),
                principal_id: DEPLOYMENT_ID.to_owned(),
                state: RuntimeInstanceState::Active,
                created_at: now,
                updated_at: now,
                version: 1,
            },
            auth::ProvisionedIdentityRecord {
                identity_key_id: identity_key_id.clone(),
                identity_public_key: event_auth.session_key.clone(),
                principal_id: DEPLOYMENT_ID.to_owned(),
                deployment_id: DEPLOYMENT_ID.to_owned(),
                instance_id,
                kind: auth::ProvisionedIdentityKind::Service,
                state: auth::ProvisionedIdentityState::Active,
                created_at: now,
                revoked_at: None,
            },
        )
        .await
        .map_err(|error| RuntimeError::Platform(format!("install Auth identity: {error}")))?;
    let operation_auth = SessionAuth::from_seed_base64url(&seed)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    Ok((
        event_auth,
        operation_auth,
        identity_key_id,
        ulid::Ulid::new().to_string(),
    ))
}

async fn ensure_builtin_portal(
    store: &SqliteAuthorizationStore,
    now: i64,
) -> Result<(), RuntimeError> {
    if store
        .get_login_portal("builtin")
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    let request_digest = trellis_protocol::digest_json(&serde_json::json!({
        "portalId": "builtin",
        "version": 1,
    }))
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let portal = LoginPortalRecord {
        portal_id: "builtin".to_owned(),
        display_name: "Trellis".to_owned(),
        entry_url: None,
        builtin: true,
        disabled: false,
        removed: false,
        local_registration_enabled: false,
        provider_ids: vec!["local".to_owned()],
        created_at: now,
        updated_at: now,
        version: 1,
    };
    let settings = LoginSettingsRecord {
        portal_id: "builtin".to_owned(),
        default_provider_id: Some("local".to_owned()),
        local_login_enabled: true,
        federated_registration_enabled: true,
        provider_selection_enabled: false,
        updated_at: now,
        version: 1,
    };
    auth::validate_login_portal(&portal, &settings)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    store
        .put_login_portal(LoginPortalMutation {
            portal,
            settings,
            expected_version: None,
            idempotency: IdempotencyResultRecord {
                scope_key: request_digest.clone(),
                purpose: "portal.ensure_builtin".to_owned(),
                signer_id: "system:startup".to_owned(),
                request_id: "builtin-v1".to_owned(),
                request_digest,
                result: serde_json::json!({ "portalId": "builtin" }),
                created_at: now,
                expires_at: now.checked_add(86_400_000).ok_or_else(|| {
                    RuntimeError::Platform("portal idempotency expiry overflow".to_owned())
                })?,
            },
            actions: Vec::new(),
        })
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    Ok(())
}

/// Resolves the NATS endpoints advertised to clients in bootstrap responses.
///
/// With an override, the advertised native endpoint is always the override server URL
/// (managed or `--nats` external mode); the advertised websocket endpoint is replaced only
/// when the override carries one (managed mode). Without an override the configured client
/// values win, falling back to the resolved native server list.
fn advertised_endpoints(
    config: &RuntimeConfig,
    resolved: &ResolvedRuntimeNatsConfig,
    nats_override: Option<&NatsEndpointOverride>,
) -> (Vec<String>, Vec<String>) {
    let configured_native = || {
        config
            .client
            .as_ref()
            .and_then(|client| client.nats_servers.clone())
            .unwrap_or_else(|| resolved.servers.split(',').map(str::to_owned).collect())
    };
    let configured_websocket = || {
        config
            .client
            .as_ref()
            .and_then(|client| client.ws_nats_servers.clone())
            .unwrap_or_default()
    };
    match nats_override {
        Some(override_) => (
            vec![override_.servers.clone()],
            override_
                .websocket
                .as_ref()
                .map_or_else(configured_websocket, |websocket| vec![websocket.clone()]),
        ),
        None => (configured_native(), configured_websocket()),
    }
}

/// Seed a usable first local administrator with a username and password, without a browser
/// or account-flow round trip.
///
/// # Errors
///
/// Returns a platform error when the platform store cannot be opened or migrated, when the
/// built-in CLI/console participant bindings cannot be installed, or when the credentials do
/// not satisfy the configured password policy.
pub async fn seed_admin_credentials(
    config: &RuntimeConfig,
    username: &str,
    password: &str,
) -> Result<String, RuntimeError> {
    let crate::StorageBackend::Sqlite(storage) = config
        .platform_storage_backend()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let store = crate::storage::SqliteStore::new(SubsystemName::Platform, storage);
    store
        .migrate()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let auth_store = SqliteAuthorizationStore::open(&store)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| RuntimeError::Platform("current time exceeds i64 milliseconds".to_owned()))?;

    let mut targets = Vec::with_capacity(2);
    for binding in [
        auth::cli_participant_binding(now)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?,
        auth::console_participant_binding(now)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?,
    ] {
        auth_store
            .put_participant_binding(binding.clone())
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let revision = auth_store
            .get_installed_participant_record(binding.participant_id.clone(), None)
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?
            .ok_or_else(|| {
                RuntimeError::Platform(format!(
                    "{} participant installation missing",
                    binding.participant_id
                ))
            })?
            .0;
        targets.push(FirstAdminAuthorityTarget {
            participant_id: binding.participant_id,
            installed_revision: revision,
        });
    }

    let password_min_length = config
        .auth
        .as_ref()
        .and_then(|auth| auth.local_identity.as_ref())
        .and_then(|local| local.password_min_length)
        .map_or(12, usize::from);
    let service = AuthService::new(
        auth_store,
        auth::AuthServiceConfig {
            password_min_length,
            ..Default::default()
        },
    )
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let public_origin = config
        .http
        .as_ref()
        .and_then(|http| http.public_origin.clone())
        .unwrap_or_else(|| "http://localhost:3000".to_owned());
    service
        .seed_local_admin(&public_origin, &targets, username, password, now)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_flow_ttl_is_positive_and_bounded_by_record_retention() {
        let with_ttl = |value: u64| {
            RuntimeConfig::from_toml_str(&format!("[platform.ttl_ms]\npending_auth = {value}\n"))
                .expect("valid TOML")
        };
        let default = RuntimeConfig::from_toml_str("").expect("empty config");
        assert_eq!(browser_flow_ttl_ms(&default).expect("default"), 15 * 60_000);
        assert_eq!(
            browser_flow_ttl_ms(&with_ttl(5_000)).expect("short test ttl"),
            5_000
        );
        assert_eq!(
            browser_flow_ttl_ms(&with_ttl(86_400_000)).expect("retention bound"),
            86_400_000
        );
        assert!(browser_flow_ttl_ms(&with_ttl(0)).is_err());
        assert!(browser_flow_ttl_ms(&with_ttl(86_400_001)).is_err());
    }

    fn config_with_client_endpoints() -> RuntimeConfig {
        RuntimeConfig::from_toml_str(
            r#"
[nats]
servers = "nats://config.example:4222"
[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"
[client]
nats_servers = ["nats://advertised.example:4222"]
ws_nats_servers = ["ws://advertised.example:8080"]
"#,
        )
        .expect("parse config")
    }

    #[test]
    fn advertised_endpoints_without_override_use_configured_client_values() {
        let config = config_with_client_endpoints();
        let resolved = config.resolve_nats_runtime().expect("resolve nats");

        let (native, websocket) = advertised_endpoints(&config, &resolved, None);
        assert_eq!(native, vec!["nats://advertised.example:4222"]);
        assert_eq!(websocket, vec!["ws://advertised.example:8080"]);
    }

    #[test]
    fn advertised_endpoints_managed_override_replaces_both_endpoints() {
        let config = config_with_client_endpoints();
        let resolved = config.resolve_nats_runtime().expect("resolve nats");
        let override_ = NatsEndpointOverride {
            servers: "nats://127.0.0.1:4222".to_string(),
            websocket: Some("ws://127.0.0.1:8080".to_string()),
        };

        let (native, websocket) = advertised_endpoints(&config, &resolved, Some(&override_));
        assert_eq!(native, vec!["nats://127.0.0.1:4222"]);
        assert_eq!(websocket, vec!["ws://127.0.0.1:8080"]);
    }

    #[test]
    fn advertised_endpoints_external_override_replaces_native_only() {
        let config = config_with_client_endpoints();
        let resolved = config.resolve_nats_runtime().expect("resolve nats");
        let override_ = NatsEndpointOverride {
            servers: "nats://external.example:4222".to_string(),
            websocket: None,
        };

        let (native, websocket) = advertised_endpoints(&config, &resolved, Some(&override_));
        assert_eq!(native, vec!["nats://external.example:4222"]);
        assert_eq!(websocket, vec!["ws://advertised.example:8080"]);
    }

    #[tokio::test]
    async fn auth_event_identity_has_an_active_deployment_before_binding() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("open store");
        let participant = auth::auth_runtime_participant_binding(1).expect("participant");
        store
            .put_participant_binding(participant.clone())
            .await
            .expect("install participant");
        let service =
            AuthService::new(store.clone(), AuthServiceConfig::default()).expect("create service");
        ensure_auth_event_session(&service, &participant, 1)
            .await
            .expect("initialize Auth event identity");

        let profile = store
            .get_deployment_profile("dep_trellis_auth_runtime")
            .await
            .expect("load deployment")
            .expect("deployment profile");
        assert_eq!(profile.state, DeploymentProfileState::Active);
    }

    #[tokio::test]
    async fn fresh_builtin_provider_deployments_are_active_and_bound() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("open store");
        for (deployment_id, display_name, participant) in [
            (
                "dep_trellis_events_runtime",
                "Trellis Events Runtime",
                auth::events_runtime_participant_binding(1).expect("events participant"),
            ),
            (
                "dep_trellis_health_runtime",
                "Trellis Health Runtime",
                auth::health_runtime_participant_binding(1).expect("health participant"),
            ),
            (
                "dep_trellis_jobs_runtime",
                "Trellis Jobs Runtime",
                auth::jobs_runtime_participant_binding(1).expect("jobs participant"),
            ),
        ] {
            store
                .put_participant_binding(participant.clone())
                .await
                .expect("install participant");
            ensure_builtin_provider_deployment(
                &store,
                deployment_id,
                display_name,
                &participant,
                1,
            )
            .await
            .expect("install provider deployment");
            let profile = store
                .get_deployment_profile(deployment_id)
                .await
                .expect("load deployment")
                .expect("deployment profile");
            assert_eq!(profile.participant_id, Some(participant.participant_id));
            assert_eq!(profile.state, DeploymentProfileState::Active);
        }
    }
}
