use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, PathBuf};
use std::sync::Arc;

use axum::extract::Query;
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_TYPE, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use nats_jwt_rs::types::{Permission, Permissions};
use nats_jwt_rs::user::User;
use nats_jwt_rs::Claims;
use nkeys::{KeyPair, KeyPairType};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    parse_session_proof, session_proof_request_digest, verify_session_proof,
    NativeBootstrapSessionProofInput, SessionProofInput, SessionProofPolicy,
    UserAuthRequestSessionProofInput,
};
use trellis_rs::service::{
    EventConsumerReplay, EventConsumerReplayBinding, EventConsumerResourceBinding,
    JobsQueueResourceBinding, JobsResourceBinding, JobsSchemaRef, KvResourceBinding,
    ServiceResourceBindings, StoreResourceBinding,
};

mod bootstrap;
mod browser;
mod error;
mod router;
mod security;
mod well_known;
use browser::BrowserFlowResponse;
use error::{map_issuance_error, HttpError};
pub(crate) use router::router;
use security::{
    canonical_origin, oauth_cookie_header, oauth_cookie_name, oidc_portal_policy_digest,
    require_oauth_browser_binding, require_portal_binding, require_portal_origin,
    require_selected_portal_origin, validate_portal_binding_digest, validate_redirect,
};

const EMBEDDED_WEB_ASSETS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
const MAX_AUTH_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024;
use url::Url;

use super::ephemeral::{
    claim_oauth_state, AuthBrowserFlow, AuthBrowserFlowKind, AuthBrowserFlowState,
    AuthEphemeralRepository, AuthOAuthKind, AuthOAuthState, AuthOAuthStatus, ConsentApproval,
    ConsentDecision, ConsentDecisionKind, ConsentRequest, BROWSER_FLOW_FORMAT,
};
use super::evidence::ParticipantRuntimeProjection;
use super::{
    portal_policy_snapshot, resolve_portal_authority_selection, AccountFlowState,
    AccountRepository, AuthService, AuthorityEvidenceRepository, AuthorizationStateError,
    CompleteIdentityLinkInput, CompletePasswordResetInput, ContextRepository,
    CreateActivationReviewInput, CreateFederatedUserInput, CreateLocalUserInput,
    CreateSessionInput, DelegationCeiling, DeploymentRepository, FirstAdminBinding,
    FirstAdminFederatedRegistration, FirstAdminRegistration, GrantBindingReplacement,
    GrantBindingState, GrantOwnerKind, GrantRepository, IdempotencyResultRecord, IdempotentOutcome,
    LocalAuthentication, LoginPortalRecord, LoginSettingsRecord, OutboxRepository,
    ParticipantBindingRecord, ParticipantBindingState, PortalRepository, PostCommitActionKind,
    PostCommitActionRecord, ProviderLoginAttributes, ProvisioningRepository,
    ResourceBindingEvidence, ResourceBindingState, ResourceProviderIdentity, SessionRepository,
};

const IDEMPOTENCY_TTL_MS: i64 = 24 * 60 * 60_000;

#[derive(Clone)]
pub(crate) struct OidcProvider {
    metadata: CoreProviderMetadata,
    client_id: ClientId,
    client_secret: Option<ClientSecret>,
    redirect_uri: RedirectUrl,
    scopes: Vec<Scope>,
    role_claims: Vec<String>,
}

