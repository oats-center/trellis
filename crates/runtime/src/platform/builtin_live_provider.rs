//! Native built-in admission through the ordinary provisioned service boundary.

use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trellis_protocol::SessionBinding;
use trellis_rs::client::{
    RuntimeAuthorizationTrust, ServiceConnectWithContractOptions, SessionAuth, TrellisClient,
    TrellisClientError,
};
use trellis_rs::generated::ParticipantDescriptor;

use super::auth::{ProvisionedDeployment, SqliteAuthorizationStore};
use super::live_provider::{load_live_provider_seed, LiveProviderRole};
use super::RuntimeError;
use crate::shutdown::StopHandle;

/// Subject on the Trellis account where a role running apart from the
/// platform asks Auth to prepare its built-in provider. Admitted sessions'
/// ACLs never include it; only the installation's own runtimes publish here.
const PREPARE_SUBJECT: &str = "trellis.auth.builtin.prepare";
/// Domain of the signed preparation request.
const PREPARE_DOMAIN: &str = "trellis.builtin-provider.prepare.v1";
/// Bound on one preparation, which can accept the role's APIs and
/// materialize its resources.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(30);

/// A role's request that Auth prepare its built-in provider, signed with the
/// role's provisioned identity seed.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareRequest {
    /// `jobs`, `health` or `events`.
    role: String,
    /// Ed25519 signature over the SHA-256 of [`prepare_transcript`].
    signature: String,
}

/// Auth's answer: the provisioned record, or why it refused.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "result")]
enum PrepareReply {
    Prepared { deployment: ProvisionedDeployment },
    Denied,
    Failed { reason: String },
}

/// Length-prefixed domain, installation and role, so a signature names one
/// role of one installation.
fn prepare_transcript(installation: &str, role: &str) -> Vec<u8> {
    let mut transcript = Vec::new();
    for component in [PREPARE_DOMAIN, installation, role] {
        transcript.extend_from_slice(&(component.len() as u32).to_be_bytes());
        transcript.extend_from_slice(component.as_bytes());
    }
    transcript
}

fn role_name(role: LiveProviderRole) -> &'static str {
    match role {
        LiveProviderRole::Platform => "platform",
        LiveProviderRole::Health => "health",
        LiveProviderRole::Jobs => "jobs",
        LiveProviderRole::Events => "events",
    }
}

fn installation_id(config: &crate::RuntimeConfig) -> Result<&str, RuntimeError> {
    config
        .auth
        .as_ref()
        .and_then(|auth| auth.instance_id.as_deref())
        .ok_or_else(|| RuntimeError::Platform("auth.instance_id is not set".into()))
}

/// Ask the Auth that serves this installation to prepare `role`'s built-in
/// provider, as an in-process platform does, and return the provisioned record.
///
/// # Errors
/// [`RuntimeError::AuthUnavailable`] when no Auth answers now. Auth refusing
/// the request, or failing to prepare the provider, fails the role.
pub(crate) async fn request_builtin_preparation(
    client: &async_nats::Client,
    config: &crate::RuntimeConfig,
    role: LiveProviderRole,
    identity_seed: &str,
) -> Result<ProvisionedDeployment, RuntimeError> {
    let platform = |error: &dyn std::fmt::Display| RuntimeError::Platform(error.to_string());
    let signer =
        SessionAuth::from_seed_base64url(identity_seed).map_err(|error| platform(&error))?;
    let name = role_name(role);
    let transcript = prepare_transcript(installation_id(config)?, name);
    let request = serde_json::to_vec(&PrepareRequest {
        role: name.into(),
        signature: signer.sign_sha256_bytes(&transcript),
    })
    .map_err(|error| platform(&error))?;
    let response = client
        .send_request(
            PREPARE_SUBJECT,
            async_nats::Request::new()
                .payload(request.into())
                .timeout(Some(PREPARE_TIMEOUT)),
        )
        .await
        .map_err(|error| {
            RuntimeError::AuthUnavailable(format!("prepare the {name} built-in provider: {error}"))
        })?;
    match serde_json::from_slice(&response.payload).map_err(|error| platform(&error))? {
        PrepareReply::Prepared { deployment } => Ok(deployment),
        PrepareReply::Denied => Err(RuntimeError::Platform(format!(
            "Auth refused to prepare the {name} built-in provider: this server's \
            live_providers.{name} seed or installation ID differs from the platform's"
        ))),
        PrepareReply::Failed { reason } => Err(RuntimeError::Platform(format!(
            "Auth could not prepare the {name} built-in provider: {reason}"
        ))),
    }
}

