//! Runtime ownership of the authoritative Auth store and its serving boundaries.

use std::{path::Path, sync::Arc, time::Duration};
use trellis_rs::__internal::{RuntimeOwner as _, RuntimeService as _};

pub mod auth;
pub mod auth_callout;
pub mod bootstrap;
mod builtin_live_provider;
mod console;
mod live_provider;
mod state;

pub(crate) use builtin_live_provider::{
    connect_builtin_live_provider, prepare_builtin_live_provider, BuiltinLiveProviderConnectOptions,
};
pub(crate) use live_provider::{
    await_live_owner, load_live_provider_seed, LiveProviderRole, LiveProviderSlots,
};

use crate::{
    shutdown::StopHandle,
    supervisor::{NatsEndpointOverride, RuntimeContext, RuntimeError, SubsystemHandle},
    ResolvedRuntimeNatsConfig, RuntimeConfig, SubsystemName,
};
use auth::SqliteAuthorizationStore;
use auth_callout::{AuthCallout, CalloutKeys};

/// Bounded work batches per wake before a worker yields to the next signal.
const EFFECT_DRAIN_TURNS: usize = 64;

pub(crate) async fn start(context: &RuntimeContext) -> Result<SubsystemHandle, RuntimeError> {
    let _owner = context.owner(crate::ownership::OwnerGroup::Platform)?;
    let nats = context
        .config
        .resolve_nats_runtime_with(
            context
                .nats_override
                .as_ref()
                .map(|value| value.runtime_servers.as_str()),
        )
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let keys = callout_keys(&context.config, &nats)?;
    let (platform_store, config, account) = (
        context.stores.platform()?.clone(),
        context.config.clone(),
        keys.target_account().to_owned(),
    );
    let store = Arc::new(
        auth::blocking(move || SqliteAuthorizationStore::open(&platform_store, &config, &account))
            .await?,
    );
    verify_jetstream_identity(&store, &context.trellis_nats).await?;
    let custody = context
        .config
        .auth
        .as_ref()
        .and_then(|auth| auth.oauth_storage_key_file.clone())
        .ok_or_else(|| RuntimeError::Platform("auth.oauth_storage_key_file is required".into()))?;
    let (opened, public_origin, keep_admin) = (
        store.clone(),
        context.config.public_origin(),
        !context.reset_admin,
    );
    let (engine, account_url) = auth::blocking(move || {
        let platform = |error: &dyn std::fmt::Display| RuntimeError::Platform(error.to_string());
        opened.initialize_issuer(crate::clock::unix_now())?;
        let storage =
            auth::OAuthStorage::open(opened.clone(), &custody).map_err(|error| platform(&error))?;
        let account_url = if !opened.has_administrator()? {
            storage
                .initialize_local_administrator(
                    &public_origin,
                    "admin",
                    None,
                    crate::clock::unix_now(),
                )
                .map_err(|error| platform(&error))?
                .1
        } else {
            storage
                .reset_local_administrator(crate::clock::unix_now(), keep_admin)
                .map_err(|error| platform(&error))?
        };
        let engine = auth::OAuthEngine::new(storage)?;
        Ok::<_, RuntimeError>((Arc::new(engine), account_url))
    })
    .await?;
    if let Some(url) = account_url {
        println!("{}", serde_json::json!({"adminAccountUrl":url}));
    }
    let oauth = context
        .config
        .resolve_oauth_listener()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let socket = tokio::net::TcpListener::bind((oauth.bind_address, oauth.port))
        .await
        .map_err(|error| {
            RuntimeError::Platform(format!("bind isolated OAuth listener: {error}"))
        })?;
    let listener = auth::OAuthListener::load(socket, &oauth.origin, &oauth.transport)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .forwarded_by(&oauth.trusted_proxies);
    let (native, websocket) =
        advertised_endpoints(&context.config, &nats, context.nats_override.as_ref());
    // Every Login Portal page shows the generated-CA notice for the
    // certificate this OAuth listener serves.
    engine.serve_generated_tls(crate::tls::serves_generated_certificate(
        &context.config,
        &oauth.transport,
    ));
    engine.serve_operational(context.trellis_nats.clone());
    let router = auth::oauth_router(
        engine.clone(),
        native.into_iter().chain(websocket).collect(),
    );
    let jetstream = async_nats::jetstream::new(context.trellis_nats.clone());
    let mut buckets = Vec::new();
    for name in [auth::MATERIAL_BUCKET, auth::REVOCATION_BUCKET] {
        let bucket = match jetstream
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: name.into(),
                storage: async_nats::jetstream::stream::StorageType::File,
                history: 1,
                ..Default::default()
            })
            .await
        {
            Ok(bucket) => bucket,
            Err(_) => jetstream.get_key_value(name).await.map_err(|error| {
                RuntimeError::Platform(format!("open Auth registry {name}: {error}"))
            })?,
        };
        let status = bucket
            .status()
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        // Cutoffs must publish before kicks and remain until the last issued
        // authority expires. Enable their per-entry TTL without replacing any
        // retained stream or evidence, including on existing installations.
        if name == auth::REVOCATION_BUCKET && !status.info.config.allow_message_ttl {
            let mut config = status.info.config.clone();
            config.allow_message_ttl = true;
            jetstream
                .update_stream(config)
                .await
                .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        }
        // Immutable material and still-usable cutoffs must survive restart and
        // must not be silently evicted by a configured bucket TTL.
        if status.info.config.storage != async_nats::jetstream::stream::StorageType::File
            || !status.max_age().is_zero()
        {
            return Err(RuntimeError::Platform(format!(
                "Auth registry {name} must retain durable verification evidence without expiry"
            )));
        }
        buckets.push(bucket);
    }
    let revocations = buckets.pop().expect("two Auth registries");
    let material = buckets.pop().expect("two Auth registries");
    let auth_nats = connect_nats(&nats, &nats.auth_creds_path).await?;
    let system = connect_nats(&nats, &nats.system_creds_path).await?;
    let provider_seed = live_provider::load_live_provider_seed(
        &context.config,
        live_provider::LiveProviderRole::Platform,
    )?;
    bootstrap::prepare_builtin_provider::<
        trellis_runtime_apis::participants::trellis_platform::Participant,
    >(&store, &context.trellis_nats, &provider_seed)
    .await?;
    while store
        .process_effects(&material, &revocations, &system, crate::clock::unix_now())
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        != 0
    {}
    let own_context = auth::OwnContextService::start(
        context.trellis_nats.clone(),
        material.clone(),
        store.clone(),
    )
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let callout = AuthCallout::start(auth_nats, system.clone(), store.clone(), keys)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    context
        .authorization_store
        .set(store.clone())
        .map_err(|_| {
            RuntimeError::Platform("Platform authorization store is already installed".into())
        })?;
    context.register_http_router(console::router(&context.config)?)?;
    let stop = StopHandle::new();
    let task_stop = stop.clone();
    let provider_store = store.clone();
    let provider_operational = context.trellis_nats.clone();
    let provider_roots: Vec<_> = nats.ca_certificate.iter().cloned().collect();
    let provider_servers = nats
        .servers
        .split(',')
        .map(|server| server.trim().to_owned())
        .collect();
    let join = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut tasks = tokio::task::JoinSet::new();
        let listener_cancel = cancel.clone();
        tasks.spawn(async move {
            listener
                .run(router, listener_cancel)
                .await
                .map_err(|error| RuntimeError::Platform(error.to_string()))
        });
        let callout_stop = task_stop.clone();
        tasks.spawn(async move { callout.run(callout_stop).await });
        let own_stop = task_stop.clone();
        tasks.spawn(async move {
            own_context
                .run(own_stop.stopped())
                .await
                .map_err(|error| RuntimeError::Platform(error.to_string()))
        });
        if trellis_rs::telemetry::process_metrics_enabled() {
            let sampler_store = store.clone();
            let sampler_stop = task_stop.clone();
            tasks.spawn(async move {
                crate::telemetry::snapshots::run_auth_sampler(sampler_store, sampler_stop)
                    .await
                    .map_err(|error| RuntimeError::Platform(error.to_string()))
            });
        }
        let worker_stop = task_stop.clone();
        let provider_oauth = engine.clone();
        tasks.spawn(async move {
            // Committed work wakes the worker at once; the tick retries backed-off
            // effects and expired claims, and prunes expired artifacts.
            let mut queued = store.work_signal();
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let ticked = tokio::select! {
                    () = worker_stop.stopped() => return Ok(()),
                    _ = interval.tick() => true,
                    changed = queued.changed() => {
                        if changed.is_err() {
                            return Ok(());
                        }
                        false
                    }
                };
                // Keep draining while turns make progress, but give other tasks a
                // turn between bounded batches.
                for _ in 0..EFFECT_DRAIN_TURNS {
                    match store
                        .process_effects(&material, &revocations, &system, crate::clock::unix_now())
                        .await
                    {
                        Ok(0) => break,
                        Ok(_) => tokio::task::yield_now().await,
                        Err(auth::AuthError::Broker(_) | auth::AuthError::Unavailable) => {
                            tracing::warn!(
                                "Auth broker effects unavailable; durable work retained"
                            );
                            break;
                        }
                        Err(error) => return Err(RuntimeError::Platform(error.to_string())),
                    }
                }
                if ticked {
                    let now = crate::clock::unix_now();
                    let (pruned, cleaned) = (store.clone(), engine.server.store().clone());
                    auth::blocking(move || {
                        pruned.prune_expired_artifacts(now)?;
                        cleaned
                            .cleanup_expired(now)
                            .map_err(|error| auth::AuthError::Invalid(error.to_string()))?;
                        Ok::<_, auth::AuthError>(())
                    })
                    .await
                    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
                }
            }
        });
        let provider_stop = task_stop.clone();
        tasks.spawn(async move {
            let event_store = provider_store.clone();
            let provider = tokio::select! {
                () = provider_stop.stopped() => return Ok(()),
                result = bootstrap::connect_platform_provider(
                    provider_store, provider_oauth, provider_operational, provider_servers,
                    provider_roots, &provider_seed,
                ) => result?,
            };
            let owner = provider.runtime_native_owner();
            let publisher = provider.event_publisher();
            let publications = async {
                let mut queued = event_store.work_signal();
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        () = provider_stop.stopped() => break Ok::<(), RuntimeError>(()),
                        _ = interval.tick() => {}
                        changed = queued.changed() => {
                            if changed.is_err() {
                                break Ok(());
                            }
                        }
                    }
                    // Claim each publication immediately before sending it, so
                    // a slow broker cannot consume another event's lease.
                    for _ in 0..EFFECT_DRAIN_TURNS {
                        let claimer = event_store.clone();
                        let Some(effect) = auth::blocking(move || {
                            claimer.claim_effects(1, crate::clock::unix_now(), 10, true)
                        })
                        .await
                        .map_err(|error| RuntimeError::Platform(error.to_string()))?
                        .into_iter()
                        .next() else {
                            break;
                        };
                        let result = async {
                            let text = |key: &str| {
                                effect.payload[key]
                                    .as_str()
                                    .ok_or(auth::AuthError::Unavailable)
                            };
                            let body = &effect.payload["event"];
                            let event = trellis_rs::client::PreparedTrellisEvent::restore(
                                text("subject")?,
                                serde_json::to_vec(body)?.into(),
                                text("descriptor")?,
                                body["eventId"]
                                    .as_str()
                                    .ok_or(auth::AuthError::Unavailable)?,
                                body["occurredAt"]
                                    .as_str()
                                    .ok_or(auth::AuthError::Unavailable)?,
                            )
                            .map_err(|error| auth::AuthError::Invalid(error.to_string()))?;
                            publisher
                                .publish_prepared(&event)
                                .await
                                .map_err(|error| auth::AuthError::Broker(error.to_string()))
                        }
                        .await;
                        let now = crate::clock::unix_now();
                        let settler = event_store.clone();
                        match result {
                            Ok(()) => {
                                auth::blocking(move || settler.complete_effect(&effect, now)).await
                            }
                            Err(error) => {
                                tracing::warn!(
                                    action_id=%effect.action_id,
                                    attempt=effect.attempts,
                                    %error,
                                    "Auth event publication remains pending"
                                );
                                auth::blocking(move || settler.retry_effect(&effect, now)).await
                            }
                        }
                        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
                    }
                }
            };
            let result = tokio::select! {
                result = provider.run() =>
                    result.map_err(|error| RuntimeError::Platform(error.to_string())),
                result = publications => result,
            };
            // Cancelling intake is not logical shutdown: retained callbacks and
            // publishers may still hold this separate Platform RPC connection.
            // End and join its recovery workers before the host stops NATS.
            let shutdown = owner
                .shutdown_native_runtime()
                .await
                .map_err(|error| RuntimeError::Platform(error.to_string()));
            result.and(shutdown)
        });
        let first = tokio::select! {
            ()=task_stop.stopped()=>Ok(()),
            result=tasks.join_next()=>match result {
                Some(Ok(Err(error)))=>Err(error),
                Some(Err(error))=>Err(RuntimeError::Platform(format!("Auth host task failed: {error}"))),
                Some(Ok(Ok(()))) if task_stop.is_stopped()=>Ok(()),
                _=>Err(RuntimeError::Platform("Auth host task stopped unexpectedly".into())),
            }
        };
        task_stop.stop();
        cancel.cancel();
        let drained = tokio::time::timeout(Duration::from_secs(12), async {
            while let Some(result) = tasks.join_next().await {
                result.map_err(|error| RuntimeError::Platform(error.to_string()))??;
            }
            Ok::<(), RuntimeError>(())
        })
        .await;
        first?;
        drained
            .map_err(|_| RuntimeError::Platform("Auth host shutdown deadline exceeded".into()))?
    }));
    Ok(SubsystemHandle {
        name: SubsystemName::Platform,
        stop,
        join,
    })
}

