//! Built-in platform admission through the ordinary native service boundary.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use trellis_rs::{
    generated::ParticipantDescriptor,
    service::{ConnectedServiceRuntime, ServiceConnectOptions},
};
use trellis_runtime_apis::{apis::trellis_auth_v1, participants::trellis_platform::Participant};

use super::auth::{
    materialize_native_kv, register_rpc, ProvisionedDeployment, SqliteAuthorizationStore,
};
use crate::supervisor::RuntimeError;

/// Commit built-in identity and actual resources before opening admission.
pub(crate) async fn prepare_builtin_provider<C: ParticipantDescriptor>(
    store: &Arc<SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    identity_seed: &str,
) -> Result<ProvisionedDeployment, RuntimeError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_secs()).ok())
        .ok_or_else(|| {
            RuntimeError::Platform("current time is outside the supported range".into())
        })?;
    let deployment = store
        .initialize_builtin_provider::<C>(identity_seed, now)
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    materialize_native_kv(store, operational, &deployment.deployment_id, now)
        .await
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    Ok(deployment)
}

/// Connect the prepared built-in through ordinary Callout and signed own-context.
pub(crate) async fn connect_platform_provider(
    store: Arc<SqliteAuthorizationStore>,
    oauth: Arc<super::auth::OAuthEngine>,
    operational: async_nats::Client,
    nats_servers: Vec<String>,
    root_certificates: Vec<std::path::PathBuf>,
    identity_seed: &str,
) -> Result<ConnectedServiceRuntime<Participant>, RuntimeError> {
    let mut trust = store
        .runtime_authorization_trust()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    trust.root_certificates = root_certificates;
    let mut provider = ConnectedServiceRuntime::<Participant>::connect(ServiceConnectOptions::new(
        nats_servers,
        trust,
        identity_seed,
    ))
    .await
    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    super::state::StateRuntime::start(operational.clone(), store.clone())
        .await?
        .register(&mut provider);
    register_rpc(
        &mut trellis_auth_v1::Provider::new(&mut provider),
        store,
        oauth,
        operational,
    );
    Ok(provider)
}

/// Runtime state for service and device bootstrap flows.
///
/// Bootstrap checks presented contracts against deployment authority and waits
/// for materialized authority before returning scoped credentials and resolved
/// runtime bindings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Bootstrap;
