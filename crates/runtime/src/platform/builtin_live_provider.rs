//! Production-only helper for connecting a built-in live provider.
//!
//! Built-in roles that serve a live surface connect through the ordinary native
//! service bootstrap and Auth Callout using their generated participant
//! evidence and the provisioned identity seed from [`super::live_provider`].
//! The returned client is an owned public-provider transport whose installed
//! deployment is validated against the fixed reserved deployment for the role.
//!
//! Every input is either generated evidence or a provisioned native seed; this
//! helper never accepts a preverified caller or an arbitrary unsigned context.

use trellis_rs::client::{ServiceConnectWithContractOptions, TrellisClient, TrellisClientError};
use trellis_rs::generated::ParticipantDescriptor;

use super::live_provider::LiveProviderRole;
use super::RuntimeError;

/// Inputs for connecting one built-in live provider through normal bootstrap.
pub struct BuiltinLiveProviderConnectOptions<'a> {
    /// Configured Trellis HTTP origin.
    pub trellis_url: &'a str,
    /// Connect/request timeout in milliseconds.
    pub timeout_ms: u64,
}

/// Connect a built-in live provider through normal bootstrap and validate its
/// installed deployment against the fixed reserved role deployment.
///
/// # Errors
///
/// Returns [`RuntimeError::Platform`] for bootstrap failure or when the
/// installed participant/deployment does not match the provisioned role.
pub async fn connect_builtin_live_provider(
    role: LiveProviderRole,
    identity_seed_base64url: &str,
    options: BuiltinLiveProviderConnectOptions<'_>,
) -> Result<TrellisClient, RuntimeError> {
    let client = match role {
        LiveProviderRole::Platform => {
            connect::<trellis_runtime_apis::participants::trellis_platform::Participant>(
                identity_seed_base64url,
                options,
            )
            .await?
        }
        LiveProviderRole::Health => {
            connect::<trellis_runtime_apis::participants::trellis_health_runtime::Participant>(
                identity_seed_base64url,
                options,
            )
            .await?
        }
        LiveProviderRole::Jobs => {
            connect::<trellis_runtime_apis::participants::trellis_jobs_runtime::Participant>(
                identity_seed_base64url,
                options,
            )
            .await?
        }
        LiveProviderRole::Events => {
            connect::<trellis_runtime_apis::participants::trellis_events_runtime::Participant>(
                identity_seed_base64url,
                options,
            )
            .await?
        }
    };
    match role {
        LiveProviderRole::Platform => {
            expect_deployment::<trellis_runtime_apis::participants::trellis_platform::Participant>(
                role, &client,
            )?;
        }
        LiveProviderRole::Health => {
            expect_deployment::<
                trellis_runtime_apis::participants::trellis_health_runtime::Participant,
            >(role, &client)?;
        }
        LiveProviderRole::Jobs => {
            expect_deployment::<
                trellis_runtime_apis::participants::trellis_jobs_runtime::Participant,
            >(role, &client)?;
        }
        LiveProviderRole::Events => {
            expect_deployment::<
                trellis_runtime_apis::participants::trellis_events_runtime::Participant,
            >(role, &client)?;
        }
    }
    Ok(client)
}

async fn connect<C: ParticipantDescriptor>(
    identity_seed_base64url: &str,
    options: BuiltinLiveProviderConnectOptions<'_>,
) -> Result<TrellisClient, RuntimeError> {
    TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
        trellis_url: options.trellis_url,
        participant_id: C::ID,
        participant_path: C::PATH,
        package_evidence: C::package_evidence(),
        provisioned_identity_seed_base64url: identity_seed_base64url,
        name: None,
        timeout_ms: options.timeout_ms,
    })
    .await
    .map_err(|error| RuntimeError::Platform(format!("built-in live provider connect: {error}")))
}

fn expect_deployment<C: ParticipantDescriptor>(
    role: LiveProviderRole,
    client: &TrellisClient,
) -> Result<String, RuntimeError> {
    let deployment_id = client
        .runtime_deployment_id()
        .map_err(|error: TrellisClientError| RuntimeError::Platform(error.to_string()))?;
    let expected = role.deployment_id();
    if deployment_id != expected {
        return Err(RuntimeError::Platform(format!(
            "built-in live provider '{}' is installed under deployment '{deployment_id}' instead of '{expected}'",
            C::ID
        )));
    }
    Ok(deployment_id)
}