pub(crate) fn callout_keys(
    config: &RuntimeConfig,
    nats: &ResolvedRuntimeNatsConfig,
) -> Result<CalloutKeys, RuntimeError> {
    let callout = config
        .resolve_nats_auth_callout()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    CalloutKeys::from_files(
        &callout.issuer_signing_seed_file,
        &callout.target_signing_seed_file,
        &callout.xkey_seed_file,
        &callout.sentinel_public_key_file,
        &nats.auth_creds_path,
        &nats.trellis_creds_path,
    )
    .map_err(|error| RuntimeError::Platform(error.to_string()))
}

async fn connect_nats(
    nats: &ResolvedRuntimeNatsConfig,
    credentials: &Path,
) -> Result<async_nats::Client, RuntimeError> {
    let mut options = async_nats::ConnectOptions::with_credentials_file(credentials)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    if let Some(ca) = &nats.ca_certificate {
        options = options.add_root_certificates(ca.clone());
    }
    options
        .connect(&nats.servers)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))
}

fn advertised_endpoints(
    config: &RuntimeConfig,
    resolved: &ResolvedRuntimeNatsConfig,
    nats_override: Option<&NatsEndpointOverride>,
) -> (Vec<String>, Vec<String>) {
    let native = || {
        config
            .client
            .as_ref()
            .and_then(|value| value.nats_servers.clone())
            .unwrap_or_else(|| resolved.servers.split(',').map(str::to_owned).collect())
    };
    let websocket = || match config.same_origin_websocket_url() {
        Some(url) => vec![url],
        None => config
            .client
            .as_ref()
            .and_then(|value| value.ws_nats_servers.clone())
            .unwrap_or_default(),
    };
    match nats_override {
        Some(value) => (
            value.advertised_native.clone().unwrap_or_else(native),
            value.advertised_websocket.clone().unwrap_or_else(websocket),
        ),
        None => (native(), websocket()),
    }
}

