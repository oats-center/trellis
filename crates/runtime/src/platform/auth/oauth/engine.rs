//! OAuth protocol processing stays in oauth-as; this adapter authenticates
//! Trellis request selection and reevaluates durable grants before admission.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use oauth_as::{
    authorization::{AuthorizationRequest, AuthorizationResponse},
    client::ClientId,
    error::{ErrorCode, ErrorResponse},
    jwt::{JwsVerifiers, P256Verifier},
    rate_limit::FixedWindowRateLimiter,
    server::{
        AuthorizationServer, ClientCredential, ServerConfig, SystemClock, TokenRequest,
        TokenRequestContext, UserApproval,
    },
    store::Storage,
    token::TokenResponse,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    sign_oauth_admission, verify_oauth_admission, OAuthAdmission, OAuthAdmissionBinding,
    OAUTH_ADMISSION_FORMAT_V1,
};

use super::super::{
    policy::evaluate, AdmissionIdentity, AuthError, Credential, GrantApproval, Mutation,
    SqliteAuthorizationStore,
};
use super::{OAuthStorage, RequestedAuthority, TokenBinding};

pub(crate) struct OAuthEngine {
    pub(crate) server: AuthorizationServer<OAuthStorage>,
    pub(super) verifiers: JwsVerifiers,
    pub(super) device_entries: FixedWindowRateLimiter,
    pub(super) upstream_http: openidconnect::reqwest::Client,
    documents: super::ClientDocuments,
    pub(super) avatars: super::super::avatars::Avatars,
    nonces: Mutex<super::nonce::DpopNonces>,
    /// Installation TLS this listener serves, for the always-on user notice.
    generated_tls: std::sync::OnceLock<bool>,
}

/// Host nonce challenges are distinct from the library's protocol failures.
#[derive(Debug)]
pub(crate) enum TokenError {
    OAuth(ErrorResponse),
    UseDpopNonce(String),
}

impl From<ErrorResponse> for TokenError {
    fn from(error: ErrorResponse) -> Self {
        Self::OAuth(error)
    }
}

/// Never Debug-format token-response extensions: admission is a credential too.
#[derive(Deserialize, Serialize)]
pub(crate) struct TrellisTokenResponse {
    #[serde(flatten)]
    pub(crate) oauth: TokenResponse,
    pub(crate) trellis_admission: String,
}