/// Auth's answer to roles running apart from the platform that ask it to
/// prepare their built-in providers.
pub(crate) struct BuiltinPreparation {
    requests: async_nats::Subscriber,
    client: async_nats::Client,
    store: Arc<SqliteAuthorizationStore>,
    config: crate::RuntimeConfig,
}

impl BuiltinPreparation {
    /// Subscribes before the platform counts as running, so a role that
    /// finds it running finds this answering.
    pub(crate) async fn start(
        client: async_nats::Client,
        store: Arc<SqliteAuthorizationStore>,
        config: crate::RuntimeConfig,
    ) -> Result<Self, RuntimeError> {
        let failed = |error: &dyn std::fmt::Display| RuntimeError::Platform(error.to_string());
        let requests = client
            .queue_subscribe(PREPARE_SUBJECT, PREPARE_SUBJECT.into())
            .await
            .map_err(|error| failed(&error))?;
        client.flush().await.map_err(|error| failed(&error))?;
        Ok(Self {
            requests,
            client,
            store,
            config,
        })
    }

    /// Answers one request at a time until `stop`, so two preparations never
    /// race each other.
    pub(crate) async fn run(mut self, stop: StopHandle) -> Result<(), RuntimeError> {
        loop {
            let message = tokio::select! {
                () = stop.stopped() => return Ok(()),
                message = self.requests.next() => message.ok_or_else(|| {
                    RuntimeError::Platform("built-in preparation subscription ended".into())
                })?,
            };
            let Some(reply) = message.reply else {
                continue;
            };
            // Never use Auth's publish rights to answer anywhere but an inbox.
            if message.payload.len() > 4096
                || reply.len() > 512
                || !reply.starts_with("_INBOX.")
                || reply.split('.').any(|part| {
                    part.is_empty()
                        || part.contains(['*', '>'])
                        || part.chars().any(char::is_whitespace)
                })
            {
                continue;
            }
            let answer = tokio::select! {
                () = stop.stopped() => return Ok(()),
                answer = self.prepare(&message.payload) => answer,
            };
            let bytes = serde_json::to_vec(&answer)
                .map_err(|error| RuntimeError::Platform(error.to_string()))?;
            if let Err(error) = self.client.publish(reply, bytes.into()).await {
                tracing::warn!(%error, "built-in preparation reply unavailable");
            }
        }
    }

    async fn prepare(&self, payload: &[u8]) -> PrepareReply {
        let Ok(request) = serde_json::from_slice::<PrepareRequest>(payload) else {
            return PrepareReply::Denied;
        };
        let Some(role) = [
            LiveProviderRole::Jobs,
            LiveProviderRole::Health,
            LiveProviderRole::Events,
        ]
        .into_iter()
        .find(|role| role_name(*role) == request.role) else {
            return PrepareReply::Denied;
        };
        // Auth prepares the role from its own copy of the role's seed and its
        // own build; the caller only proves that it holds the same seed.
        let (seed, installation) = match load_live_provider_seed(&self.config, role)
            .and_then(|seed| Ok((seed, installation_id(&self.config)?)))
        {
            Ok(found) => found,
            Err(error) => {
                return PrepareReply::Failed {
                    reason: error.to_string(),
                }
            }
        };
        let key = SessionAuth::from_seed_base64url(&seed)
            .ok()
            .and_then(|expected| URL_SAFE_NO_PAD.decode(expected.session_key).ok())
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .and_then(|key| ed25519_dalek::VerifyingKey::from_bytes(&key).ok());
        let signature = URL_SAFE_NO_PAD
            .decode(&request.signature)
            .ok()
            .and_then(|signature| ed25519_dalek::Signature::from_slice(&signature).ok());
        let digest = Sha256::digest(prepare_transcript(installation, &request.role));
        if !key
            .zip(signature)
            .is_some_and(|(key, signature)| key.verify_strict(&digest, &signature).is_ok())
        {
            tracing::warn!(
                role = %request.role,
                "refused a built-in preparation that role's seed did not sign"
            );
            return PrepareReply::Denied;
        }
        match prepare_builtin_live_provider(role, &self.store, &self.client, &seed).await {
            Ok(deployment) => PrepareReply::Prepared { deployment },
            Err(error) => PrepareReply::Failed {
                reason: error.to_string(),
            },
        }
    }
}