/// Initialize the Revision 7 administrator and explicitly register the bundled
/// Console as an ordinary public browser client. Existing databases are retained.
///
/// # Errors
/// Returns an error for unavailable custody, migrations, existing administrator
/// initialization, invalid credentials, or normal client-registration rejection.
pub async fn seed_admin_credentials(
    config: &RuntimeConfig,
    username: &str,
    password: &str,
) -> Result<String, RuntimeError> {
    initialize_admin_credentials(config, username, Some(password))
        .await
        .map(|result| result.0)
}

/// Initialize the first administrator using an optional password. Without a
/// password, returns a one-time setup URL on the isolated OAuth origin.
///
/// # Errors
/// Refuses an already initialized installation, invalid credentials, missing
/// custody, or failed atomic account/client initialization.
pub async fn initialize_admin_credentials(
    config: &RuntimeConfig,
    username: &str,
    password: Option<&str>,
) -> Result<(String, Option<String>), RuntimeError> {
    let (config, username, password) = (
        config.clone(),
        username.to_owned(),
        password.map(str::to_owned),
    );
    auth::blocking(move || initialize_admin_credentials_in(&config, &username, password.as_deref()))
        .await
}

fn initialize_admin_credentials_in(
    config: &RuntimeConfig,
    username: &str,
    password: Option<&str>,
) -> Result<(String, Option<String>), RuntimeError> {
    let crate::StorageBackend::Sqlite(storage) = config
        .platform_storage_backend()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let store = crate::storage::SqliteStore::new(SubsystemName::Platform, storage);
    store
        .migrate()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let nats = config
        .resolve_nats_runtime_with(None)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let keys = callout_keys(config, &nats)?;
    let auth = Arc::new(SqliteAuthorizationStore::open(
        &store,
        config,
        keys.target_account(),
    )?);
    let now = crate::clock::unix_now();
    auth.initialize_issuer(now)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let minimum = config
        .auth
        .as_ref()
        .and_then(|value| value.local_identity.as_ref())
        .and_then(|value| value.password_min_length)
        .unwrap_or(12);
    if password.is_some_and(|password| password.len() < usize::from(minimum)) {
        return Err(RuntimeError::Platform(
            "administrator password does not meet configured minimum length".into(),
        ));
    }
    let custody = config
        .auth
        .as_ref()
        .and_then(|auth| auth.oauth_storage_key_file.as_deref())
        .ok_or_else(|| RuntimeError::Platform("auth.oauth_storage_key_file is required".into()))?;
    let storage = auth::OAuthStorage::open(auth.clone(), custody)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let initialized: bool = auth
        .has_administrator()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    if initialized {
        return Err(RuntimeError::Platform(
            "administrator is already initialized; use explicit reset recovery".into(),
        ));
    }
    if let Some(password) = password {
        auth.seed_administrator_and_console(&config.public_origin(), username, password, now)
            .map(|principal| (principal, None))
            .map_err(|error| RuntimeError::Platform(error.to_string()))
    } else {
        storage
            .initialize_local_administrator(&config.public_origin(), username, None, now)
            .map_err(|error| RuntimeError::Platform(error.to_string()))
    }
}