pub(crate) async fn discover_oidc_providers(
    config: Option<&crate::config::OAuthConfig>,
    public_origin: &str,
) -> Result<BTreeMap<String, OidcProvider>, AuthorizationStateError> {
    let Some(config) = config else {
        return Ok(BTreeMap::new());
    };
    let redirect_base = config.redirect_base.as_deref().unwrap_or(public_origin);
    let http_client = openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            AuthorizationStateError::Storage(format!("failed to build OIDC HTTP client: {error}"))
        })?;
    let mut providers = BTreeMap::new();
    for (provider_id, provider) in &config.providers {
        if provider.provider_type != "oidc" {
            return Err(AuthorizationStateError::InvalidRecord(format!(
                "OAuth provider {provider_id} type must be oidc"
            )));
        }
        if provider
            .role_claims
            .iter()
            .any(|pointer| !valid_json_pointer(pointer))
        {
            return Err(AuthorizationStateError::InvalidRecord(format!(
                "OAuth provider {provider_id} has an invalid role_claims JSON Pointer"
            )));
        }
        let issuer = provider.issuer.clone().ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(format!(
                "OAuth provider {provider_id} has no issuer"
            ))
        })?;
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer).map_err(|_| {
                AuthorizationStateError::InvalidRecord(format!(
                    "OAuth provider {provider_id} issuer is invalid"
                ))
            })?,
            &http_client,
        )
        .await
        .map_err(|error| {
            AuthorizationStateError::Storage(format!(
                "OIDC discovery failed for {provider_id}: {error}"
            ))
        })?;
        let client_id = ClientId::new(provider.client_id.clone().ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(format!(
                "OAuth provider {provider_id} has no client_id"
            ))
        })?);
        let secret = match (&provider.client_secret, &provider.client_secret_file) {
            (Some(secret), None) => Some(secret.clone()),
            (None, Some(path)) => Some(fs::read_to_string(path).map_err(|error| {
                AuthorizationStateError::Storage(format!(
                    "failed to read OAuth provider {provider_id} secret: {error}"
                ))
            })?),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(AuthorizationStateError::InvalidRecord(format!(
                    "OAuth provider {provider_id} has two client secrets"
                )))
            }
        };
        let redirect_uri = RedirectUrl::new(format!(
            "{}/{provider_id}",
            redirect_base.trim_end_matches('/')
        ))
        .map_err(|_| {
            AuthorizationStateError::InvalidRecord(format!(
                "OAuth provider {provider_id} redirect URI is invalid"
            ))
        })?;
        providers.insert(
            provider_id.clone(),
            OidcProvider {
                metadata,
                client_id,
                client_secret: secret.map(|secret| ClientSecret::new(secret.trim().to_owned())),
                redirect_uri,
                scopes: provider
                    .scopes
                    .clone()
                    .unwrap_or_else(|| {
                        vec![
                            "openid".to_owned(),
                            "profile".to_owned(),
                            "email".to_owned(),
                        ]
                    })
                    .into_iter()
                    .map(Scope::new)
                    .collect(),
                role_claims: provider.role_claims.clone(),
            },
        );
    }
    Ok(providers)
}

fn valid_json_pointer(pointer: &str) -> bool {
    pointer.starts_with('/')
        && !pointer.as_bytes().iter().enumerate().any(|(index, byte)| {
            *byte == b'~' && !matches!(pointer.as_bytes().get(index + 1), Some(b'0' | b'1'))
        })
}

#[derive(Clone)]
pub(super) struct AuthHttpState<R, E> {
    nats: async_nats::Client,
    service: AuthService<R>,
    ephemeral: E,
    issuer: NatsBootstrapIssuer,
    authorization_contexts: super::AuthorizationContextService,
    public_origin: String,
    allowed_redirect_origins: Vec<String>,
    native_nats_servers: Vec<String>,
    websocket_nats_servers: Vec<String>,
    oidc_providers: BTreeMap<String, OidcProvider>,
    proof_policy: SessionProofPolicy,
    web_source: WebSource,
    portal_source: WebSource,
    console_source: WebSource,
    console_source_is_override: bool,
    browser_flow_ttl_ms: i64,
}

#[derive(Clone)]
enum WebSource {
    Embedded,
    Directory(PathBuf),
    Proxy(axum::Router),
}

pub(crate) struct AuthHttpOptions<R, E> {
    pub nats: async_nats::Client,
    pub service: AuthService<R>,
    pub ephemeral: E,
    pub issuer: NatsBootstrapIssuer,
    pub authorization_contexts: super::AuthorizationContextService,
    pub public_origin: String,
    pub allowed_origins: Vec<String>,
    pub native_nats_servers: Vec<String>,
    pub websocket_nats_servers: Vec<String>,
    pub oidc_providers: BTreeMap<String, OidcProvider>,
    pub rate_limit_max: u32,
    pub rate_limit_window_ms: u64,
    pub browser_flow_ttl_ms: i64,
    pub web_source: Option<crate::config::WebSourceConfig>,
    pub portal_source: Option<crate::config::WebSourceConfig>,
    pub console_source: Option<crate::config::WebSourceConfig>,
}

