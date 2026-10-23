//! Native built-in admission through the ordinary provisioned service boundary.

use trellis_protocol::SessionBinding;
use trellis_rs::client::{
    RuntimeAuthorizationTrust, ServiceConnectWithContractOptions, TrellisClient,
};
use trellis_rs::generated::ParticipantDescriptor;

use super::live_provider::LiveProviderRole;
use super::RuntimeError;

/// Prepare the selected generated role through ordinary provisioning and native
/// declaration materialization, before its Callout admission is attempted.
pub(crate) async fn prepare_builtin_live_provider(
    role: LiveProviderRole,
    store: &std::sync::Arc<super::auth::SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    identity_seed: &str,
) -> Result<super::auth::ProvisionedDeployment, RuntimeError> {
    match role {
        LiveProviderRole::Platform => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_platform::Participant,
            >(store, operational, identity_seed)
            .await
        }
        LiveProviderRole::Health => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_health_runtime::Participant,
            >(store, operational, identity_seed)
            .await
        }
        LiveProviderRole::Jobs => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_jobs_runtime::Participant,
            >(store, operational, identity_seed)
            .await
        }
        LiveProviderRole::Events => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_events_runtime::Participant,
            >(store, operational, identity_seed)
            .await
        }
    }
}

/// Configured transport/trust and the actual provisioned record to check locally.
/// Expected identities are never sent to the SDK as admission assertions.
#[derive(Debug)]
pub struct BuiltinLiveProviderConnectOptions {
    /// Concrete configured native endpoints.
    pub nats_servers: Vec<String>,
    /// Issuer and installation trust established by provisioning or the selected Auth origin.
    pub trust: RuntimeAuthorizationTrust,
    /// Connect/request timeout in milliseconds.
    pub timeout_ms: u64,
}

/// Connect normally and compare the installed signed identity with provisioning.
///
/// # Errors
/// Admission or a mismatch with the generated role and persisted record fails.
pub async fn connect_builtin_live_provider(
    role: LiveProviderRole,
    identity_seed_base64url: &str,
    options: BuiltinLiveProviderConnectOptions,
) -> Result<TrellisClient, RuntimeError> {
    match role {
        LiveProviderRole::Platform => {
            connect::<trellis_runtime_apis::participants::trellis_platform::Participant>(
                identity_seed_base64url,
                options,
            )
            .await
        }
        LiveProviderRole::Health => {
            connect::<trellis_runtime_apis::participants::trellis_health_runtime::Participant>(
                identity_seed_base64url,
                options,
            )
            .await
        }
        LiveProviderRole::Jobs => {
            connect::<trellis_runtime_apis::participants::trellis_jobs_runtime::Participant>(
                identity_seed_base64url,
                options,
            )
            .await
        }
        LiveProviderRole::Events => {
            connect::<trellis_runtime_apis::participants::trellis_events_runtime::Participant>(
                identity_seed_base64url,
                options,
            )
            .await
        }
    }
}

async fn connect<C: ParticipantDescriptor>(
    identity_seed_base64url: &str,
    options: BuiltinLiveProviderConnectOptions,
) -> Result<TrellisClient, RuntimeError> {
    let client = TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
        nats_servers: options.nats_servers,
        trust: options.trust,
        participant_id: C::ID,
        provisioned_identity_seed_base64url: identity_seed_base64url,
        name: None,
        timeout_ms: options.timeout_ms,
    })
    .await
    .map_err(|error| {
        RuntimeError::Platform(format!("built-in native provider connect: {error}"))
    })?;
    let context = client
        .authorization_context()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?
        .ok_or_else(|| {
            RuntimeError::Platform("built-in provider has no installed authority".into())
        })?;
    if !matches!(
        &context.context.unsigned.binding,
        SessionBinding::Service { participant_id, .. } if participant_id == C::ID
    ) {
        return Err(RuntimeError::Platform(
            "installed built-in authority does not match the generated role".into(),
        ));
    }
    Ok(client)
}
