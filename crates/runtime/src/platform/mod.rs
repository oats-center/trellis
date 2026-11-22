//! Runtime ownership of the authoritative Auth store and its serving boundaries.

use std::{sync::Arc, time::Duration};
use trellis_rs::__internal::{RuntimeOwner as _, RuntimeService as _};

pub mod auth;
pub mod auth_callout;
pub mod bootstrap;
mod builtin_live_provider;
mod console;
mod live_provider;
mod state;
mod webhooks;

pub(crate) use builtin_live_provider::{
    connect_builtin_live_provider, prepare_builtin_live_provider, request_builtin_preparation,
    BuiltinLiveProviderConnectOptions,
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
                .and_then(|value| value.runtime_servers.as_deref()),
        )
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let keys = callout_keys(&context.config)?;
    let (platform_store, config, account) = (
        context.stores.platform()?.clone(),
        context.config.clone(),
        keys.target_account().to_owned(),
    );
    let store = Arc::new(
        auth::blocking(move || SqliteAuthorizationStore::open(&platform_store, &config, &account))
            .await?,
    );
    // Another process can hold the database's write lock while the server
    // starts, such as a backup or an operator's query. Startup waits for it.
    let probe = store.clone();
    until_stored(context, "Auth startup", move || {
        let probe = probe.clone();
        auth::blocking(move || probe.writable())
    })
    .await?;
    let custody = auth::oauth_storage_key(&context.config)?;
    let console_origin = context
        .config
        .resolve_console_origin()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let (opened, keep_admin) = (store.clone(), !context.take_reset_admin());
    let (engine, account_url) = auth::blocking(move || {
        let platform = |error: &dyn std::fmt::Display| RuntimeError::Platform(error.to_string());
        opened.initialize_issuer(crate::clock::unix_now())?;
        let storage =
            auth::OAuthStorage::open(opened.clone(), &custody).map_err(|error| platform(&error))?;
        let account_url = if !opened.has_administrator()? {
            storage
                .initialize_local_administrator(
                    &console_origin,
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
    let (native, websocket) =
        advertised_endpoints(&context.config, &nats, context.nats_override.as_ref());
    // Every Login Portal page shows the generated-CA notice for the
    // certificate the Trellis listener serves.
    engine.serve_generated_tls(crate::tls::serves_generated_certificate(
        &context.config,
        context.config.http_transport(),
    ));
    engine.serve_operational(context.trellis_nats.clone());
    let router = auth::oauth_router(
        engine.clone(),
        native.into_iter().chain(websocket).collect(),
    );
    let jetstream = async_nats::jetstream::new(context.trellis_nats.clone());
    let mut buckets = Vec::new();
    for name in [auth::MATERIAL_BUCKET, auth::REVOCATION_BUCKET] {
        let bucket = match Box::pin(
            jetstream.create_key_value(async_nats::jetstream::kv::Config {
                bucket: name.into(),
                storage: async_nats::jetstream::stream::StorageType::File,
                history: 1,
                ..Default::default()
            }),
        )
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
    let (auth_creds, system_creds) = nats.platform_creds()?;
    let stopping = || context.is_stopping();
    // Connections, provider preparation and the Auth services each start a
    // large state machine; on the heap they keep the debug stack frames of
    // this startup, which awaits all of them, small.
    let auth_nats = Box::pin(crate::supervisor::connect_nats(&nats, auth_creds, stopping)).await?;
    let system = Box::pin(crate::supervisor::connect_nats(
        &nats,
        system_creds,
        stopping,
    ))
    .await?;
    // Provider notices reach sessions in the application account.
    let notices = context.trellis_nats.clone();
    // Every built-in this process starts must be one this server can install
    // before any of them is.
    let mut builtins = Vec::new();
    for role in LiveProviderRole::roles_for_mode(context.mode) {
        let (participant, evidence) = builtin_live_provider::builtin_build(role)?;
        let seed = live_provider::load_live_provider_seed(&context.config, role)?;
        builtins.push((participant, evidence, seed));
    }
    let checking = store.clone();
    until_stored(context, "Auth startup", move || {
        let (checking, builtins) = (checking.clone(), builtins.clone());
        auth::blocking(move || checking.check_builtin_upgrades(builtins, crate::clock::unix_now()))
    })
    .await?;
    let provider_seed = live_provider::load_live_provider_seed(
        &context.config,
        live_provider::LiveProviderRole::Platform,
    )?;
    bootstrap::prepare_builtin_provider::<
        trellis_runtime_apis::participants::trellis_platform::Participant,
    >(&store, &context.trellis_nats, &provider_seed)
    .await?;
    // Startup drains committed effects before serving. A database another
    // process holds locked, or a broker that is not answering yet, delays it;
    // only an unusable database stops startup.
    let mut backoff = crate::storage::Backoff::default();
    loop {
        match Box::pin(store.process_effects(
            &material,
            &revocations,
            &system,
            &notices,
            crate::clock::unix_now(),
        ))
        .await
        {
            Ok(0) => break,
            Ok(_) => backoff.succeeded(),
            Err(error) if error.storage_failure() == crate::storage::StorageFailure::Unusable => {
                return Err(RuntimeError::Platform(format!(
                    "the Auth database is unusable: {error}"
                )));
            }
            Err(error) => {
                let delay = backoff.failed();
                retrying("Auth startup effects", &error, delay);
                tokio::time::sleep(delay).await;
                if context.is_stopping() {
                    return Err(RuntimeError::Platform(format!(
                        "stopped while Auth startup effects were failing: {error}"
                    )));
                }
            }
        }
    }
    let own_context = Box::pin(auth::OwnContextService::start(
        context.trellis_nats.clone(),
        material.clone(),
        store.clone(),
    ))
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let callout = Box::pin(AuthCallout::start(
        auth_nats,
        system.clone(),
        store.clone(),
        keys,
    ))
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    // Roles running apart from the platform ask it to prepare their providers.
    let preparation = Box::pin(builtin_live_provider::BuiltinPreparation::start(
        context.trellis_nats.clone(),
        store.clone(),
        context.config.clone(),
    ))
    .await?;
    *context
        .authorization_store
        .lock()
        .map_err(|_| RuntimeError::Platform("authorization store lock poisoned".into()))? =
        Some(store.clone());
    let (publisher, published) = tokio::sync::watch::channel(None);
    let records_path = context
        .stores
        .platform()?
        .path()
        .with_file_name(crate::storage::WEBHOOKS_DATABASE);
    let webhooks = Box::pin(webhooks::Webhooks::open(
        engine.server.store().clone(),
        &records_path,
        context.stores.platform()?.busy_timeout(),
        context.trellis_nats.clone(),
        published,
        &context.config.public_origin(),
        &context
            .config
            .resolve_console_origin()
            .map_err(|error| RuntimeError::Platform(error.to_string()))?,
    ))
    .await?;
    // Sign-in, the Console, and webhook intake share the Trellis listener.
    context.register_http_router(
        router
            .merge(console::router(&context.config)?)
            .merge(webhooks.router()),
    )?;
    let stop = StopHandle::new();
    let task_stop = stop.clone();
    let provider_store = store.clone();
    let provider_operational = context.trellis_nats.clone();
    let provider_roots: Vec<_> = nats.ca_certificate.iter().cloned().collect();
    let provider_server_name = nats.tls_server_name.clone();
    let provider_servers = nats.server_urls();
    let join = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut tasks = tokio::task::JoinSet::new();
        let callout_stop = task_stop.clone();
        tasks.spawn(async move { callout.run(callout_stop).await });
        let preparation_stop = task_stop.clone();
        tasks.spawn(async move { preparation.run(preparation_stop).await });
        let webhook_stop = task_stop.clone();
        let webhook_changes = store.webhook_signal();
        let webhook_runtime = webhooks.clone();
        tasks.spawn(async move { webhook_runtime.run(webhook_changes, webhook_stop).await });
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
            // Offline instances are retired, and long-ended ones cleaned up,
            // every five minutes, but not in the first minutes after Auth
            // starts, while processes that were connected reconnect.
            let quiet_until = crate::clock::unix_now() + auth::STARTUP_QUIET_SECONDS;
            let mut next_retirement = 0;
            // A database another process holds locked, or a passing I/O error,
            // fails a pass; the work stays durable and the pass runs again.
            let mut backoff = crate::storage::Backoff::default();
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
                let pass = async {
                    // Keep draining while turns make progress, but give other
                    // tasks a turn between bounded batches.
                    for _ in 0..EFFECT_DRAIN_TURNS {
                        match store
                            .process_effects(
                                &material,
                                &revocations,
                                &system,
                                &notices,
                                crate::clock::unix_now(),
                            )
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
                            Err(error) => return Err(error),
                        }
                    }
                    if !ticked {
                        return Ok(());
                    }
                    let now = crate::clock::unix_now();
                    let (pruned, cleaned) = (store.clone(), engine.server.store().clone());
                    let retire = now >= next_retirement;
                    auth::blocking(move || {
                        pruned.prune_expired_artifacts(now)?;
                        cleaned
                            .cleanup_expired(now)
                            .map_err(|error| auth::AuthError::Invalid(error.to_string()))?;
                        if retire {
                            let retired = pruned.retire_offline_instances(now, quiet_until)?;
                            let deleted = pruned.clean_ended_instances(now)?;
                            if retired + deleted > 0 {
                                tracing::info!(retired, deleted, "Retired offline instances");
                            }
                        }
                        Ok::<_, auth::AuthError>(())
                    })
                    .await?;
                    if retire {
                        next_retirement = now + 300;
                    }
                    Ok(())
                }
                .await;
                match pass {
                    Ok(()) => backoff.succeeded(),
                    Err(error)
                        if error.storage_failure() == crate::storage::StorageFailure::Unusable =>
                    {
                        return Err(RuntimeError::Platform(format!(
                            "the Auth database is unusable: {error}"
                        )));
                    }
                    Err(error) => {
                        let delay = backoff.failed();
                        retrying("Auth background work", &error, delay);
                        tokio::select! {
                            () = worker_stop.stopped() => return Ok(()),
                            () = tokio::time::sleep(delay) => {}
                        }
                    }
                }
            }
        });
        let provider_stop = task_stop.clone();
        tasks.spawn(async move {
            let event_store = provider_store.clone();
            // Connecting and serving the provider are large state machines; on
            // the heap they keep this task's debug stack frames small.
            let provider = tokio::select! {
                () = provider_stop.stopped() => return Ok(()),
                result = Box::pin(bootstrap::connect_platform_provider(
                    provider_store, provider_oauth, provider_operational, provider_servers,
                    provider_roots, provider_server_name, &provider_seed, webhooks,
                )) => result?,
            };
            // Webhook intake publishes `Received` events as the platform.
            publisher.send_replace(Some(provider.event_publisher()));
            let owner = provider.runtime_native_owner();
            let publisher = provider.event_publisher();
            let publications = Box::pin(async {
                let mut queued = event_store.work_signal();
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut backoff = crate::storage::Backoff::default();
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
                    let pass = async {
                        for _ in 0..EFFECT_DRAIN_TURNS {
                            let claimer = event_store.clone();
                            let Some(effect) = auth::blocking(move || {
                                claimer.claim_effects(1, crate::clock::unix_now(), 10, true)
                            })
                            .await?
                            .into_iter()
                            .next() else {
                                break;
                            };
                            let result = async {
                                let text = |value: &serde_json::Value, key: &str| {
                                    value[key].as_str().map(str::to_owned).ok_or_else(|| {
                                        auth::AuthError::Invalid(format!(
                                            "event publication has no {key}"
                                        ))
                                    })
                                };
                                let body = &effect.payload["event"];
                                let event = trellis_rs::client::PreparedTrellisEvent::restore(
                                    text(&effect.payload, "subject")?,
                                    serde_json::to_vec(body)?.into(),
                                    text(&effect.payload, "descriptor")?,
                                    text(body, "eventId")?,
                                    text(body, "occurredAt")?,
                                )
                                .map_err(|error| auth::AuthError::Invalid(error.to_string()))?;
                                publisher
                                    .publish_prepared(&event)
                                    .await
                                    .map_err(|error| auth::AuthError::Broker(error.to_string()))
                            }
                            .await;
                            let now = crate::clock::unix_now();
                            let (settler, claimed) = (event_store.clone(), effect.clone());
                            let settled = match result {
                                Ok(()) => {
                                    auth::blocking(move || settler.complete_effect(&claimed, now))
                                        .await
                                }
                                Err(error)
                                    if error.storage_failure()
                                        == crate::storage::StorageFailure::Permanent =>
                                {
                                    Err(error)
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        action_id=%effect.action_id,
                                        attempt=effect.attempts,
                                        %error,
                                        "Auth event publication remains pending"
                                    );
                                    auth::blocking(move || settler.retry_effect(&claimed, now))
                                        .await
                                }
                            };
                            // One publication that fails the same way every
                            // time is set aside, so the ones after it go out.
                            match settled {
                                Err(error)
                                    if error.storage_failure()
                                        == crate::storage::StorageFailure::Permanent =>
                                {
                                    tracing::error!(
                                        action_id=%effect.action_id,
                                        attempt=effect.attempts,
                                        %error,
                                        "Auth event publication set aside: it cannot succeed"
                                    );
                                    let settler = event_store.clone();
                                    let reason = error.to_string();
                                    auth::blocking(move || {
                                        settler.set_aside_effect(&effect, &reason, now)
                                    })
                                    .await?;
                                }
                                settled => {
                                    settled?;
                                }
                            }
                        }
                        Ok::<(), auth::AuthError>(())
                    }
                    .await;
                    // A claim or settlement the database refused leaves the
                    // publication pending; its lease runs out and it is sent
                    // again.
                    match pass {
                        Ok(()) => backoff.succeeded(),
                        Err(error)
                            if error.storage_failure()
                                == crate::storage::StorageFailure::Unusable =>
                        {
                            break Err(RuntimeError::Platform(format!(
                                "the Auth database is unusable: {error}"
                            )));
                        }
                        Err(error) => {
                            let delay = backoff.failed();
                            retrying("Auth event publication", &error, delay);
                            tokio::select! {
                                () = provider_stop.stopped() => break Ok(()),
                                () = tokio::time::sleep(delay) => {}
                            }
                        }
                    }
                }
            });
            let result = tokio::select! {
                result = Box::pin(provider.run()) =>
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

/// Runs a startup `step` that writes the Auth database until it succeeds. A
/// failure that can pass, such as a lock another process holds, is logged and
/// the step runs again after a growing wait; any other failure ends startup.
async fn until_stored<T, F>(
    context: &RuntimeContext,
    work: &str,
    mut step: impl FnMut() -> F,
) -> Result<T, RuntimeError>
where
    F: std::future::Future<Output = Result<T, auth::AuthError>>,
{
    let mut backoff = crate::storage::Backoff::default();
    loop {
        match step().await {
            Ok(value) => return Ok(value),
            Err(error) if error.storage_failure() == crate::storage::StorageFailure::Transient => {
                let delay = backoff.failed();
                retrying(work, &error, delay);
                tokio::time::sleep(delay).await;
                if context.is_stopping() {
                    return Err(RuntimeError::Platform(format!(
                        "stopped while {work} was failing: {error}"
                    )));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Logs a failed pass of Auth background `work` that runs again after `delay`:
/// a warning for a failure that can pass, an error for one that recurs until
/// someone fixes its cause.
fn retrying(work: &str, error: &auth::AuthError, delay: Duration) {
    let retry_in_ms = delay.as_millis();
    if error.storage_failure() == crate::storage::StorageFailure::Transient {
        tracing::warn!(work, %error, retry_in_ms, "Auth work failed; it will run again");
    } else {
        tracing::error!(work, %error, retry_in_ms, "Auth work failed; it will run again");
    }
}

pub(crate) fn callout_keys(config: &RuntimeConfig) -> Result<CalloutKeys, RuntimeError> {
    let callout = config
        .resolve_nats_auth_callout()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let users = &config.secrets.nats.users;
    let jwt = |user: Option<&crate::NatsUser>, name| {
        user.map(|user| user.jwt.clone())
            .ok_or(crate::ConfigError::MissingSecret { name })
    };
    CalloutKeys::from_secrets(
        &callout,
        &jwt(users.auth.as_ref(), "nats.users.auth")?,
        &jwt(users.trellis.as_ref(), "nats.users.trellis")?,
    )
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
            .unwrap_or_else(|| resolved.server_urls())
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
    let console_origin = config
        .resolve_console_origin()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let crate::StorageBackend::Sqlite(storage) = config
        .platform_storage_backend()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let store = crate::storage::SqliteStore::new(SubsystemName::Platform, storage);
    store
        .migrate()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let keys = callout_keys(config)?;
    let auth = Arc::new(SqliteAuthorizationStore::open(
        &store,
        config,
        keys.target_account(),
    )?);
    let now = crate::clock::unix_now();
    auth.initialize_issuer(now)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let custody = auth::oauth_storage_key(config)?;
    let storage = auth::OAuthStorage::open(auth.clone(), &custody)
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
        auth.seed_administrator_and_console(&console_origin, username, password, now)
            .map(|principal| (principal, None))
            .map_err(|error| RuntimeError::Platform(error.to_string()))
    } else {
        storage
            .initialize_local_administrator(&console_origin, username, None, now)
            .map_err(|error| RuntimeError::Platform(error.to_string()))
    }
}

/// Bucket recording which installation, and which of its platform databases,
/// a JetStream belongs to.
const IDENTITY_BUCKET: &str = "TRELLIS_PLATFORM_IDENTITY";
/// The installation's instance ID.
const INSTALLATION_KEY: &str = "installation";
/// The platform database's JetStream identity.
const STORE_KEY: &str = "store";

/// Reads the broker's identity record: the bucket when it exists, with the
/// installation ID and the platform database's JetStream identity it holds.
/// Opening an existing bucket and reading its keys only reads.
///
/// # Errors
///
/// Returns [`RuntimeError::BrokerIdentity`] when the record can't be read, or
/// isn't kept in persistent, nonexpiring storage.
pub(crate) async fn read_broker_identity(
    jetstream: &async_nats::jetstream::Context,
) -> Result<
    (
        Option<async_nats::jetstream::kv::Store>,
        Option<String>,
        Option<String>,
    ),
    RuntimeError,
> {
    use async_nats::jetstream::{context::GetStreamErrorKind, ErrorCode};
    let failed = |error: &dyn std::fmt::Display| {
        RuntimeError::BrokerIdentity(format!("cannot read its identity record: {error}"))
    };
    let marker = match jetstream.get_stream(format!("KV_{IDENTITY_BUCKET}")).await {
        Ok(_) => jetstream
            .get_key_value(IDENTITY_BUCKET)
            .await
            .map_err(|error| failed(&error))?,
        Err(error)
            if matches!(
                error.kind(),
                GetStreamErrorKind::JetStream(error)
                    if error.error_code() == ErrorCode::STREAM_NOT_FOUND
            ) =>
        {
            return Ok((None, None, None))
        }
        Err(error) => return Err(failed(&error)),
    };
    let status = marker.status().await.map_err(|error| failed(&error))?;
    if status.info.config.storage != async_nats::jetstream::stream::StorageType::File
        || !status.max_age().is_zero()
    {
        return Err(RuntimeError::BrokerIdentity(
            "its identity record needs persistent, nonexpiring storage".into(),
        ));
    }
    let mut recorded = [None, None];
    for (key, value) in [INSTALLATION_KEY, STORE_KEY].into_iter().zip(&mut recorded) {
        *value = marker
            .get(key)
            .await
            .map_err(|error| failed(&error))?
            .map(|value| String::from_utf8_lossy(&value).into_owned());
    }
    let [installation, store] = recorded;
    Ok((Some(marker), installation, store))
}

/// Confirms that `client`'s JetStream belongs to this installation before the
/// runtime creates or changes anything there: no lease, stream or bucket is
/// touched on a broker that fails this.
///
/// The broker records the installation's instance ID and the JetStream
/// identity of the platform database bound to it. A role running the platform
/// compares both, and on a broker nothing has claimed binds them to itself. A
/// role apart from the platform has no platform database, so it reads the
/// record without writing and compares the instance ID; it waits while the
/// broker has none, until a platform of its installation binds it. Returns
/// false when `stopping` ends that wait.
///
/// Resource evidence in the platform database must never be reused against
/// another JetStream, nor this installation's work run on another's broker.
pub(crate) async fn confirm_broker(
    config: &RuntimeConfig,
    stores: &crate::storage::RuntimeStores,
    client: &async_nats::Client,
    stopping: impl Fn() -> bool,
) -> Result<bool, RuntimeError> {
    use async_nats::jetstream::kv;
    let refuse = RuntimeError::BrokerIdentity;
    let failed = |error: &dyn std::fmt::Display| {
        RuntimeError::BrokerIdentity(format!("cannot read its identity record: {error}"))
    };
    let installation = config
        .auth
        .as_ref()
        .and_then(|auth| auth.instance_id.clone())
        .ok_or_else(|| {
            refuse("auth.instance_id is needed to confirm the broker is this installation's".into())
        })?;
    let jetstream = async_nats::jetstream::new(client.clone());
    let read = || read_broker_identity(&jetstream);
    let other_installation = |bound: &str| {
        refuse(format!(
            "its JetStream belongs to installation {bound}, not this installation \
            {installation}; check [nats] servers and the Trellis NATS user"
        ))
    };

    let Ok(platform) = stores.platform().cloned() else {
        let mut waiting = false;
        loop {
            match read().await?.1 {
                Some(bound) if bound == installation => return Ok(true),
                Some(bound) => return Err(other_installation(&bound)),
                None if stopping() => return Ok(false),
                None => {}
            }
            if !waiting {
                tracing::warn!(
                    %installation,
                    "the NATS broker is not bound to an installation yet; waiting for this \
                    installation's platform to bind it before using it"
                );
                waiting = true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };
    let marked = platform.clone();
    let (identity, bound) = crate::blocking::spawn(move || {
        marked
            .open()?
            .query_row(
                "SELECT jetstream_id,jetstream_bound FROM trellis_platform_store_marker WHERE id=1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )
            .map_err(|error| {
                RuntimeError::Platform(format!("read the platform store marker: {error}"))
            })
    })
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))??;
    let (marker, bound_installation, store) = read().await?;
    match (&bound_installation, &store) {
        (Some(other), _) if *other != installation => return Err(other_installation(other)),
        (_, Some(store)) if *store != identity => {
            return Err(refuse(
                "its JetStream is bound to a different platform database; restore the \
                matching database or JetStream store"
                    .into(),
            ))
        }
        (_, None) if bound => {
            return Err(refuse(
                "the platform database was bound to a JetStream this broker has no record of; \
                restore the matching JetStream store"
                    .into(),
            ))
        }
        _ => {}
    }
    // The broker is this installation's, or nothing has claimed it yet.
    let marker = match marker {
        Some(marker) => marker,
        None => jetstream
            .create_key_value(kv::Config {
                bucket: IDENTITY_BUCKET.into(),
                storage: async_nats::jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await
            .map_err(|error| failed(&error))?,
    };
    for (key, value, recorded) in [
        (INSTALLATION_KEY, &installation, &bound_installation),
        (STORE_KEY, &identity, &store),
    ] {
        if recorded.is_some() {
            continue;
        }
        match marker.create(key, value.clone().into()).await {
            Ok(_) => {}
            // Another runtime bound it first; the check below compares.
            Err(error) if error.kind() == kv::CreateErrorKind::AlreadyExists => {}
            Err(error) => return Err(failed(&error)),
        }
        if marker
            .get(key)
            .await
            .map_err(|error| failed(&error))?
            .as_deref()
            != Some(value.as_bytes())
        {
            return Err(refuse(
                "another runtime bound it to a different identity at the same time".into(),
            ));
        }
    }
    if !bound {
        crate::blocking::spawn(move || {
            match platform.open()?.execute(
                "UPDATE trellis_platform_store_marker SET jetstream_bound=1 \
                WHERE id=1 AND jetstream_id=?1",
                [&identity],
            ) {
                Ok(1) => Ok(()),
                Ok(_) => Err(RuntimeError::Platform(
                    "the platform store marker changed while binding the broker".into(),
                )),
                Err(error) => Err(RuntimeError::Platform(format!(
                    "record the broker binding in the platform database: {error}"
                ))),
            }
        })
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))??;
    }
    Ok(true)
}