#[derive(Clone)]
pub(crate) struct NatsBootstrapIssuer {
    signing_key: Arc<KeyPair>,
    auth_account: String,
    maximum_lifetime_seconds: i64,
}

impl NatsBootstrapIssuer {
    pub(crate) fn from_files(
        signing_seed_file: &std::path::Path,
        auth_user_creds_file: &std::path::Path,
        maximum_lifetime_seconds: u64,
    ) -> Result<Self, AuthorizationStateError> {
        let seed = fs::read_to_string(signing_seed_file).map_err(|error| {
            AuthorizationStateError::Storage(format!(
                "failed to read auth issuer signing seed: {error}"
            ))
        })?;
        let signing_key = KeyPair::from_seed(seed.trim()).map_err(|error| {
            AuthorizationStateError::Storage(format!("invalid auth issuer signing seed: {error}"))
        })?;
        if signing_key.key_pair_type() != KeyPairType::Account {
            return Err(AuthorizationStateError::InvalidRecord(
                "auth issuer signing seed must be an account NKey".to_owned(),
            ));
        }
        let credentials = fs::read_to_string(auth_user_creds_file).map_err(|error| {
            AuthorizationStateError::Storage(format!("failed to read auth user creds: {error}"))
        })?;
        let jwt = credentials
            .lines()
            .skip_while(|line| *line != "-----BEGIN NATS USER JWT-----")
            .skip(1)
            .find(|line| !line.trim().is_empty())
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "auth user creds contain no user JWT".to_owned(),
                )
            })?;
        let claims = Claims::<User>::decode(jwt).map_err(|error| {
            AuthorizationStateError::InvalidRecord(format!("auth user JWT is invalid: {error}"))
        })?;
        let auth_account = claims
            .payload()
            .issuer_account
            .clone()
            .unwrap_or_else(|| claims.iss.clone());
        Ok(Self {
            signing_key: Arc::new(signing_key),
            auth_account,
            maximum_lifetime_seconds: i64::try_from(maximum_lifetime_seconds).map_err(|_| {
                AuthorizationStateError::InvalidRecord(
                    "maximum bootstrap JWT lifetime is too large".to_owned(),
                )
            })?,
        })
    }

    fn deny_all_user_jwt(
        &self,
        session_nkey: &str,
        expires_at_seconds: i64,
        now_seconds: i64,
    ) -> Result<IssuedBootstrapJwt, AuthorizationStateError> {
        let (kind, _) = nkeys::from_public_key(session_nkey).map_err(|_| {
            AuthorizationStateError::InvalidRecord(
                "sessionNkey is not a canonical NATS public key".to_owned(),
            )
        })?;
        if KeyPairType::from(kind) != KeyPairType::User {
            return Err(AuthorizationStateError::InvalidRecord(
                "sessionNkey must be a NATS User NKey".to_owned(),
            ));
        }
        let deny = Permission {
            allow: Vec::new(),
            deny: vec![">".to_owned()],
        };
        let mut claims = User::new_claims("trellis-session".to_owned(), session_nkey.to_owned());
        let expires_at = expires_at_seconds.min(
            now_seconds
                .checked_add(self.maximum_lifetime_seconds)
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(
                        "bootstrap JWT expiry overflows".to_owned(),
                    )
                })?,
        );
        claims.exp = Some(expires_at);
        let user = claims.payload_mut();
        user.issuer_account = Some(self.auth_account.clone());
        user.permissions.permissions = Permissions {
            publish: deny.clone(),
            subscribe: deny,
            resp: None,
        };
        let jwt = claims.encode(&self.signing_key).map_err(|error| {
            AuthorizationStateError::Storage(format!(
                "failed to sign session bootstrap JWT: {error}"
            ))
        })?;
        Ok(IssuedBootstrapJwt { jwt, expires_at })
    }
}

struct IssuedBootstrapJwt {
    jwt: String,
    expires_at: i64,
}