// Resource evidence must never be reused after restoring a mismatched NATS store.
async fn verify_jetstream_identity(
    auth_store: &Arc<SqliteAuthorizationStore>,
    client: &async_nats::Client,
) -> Result<(), RuntimeError> {
    use async_nats::jetstream::{context::GetStreamErrorKind, kv, ErrorCode};
    const BUCKET: &str = "TRELLIS_PLATFORM_IDENTITY";
    const KEY: &str = "store";
    let reader = auth_store.clone();
    let (identity, bound) = auth::blocking(move || reader.jetstream_identity()).await?;
    let jetstream = async_nats::jetstream::new(client.clone());
    let marker = match jetstream.get_stream(format!("KV_{BUCKET}")).await {
        Ok(_) => Some(
            jetstream
                .get_key_value(BUCKET)
                .await
                .map_err(|error| RuntimeError::Platform(error.to_string()))?,
        ),
        Err(error)
            if matches!(
                error.kind(),
                GetStreamErrorKind::JetStream(error)
                    if error.error_code()==ErrorCode::STREAM_NOT_FOUND
            ) =>
        {
            None
        }
        Err(error) => return Err(RuntimeError::Platform(error.to_string())),
    };
    if bound && marker.is_none() {
        return Err(RuntimeError::Platform(
            "JetStream identity is missing; restore the matching store".into(),
        ));
    }
    let marker = match marker {
        Some(marker) => marker,
        None => jetstream
            .create_key_value(kv::Config {
                bucket: BUCKET.into(),
                storage: async_nats::jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?,
    };
    let status = marker
        .status()
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    if status.info.config.storage != async_nats::jetstream::stream::StorageType::File
        || !status.max_age().is_zero()
    {
        return Err(RuntimeError::Platform(
            "JetStream identity requires persistent nonexpiring storage".into(),
        ));
    }
    let current = marker
        .get(KEY)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    match current {
        Some(current) if current.as_ref() != identity.as_bytes() => {
            return Err(RuntimeError::Platform(
                "JetStream belongs to a different platform database".into(),
            ))
        }
        None if bound => {
            return Err(RuntimeError::Platform(
                "JetStream identity record is missing; restore the matching store".into(),
            ))
        }
        None => {
            marker
                .create(KEY, identity.clone().into())
                .await
                .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        }
        _ => {}
    }
    if marker
        .get(KEY)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .as_deref()
        != Some(identity.as_bytes())
    {
        return Err(RuntimeError::Platform(
            "JetStream identity changed while binding the database".into(),
        ));
    }
    if !bound {
        let writer = auth_store.clone();
        auth::blocking(move || writer.mark_jetstream_bound(&identity)).await?;
    }
    Ok(())
}
