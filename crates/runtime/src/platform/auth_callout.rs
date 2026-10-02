//! Rust-owned NATS authorization callout.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::HeaderMap;
use base64::engine::general_purpose::STANDARD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::StreamExt;
use nats_jwt_rs::authorization::{AuthRequest, AuthResponse};
use nats_jwt_rs::types::{Permission, Permissions, ResponsePermission};
use nats_jwt_rs::user::User;
use nats_jwt_rs::Claims;
use nkeys::{KeyPair, KeyPairType, XKey};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use trellis_protocol::{TransportAuthorizationV1, VerifiedAuthorizationContext};

use super::auth::{
    AuthAttachmentState, AuthConnectionPresence, AuthEphemeralRepository,
    AuthorizationContextService, AuthorizationStateError, NatsAuthEphemeralRepository,
};
use crate::shutdown::StopHandle;
use crate::supervisor::RuntimeError;

const AUTH_CALLOUT_SUBJECT: &str = "$SYS.REQ.USER.AUTH";
const AUTH_CALLOUT_QUEUE: &str = "trellis";
const DISCONNECT_SUBJECT: &str = "$SYS.ACCOUNT.*.DISCONNECT";
const SERVER_XKEY_HEADER: &str = "Nats-Server-Xkey";
const CONNECT_TOKEN_FORMAT: &str = "trellis.nats-connect-token.v2";
const MAX_CONCURRENT_REQUESTS: usize = 32;
const SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// Bounded window in which a written attachment record is considered a pending
/// admission rather than an established socket: long enough for a successful
/// callout response to reach the broker and register, not a socket lifetime.
const ADMISSION_PENDING_MS: i64 = 60_000;
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const RECONCILE_INTERVAL_MS: i64 = 30_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NatsConnectToken {
    format: String,
    context_digest: String,
    routing_jwt: String,
}

#[derive(Debug, Deserialize)]
struct DisconnectEvent {
    server: Option<DisconnectedServer>,
    client: Option<DisconnectedClient>,
}

#[derive(Debug, Deserialize)]
struct DisconnectedServer {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DisconnectedClient {
    id: Option<u64>,
    #[serde(alias = "user", alias = "userNkey")]
    user_nkey: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CalloutKeys {
    auth_signing_key: Arc<KeyPair>,
    target_signing_key: Arc<KeyPair>,
    xkey: XKey,
    auth_account: String,
    target_account: String,
    sentinel_public_key: String,
}

impl CalloutKeys {
    pub(crate) fn from_files(
        auth_signing_seed_file: &Path,
        target_signing_seed_file: &Path,
        xkey_seed_file: &Path,
        sentinel_public_key_file: &Path,
        auth_user_creds_file: &Path,
        target_user_creds_file: &Path,
    ) -> Result<Self, AuthorizationStateError> {
        let auth_signing_key = account_key(auth_signing_seed_file, "auth issuer")?;
        let target_signing_key = account_key(target_signing_seed_file, "target issuer")?;
        let auth_user = user_claims(auth_user_creds_file, "auth user")?;
        let target_user = user_claims(target_user_creds_file, "target user")?;
        let auth_account = issuer_account(&auth_user);
        let sentinel_public_key = std::fs::read_to_string(sentinel_public_key_file)
            .map_err(|error| {
                AuthorizationStateError::InvalidRecord(format!(
                    "cannot read auth sentinel identity: {error}"
                ))
            })?
            .trim()
            .to_owned();
        if !is_nkey(&sentinel_public_key, KeyPairType::User) {
            return Err(denied("auth sentinel identity is not a user NKey"));
        }
        let target_account = issuer_account(&target_user);
        let xkey_seed = read_secret(xkey_seed_file, "auth-callout xkey")?;
        let xkey = XKey::from_seed(&xkey_seed).map_err(|error| {
            AuthorizationStateError::InvalidRecord(format!(
                "auth-callout xkey seed is invalid: {error}"
            ))
        })?;
        Ok(Self {
            auth_signing_key: Arc::new(auth_signing_key),
            target_signing_key: Arc::new(target_signing_key),
            xkey,
            auth_account,
            target_account,
            sentinel_public_key,
        })
    }

    pub(crate) fn target_account(&self) -> &str {
        &self.target_account
    }

    fn validate_bootstrap_jwt(
        &self,
        jwt: &str,
        session_nkey: &str,
        now_seconds: i64,
    ) -> Result<(), AuthorizationStateError> {
        let claims =
            Claims::<User>::decode(jwt).map_err(|_| denied("session bootstrap JWT is invalid"))?;
        if claims.iss != self.auth_signing_key.public_key()
            || claims.sub != session_nkey
            || claims.payload().issuer_account.as_deref() != Some(self.auth_account.as_str())
            || claims
                .exp
                .is_none_or(|expires_at| expires_at <= now_seconds)
            || claims.payload().permissions.bearer_token == Some(true)
        {
            return invalid_denial();
        }
        let permissions = &claims.payload().permissions.permissions;
        if permissions.publish.allow.is_empty()
            && permissions.publish.deny == [">"]
            && permissions.subscribe.allow.is_empty()
            && permissions.subscribe.deny == [">"]
            && permissions.resp.is_none()
        {
            Ok(())
        } else {
            invalid_denial()
        }
    }

    fn validate_sentinel(&self, jwt: &str) -> Result<(), AuthorizationStateError> {
        let claims = Claims::<User>::decode(jwt).map_err(|_| denied("NATS sentinel is invalid"))?;
        let permissions = &claims.payload().permissions.permissions;
        let issued_by_auth_account =
            claims.iss == self.auth_account && claims.payload().issuer_account.is_none();
        let issued_by_auth_signer = claims.iss == self.auth_signing_key.public_key()
            && claims.payload().issuer_account.as_deref() == Some(self.auth_account.as_str());
        if !(issued_by_auth_account || issued_by_auth_signer)
            || claims.sub != self.sentinel_public_key
            || claims.exp.is_some()
            || claims.payload().permissions.bearer_token != Some(true)
            || !permissions.publish.allow.is_empty()
            || permissions.publish.deny != [">"]
            || !permissions.subscribe.allow.is_empty()
            || permissions.subscribe.deny != [">"]
            || permissions.resp.is_some()
        {
            return Err(denied("NATS sentinel is not an exact deny-all credential"));
        }
        Ok(())
    }

    fn authorized_user_jwt(
        &self,
        user_nkey: &str,
        permissions: &TransportAuthorizationV1,
        authenticated_name: &str,
        expires_at_seconds: Option<i64>,
    ) -> Result<String, AuthorizationStateError> {
        let mut claims = User::new_claims(authenticated_name.to_owned(), user_nkey.to_owned());
        claims.aud = Some(self.target_account.clone());
        claims.exp = expires_at_seconds;
        let payload = claims.payload_mut();
        payload.issuer_account = Some(self.target_account.clone());
        let response = match &permissions.response {
            Some(response) => Some(ResponsePermission {
                max_messages: i64::try_from(response.max_messages).map_err(|_| {
                    AuthorizationStateError::InvalidRecord(
                        "response message count exceeds i64".to_owned(),
                    )
                })?,
                ttl: Duration::from_millis(response.ttl_ms),
            }),
            None => None,
        };
        payload.permissions.permissions = Permissions {
            publish: Permission {
                allow: permissions.publish_allow.clone(),
                deny: Vec::new(),
            },
            subscribe: Permission {
                allow: permissions.subscribe_allow.clone(),
                deny: Vec::new(),
            },
            resp: response,
        };
        claims.encode(&self.target_signing_key).map_err(|error| {
            AuthorizationStateError::Storage(format!("failed to sign NATS user JWT: {error}"))
        })
    }