fn flow_response(flow: AuthBrowserFlow) -> BrowserFlowResponse {
    BrowserFlowResponse {
        flow_id: flow.flow_id,
        state: flow.state,
        expires_at: flow.expires_at,
        providers: Vec::new(),
        registration_enabled: false,
        federated_registration_enabled: false,
        consent_view: serde_json::to_value(flow.consent).unwrap_or(Value::Null),
        redirect_target: flow.redirect_target,
    }
}

fn browser_consent(
    binding: &ParticipantBindingRecord,
    installed_revision: u64,
) -> Result<ConsentRequest, HttpError> {
    let ceiling = DelegationCeiling {
        capabilities: Vec::new(),
        exact_restrictions: None,
        platform_privileges: Vec::new(),
    };
    super::policy::consent_request(binding, installed_revision, None, &ceiling, &[], None)
        .map_err(Into::into)
}

async fn load_flow(
    repository: &impl AuthEphemeralRepository,
    flow_id: &str,
) -> Result<AuthBrowserFlow, HttpError> {
    let flow = repository
        .get_browser_flow(flow_id)
        .await?
        .ok_or_else(|| HttpError::not_found("flow_not_found"))?;
    if flow.expires_at < now_ms()? && flow.state != AuthBrowserFlowState::Expired {
        let expected = flow.version;
        let mut expired = flow;
        expired.state = AuthBrowserFlowState::Expired;
        expired.completed_at = Some(expired.expires_at);
        expired.version += 1;
        repository.replace_browser_flow(expected, expired).await?;
        return Err(HttpError::gone("flow_expired"));
    }
    Ok(flow)
}

fn idempotency(
    scope: &str,
    purpose: &str,
    signer_id: &str,
    request_id: &str,
    request_digest: &str,
    now: i64,
) -> Result<IdempotencyResultRecord, HttpError> {
    Ok(IdempotencyResultRecord {
        scope_key: digest_parts(&[scope, purpose, signer_id, request_id]),
        purpose: purpose.to_owned(),
        signer_id: signer_id.to_owned(),
        request_id: request_id.to_owned(),
        request_digest: request_digest.to_owned(),
        result: json!({}),
        created_at: now,
        expires_at: checked_add(now, IDEMPOTENCY_TTL_MS)?,
    })
}

fn now_ms() -> Result<i64, HttpError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| HttpError::internal("clock_before_epoch"))?
        .as_millis()
        .try_into()
        .map_err(|_| HttpError::internal("clock_overflow"))
}

fn checked_add(value: i64, duration: i64) -> Result<i64, HttpError> {
    value
        .checked_add(duration)
        .filter(|value| *value <= super::MAX_PROTOCOL_INTEGER as i64)
        .ok_or_else(|| HttpError::internal("timestamp_overflow"))
}

fn digest_parts(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

fn proof_request_digest(raw: &Value) -> Result<String, trellis_protocol::ProtocolError> {
    session_proof_request_digest(raw)
}

fn session_public_key_to_user_nkey(session_public_key: &str) -> Result<String, HttpError> {
    let key = URL_SAFE_NO_PAD
        .decode(session_public_key)
        .map_err(|_| HttpError::bad_request("invalid_session_key"))?;
    if key.len() != 32 || URL_SAFE_NO_PAD.encode(&key) != session_public_key {
        return Err(HttpError::bad_request("invalid_session_key"));
    }
    let mut encoded = Vec::with_capacity(35);
    encoded.push(20 << 3);
    encoded.extend_from_slice(&key);
    let mut crc = 0_u16;
    for byte in &encoded {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    encoded.extend_from_slice(&crc.to_le_bytes());
    Ok(data_encoding::BASE32_NOPAD.encode(&encoded))
}

fn first_admin_token_hash(token: &str) -> Result<String, HttpError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| HttpError::bad_request("invalid_account_flow"))?;
    if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(&decoded) != token {
        return Err(HttpError::bad_request("invalid_account_flow"));
    }
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(decoded)))
}

async fn load_account_flow_by_token(
    repository: &impl AccountRepository,
    token: &str,
) -> Result<Option<(String, super::AccountFlowRecord)>, HttpError> {
    let mut hashes = vec![URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))];
    if let Ok(hash) = first_admin_token_hash(token) {
        hashes.insert(0, hash);
    }
    for hash in hashes {
        if let Some(flow) = repository.get_account_flow_by_hash(&hash).await? {
            return Ok(Some((hash, flow)));
        }
    }
    Ok(None)
}