/// Prepare the selected generated role through ordinary provisioning and native
/// declaration materialization, before its Callout admission is attempted.
pub(crate) async fn prepare_builtin_live_provider(
    role: LiveProviderRole,
    store: &std::sync::Arc<super::auth::SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    identity_seed: &str,
) -> Result<super::auth::ProvisionedDeployment, RuntimeError> {
    Ok(match role {
        LiveProviderRole::Platform => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_platform::Participant,
            >(store, operational, identity_seed)
            .await?
        }
        LiveProviderRole::Health => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_health_runtime::Participant,
            >(store, operational, identity_seed)
            .await?
        }
        LiveProviderRole::Jobs => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_jobs_runtime::Participant,
            >(store, operational, identity_seed)
            .await?
        }
        LiveProviderRole::Events => {
            super::bootstrap::prepare_builtin_provider::<
                trellis_runtime_apis::participants::trellis_events_runtime::Participant,
            >(store, operational, identity_seed)
            .await?
        }
    })
}

/// The build of `role`'s built-in this server ships, as its participant ID and
/// package evidence.
pub(crate) fn builtin_build(
    role: LiveProviderRole,
) -> Result<(String, trellis_idl::PackageEvidence), RuntimeError> {
    fn build<C: ParticipantDescriptor>(
    ) -> Result<(String, trellis_idl::PackageEvidence), RuntimeError> {
        let platform = |error: &dyn std::fmt::Display| RuntimeError::Platform(error.to_string());
        C::validate().map_err(|error| platform(&error))?;
        let evidence = serde_json::to_value(C::package_evidence())
            .and_then(serde_json::from_value)
            .map_err(|error| platform(&error))?;
        Ok((C::ID.to_owned(), evidence))
    }
    match role {
        LiveProviderRole::Platform => {
            build::<trellis_runtime_apis::participants::trellis_platform::Participant>()
        }
        LiveProviderRole::Health => {
            build::<trellis_runtime_apis::participants::trellis_health_runtime::Participant>()
        }
        LiveProviderRole::Jobs => {
            build::<trellis_runtime_apis::participants::trellis_jobs_runtime::Participant>()
        }
        LiveProviderRole::Events => {
            build::<trellis_runtime_apis::participants::trellis_events_runtime::Participant>()
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
/// [`RuntimeError::AuthUnavailable`] means Auth or the broker could not admit
/// the provider now, and a later attempt can succeed.
pub async fn connect_builtin_live_provider(
    role: LiveProviderRole,
    identity_seed_base64url: &str,
    options: BuiltinLiveProviderConnectOptions,
) -> Result<TrellisClient, RuntimeError> {
    // Each connection is a large state machine; on the heap it keeps this
    // dispatch's debug stack frame small.
    match role {
        LiveProviderRole::Platform => {
            Box::pin(connect::<
                trellis_runtime_apis::participants::trellis_platform::Participant,
            >(identity_seed_base64url, options))
            .await
        }
        LiveProviderRole::Health => {
            Box::pin(connect::<
                trellis_runtime_apis::participants::trellis_health_runtime::Participant,
            >(identity_seed_base64url, options))
            .await
        }
        LiveProviderRole::Jobs => {
            Box::pin(connect::<
                trellis_runtime_apis::participants::trellis_jobs_runtime::Participant,
            >(identity_seed_base64url, options))
            .await
        }
        LiveProviderRole::Events => {
            Box::pin(connect::<
                trellis_runtime_apis::participants::trellis_events_runtime::Participant,
            >(identity_seed_base64url, options))
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
        let message = format!("built-in native provider connect: {error}");
        // Auth or the broker failing to admit the provider now, as when Auth
        // stops serving or moves to another server, can pass; anything else
        // fails the same way on every attempt.
        match error {
            TrellisClientError::Timeout
            | TrellisClientError::Nats(_)
            | TrellisClientError::NatsConnect(_)
            | TrellisClientError::NatsRequest(_)
            | TrellisClientError::ServiceUnavailable(_)
            | TrellisClientError::AuthorizationUnavailable(_)
            | TrellisClientError::TransportUnavailable(_) => RuntimeError::AuthUnavailable(message),
            TrellisClientError::BootstrapHttp { status, .. } if status >= 500 => {
                RuntimeError::AuthUnavailable(message)
            }
            _ => RuntimeError::Platform(message),
        }
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