    fn response(
        &self,
        request: &AuthRequest,
        user_jwt: Option<String>,
        denial_code: Option<&str>,
    ) -> Result<Vec<u8>, AuthorizationStateError> {
        let mut claims = AuthResponse::generic_claim(request.user_nkey.clone());
        claims.aud = Some(request.server.id.clone());
        let response = claims.payload_mut();
        response.issuer_account = Some(self.auth_account.clone());
        match user_jwt {
            Some(jwt) => response.jwt = jwt,
            None => response.error = denial_code.unwrap_or("internal_error").to_owned(),
        }
        let encoded = claims.encode(&self.auth_signing_key).map_err(|error| {
            AuthorizationStateError::Storage(format!(
                "failed to sign NATS authorization response: {error}"
            ))
        })?;
        Ok(encoded.into_bytes())
    }
}

/// Runtime-owned NATS authorization callout processor.
pub(crate) struct AuthCallout {
    subscriber: async_nats::Subscriber,
    disconnect_subscriber: async_nats::Subscriber,
    processor: CalloutProcessor,
}

#[derive(Clone)]
struct CalloutProcessor {
    client: async_nats::Client,
    contexts: AuthorizationContextService,
    ephemeral: NatsAuthEphemeralRepository,
    repository: super::auth::SqliteAuthorizationStore,
    keys: CalloutKeys,
    system_client: async_nats::Client,
    limiter: Arc<CalloutLimiter>,
}

#[derive(Debug, Default)]
struct CalloutLimiter {
    in_flight: std::sync::Mutex<usize>,
}

#[derive(Debug)]
struct CalloutPermit {
    limiter: Arc<CalloutLimiter>,
}

impl CalloutLimiter {
    fn try_acquire(self: &Arc<Self>) -> Option<CalloutPermit> {
        let mut in_flight = self.in_flight.lock().ok()?;
        if *in_flight >= MAX_CONCURRENT_REQUESTS {
            return None;
        }
        *in_flight += 1;
        Some(CalloutPermit {
            limiter: Arc::clone(self),
        })
    }
}

impl Drop for CalloutPermit {
    fn drop(&mut self) {
        let Ok(mut in_flight) = self.limiter.in_flight.lock() else {
            return;
        };
        *in_flight -= 1;
    }
}

impl AuthCallout {
    pub(crate) async fn start(
        client: async_nats::Client,
        system_client: async_nats::Client,
        ephemeral: NatsAuthEphemeralRepository,
        repository: super::auth::SqliteAuthorizationStore,
        contexts: super::auth::AuthorizationContextService,
        keys: CalloutKeys,
    ) -> Result<Self, AuthorizationStateError> {
        let subscriber = client
            .queue_subscribe(AUTH_CALLOUT_SUBJECT, AUTH_CALLOUT_QUEUE.to_owned())
            .await
            .map_err(|error| {
                AuthorizationStateError::Storage(format!(
                    "failed to subscribe to NATS authorization callout: {error}"
                ))
            })?;
        let disconnect_subscriber =
            system_client
                .subscribe(DISCONNECT_SUBJECT)
                .await
                .map_err(|error| {
                    AuthorizationStateError::Storage(format!(
                        "failed to subscribe to NATS disconnect events: {error}"
                    ))
                })?;
        Ok(Self {
            subscriber,
            disconnect_subscriber,
            processor: CalloutProcessor {
                client,
                contexts,
                ephemeral,
                repository,
                keys,
                system_client,
                limiter: Arc::new(CalloutLimiter::default()),
            },
        })
    }

    pub(crate) async fn run(mut self, stop: StopHandle) -> Result<(), RuntimeError> {
        let mut requests = tokio::task::JoinSet::new();
        let mut reconcile = tokio::time::interval(RECONCILE_INTERVAL);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = stop.stopped() => break,
                result = requests.join_next(), if !requests.is_empty() => {
                    handle_request_completion(result)?;
                }
                message = self.subscriber.next(), if requests.len() < MAX_CONCURRENT_REQUESTS => {
                    let Some(message) = message else {
                        return Err(RuntimeError::Platform(
                            "NATS authorization callout subscription closed".to_owned(),
                        ));
                    };
                    let processor = self.processor.clone();
                    requests.spawn(async move { processor.process(message).await });
                }
                message = self.disconnect_subscriber.next() => {
                    let Some(message) = message else {
                        return Err(RuntimeError::Platform(
                            "NATS disconnect subscription closed".to_owned(),
                        ));
                    };
                    self.processor.process_disconnect(&message.payload).await
                        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
                }
                _ = reconcile.tick() => {
                    if let Err(error) = self.processor.reconcile().await {
                        tracing::warn!(%error, "attachment reconciliation failed");
                    }
                }
            }
        }

        let drain = async {
            while let Some(result) = requests.join_next().await {
                handle_request_completion(Some(result))?;
            }
            Ok::<(), RuntimeError>(())
        };
        if tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, drain)
            .await
            .is_err()
        {
            requests.abort_all();
        }
        Ok(())
    }
}

impl CalloutProcessor {
    async fn process_disconnect(&self, payload: &[u8]) -> Result<(), AuthorizationStateError> {
        let Ok(event) = serde_json::from_slice::<DisconnectEvent>(payload) else {
            return Ok(());
        };
        let (Some(server), Some(client)) = (event.server, event.client) else {
            return Ok(());
        };
        let (Some(server_id), Some(client_id), Some(user_nkey)) =
            (server.id, client.id, client.user_nkey)
        else {
            return Ok(());
        };
        if server_id.is_empty() {
            return Ok(());
        }
        let client_id = client_id.to_string();
        let Some(connection) = self
            .ephemeral
            .list_connection_presence(None)
            .await?
            .into_iter()
            .find(|connection| {
                connection.server_id == server_id && connection.client_id == client_id
            })
        else {
            return Ok(());
        };
        if !connection.matches_disconnect_user(&user_nkey) {
            tracing::warn!(
                server_id = %server_id,
                client_id = %client_id,
                "NATS disconnect user does not match the retained physical attachment"
            );
            return Ok(());
        }
        close_retained_attachment(
            &self.ephemeral,
            &self.repository,
            &connection,
            "disconnected",
        )
        .await
    }

    /// Converges retained attachment records with authoritative broker
    /// inventory. Unreachable servers stay visible; only a successful inventory
    /// read proves an attachment absent.
    async fn reconcile(&self) -> Result<(), AuthorizationStateError> {
        reconcile_retained_attachments(&self.ephemeral, &self.system_client, &self.repository).await
    }