impl SqliteAuthorizationStore {
    pub(in crate::platform::auth) fn oauth_family_deadline_in(
        &self,
        connection: &crate::platform::auth::sqlite::Connection,
        grant: &str,
        family: &str,
        now: i64,
    ) -> Result<i64, AuthError> {
        connection
            .query_row(
                "SELECT expires_at FROM auth_oauth_families \
                WHERE family_lookup_hash=?1 \
                AND oauth_grant_id=?2 \
                AND revoked_at IS NULL \
                AND expires_at>?3",
                params![
                    URL_SAFE_NO_PAD.encode(Sha256::digest(family.as_bytes())),
                    grant,
                    now
                ],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(AuthError::Denied)
    }

    /// Shared token/RPC minting owner: original family and current grant policy,
    /// never a rotated refresh secret or a different family's lifetime.
    pub(in crate::platform::auth) fn sign_oauth_admission_in(
        &self,
        connection: &crate::platform::auth::sqlite::Connection,
        identity: &AdmissionIdentity,
        ephemeral_nkey: &str,
        current: &super::super::policy::Evaluation,
        now: i64,
    ) -> Result<trellis_protocol::SignedOAuthAdmission, AuthError> {
        self.current_signer(connection)?;
        let (Credential::OAuthGrant(grant), Some(family)) =
            (&identity.credential, &identity.refresh_family_id)
        else {
            return Err(AuthError::Denied);
        };
        if !current.required_missing.is_empty() {
            return Err(AuthError::RequiredMissing);
        }
        let deadline = self.oauth_family_deadline_in(connection, grant, family, now)?;
        let (principal, login, client, kind, value, jkt): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = connection.query_row(
            "SELECT principal_id,login_session_id,client_id,binding_kind,binding_value,\
            durable_dpop_jkt \
            FROM auth_oauth_grants \
            WHERE oauth_grant_id=?1",
            [grant],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        if principal != current.principal_id {
            return Err(AuthError::Denied);
        }
        let binding = match kind.as_str() {
            "browser" => OAuthAdmissionBinding::Browser {
                browser_origin: value,
            },
            "native" => OAuthAdmissionBinding::Native {
                native_dpop_jkt: value,
            },
            _ => return Err(AuthError::Denied),
        };
        let (kind, key) = nkeys::from_public_key(ephemeral_nkey).map_err(|_| AuthError::Denied)?;
        if nkeys::KeyPairType::from(kind) != nkeys::KeyPairType::User
            || URL_SAFE_NO_PAD.encode(key) != identity.session_public_key
        {
            return Err(AuthError::Denied);
        }
        let admission_ttl = i64::from(self.settings.oauth_admission_lifetime_seconds);
        let expires = current
            .not_after
            .min(deadline)
            .min(now.saturating_add(admission_ttl));
        if expires <= now {
            return Err(AuthError::Denied);
        }
        Ok(sign_oauth_admission(
            OAuthAdmission {
                format: OAUTH_ADMISSION_FORMAT_V1.into(),
                issuer_key_id: self.current_issuer(connection)?.key_id,
                trellis_instance_id: self.settings.instance_id.clone(),
                audience_nats_account: self.settings.account.clone(),
                oauth_grant_id: grant.clone(),
                refresh_family_id: family.clone(),
                login_session_id: login,
                principal_id: principal,
                client_id: client,
                binding,
                persistent_dpop_jkt: jkt,
                runtime_id: identity.runtime_id.clone(),
                ephemeral_nkey: ephemeral_nkey.into(),
                issued_at: now,
                not_before: now,
                expires_at: expires,
            },
            &self.current_signer(connection)?,
        )?)
    }

    pub(in crate::platform::auth) fn provisioned_admission_identity_in(
        &self,
        connection: &crate::platform::auth::sqlite::Connection,
        credential: &[u8],
        ephemeral_nkey: &str,
        inbox_prefix: &str,
        now: i64,
    ) -> Result<AdmissionIdentity, AuthError> {
        if credential.is_empty() || credential.len() > 64 * 1024 {
            return Err(AuthError::Denied);
        }
        let unsigned =
            serde_json::from_slice::<trellis_protocol::SignedProvisionedAdmission>(credential)
                .map_err(|_| AuthError::Denied)?;
        let public_key: String = connection
            .query_row(
                "SELECT identity_public_key \
                FROM auth_provisioned_identities \
                WHERE identity_key_id=?1 \
                AND state='active'",
                [&unsigned.credential.credential_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(AuthError::Denied)?;
        let body = trellis_protocol::verify_provisioned_admission(
            credential,
            &public_key,
            &self.settings.instance_id,
            &self.settings.account,
            ephemeral_nkey,
            now,
        )
        .map_err(|_| AuthError::Denied)?;
        let body = body.credential;
        let credential = super::super::Credential::ProvisionedIdentity(body.credential_id);
        super::super::authorization_sessions::credential_snapshot(connection, &credential, now)?;
        let (_, key) = nkeys::from_public_key(ephemeral_nkey).map_err(|_| AuthError::Denied)?;
        Ok(AdmissionIdentity {
            credential,
            refresh_family_id: None,
            runtime_id: body.runtime_id,
            session_public_key: URL_SAFE_NO_PAD.encode(key),
            inbox_prefix: inbox_prefix.to_owned(),
        })
    }

    pub(in crate::platform::auth) fn oauth_admission_identity_in(
        &self,
        connection: &crate::platform::auth::sqlite::Connection,
        credential: &[u8],
        ephemeral_nkey: &str,
        inbox_prefix: &str,
        now: i64,
    ) -> Result<AdmissionIdentity, AuthError> {
        if credential.is_empty() || credential.len() > 64 * 1024 {
            return Err(AuthError::Denied);
        }
        let unverified: trellis_protocol::SignedOAuthAdmission =
            serde_json::from_slice(credential).map_err(|_| AuthError::Denied)?;
        let issuer = Self::resolve_issuer_in(connection, &unverified.credential.issuer_key_id)?;
        let admission = verify_oauth_admission(
            credential,
            &issuer,
            &self.settings.instance_id,
            &self.settings.account,
            ephemeral_nkey,
            now,
            300,
        )?;
        let body = admission.credential;
        let grant: Option<(String, String, String, String, String, String)> = connection
            .query_row(
                "SELECT principal_id,login_session_id,client_id,binding_kind,binding_value,\
                durable_dpop_jkt \
                FROM auth_oauth_grants \
                WHERE oauth_grant_id=?1 \
                AND revoked_at IS NULL \
                AND expires_at>?2",
                params![body.oauth_grant_id, now],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let (principal, login, client, kind, value, jkt) = grant.ok_or(AuthError::Denied)?;
        let binding_matches = match &body.binding {
            OAuthAdmissionBinding::Browser { browser_origin } => {
                kind == "browser" && browser_origin == &value
            }
            OAuthAdmissionBinding::Native { native_dpop_jkt } => {
                kind == "native" && native_dpop_jkt == &value
            }
        };
        let live_family: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_oauth_families \
            WHERE family_lookup_hash=?1 \
            AND oauth_grant_id=?2 \
            AND revoked_at IS NULL \
            AND expires_at>?3)",
            params![
                URL_SAFE_NO_PAD.encode(Sha256::digest(body.refresh_family_id.as_bytes())),
                body.oauth_grant_id,
                now
            ],
            |row| row.get(0),
        )?;
        if principal != body.principal_id
            || login != body.login_session_id
            || client != body.client_id
            || jkt != body.persistent_dpop_jkt
            || !binding_matches
            || !live_family
        {
            return Err(AuthError::Denied);
        }
        let (_, key) = nkeys::from_public_key(ephemeral_nkey).map_err(|_| AuthError::Denied)?;
        Ok(AdmissionIdentity {
            credential: Credential::OAuthGrant(body.oauth_grant_id),
            refresh_family_id: Some(body.refresh_family_id),
            runtime_id: body.runtime_id,
            session_public_key: URL_SAFE_NO_PAD.encode(key),
            inbox_prefix: inbox_prefix.into(),
        })
    }
}

struct OAuthAudit;

impl oauth_as::events::EventSink for OAuthAudit {
    fn on_event(&self, event: oauth_as::events::Event<'_>) {
        // Explicit fields only: never Debug-format engine events or credentials.
        match event {
            oauth_as::events::Event::RefreshTokenReuseDetected {
                family_id,
                records_revoked,
                containment_failed,
                ..
            } => tracing::warn!(
                security_event = "oauth_refresh_reuse",
                family_digest = %URL_SAFE_NO_PAD.encode(Sha256::digest(family_id.as_bytes())),
                records_revoked, containment_failed,
                "Spent OAuth refresh token reused"
            ),
            oauth_as::events::Event::AuthorizationCodeReplayDetected {
                tokens_revoked,
                containment_failed,
                ..
            } => tracing::warn!(
                security_event = "oauth_authorization_code_replay",
                tokens_revoked,
                containment_failed,
                "Consumed OAuth authorization code replayed"
            ),
            oauth_as::events::Event::ClientAuthenticationFailed { .. } => tracing::debug!(
                security_event = "oauth_client_authentication_failed",
                "OAuth client authentication denied"
            ),
            _ => {}
        }
    }
}

fn rejected() -> ErrorResponse {
    ErrorResponse::new(ErrorCode::InvalidGrant)
}
impl OAuthEngine {
    /// Records whether this OAuth listener serves Trellis-generated certificates,
    /// so every Portal page can show the not-for-production banner.
    pub(crate) fn serve_generated_tls(&self, generated: bool) {
        let _ = self.generated_tls.set(generated);
    }

    pub(crate) fn generated_tls(&self) -> bool {
        self.generated_tls.get().copied().unwrap_or(false)
    }

    pub(crate) fn new(storage: OAuthStorage) -> Result<Self, AuthError> {
        let origin = url::Url::parse(&storage.authority.settings.oauth_origin)
            .map_err(|_| AuthError::Denied)?;
        let loopback = match origin.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        // Reservation and browser binding use this same origin identity. A URL
        // prefix or noncanonical representation could otherwise let an ordinary
        // application register a callback on the cookie-bearing origin.
        if origin.origin().ascii_serialization() != storage.authority.settings.oauth_origin
            || !(origin.scheme() == "https" || (origin.scheme() == "http" && loopback))
        {
            return Err(AuthError::Denied);
        }
        {
            let connection = storage
                .authority
                .connection
                .lock()
                .map_err(|_| AuthError::Unavailable)?;
            let mut clients = connection.prepare(
                "SELECT redirect_uris_json FROM auth_oauth_clients WHERE state='active'",
            )?;
            for callbacks in clients.query_map([], |row| row.get::<_, String>(0))? {
                for callback in serde_json::from_str::<Vec<String>>(&callbacks?)? {
                    let callback = url::Url::parse(&callback).map_err(|_| AuthError::Denied)?;
                    super::super::warn_oauth_cookie_isolation(&origin, &callback);
                }
            }
        }
        let origin = storage.authority.settings.oauth_origin.clone();
        let documents = super::ClientDocuments::new(origin.clone())?;
        let avatars = super::super::avatars::Avatars::new(storage.authority.clone())?;
        let upstream_http =
            super::oidc::http_client(&storage.authority.settings.upstream_tls_roots_pem)?;
        let mut config = ServerConfig::new(&origin, format!("{origin}/device"));
        config.authorization_code_ttl = Duration::from_secs(60);
        config.access_token_ttl = Duration::from_secs(300);
        config.refresh_token_ttl = Some(Duration::from_secs(30 * 24 * 60 * 60));
        config.require_dpop = true;
        config.device_code_ttl = Duration::from_secs(300);
        config.authorization_details_types_supported = Some(vec![
            "trellis_capabilities".into(),
            "trellis_platform_privileges".into(),
        ]);
        config.allowed_resources = Some(Vec::new().into_boxed_slice());
        let server = AuthorizationServer::with_clock(config, storage, SystemClock)
            .with_jws_verifier(Arc::new(P256Verifier))
            .with_rate_limiter(Box::new(FixedWindowRateLimiter::new()))
            .with_event_sink(Box::new(OAuthAudit));
        let mut verifiers = JwsVerifiers::new();
        verifiers.install(Arc::new(P256Verifier));
        Ok(Self {
            server,
            verifiers,
            documents,
            avatars,
            upstream_http,
            device_entries: FixedWindowRateLimiter::new(),
            generated_tls: std::sync::OnceLock::new(),
            nonces: Mutex::new(
                super::nonce::DpopNonces::new(Instant::now())
                    .map_err(|_| AuthError::Unavailable)?,
            ),
        })
    }

    /// Intake may retrieve an unknown public client's metadata. Admission and
    /// renewal use the already approved durable grant, never an HTTP fetch.
    pub(crate) async fn prepare_client(&self, client: &ClientId) -> Result<(), ErrorResponse> {
        let state: Option<String> = {
            let connection = self
                .server
                .store()
                .authority
                .connection
                .lock()
                .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?;
            connection
                .query_row(
                    "SELECT state FROM auth_oauth_clients WHERE client_id=?1",
                    [client.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?
        };
        if let Some(state) = state {
            return if state == "active" {
                Ok(())
            } else {
                Err(ErrorResponse::new(ErrorCode::InvalidClient))
            };
        }
        let document = self
            .documents
            .get(client.as_str())
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::InvalidClient))?;
        self.server
            .store()
            .install_client_document(&document)
            .map_err(|error| {
                ErrorResponse::new(match error {
                    AuthError::ResourceLimit | AuthError::Conflict => {
                        ErrorCode::TemporarilyUnavailable
                    }
                    _ => ErrorCode::ServerError,
                })
            })
    }

    /// Called after the portal authenticates the login and CSRF-protected choices.
    /// The engine validates the exact callback and PKCE before any grant is stored.
    pub(crate) async fn approve(
        &self,
        request: &AuthorizationRequest<'_>,
        identity: &Mutation,
        approval: &GrantApproval,
    ) -> Result<AuthorizationResponse, super::portal::ApprovalError> {
        let requested = RequestedAuthority::parse(
            request
                .authorization_details
                .as_deref()
                .ok_or_else(|| ErrorResponse::new(ErrorCode::InvalidAuthorizationDetails))?,
        )?;
        let mut validated = self
            .server
            .validate_authorization_request(request)
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::InvalidRequest))?;
        if validated.client_id.as_str() != approval.client_id
            || validated.dpop_jkt.as_deref() != Some(&approval.durable_dpop_jkt)
            || requested.capabilities.required != approval.selection.required
            || requested.capabilities.optional != approval.selection.optional
            || requested.required_privileges != approval.required_privileges
            || !requested
                .required_privileges
                .iter()
                .all(|privilege| approval.privileges.contains(privilege))
            || !approval.privileges.iter().all(|privilege| {
                requested.required_privileges.contains(privilege)
                    || requested.optional_privileges.contains(privilege)
            })
        {
            return Err(rejected().into());
        }
        if approval.binding_kind == "browser" {
            let callback = url::Url::parse(&validated.redirect_uri).map_err(|_| rejected())?;
            if callback.origin().ascii_serialization() != approval.binding_value {
                return Err(rejected().into());
            }
        }
        let now = crate::clock::unix_now();
        let grant = self
            .server
            .store()
            .authority
            .approve_grant(identity, approval, now)
            .map_err(super::portal::ApprovalError::from)?;
        // OAuth reports what was actually granted, not unchecked request JSON.
        let actual = self
            .server
            .store()
            .authority
            .transaction(|connection| {
                let current = evaluate(connection, &Credential::OAuthGrant(grant.clone()), now)?;
                if !current.required_missing.is_empty() {
                    return Err(AuthError::RequiredMissing);
                }
                let approved: Vec<_> = current
                    .capabilities
                    .iter()
                    .map(|capability| capability.capability_id.clone())
                    .collect();
                let mut details = vec![serde_json::json!({
                    "type":"trellis_capabilities",
                    "required":approval.selection.required,
                    "optional": approved
                            .into_iter()
                            .filter(|name| !approval.selection.required.contains(name))
                            .collect::<Vec<_>>()
                })];
                if !approval.privileges.is_empty() {
                    details.push(serde_json::json!({
                    "type":"trellis_platform_privileges",
                    "required":requested.required_privileges,
                    "optional": approval
                            .privileges
                            .iter()
                            .filter(|privilege| !requested.required_privileges.contains(privilege))
                            .collect::<Vec<_>>()
                }));
                }
                Ok(serde_json::to_string(&details)?)
            })
            .map_err(|_| rejected())?;
        validated.authorization_details = oauth_as::rar::AuthorizationDetails::parse(&actual)?;
        self.server
            .issue_authorization_code(UserApproval::granted(&validated, grant))
            .await
            .map_err(|_| rejected().into())
    }

    /// Verify the same DPoP proof twice without claiming it twice: the precheck
    /// authenticates extension claims, then the engine owns protocol/replay state.
    pub(crate) async fn token(
        &self,
        request: TokenRequest,
        proof: &str,
        runtime_id: &str,
        ephemeral_nkey: &str,
    ) -> Result<TrellisTokenResponse, TokenError> {
        if matches!(request, TokenRequest::ClientCredentials { .. }) {
            return Err(ErrorResponse::new(ErrorCode::UnsupportedGrantType).into());
        }
        let uri = format!(
            "{}/token",
            self.server.store().authority.settings.oauth_origin
        );
        let binding = TokenBinding::verify(
            &self.verifiers,
            proof,
            &uri,
            runtime_id,
            ephemeral_nkey,
            SystemTime::now(),
        )
        .map_err(|_| ErrorResponse::new(ErrorCode::InvalidDpopProof))?;
        // Only the signature/HTTP/extension-verified proof supplies this nonce.
        // Challenge before the library can consume a code, refresh token or jti.
        let challenge = self
            .nonces
            .lock()
            .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?
            .check(binding.nonce.as_deref(), Instant::now())
            .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?;
        if let Some(nonce) = challenge {
            return Err(TokenError::UseDpopNonce(nonce));
        }
        let context =
            TokenRequestContext::new(ClientCredential::secret(None)).with_dpop_proof(proof);
        if let TokenRequest::DeviceCode {
            device_code,
            client_id,
            ..
        } = &request
        {
            let selection = self
                .server
                .store()
                .device_selection(device_code)
                .map_err(|_| rejected())?;
            if selection.persistent_dpop_jkt != binding.persistent_dpop_jkt
                || selection.client_id != client_id.as_str()
            {
                return Err(rejected().into());
            }
        }
        let mut response = self.server.token_with_context(request, context).await?;
        let token = self
            .server
            .store()
            .get_token(&response.access_token)
            .await
            .map_err(|_| rejected())?
            .ok_or_else(rejected)?;
        if token.jkt.as_deref() != Some(binding.persistent_dpop_jkt.as_str()) {
            return Err(rejected().into());
        }
        let grant = token.subject.as_deref().ok_or_else(rejected)?;
        let family = token.family_id.as_deref().ok_or_else(rejected)?;
        let now = crate::clock::unix_now();
        let signed = self
            .server
            .store()
            .authority
            .transaction(|connection| {
                self.server.store().authority.current_signer(connection)?;
                let current = evaluate(connection, &Credential::OAuthGrant(grant.into()), now)?;
                if !current.required_missing.is_empty() {
                    return Err(AuthError::RequiredMissing);
                }
                let (_, key) = nkeys::from_public_key(&binding.ephemeral_nkey)
                    .map_err(|_| AuthError::Denied)?;
                let identity = AdmissionIdentity {
                    credential: Credential::OAuthGrant(grant.into()),
                    refresh_family_id: Some(family.into()),
                    runtime_id: binding.runtime_id,
                    session_public_key: URL_SAFE_NO_PAD.encode(key),
                    inbox_prefix: trellis_protocol::admission_inbox_prefix(
                        &URL_SAFE_NO_PAD.encode(key),
                    )?,
                };
                let signed = self.server.store().authority.sign_oauth_admission_in(
                    connection,
                    &identity,
                    &binding.ephemeral_nkey,
                    &current,
                    now,
                )?;
                if signed.credential.persistent_dpop_jkt != binding.persistent_dpop_jkt
                    || signed.credential.client_id != token.client_id.as_str()
                {
                    return Err(AuthError::Denied);
                }
                let expires = signed.credential.expires_at;
                // Read the exact granted details from the same current snapshot.
                let approved: Vec<_> = current
                    .capabilities
                    .iter()
                    .map(|capability| capability.capability_id.clone())
                    .collect();
                let mut actual = vec![serde_json::json!({
                    "type":"trellis_capabilities",
                    "required":current.selection.required,
                    "optional": approved
                            .into_iter()
                            .filter(|name| !current.selection.required.contains(name))
                            .collect::<Vec<_>>()
                })];
                if !current.platform_privileges.is_empty() {
                    let required: String = connection.query_row(
                        "SELECT required_privileges_json \
                    FROM auth_oauth_grants \
                    WHERE oauth_grant_id=?1",
                        [grant],
                        |row| row.get(0),
                    )?;
                    let required: Vec<trellis_protocol::PlatformPrivilege> =
                        serde_json::from_str(&required)?;
                    actual.push(serde_json::json!({
                        "type":"trellis_platform_privileges",
                        "required":required,
                        "optional": current
                                .platform_privileges
                                .iter()
                                .filter(|privilege| !required.contains(privilege))
                                .collect::<Vec<_>>()
                    }));
                }
                let actual = serde_json::Value::Array(actual);
                Ok((signed, actual, expires))
            })
            .map_err(|_| rejected())?;
        response.expires_in = response.expires_in.min((signed.2 - now) as u64);
        response.authorization_details =
            oauth_as::rar::AuthorizationDetails::parse(&signed.1.to_string())?;
        Ok(TrellisTokenResponse {
            oauth: response,
            trellis_admission: serde_json::to_string(&signed.0).map_err(|_| rejected())?,
        })
    }
}