fn getrandom_bytes() -> Result<[u8; 16], HttpError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| HttpError::internal("entropy_unavailable"))?;
    Ok(bytes)
}

fn project_service_resource_bindings(
    participant: &ParticipantRuntimeProjection,
    evidence: &[ResourceBindingEvidence],
    participant_id: &str,
) -> Result<ServiceResourceBindings, HttpError> {
    let mut resources = ServiceResourceBindings::default();
    let mut job_queues = BTreeMap::new();
    let mut jobs_namespace = None;
    let mut jobs_work_stream = None;

    for binding in evidence {
        if binding.state != ResourceBindingState::Available {
            continue;
        }
        match &binding.provider_identity {
            ResourceProviderIdentity::Kv { bucket } => {
                let Some(super::resources::ResourceActual::Kv {
                    history,
                    ttl_ms,
                    max_value_bytes,
                }) = binding.actual.as_ref()
                else {
                    return Err(HttpError::internal("resource_binding_invalid"));
                };
                resources.kv.insert(
                    binding.local_name.clone(),
                    KvResourceBinding {
                        bucket: bucket.clone(),
                        history: i64::try_from(*history)
                            .map_err(|_| HttpError::internal("resource_binding_invalid"))?,
                        max_value_bytes: max_value_bytes
                            .as_ref()
                            .copied()
                            .map(i64::try_from)
                            .transpose()
                            .map_err(|_| HttpError::internal("resource_binding_invalid"))?,
                        ttl_ms: i64::try_from(*ttl_ms)
                            .map_err(|_| HttpError::internal("resource_binding_invalid"))?,
                    },
                );
            }
            ResourceProviderIdentity::Store { bucket } => {
                let Some(super::resources::ResourceActual::Store {
                    ttl_ms,
                    max_object_bytes,
                    max_total_bytes,
                }) = binding.actual.as_ref()
                else {
                    return Err(HttpError::internal("resource_binding_invalid"));
                };
                resources.store.insert(
                    binding.local_name.clone(),
                    StoreResourceBinding {
                        name: bucket.clone(),
                        max_object_bytes: max_object_bytes
                            .as_ref()
                            .copied()
                            .map(i64::try_from)
                            .transpose()
                            .map_err(|_| HttpError::internal("resource_binding_invalid"))?,
                        max_total_bytes: max_total_bytes
                            .as_ref()
                            .copied()
                            .map(i64::try_from)
                            .transpose()
                            .map_err(|_| HttpError::internal("resource_binding_invalid"))?,
                        ttl_ms: i64::try_from(*ttl_ms)
                            .map_err(|_| HttpError::internal("resource_binding_invalid"))?,
                    },
                );
            }
            ResourceProviderIdentity::State { .. } => {}
            ResourceProviderIdentity::JobQueue {
                namespace,
                work_stream,
                publish_prefix,
                updates_prefix,
                work_subject,
                consumer,
            } => {
                let config = participant
                    .resources
                    .get(&binding.local_name)
                    .ok_or_else(|| HttpError::internal("job_queue_binding_invalid"))?;
                let max_deliver = config.retry_attempts.unwrap_or(5);
                let backoff_ms = if config.retry_attempts.is_none() {
                    vec![5_000, 30_000, 120_000, 600_000]
                } else {
                    config.retry_backoff_ms.clone()
                }
                .into_iter()
                .map(i64::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| HttpError::internal("job_queue_binding_invalid"))?;
                if jobs_namespace
                    .as_ref()
                    .is_some_and(|current| current != namespace)
                    || jobs_work_stream
                        .as_ref()
                        .is_some_and(|current| current != work_stream)
                {
                    return Err(HttpError::internal("job_resource_identity_mismatch"));
                }
                jobs_namespace = Some(namespace.clone());
                jobs_work_stream = Some(work_stream.clone());
                job_queues.insert(
                    binding.local_name.clone(),
                    JobsQueueResourceBinding {
                        queue_type: binding.local_name.clone(),
                        publish_prefix: publish_prefix.clone(),
                        updates_prefix: updates_prefix.clone(),
                        work_subject: work_subject.clone(),
                        consumer_name: consumer.clone(),
                        payload: JobsSchemaRef {
                            schema: config
                                .payload_schema
                                .clone()
                                .ok_or_else(|| HttpError::internal("job_queue_binding_invalid"))?,
                        },
                        update: config
                            .update_schema
                            .clone()
                            .map(|schema| JobsSchemaRef { schema }),
                        result: config
                            .result_schema
                            .clone()
                            .map(|schema| JobsSchemaRef { schema }),
                        max_deliver: i64::from(max_deliver),
                        backoff_ms: backoff_ms.clone(),
                        ack_wait_ms: backoff_ms.first().copied().unwrap_or(30_000),
                        default_deadline_ms: config
                            .deadline_ms
                            .map(i64::try_from)
                            .transpose()
                            .map_err(|_| HttpError::internal("job_queue_binding_invalid"))?,
                        key_concurrency: config.job_key_path.as_ref().map(|path| {
                            trellis_rs::jobs::bindings::JobKeyConcurrencyBinding {
                                key: vec![format!("/{}", path.join("/"))],
                                max_active: 1,
                                heartbeat_interval_ms: 1_000,
                                heartbeat_ttl_ms: 5_000,
                                stale_policy: trellis_rs::jobs::bindings::JobKeyStalePolicy::Block,
                            }
                        }),
                        queue: config.job_key_policy.as_deref().map(|policy| {
                            trellis_rs::jobs::bindings::JobQueueDepthBinding {
                                max_queued_per_key: if policy == "reject" { 0 } else { 100 },
                                when_full: match policy {
                                    "supersede" => {
                                        trellis_rs::jobs::bindings::JobQueueWhenFull::ReplaceOldest
                                    }
                                    _ => trellis_rs::jobs::bindings::JobQueueWhenFull::Reject,
                                },
                            }
                        }),
                    },
                );
            }
            ResourceProviderIdentity::EventConsumer {
                stream,
                consumer,
                replay_consumer,
                filter_subjects,
            } => {
                let config = participant
                    .resources
                    .get(&binding.local_name)
                    .ok_or_else(|| HttpError::internal("event_consumer_binding_invalid"))?;
                let max_deliver = i64::from(config.retry_attempts.unwrap_or(6));
                let backoff_ms: Vec<i64> = if config.retry_backoff_ms.is_empty() {
                    [5_000, 30_000, 120_000, 600_000, 1_800_000]
                        .into_iter()
                        .take(max_deliver.saturating_sub(1) as usize)
                        .collect()
                } else {
                    config
                        .retry_backoff_ms
                        .iter()
                        .copied()
                        .map(i64::try_from)
                        .collect::<Result<_, _>>()
                        .map_err(|_| HttpError::internal("event_consumer_binding_invalid"))?
                };
                resources.event_consumers.insert(
                    binding.local_name.clone(),
                    EventConsumerResourceBinding {
                        stream: stream.clone(),
                        consumer_name: consumer.clone(),
                        resource_id: binding.binding_id.clone(),
                        filter_subjects: filter_subjects.clone(),
                        replay: if config.consumer_replay_all {
                            EventConsumerReplay::All
                        } else {
                            EventConsumerReplay::New
                        },
                        concurrency: config.consumer_concurrency.unwrap_or(1),
                        ack_wait_ms: backoff_ms.first().copied().unwrap_or(30_000),
                        max_deliver,
                        backoff_ms,
                        replay_binding: EventConsumerReplayBinding {
                            stream: trellis_events_runtime::REPLAY_STREAM.to_owned(),
                            consumer_name: replay_consumer.clone(),
                        },
                    },
                );
            }
        }
    }
    if !job_queues.is_empty() {
        resources.jobs = Some(JobsResourceBinding {
            service_name: participant_id.to_owned(),
            namespace: jobs_namespace
                .ok_or_else(|| HttpError::internal("job_namespace_missing"))?,
            work_stream: jobs_work_stream,
            queues: job_queues,
        });
    }
    Ok(resources)
}

#[cfg(test)]
mod tests;