    #[tracing::instrument(
        name = "trellis.auth.callout",
        skip_all,
        fields(trellis.surface = "auth", trellis.operation = "request")
    )]
    async fn process(&self, message: async_nats::Message) -> Result<(), AuthorizationStateError> {
        let total_started = std::time::Instant::now();
        let process_result = async {
            let reply = message
                .reply
                .clone()
                .ok_or_else(|| denied("authorization request has no reply subject"))?;
            let server_xkey = server_xkey(message.headers.as_ref())?;
            let decrypted = self
                .keys
                .xkey
                .open(&message.payload, &server_xkey)
                .map_err(|_| denied("authorization request could not be decrypted"))?;
            let encoded_request = std::str::from_utf8(&decrypted)
                .map_err(|_| denied("authorization request JWT is not UTF-8"))?;
            let claims = Claims::<AuthRequest>::decode(encoded_request)
                .map_err(|_| denied("authorization request JWT is invalid"))?;
            let request = claims.payload();
            let now_seconds = now_millis()? / 1_000;
            if claims.iss != request.server.id
                || claims.sub != self.keys.auth_account
                || claims.aud.as_deref() != Some("nats-authorization-request")
                || claims
                    .exp
                    .is_none_or(|expires_at| expires_at <= now_seconds)
                || claims
                    .nbf
                    .is_some_and(|not_before| not_before > now_seconds)
                || claims.iat > now_seconds.saturating_add(5) as u64
                || request.server.xkey.as_deref() != Some(server_xkey.public_key().as_str())
                || request.client_info.id == 0
                || request.client_info.host.is_empty()
                || request.client_info.nonce.is_empty()
                || !is_nkey(&request.server.id, KeyPairType::Server)
                || !is_nkey(&request.user_nkey, KeyPairType::User)
            {
                return Err(denied("authorization request identity is invalid"));
            }

            let permit = self.limiter.try_acquire();
            let (user_jwt, denial_code) = match permit.as_ref() {
                None => (None, Some("rate_limited")),
                Some(_) => match self.authorize(request).await {
                    Ok(jwt) => (Some(jwt), None),
                    Err(error) => {
                        tracing::debug!(error = %error, "NATS connection authorization denied");
                        (None, Some(callout_denial_code(&error)))
                    }
                },
            };
            let outcome = match permit.as_ref() {
                None => crate::telemetry::Outcome::RateLimited,
                Some(_) => match denial_code {
                    Some(_) => crate::telemetry::Outcome::Denied,
                    None => crate::telemetry::Outcome::Ok,
                },
            };
            let response = self.keys.response(request, user_jwt, denial_code)?;
            let encrypted = self
                .keys
                .xkey
                .seal(&response, &server_xkey)
                .map_err(|error| {
                    AuthorizationStateError::Storage(format!(
                        "failed to encrypt NATS authorization response: {error}"
                    ))
                })?;
            self.client
                .publish(reply, encrypted.into())
                .await
                .map_err(|error| {
                    AuthorizationStateError::Storage(format!(
                        "failed to publish NATS authorization response: {error}"
                    ))
                })?;
            Ok(outcome)
        }
        .await;
        crate::telemetry::record_duration(
            crate::telemetry::DurationMetric::AuthCallout,
            total_started.elapsed(),
            "auth",
            "request",
            "total",
            match &process_result {
                Ok(outcome) => *outcome,
                Err(_) => crate::telemetry::Outcome::Error,
            },
        );
        process_result.map(|_outcome| ())
    }

    #[tracing::instrument(
        name = "trellis.auth.callout.authorize",
        skip_all,
        fields(trellis.surface = "auth", trellis.operation = "authorize")
    )]
    async fn authorize(&self, request: &AuthRequest) -> Result<String, AuthorizationStateError> {
        let total_started = std::time::Instant::now();
        let authorize_result = async {
            let now = now_millis()?;
            let now_seconds = now / 1_000;
            let connect = &request.connect_opts;
            let token: NatsConnectToken = serde_json::from_str(
                connect
                    .auth_token
                    .as_deref()
                    .ok_or_else(|| denied("NATS connect token is missing"))?,
            )
            .map_err(|_| denied("NATS connect token is invalid"))?;
            if token.format != CONNECT_TOKEN_FORMAT {
                return Err(denied("NATS connect token format is invalid"));
            }
            let session_nkey = connect
                .nkey
                .as_deref()
                .ok_or_else(|| denied("session NKey is missing"))?;
            self.keys.validate_sentinel(
                connect
                    .jwt
                    .as_deref()
                    .ok_or_else(|| denied("NATS sentinel is missing"))?,
            )?;
            self.keys
                .validate_bootstrap_jwt(&token.routing_jwt, session_nkey, now_seconds)?;
            verify_nats_nonce_signature(
                session_nkey,
                &request.client_info.nonce,
                connect
                    .sig
                    .as_deref()
                    .ok_or_else(|| denied("NATS nonce signature is missing"))?,
            )?;

            let context_started = std::time::Instant::now();
            let resolved_context = self
                .contexts
                .validator_cache()
                .resolve_admission_context(&token.context_digest, now_seconds)
                .await;
            crate::telemetry::record_duration(
                crate::telemetry::DurationMetric::AuthCallout,
                context_started.elapsed(),
                "auth",
                "authorize",
                "context",
                if resolved_context.is_ok() {
                    crate::telemetry::Outcome::Ok
                } else {
                    crate::telemetry::Outcome::Error
                },
            );
            let verified_context = resolved_context.map_err(|error| denied(error.to_string()))?;
            verify_connect_nkey_matches_context(session_nkey, &verified_context)?;

            let client_id = request.client_info.id.to_string();
            let connection_id = connection_id(&request.server.id, &client_id, &request.user_nkey)?;
            let admitted_policy = verified_context
                .signed_context()
                .unsigned
                .transport_authorization
                .clone();
            let admitted_policy_digest = admitted_policy
                .digest()
                .map_err(|error| denied(error.to_string()))?;
            let mut presence = AuthConnectionPresence {
                storage_revision: 0,
                format: "trellis.auth-connection-presence.v2".to_owned(),
                connection_id: connection_id.clone(),
                runtime_connection_id: verified_context.connection_id().to_owned(),
                session_key: verified_context
                    .signed_context()
                    .unsigned
                    .session_key
                    .clone(),
                login_session_id: verified_context.login_session_id().map(str::to_owned),
                principal_id: verified_context.principal_id().to_owned(),
                principal_kind: verified_context.principal_kind(),
                participant_id: verified_context.participant_id().to_owned(),
                deployment_id: verified_context
                    .signed_context()
                    .unsigned
                    .deployment_id
                    .clone(),
                instance_id: verified_context
                    .signed_context()
                    .unsigned
                    .instance_id
                    .clone(),
                context_digest: verified_context.context_digest().to_owned(),
                transport_authorization_digest: admitted_policy_digest,
                attachment_state: AuthAttachmentState::Pending,
                pending_deadline: Some(
                    now.checked_add(ADMISSION_PENDING_MS)
                        .ok_or_else(|| denied("pending admission deadline overflowed"))?,
                ),
                server_id: request.server.id.clone(),
                client_id,
                user_nkey: request.user_nkey.clone(),
                remote_address: Some(request.client_info.host.clone()),
                connected_at: now,
                last_seen_at: now,
                version: 2,
            };
            let presence_started = std::time::Instant::now();
            let presence_revision = self
                .ephemeral
                .put_connection_presence(presence.clone())
                .await;
            crate::telemetry::record_duration(
                crate::telemetry::DurationMetric::AuthCallout,
                presence_started.elapsed(),
                "auth",
                "authorize",
                "presence",
                if presence_revision.is_ok() {
                    crate::telemetry::Outcome::Ok
                } else {
                    crate::telemetry::Outcome::Error
                },
            );
            presence.storage_revision = presence_revision?;
            tracing::info!(
                context_digest = %presence.context_digest,
                runtime_connection_id = %presence.runtime_connection_id,
                physical_connection_id = %presence.connection_id,
                server_id = %presence.server_id,
                client_id = %presence.client_id,
                presence_revision = presence.storage_revision,
                participant_id = %presence.participant_id,
                "recorded physical connection presence before final admission validation"
            );

            let result = async {
                let permissions_started = std::time::Instant::now();
                let allowed = self
                    .contexts
                    .current_transport_policy(verified_context.context_digest(), now)
                    .await;
                crate::telemetry::record_duration(
                    crate::telemetry::DurationMetric::AuthCallout,
                    permissions_started.elapsed(),
                    "auth",
                    "authorize",
                    "permissions",
                    if allowed.is_ok() {
                        crate::telemetry::Outcome::Ok
                    } else {
                        crate::telemetry::Outcome::Error
                    },
                );
                let allowed = allowed.map_err(|error| denied(error.to_string()))?;
                let admitted = &admitted_policy;
                if !admitted
                    .is_covered_by(&allowed)
                    .map_err(|error| denied(error.to_string()))?
                {
                    return Err(denied(
                        "admitted transport policy is not covered by current authority",
                    ));
                }
                tracing::debug!(
                    publish_count = admitted.publish_allow.len(),
                    subscribe_count = admitted.subscribe_allow.len(),
                    hard_expires_at = ?admitted.hard_expires_at,
                    "installed signed NATS authorization policy"
                );
                let expires_at_seconds = match admitted.hard_expires_at {
                    Some(deadline) => {
                        if deadline <= now_seconds {
                            return Err(denied(
                                "admitted transport policy hard deadline has elapsed",
                            ));
                        }
                        Some(deadline)
                    }
                    None => None,
                };
                let authenticated_name = presence.authenticated_name();
                let jwt_started = std::time::Instant::now();
                let jwt = self.keys.authorized_user_jwt(
                    &request.user_nkey,
                    admitted,
                    &authenticated_name,
                    expires_at_seconds,
                );
                crate::telemetry::record_duration(
                    crate::telemetry::DurationMetric::AuthCallout,
                    jwt_started.elapsed(),
                    "auth",
                    "authorize",
                    "jwt",
                    if jwt.is_ok() {
                        crate::telemetry::Outcome::Ok
                    } else {
                        crate::telemetry::Outcome::Error
                    },
                );
                let jwt = jwt?;
                if self
                    .contexts
                    .validator_cache()
                    .runtime_revocation_time(&token.context_digest)
                    .map_err(|error| denied(error.to_string()))?
                    .is_some()
                {
                    self.ephemeral
                        .delete_connection_presence(&connection_id, presence.storage_revision)
                        .await?;
                    return Err(denied("authorization context is not admissible"));
                }
                let post_commit_started = std::time::Instant::now();
                let post_commit = self
                    .repository
                    .enqueue_post_commit_actions(vec![super::auth::connection_event_action::<
                        trellis_runtime_apis::apis::trellis_auth_v1::events::ConnectionsOpened,
                    >(
                        &presence,
                        "Auth.Connections.Opened",
                        "opened",
                        None,
                        now,
                    )?])
                    .await;
                crate::telemetry::record_duration(
                    crate::telemetry::DurationMetric::AuthCallout,
                    post_commit_started.elapsed(),
                    "auth",
                    "authorize",
                    "post_commit",
                    if post_commit.is_ok() {
                        crate::telemetry::Outcome::Ok
                    } else {
                        crate::telemetry::Outcome::Error
                    },
                );
                post_commit?;
                tracing::debug!(
                    connection_id = %verified_context.connection_id(),
                    login_session_id = ?verified_context.login_session_id(),
                    context_digest = %token.context_digest,
                    "NATS authorization callout admitted"
                );
                Ok(jwt)
            }
            .await;
            if result.is_err() {
                tracing::warn!(
                    context_digest = %presence.context_digest,
                    physical_connection_id = %presence.connection_id,
                    presence_revision = presence.storage_revision,
                    error = %result.as_ref().expect_err("checked error"),
                    "final admission validation failed; deleting exact presence revision"
                );
                self.ephemeral
                    .delete_connection_presence(&connection_id, presence.storage_revision)
                    .await?;
            } else {
                tracing::info!(
                    context_digest = %presence.context_digest,
                    physical_connection_id = %presence.connection_id,
                    presence_revision = presence.storage_revision,
                    "final admission validation succeeded"
                );
            }
            result
        }
        .await;
        crate::telemetry::record_duration(
            crate::telemetry::DurationMetric::AuthCallout,
            total_started.elapsed(),
            "auth",
            "authorize",
            "total",
            if authorize_result.is_ok() {
                crate::telemetry::Outcome::Ok
            } else {
                crate::telemetry::Outcome::Error
            },
        );
        authorize_result
    }
}

/// Converges retained attachment records with authoritative broker inventory.
///
/// A server that does not answer leaves its records retained. When broad
/// inventory does not report a cid, the owning server's targeted `CONNZ(cid)`
/// probe is consulted once a pending admission has waited out its deadline: a
/// missing connection proves absence and retires the record, an exact
/// authenticated marker confirms it, and a different identity now holding the
/// cid is kicked and the stale record closed. Missing or unavailable evidence
/// is never treated as absence, so a live socket cannot become untracked
/// merely because confirmation could not be obtained.
async fn reconcile_retained_attachments<E: AuthEphemeralRepository>(
    ephemeral: &E,
    system_client: &async_nats::Client,
    repository: &super::auth::SqliteAuthorizationStore,
) -> Result<(), AuthorizationStateError> {
    use std::collections::{BTreeSet, HashMap};

    let now = now_millis()?;
    let retained = ephemeral.list_connection_presence(None).await?;
    let retained_physical: HashMap<(String, String), ()> = retained
        .iter()
        .map(|record| ((record.server_id.clone(), record.client_id.clone()), ()))
        .collect();

    let mut servers: BTreeSet<String> = retained
        .iter()
        .map(|record| record.server_id.clone())
        .collect();
    match super::auth::transport_attachments::discover_servers(system_client).await {
        Ok(discovered) => servers.extend(discovered),
        Err(error) => {
            tracing::warn!(%error, "attachment reconciliation could not discover servers")
        }
    }

    let mut observed: HashMap<(String, u64), String> = HashMap::new();
    let mut answered: BTreeSet<String> = BTreeSet::new();
    for server_id in &servers {
        match super::auth::transport_attachments::paginate_connz(system_client, server_id).await {
            Ok(connections) => {
                answered.insert(server_id.clone());
                for connection in connections {
                    if connection.is_callout_owned() {
                        if let Some(user) = connection.authorized_user {
                            observed.insert((server_id.clone(), connection.cid), user);
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(
                server_id = %server_id,
                %error,
                "attachment reconciliation could not read broker inventory"
            ),
        }
    }

    for record in &retained {
        let Ok(cid) = record.client_id.parse::<u64>() else {
            continue;
        };
        let evidence = match observed.get(&(record.server_id.clone(), cid)) {
            // Broad inventory is authoritative and always actionable.
            Some(user) => AttachmentMarkerEvidence::Present(Some(user.clone())),
            // Broad inventory did not report this cid. It only proves anything
            // once the owning server answered and a pending admission has
            // waited out its deadline; the targeted probe then confirms the
            // exact attachment, proves absence, or reports the different
            // identity now holding the cid.
            None => {
                let past_pending = match record.attachment_state {
                    AuthAttachmentState::Pending => record
                        .pending_deadline
                        .is_some_and(|deadline| deadline <= now),
                    AuthAttachmentState::Confirmed => true,
                };
                if past_pending && answered.contains(&record.server_id) {
                    probe_attachment(system_client, &record.server_id, cid).await
                } else {
                    AttachmentMarkerEvidence::Unknown
                }
            }
        };
        match evidence {
            AttachmentMarkerEvidence::Present(Some(user))
                if record.matches_disconnect_user(&user) =>
            {
                if record.attachment_state == AuthAttachmentState::Pending
                    || now.saturating_sub(record.last_seen_at) >= RECONCILE_INTERVAL_MS
                {
                    let mut confirmed = record.clone();
                    confirmed.attachment_state = AuthAttachmentState::Confirmed;
                    confirmed.pending_deadline = None;
                    confirmed.last_seen_at = now;
                    if let Err(error) = ephemeral
                        .replace_connection_presence(record.storage_revision, confirmed)
                        .await
                    {
                        tracing::debug!(
                            %error,
                            "attachment confirmation raced with another writer"
                        );
                    }
                }
            }
            AttachmentMarkerEvidence::Present(_) => {
                tracing::warn!(
                    server_id = %record.server_id,
                    cid,
                    "retained attachment identity no longer matches its socket"
                );
                request_untracked_kick(system_client, &record.server_id, cid).await?;
                close_retained_attachment(
                    ephemeral,
                    repository,
                    record,
                    "reconciled_identity_mismatch",
                )
                .await?;
            }
            AttachmentMarkerEvidence::Absent => {
                close_retained_attachment(ephemeral, repository, record, "reconciled_absent")
                    .await?;
            }
            AttachmentMarkerEvidence::Unknown => {}
        }
    }

    for (server_id, cid) in observed.keys() {
        if retained_physical.contains_key(&(server_id.clone(), cid.to_string())) {
            continue;
        }
        request_untracked_kick(system_client, server_id, *cid).await?;
    }
    Ok(())
}

/// Authenticated-marker evidence for one retained attachment's cid.
enum AttachmentMarkerEvidence {
    /// The owning server proved no connection exists at this cid.
    Absent,
    /// A connection exists at this cid; the broker's authenticated user field,
    /// when it reported one.
    Present(Option<String>),
    /// Evidence could not be obtained; never treated as absence.
    Unknown,
}

async fn probe_attachment(
    system_client: &async_nats::Client,
    server_id: &str,
    cid: u64,
) -> AttachmentMarkerEvidence {
    match super::auth::transport_attachments::connz(system_client, server_id, 0, Some(cid)).await {
        Ok((connections, _)) => match connections.into_iter().next() {
            Some(connection) => AttachmentMarkerEvidence::Present(connection.authorized_user),
            None => AttachmentMarkerEvidence::Absent,
        },
        Err(error) => {
            tracing::warn!(
                server_id = %server_id,
                cid,
                %error,
                "attachment identity could not be probed"
            );
            AttachmentMarkerEvidence::Unknown
        }
    }
}

async fn request_untracked_kick(
    system_client: &async_nats::Client,
    server_id: &str,
    cid: u64,
) -> Result<(), AuthorizationStateError> {
    let outcome =
        super::auth::transport_attachments::request_kick(system_client, server_id, cid).await?;
    tracing::warn!(
        server_id = %server_id,
        cid,
        ?outcome,
        "kicked untracked callout-owned NATS attachment"
    );
    Ok(())
}

async fn close_retained_attachment<E: AuthEphemeralRepository>(
    ephemeral: &E,
    repository: &super::auth::SqliteAuthorizationStore,
    connection: &AuthConnectionPresence,
    reason: &str,
) -> Result<(), AuthorizationStateError> {
    ephemeral
        .delete_connection_presence(&connection.connection_id, connection.storage_revision)
        .await?;
    let now = now_millis()?;
    repository
        .enqueue_post_commit_actions(vec![super::auth::connection_event_action::<
            trellis_runtime_apis::apis::trellis_auth_v1::events::ConnectionsClosed,
        >(
            connection,
            "Auth.Connections.Closed",
            "closed",
            Some(reason),
            now,
        )?])
        .await
}

fn connection_id(
    server_id: &str,
    client_id: &str,
    user_nkey: &str,
) -> Result<String, AuthorizationStateError> {
    trellis_protocol::digest_json(&serde_json::json!({
        "serverId": server_id,
        "clientId": client_id,
        "userNkey": user_nkey,
    }))
    .map_err(|error| denied(error.to_string()))
}

fn handle_request_completion(
    result: Option<Result<Result<(), AuthorizationStateError>, tokio::task::JoinError>>,
) -> Result<(), RuntimeError> {
    match result {
        Some(Ok(Ok(()))) => Ok(()),
        Some(Ok(Err(error))) => {
            tracing::warn!(error = %error, "NATS authorization callout request denied");
            Ok(())
        }
        Some(Err(error)) => Err(RuntimeError::Platform(format!(
            "NATS authorization callout task failed: {error}"
        ))),
        None => Err(RuntimeError::Platform(
            "NATS authorization callout task set closed unexpectedly".to_owned(),
        )),
    }
}

fn callout_denial_code(error: &AuthorizationStateError) -> &'static str {
    match error {
        AuthorizationStateError::ApprovalRequired { .. } => "authority_pending",
        AuthorizationStateError::NotAuthorized => "not_authorized",
        AuthorizationStateError::WrongPrincipalKind => "wrong_principal_kind",
        AuthorizationStateError::SessionMissing => "session_not_found",
        AuthorizationStateError::SessionExpired => "session_expired",
        AuthorizationStateError::SessionRevoked => "session_revoked",
        AuthorizationStateError::NotFound
        | AuthorizationStateError::PrincipalMissing
        | AuthorizationStateError::PrincipalInactive
        | AuthorizationStateError::IdentityMissing => "principal_inactive",
        AuthorizationStateError::ParticipantMissing
        | AuthorizationStateError::ParticipantDigestMismatch
        | AuthorizationStateError::NeedsDigestMismatch => "participant_changed",
        AuthorizationStateError::AuthorityPending => "authority_pending",
        AuthorizationStateError::AuthorityMissing
        | AuthorizationStateError::AuthorityRejected
        | AuthorizationStateError::AuthorityRevoked
        | AuthorizationStateError::AuthorityStale
        | AuthorizationStateError::AuthorityExpired
        | AuthorizationStateError::RequiredDependencyUnavailable(_)
        | AuthorizationStateError::RequiredResourceUnavailable(_)
        | AuthorizationStateError::MaterializationStale
        | AuthorizationStateError::ContextLifetimeUnavailable
        | AuthorizationStateError::ContextSnapshotChanged
        | AuthorizationStateError::PortalPolicyChanged => "authority_unavailable",
        AuthorizationStateError::DeploymentInactive => "deployment_inactive",
        AuthorizationStateError::InstanceInactive => "instance_inactive",
        AuthorizationStateError::DeviceInactive => "device_inactive",
        AuthorizationStateError::ActivationMissing => "delegation_missing",
        AuthorizationStateError::DelegationExpired => "delegation_expired",
        AuthorizationStateError::StorageConflict => "authority_unavailable",
        AuthorizationStateError::RevisionConflict { .. } => "authority_unavailable",
        AuthorizationStateError::CurrentIssuerConflict => "issuer_current",
        AuthorizationStateError::IssuerMissing => "issuer_missing",
        AuthorizationStateError::Storage(_) => "internal_error",
        AuthorizationStateError::InvalidRecord(_) => "invalid_auth_token",
    }
}

fn server_xkey(headers: Option<&HeaderMap>) -> Result<XKey, AuthorizationStateError> {
    let encoded = headers
        .and_then(|headers| headers.get(SERVER_XKEY_HEADER))
        .map(ToString::to_string)
        .ok_or_else(|| denied("authorization request has no server XKey"))?;
    XKey::from_public_key(&encoded).map_err(|_| denied("authorization request XKey is invalid"))
}

fn verify_nats_nonce_signature(
    session_nkey: &str,
    nonce: &str,
    encoded_signature: &str,
) -> Result<(), AuthorizationStateError> {
    let key =
        KeyPair::from_public_key(session_nkey).map_err(|_| denied("session NKey is invalid"))?;
    if key.key_pair_type() != KeyPairType::User {
        return Err(denied("session NKey is not a user key"));
    }
    let signature = STANDARD
        .decode(encoded_signature)
        .or_else(|_| URL_SAFE_NO_PAD.decode(encoded_signature))
        .map_err(|_| denied("NATS nonce signature is invalid"))?;
    key.verify(nonce.as_bytes(), &signature)
        .map_err(|_| denied("NATS nonce signature is invalid"))
}

fn verify_connect_nkey_matches_context(
    session_nkey: &str,
    context: &VerifiedAuthorizationContext,
) -> Result<(), AuthorizationStateError> {
    // NATS User NKeys encode the same raw 32-byte Ed25519 public key bound by
    // the verified authorization context.
    let key =
        KeyPair::from_public_key(session_nkey).map_err(|_| denied("session NKey is invalid"))?;
    if key.key_pair_type() != KeyPairType::User {
        return Err(denied("session NKey is not a user key"));
    }
    let (_, nkey_bytes) =
        nkeys::from_public_key(session_nkey).map_err(|_| denied("session NKey is invalid"))?;
    if nkey_bytes
        .as_slice()
        .ct_eq(context.session_key().as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(denied(
            "session NKey does not encode the verified context key",
        ));
    }
    Ok(())
}

fn is_nkey(value: &str, expected: KeyPairType) -> bool {
    KeyPair::from_public_key(value).is_ok_and(|key| key.key_pair_type() == expected)
}

fn account_key(path: &Path, label: &str) -> Result<KeyPair, AuthorizationStateError> {
    let seed = read_secret(path, label)?;
    let key = KeyPair::from_seed(&seed).map_err(|error| {
        AuthorizationStateError::InvalidRecord(format!("{label} signing seed is invalid: {error}"))
    })?;
    if key.key_pair_type() != KeyPairType::Account {
        return invalid(format!("{label} signing seed must be an account NKey"));
    }
    Ok(key)
}

fn user_claims(path: &Path, label: &str) -> Result<Claims<User>, AuthorizationStateError> {
    let credentials = fs::read_to_string(path).map_err(|error| {
        AuthorizationStateError::Storage(format!("failed to read {label} creds: {error}"))
    })?;
    let jwt = credentials
        .lines()
        .skip_while(|line| *line != "-----BEGIN NATS USER JWT-----")
        .skip(1)
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(format!("{label} creds contain no user JWT"))
        })?;
    Claims::<User>::decode(jwt).map_err(|error| {
        AuthorizationStateError::InvalidRecord(format!("{label} JWT is invalid: {error}"))
    })
}

fn issuer_account(claims: &Claims<User>) -> String {
    claims
        .payload()
        .issuer_account
        .clone()
        .unwrap_or_else(|| claims.iss.clone())
}

fn read_secret(path: &Path, label: &str) -> Result<String, AuthorizationStateError> {
    fs::read_to_string(path)
        .map(|value| value.trim().to_owned())
        .map_err(|error| {
            AuthorizationStateError::Storage(format!("failed to read {label} seed: {error}"))
        })
}

fn now_millis() -> Result<i64, AuthorizationStateError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| AuthorizationStateError::Storage("current time exceeds i64".to_owned()))
}

fn denied(message: impl Into<String>) -> AuthorizationStateError {
    AuthorizationStateError::InvalidRecord(message.into())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, AuthorizationStateError> {
    Err(AuthorizationStateError::InvalidRecord(message.into()))
}

fn invalid_denial<T>() -> Result<T, AuthorizationStateError> {
    Err(denied(
        "session bootstrap JWT is not an exact deny-all credential",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trellis_protocol::{TransportResponseAuthorizationV1, TRANSPORT_AUTHORIZATION_FORMAT_V1};

    #[test]
    fn disconnect_advisory_reconstructs_exact_connection_identity() {
        let advisory: DisconnectEvent = serde_json::from_value(serde_json::json!({
            "server": { "id": "srv-A" },
            "client": { "id": 42, "user": "USESSION" }
        }))
        .unwrap();
        let server = advisory.server.unwrap();
        let client = advisory.client.unwrap();
        assert_eq!(server.id.as_deref(), Some("srv-A"));
        assert_eq!(client.id, Some(42));
        assert_eq!(client.user_nkey.as_deref(), Some("USESSION"));
        assert_eq!(
            connection_id(
                server.id.as_deref().unwrap(),
                &client.id.unwrap().to_string(),
                client.user_nkey.as_deref().unwrap(),
            ),
            connection_id("srv-A", "42", "USESSION")
        );
    }

    #[test]
    fn bootstrap_and_issued_jwts_preserve_the_account_boundary(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let auth_signing_key = Arc::new(KeyPair::new_account());
        let target_signing_key = Arc::new(KeyPair::new_account());
        let auth_account = KeyPair::new_account().public_key();
        let target_account = KeyPair::new_account().public_key();
        let keys = CalloutKeys {
            auth_signing_key: Arc::clone(&auth_signing_key),
            target_signing_key: Arc::clone(&target_signing_key),
            xkey: XKey::new(),
            auth_account: auth_account.clone(),
            target_account: target_account.clone(),
            sentinel_public_key: KeyPair::new_user().public_key(),
        };
        let session = KeyPair::new_user();
        let session_nkey = session.public_key();
        let deny = Permission {
            allow: Vec::new(),
            deny: vec![">".to_owned()],
        };
        let mut bootstrap = User::new_claims("session".to_owned(), session_nkey.clone());
        bootstrap.payload_mut().issuer_account = Some(auth_account.clone());
        bootstrap.payload_mut().permissions.permissions = Permissions {
            publish: deny.clone(),
            subscribe: deny,
            resp: None,
        };
        bootstrap.exp = Some(100);
        let bootstrap = bootstrap.encode(&auth_signing_key)?;
        keys.validate_bootstrap_jwt(&bootstrap, &session_nkey, 1)?;
        assert!(keys
            .validate_bootstrap_jwt(&bootstrap, &session_nkey, 100)
            .is_err());
        let mut mismatched_bootstrap = Claims::<User>::decode(&bootstrap)?;
        mismatched_bootstrap.sub = KeyPair::new_user().public_key();
        let mismatched_bootstrap = mismatched_bootstrap.encode(&auth_signing_key)?;
        assert!(keys
            .validate_bootstrap_jwt(&mismatched_bootstrap, &session_nkey, 1)
            .is_err());

        let nonce = "server-nonce";
        let signature = session.sign(nonce.as_bytes())?;
        verify_nats_nonce_signature(&session_nkey, nonce, &STANDARD.encode(&signature))?;
        verify_nats_nonce_signature(&session_nkey, nonce, &URL_SAFE_NO_PAD.encode(&signature))?;
        assert!(verify_nats_nonce_signature(
            &session_nkey,
            "different-server-nonce",
            &STANDARD.encode(&signature),
        )
        .is_err());

        let issued_user_nkey = KeyPair::new_user().public_key();
        let authenticated_name = "trellis.auth.v1:digest:srv-A:42";
        let issued = keys.authorized_user_jwt(
            &issued_user_nkey,
            &TransportAuthorizationV1 {
                format: TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
                account: target_account.clone(),
                publish_allow: vec!["rpc.v1.Example".to_owned()],
                subscribe_allow: vec!["_INBOX.example.>".to_owned()],
                response: Some(TransportResponseAuthorizationV1 {
                    max_messages: 65_535,
                    ttl_ms: 120_000,
                }),
                hard_expires_at: None,
            },
            authenticated_name,
            Some(200),
        )?;
        let payload = issued
            .split('.')
            .nth(1)
            .ok_or("issued JWT has no payload")?;
        let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
        assert_eq!(claims["iss"], target_signing_key.public_key());
        assert_eq!(claims["sub"], issued_user_nkey);
        assert_eq!(claims["name"], authenticated_name);
        assert_eq!(claims["aud"], target_account);
        assert_eq!(claims["exp"], 200);
        assert_eq!(claims["nats"]["issuer_account"], target_account);
        assert_eq!(
            claims["nats"]["pub"]["allow"],
            serde_json::json!(["rpc.v1.Example"])
        );
        assert_eq!(
            claims["nats"]["sub"]["allow"],
            serde_json::json!(["_INBOX.example.>"])
        );
        assert_eq!(claims["nats"]["resp"]["max"], 65_535);

        let device_without_apis = keys.authorized_user_jwt(
            &issued_user_nkey,
            &TransportAuthorizationV1 {
                format: TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
                account: target_account.clone(),
                publish_allow: Vec::new(),
                subscribe_allow: Vec::new(),
                response: None,
                hard_expires_at: None,
            },
            authenticated_name,
            None,
        )?;
        let payload = device_without_apis
            .split('.')
            .nth(1)
            .ok_or("issued JWT has no payload")?;
        let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
        assert!(claims["nats"]["resp"].is_null());
        assert!(claims["exp"].is_null());

        let device_with_apis = keys.authorized_user_jwt(
            &issued_user_nkey,
            &TransportAuthorizationV1 {
                format: TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
                account: target_account.clone(),
                publish_allow: Vec::new(),
                subscribe_allow: Vec::new(),
                response: Some(TransportResponseAuthorizationV1 {
                    max_messages: 65_535,
                    ttl_ms: 120_000,
                }),
                hard_expires_at: None,
            },
            authenticated_name,
            Some(500),
        )?;
        let payload = device_with_apis
            .split('.')
            .nth(1)
            .ok_or("issued JWT has no payload")?;
        let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
        assert_eq!(claims["nats"]["resp"]["max"], 65_535);
        assert_eq!(claims["exp"], 500);

        let mut request = AuthRequest {
            user_nkey: issued_user_nkey.clone(),
            ..Default::default()
        };
        request.server.id = KeyPair::new_server().public_key();
        let response = keys.response(&request, Some(issued), None)?;
        let response = std::str::from_utf8(&response)?;
        let payload = response
            .split('.')
            .nth(1)
            .ok_or("authorization response JWT has no payload")?;
        let response: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
        assert_eq!(response["iss"], auth_signing_key.public_key());
        assert_eq!(response["sub"], issued_user_nkey);
        assert_eq!(response["aud"], request.server.id);
        assert_eq!(response["nats"]["issuer_account"], auth_account);
        Ok(())
    }

    #[test]
    fn callout_limiter_bounds_total_in_flight() {
        let limiter = Arc::new(CalloutLimiter::default());
        let permits: Vec<_> = (0..MAX_CONCURRENT_REQUESTS)
            .map(|_| limiter.try_acquire().unwrap())
            .collect();
        assert!(limiter.try_acquire().is_none());
        drop(permits);
        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn callout_denials_never_expose_internal_causes() {
        let secret = "postgres://admin:secret@internal/auth";
        let code = callout_denial_code(&AuthorizationStateError::Storage(secret.to_owned()));
        assert_eq!(code, "internal_error");
        assert!(!code.contains(secret));
    }

    /// Resolves the pinned NATS binary, preferring the shared cache so a
    /// prepared machine never needs a network fetch to run the live test.
    #[cfg(feature = "nats-leases")]
    fn pinned_nats_binary() -> std::path::PathBuf {
        use trellis_local_nats::{NatsBinarySource, NatsServerBinary};
        if let Some(home) = std::env::var_os("HOME") {
            let candidate = std::path::PathBuf::from(home)
                .join(".cache/trellis")
                .join(format!(
                    "nats-server-v{}",
                    trellis_local_nats::pinned_version().expect("pinned nats version")
                ));
            if candidate.is_file() {
                if let Ok(path) =
                    NatsServerBinary::resolve(&NatsBinarySource::Path(candidate), None)
                {
                    return path;
                }
            }
        }
        let cache = std::env::temp_dir().join("trellis-callout-reconcile-nats-cache");
        std::fs::create_dir_all(&cache).expect("private nats cache dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))
                .expect("private nats cache permissions");
        }
        NatsServerBinary::resolve(&NatsBinarySource::DownloadPinned, Some(&cache))
            .expect("resolve pinned nats-server")
    }

    #[cfg(feature = "nats-leases")]
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local addr")
            .port()
    }

    /// A valid retained attachment for one server/client, already past its
    /// pending-admission deadline.
    #[cfg(feature = "nats-leases")]
    fn retained_attachment(server_id: &str, cid: u64, now: i64) -> AuthConnectionPresence {
        use base64::Engine as _;
        use trellis_protocol::TransportAuthorizationV1;
        let session_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32])
                .verifying_key()
                .to_bytes(),
        );
        AuthConnectionPresence {
            storage_revision: 0,
            format: "trellis.auth-connection-presence.v2".to_owned(),
            connection_id: trellis_protocol::digest_json(&serde_json::json!({
                "serverId": server_id,
                "clientId": cid,
            }))
            .expect("connection id digest"),
            runtime_connection_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            session_key,
            login_session_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned()),
            principal_id: "usr_reconcile".to_owned(),
            principal_kind: trellis_protocol::AuthorizationPrincipalKind::User,
            participant_id: "app-reconcile".to_owned(),
            deployment_id: None,
            instance_id: None,
            context_digest: trellis_protocol::digest_json(&serde_json::json!("context"))
                .expect("context digest"),
            transport_authorization_digest: TransportAuthorizationV1 {
                format: TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
                account: nkeys::KeyPair::new_account().public_key(),
                publish_allow: vec!["rpc.v1.Example".to_owned()],
                subscribe_allow: vec!["_INBOX.>".to_owned()],
                response: None,
                hard_expires_at: None,
            }
            .digest()
            .expect("transport authorization digest"),
            attachment_state: AuthAttachmentState::Pending,
            pending_deadline: Some(now - 1_000),
            server_id: server_id.to_owned(),
            client_id: cid.to_string(),
            user_nkey: "UDUMMYRECONCILE".to_owned(),
            remote_address: Some("127.0.0.1".to_owned()),
            connected_at: now,
            last_seen_at: now,
            version: 2,
        }
    }

    /// Reconciliation only acts on authoritative evidence from a real broker:
    /// a record whose owning server did not answer inventory stays tracked (so
    /// a live socket cannot become untracked and escape later enforcement), an
    /// answered server reporting no such connection retires the record, and an
    /// answered server reporting a different authenticated identity at the
    /// same cid kicks that socket and closes the stale record.
    #[cfg(feature = "nats-leases")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconciliation_retains_attachments_whose_server_did_not_answer() {
        use crate::platform::auth::transport_attachments::{connz, discover_servers};
        use crate::platform::auth::SqliteAuthorizationStore;
        use trellis_local_nats::{ManagedNatsServer, NatsOutput};

        let binary = pinned_nats_binary();
        let dir = tempfile::tempdir().expect("temp dir");
        let config_path = dir.path().join("nats.conf");
        let nats_port = free_port();
        let http_port = free_port();
        let ws_port = free_port();
        let config = format!(
            "port: {nats_port}\nhttp_port: {http_port}\n\
             websocket {{ port: {ws_port}, no_tls: true }}\n\
             jetstream {{ store_dir: \"{store}\" }}\n\
             accounts {{\n\
               SYS {{\n\
                 users: [ {{ user: \"sys\", password: \"pw\" }} ]\n\
               }}\n\
               APP {{\n\
                 users: [ {{ user: \"app\", password: \"pw\" }} ]\n\
                 jetstream: enabled\n\
               }}\n\
             }}\n\
             system_account: SYS\n",
            store = dir.path().join("jetstream").display(),
        );
        std::fs::write(&config_path, config).expect("write broker config");
        let mut server = ManagedNatsServer::start(
            &binary,
            &config_path,
            nats_port,
            http_port,
            ws_port,
            &dir.path().join("nats.pid"),
            &NatsOutput::Log {
                path: dir.path().join("nats.log"),
                mirror: false,
            },
        )
        .expect("start broker");
        let url = format!("nats://127.0.0.1:{nats_port}");
        let system =
            async_nats::ConnectOptions::with_user_and_password("sys".to_owned(), "pw".to_owned())
                .connect(&url)
                .await
                .expect("connect system account");
        let app =
            async_nats::ConnectOptions::with_user_and_password("app".to_owned(), "pw".to_owned())
                .connect(&url)
                .await
                .expect("connect trellis account");
        let ephemeral = NatsAuthEphemeralRepository::ensure(app.clone())
            .await
            .expect("open ephemeral repository");
        let repository =
            SqliteAuthorizationStore::open_in_memory().expect("open sqlite auth store");
        let real_server = discover_servers(&system)
            .await
            .expect("discover servers")
            .into_iter()
            .next()
            .expect("one broker identity");
        let now = now_millis().expect("current time");

        // The server answered inventory and reports no such connection, so
        // absence is proven and the record retires.
        let absent = retained_attachment(&real_server, 999_999, now);
        let revision = ephemeral
            .put_connection_presence(absent.clone())
            .await
            .expect("record absent attachment");
        let mut absent = absent;
        absent.storage_revision = revision;
        reconcile_retained_attachments(&ephemeral, &system, &repository)
            .await
            .expect("reconcile with authoritative absence");
        assert!(
            !ephemeral
                .list_connection_presence(None)
                .await
                .expect("list attachments")
                .iter()
                .any(|record| record.connection_id == absent.connection_id),
            "authoritative broker absence must retire the record"
        );

        // No server answers for this record's identity, so absence is unknown.
        let unreachable = retained_attachment("UNKNOWN_SERVER_IDENTITY", 42, now);
        let revision = ephemeral
            .put_connection_presence(unreachable.clone())
            .await
            .expect("record unreachable attachment");
        let mut unreachable = unreachable;
        unreachable.storage_revision = revision;
        reconcile_retained_attachments(&ephemeral, &system, &repository)
            .await
            .expect("reconcile with unavailable inventory");
        assert!(
            ephemeral
                .list_connection_presence(None)
                .await
                .expect("list attachments")
                .iter()
                .any(|record| record.connection_id == unreachable.connection_id),
            "an attachment whose server did not answer inventory must be retained"
        );

        // The same server/cid is now held by a different authenticated
        // identity. That socket must be kicked and the stale record closed,
        // not retained forever.
        let (inventory, _) = connz(&system, &real_server, 0, None)
            .await
            .expect("read broker inventory");
        let app_cid = inventory
            .iter()
            .find(|connection| connection.authorized_user.as_deref() == Some("app"))
            .expect("the app socket is inventoried")
            .cid;
        let mismatched = retained_attachment(&real_server, app_cid, now);
        let revision = ephemeral
            .put_connection_presence(mismatched.clone())
            .await
            .expect("record mismatched attachment");
        let mut mismatched = mismatched;
        mismatched.storage_revision = revision;
        reconcile_retained_attachments(&ephemeral, &system, &repository)
            .await
            .expect("reconcile with a mismatched identity");
        assert!(
            !ephemeral
                .list_connection_presence(None)
                .await
                .expect("list attachments")
                .iter()
                .any(|record| record.connection_id == mismatched.connection_id),
            "a mismatched identity must retire the stale record"
        );
        let mut kicked = false;
        for _ in 0..50 {
            let (connections, _) = connz(&system, &real_server, 0, Some(app_cid))
                .await
                .expect("probe kicked socket");
            if connections.is_empty() {
                kicked = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            kicked,
            "the socket holding the mismatched identity must be kicked"
        );

        server.stop().expect("stop broker");
    }
}
