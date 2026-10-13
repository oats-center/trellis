//! Native public OAuth client. RFC 8628 and refresh use the production HTTP
//! endpoints; the existing custody lease owns refresh serialization and commits.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::time::Instant;
use trellis_protocol::{
    verify_oauth_admission, OAuthAdmissionBinding, SignedIssuerRotation, SignedOAuthAdmission,
};

use super::oauth_store::{NativeCredentialLease, NativeCredentialScope, NativeOAuthGrant};
use crate::client::{RuntimeAuthorizationTrust, SessionAuth};

/// Initial native connection metadata authenticated by the explicitly selected
/// OAuth origin. Never use discovery to replace a stored credential's issuer pin.
#[derive(Debug)]
pub struct NativeOAuthServer {
    /// Selected canonical OAuth issuer origin.
    pub issuer: String,
    /// Native NATS/TLS transport endpoints advertised by that origin.
    pub nats_servers: Vec<String>,
    /// Initial installation trust; for existing credentials this is only a
    /// successor hint that must be authenticated from their retained issuer pin.
    pub trust: RuntimeAuthorizationTrust,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    trellis: DiscoveryTrellis,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryTrellis {
    trellis_instance_id: String,
    audience_nats_account: String,
    nats_endpoints: Vec<String>,
    authority_format: String,
    admission_format: String,
    issuers: Vec<trellis_protocol::AuthorityIssuerKey>,
    rotation_sequence: trellis_protocol::live::U64s,
    required_authorization_details_types: Vec<String>,
}

/// Discover initial trust only from the explicitly selected HTTPS OAuth origin
/// (HTTP is permitted solely for literal loopback hosts). Redirects are rejected,
/// including same-origin redirects; neither broker metadata nor token responses
/// can establish or replace this pin.
pub async fn discover_native_oauth(
    selected_origin: &str,
) -> Result<NativeOAuthServer, NativeOAuthError> {
    let origin = url::Url::parse(selected_origin).map_err(|_| NativeOAuthError::InvalidResponse)?;
    let loopback = match origin.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if !(origin.scheme() == "https" || (origin.scheme() == "http" && loopback))
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(NativeOAuthError::InvalidResponse);
    }
    let issuer = origin.origin().ascii_serialization();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| NativeOAuthError::Unavailable)?;
    let mut response = http
        .get(format!("{issuer}/.well-known/oauth-authorization-server"))
        .send()
        .await
        .map_err(|_| NativeOAuthError::Unavailable)?;
    if !response.status().is_success() {
        return Err(NativeOAuthError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| NativeOAuthError::Unavailable)?
    {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err(NativeOAuthError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    let document: DiscoveryDocument =
        serde_json::from_slice(&bytes).map_err(|_| NativeOAuthError::InvalidResponse)?;
    let metadata = document.trellis;
    let instance = metadata
        .trellis_instance_id
        .parse::<ulid::Ulid>()
        .map_err(|_| NativeOAuthError::InvalidResponse)?;
    let account = nkeys::KeyPair::from_public_key(&metadata.audience_nats_account)
        .map_err(|_| NativeOAuthError::InvalidResponse)?;
    if document.issuer != issuer
        || instance.to_string() != metadata.trellis_instance_id
        || !metadata.audience_nats_account.starts_with('A')
        || account.public_key() != metadata.audience_nats_account
        || metadata.authority_format != trellis_protocol::SESSION_AUTHORITY_FORMAT_V1
        || metadata.admission_format != trellis_protocol::OAUTH_ADMISSION_FORMAT_V1
        || metadata.required_authorization_details_types.is_empty()
        || metadata
            .required_authorization_details_types
            .iter()
            .any(|kind| kind != "trellis_capabilities")
        || metadata.nats_endpoints.is_empty()
    {
        return Err(NativeOAuthError::InvalidResponse);
    }
    let mut nats_servers = Vec::new();
    for endpoint in &metadata.nats_endpoints {
        let parsed = url::Url::parse(endpoint).map_err(|_| NativeOAuthError::InvalidResponse)?;
        if !matches!(parsed.scheme(), "nats" | "tls" | "ws" | "wss")
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(NativeOAuthError::InvalidResponse);
        }
        if matches!(parsed.scheme(), "nats" | "tls") {
            endpoint
                .parse::<async_nats::ServerAddr>()
                .map_err(|_| NativeOAuthError::InvalidResponse)?;
            nats_servers.push(endpoint.clone());
        }
    }
    if nats_servers.is_empty() {
        return Err(NativeOAuthError::InvalidResponse);
    }
    let mut current = None;
    let mut ids = std::collections::BTreeSet::new();
    for key in metadata.issuers {
        if key.verifying_key().is_err() || !ids.insert(key.key_id.clone()) {
            return Err(NativeOAuthError::InvalidResponse);
        }
        if key.state == trellis_protocol::AuthorityIssuerState::Active
            && current.replace(key).is_some()
        {
            return Err(NativeOAuthError::InvalidResponse);
        }
    }
    Ok(NativeOAuthServer {
        issuer,
        nats_servers,
        trust: RuntimeAuthorizationTrust {
            instance_id: metadata.trellis_instance_id,
            account: metadata.audience_nats_account,
            issuer: current.ok_or(NativeOAuthError::InvalidResponse)?,
            rotation_sequence: metadata.rotation_sequence.get(),
        },
    })
}

/// Expected native OAuth failures. HTTP response bodies and credentials are never
/// included in errors. A possibly consumed refresh token requires a new standard
/// authorization flow; unavailable issuer trust can recover without erasing the grant.
#[derive(Debug, thiserror::Error)]
pub enum NativeOAuthError {
    /// Protected storage could not be opened or committed.
    #[error("native OAuth custody unavailable")]
    Custody(#[source] std::io::Error),
    /// Selected installation or pinned issuer does not match the credential.
    #[error("native OAuth trust or response mismatch")]
    InvalidResponse,
    /// No usable credential, or a potentially consumed credential needs recovery.
    #[error("standard OAuth authorization required")]
    RecoveryRequired,
    /// The user denied authorization.
    #[error("OAuth authorization denied")]
    Denied,
    /// Device authorization expired before approval.
    #[error("OAuth device authorization expired")]
    Expired,
    /// A bounded HTTP exchange failed; durable credentials remain stored.
    #[error("OAuth endpoint unavailable")]
    Unavailable,
    /// A discovery hint could not be authenticated from the retained issuer pin.
    /// Retry after trust recovery; no unsubmitted refresh credential is consumed.
    #[error("OAuth issuer trust unavailable")]
    TrustUnavailable,
}

/// Verified in-memory inputs for the ordinary one-step native NATS connection.
/// Never persisted or Debug-formatted: admission itself is a credential.
pub struct NativeOAuthSession {
    /// Fresh ephemeral Ed25519 signing material, distinct from the durable P-256 key.
    pub auth: SessionAuth,
    /// Random 128-bit runtime nonce bound by the signed OAuth admission.
    pub runtime_id: String,
    /// Issuer-verified short-lived admission for exactly this runtime and key.
    pub admission: SignedOAuthAdmission,
    /// Installation trust authenticated from the selected pin and signed rotations.
    pub trust: RuntimeAuthorizationTrust,
    /// Actual granted RFC 9396 details, including optional deselections.
    pub authorization_details: serde_json::Value,
    client: NativeOAuthClient,
}

impl NativeOAuthSession {
    /// Rotate using current trust authenticated by the connection's authority
    /// owner, retaining the runtime, session signing key, and exclusive custody.
    /// Ambiguous responses require a new authorization flow, never token retry.
    /// Dropping this future retains custody and any durable spent marker.
    pub async fn refresh(
        &mut self,
        trust: RuntimeAuthorizationTrust,
    ) -> Result<(), NativeOAuthError> {
        let scope = self
            .client
            .lease
            .lock()
            .map_err(|_| NativeOAuthError::RecoveryRequired)?
            .scope()
            .clone();
        if trust.instance_id != scope.instance_id
            || trust.account != scope.account
            || trust.issuer.state != trellis_protocol::AuthorityIssuerState::Active
            || trust.issuer.verifying_key().is_err()
            || trust.rotation_sequence < self.client.trust.rotation_sequence
            || (trust.rotation_sequence == self.client.trust.rotation_sequence
                && trust.issuer != self.client.trust.issuer)
        {
            return Err(NativeOAuthError::InvalidResponse);
        }
        let lease = Arc::clone(&self.client.lease);
        let retained = trust.clone();
        tokio::task::spawn_blocking(move || {
            lease
                .lock()
                .map_err(|_| std::io::Error::other("OAuth custody poisoned"))?
                .retain_trust(&retained)
        })
        .await
        .map_err(|_| NativeOAuthError::RecoveryRequired)?
        .map_err(NativeOAuthError::Custody)?;
        self.client.trust = trust;
        let bytes = self.client.rotate().await?;
        let (admission, details) = self.client.install_response(&bytes).await?;
        self.admission = admission;
        self.authorization_details = details;
        self.trust = self.client.trust.clone();
        Ok(())
    }
}

/// Locked native credential plus bounded, non-redirecting OAuth HTTP transport.
/// Opening the same credential in another process waits for this lease to drop.
pub struct NativeOAuthClient {
    lease: Arc<Mutex<NativeCredentialLease>>,
    http: reqwest::Client,
    trust: RuntimeAuthorizationTrust,
    auth: SessionAuth,
    runtime_id: String,
}

/// An outstanding RFC 8628 flow. Only the user-facing code and verification URI
/// are exposed; the device credential and key remain private and non-Debug.
pub struct NativeDeviceAuthorization {
    client: NativeOAuthClient,
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Duration,
    deadline: Instant,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct TokenResponse {
    token_type: String,
    refresh_token: String,
    trellis_admission: String,
    authorization_details: serde_json::Value,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

impl NativeOAuthClient {
    /// Open owner-only custody off the async executor. The caller must explicitly
    /// approve the first issuer pin; retained pins are restored rather than replaced
    /// by discovery. Cold resume authenticates any successor before token rotation.
    pub async fn open(
        directory: PathBuf,
        scope: NativeCredentialScope,
        trust: RuntimeAuthorizationTrust,
    ) -> Result<Self, NativeOAuthError> {
        if scope.instance_id != trust.instance_id
            || scope.account != trust.account
            || trust.issuer.state != trellis_protocol::AuthorityIssuerState::Active
            || trust.issuer.verifying_key().is_err()
        {
            return Err(NativeOAuthError::InvalidResponse);
        }
        let (lease, trust) = tokio::task::spawn_blocking(move || {
            let mut lease = NativeCredentialLease::lock(&directory, scope)?;
            let trust = lease.restore_trust(&trust)?;
            Ok::<_, std::io::Error>((lease, trust))
        })
        .await
        .map_err(|_| NativeOAuthError::Unavailable)?
        .map_err(NativeOAuthError::Custody)?;
        let auth =
            SessionAuth::from_seed_base64url(&URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()))
                .map_err(|_| NativeOAuthError::Unavailable)?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| NativeOAuthError::Unavailable)?;
        Ok(Self {
            lease: Arc::new(Mutex::new(lease)),
            http,
            trust,
            auth,
            runtime_id: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>()),
        })
    }

    /// Start the server's RFC 8628 user flow with the generated app's standard RAR
    /// details. No browser-origin shortcut or implied native consent is requested.
    pub async fn start_device(
        self,
        authorization_details: &serde_json::Value,
    ) -> Result<NativeDeviceAuthorization, NativeOAuthError> {
        let details = serde_json::to_string(authorization_details)
            .map_err(|_| NativeOAuthError::InvalidResponse)?;
        if details.len() > 32 * 1024 {
            return Err(NativeOAuthError::InvalidResponse);
        }
        let bytes = self
            .exchange(
                "/device_authorization",
                &[("authorization_details", details)],
            )
            .await
            .map_err(exchange_error)?;
        let response: DeviceResponse =
            serde_json::from_slice(&bytes).map_err(|_| NativeOAuthError::InvalidResponse)?;
        let uri = url::Url::parse(&response.verification_uri)
            .map_err(|_| NativeOAuthError::InvalidResponse)?;
        if uri.origin().ascii_serialization()
            != self
                .lease
                .lock()
                .map_err(|_| NativeOAuthError::RecoveryRequired)?
                .scope()
                .issuer
            || !uri.username().is_empty()
            || uri.password().is_some()
            || response.device_code.is_empty()
            || response.user_code.is_empty()
            || response.expires_in == 0
            || response.expires_in > 300
            || response.interval == 0
            || response.interval > 300
        {
            return Err(NativeOAuthError::InvalidResponse);
        }
        Ok(NativeDeviceAuthorization {
            client: self,
            device_code: response.device_code,
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            interval: Duration::from_secs(response.interval),
            deadline: Instant::now() + Duration::from_secs(response.expires_in),
        })
    }

    /// Resume after process restart with a fresh ephemeral runtime key. The spent
    /// issuer chain is authenticated before spending the token. Its spent marker
    /// remains durable unless the endpoint returns a known, unsubmitted successor.
    pub async fn resume(mut self) -> Result<NativeOAuthSession, NativeOAuthError> {
        self.refresh_trust().await?;
        let bytes = self.rotate().await?;
        self.install(&bytes).await
    }

    async fn refresh_trust(&mut self) -> Result<(), NativeOAuthError> {
        use crate::__types::trellis::{AuthIssuerChainRequest, AuthIssuerChainResponse};

        let scope = self
            .lease
            .lock()
            .map_err(|_| NativeOAuthError::RecoveryRequired)?
            .scope()
            .clone();
        let target = discover_native_oauth(&scope.issuer)
            .await
            .map_err(|_| NativeOAuthError::TrustUnavailable)?
            .trust;
        if target.instance_id != scope.instance_id
            || target.account != scope.account
            || target.rotation_sequence < self.trust.rotation_sequence
        {
            return Err(NativeOAuthError::TrustUnavailable);
        }
        let mut authenticated = self.trust.clone();
        if target.issuer.key_id != authenticated.issuer.key_id {
            let request = AuthIssuerChainRequest {
                pinned_key_id: authenticated.issuer.key_id.clone().into(),
                target_key_id: target.issuer.key_id.clone().into(),
                extra: Default::default(),
            };
            let mut uri = url::Url::parse(&format!(
                "{}/.well-known/trellis/issuer-chain",
                scope.issuer
            ))
            .map_err(|_| NativeOAuthError::TrustUnavailable)?;
            uri.query_pairs_mut()
                .append_pair("pinnedKeyId", &request.pinned_key_id)
                .append_pair("targetKeyId", &request.target_key_id);
            let mut response = self
                .http
                .get(uri)
                .send()
                .await
                .map_err(|_| NativeOAuthError::TrustUnavailable)?;
            if !response.status().is_success() {
                return Err(NativeOAuthError::TrustUnavailable);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| NativeOAuthError::TrustUnavailable)?
            {
                if bytes.len() + chunk.len() > 1024 * 1024 {
                    return Err(NativeOAuthError::TrustUnavailable);
                }
                bytes.extend_from_slice(&chunk);
            }
            let chain: AuthIssuerChainResponse =
                serde_json::from_slice(&bytes).map_err(|_| NativeOAuthError::TrustUnavailable)?;
            if chain.rotations.len() > 128 {
                return Err(NativeOAuthError::TrustUnavailable);
            }
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            let mut seen = std::collections::BTreeSet::from([authenticated.issuer.key_id.clone()]);
            for blob in chain.rotations {
                let rotation: SignedIssuerRotation = serde_json::from_slice(&blob.0 .0)
                    .map_err(|_| NativeOAuthError::TrustUnavailable)?;
                authenticated.issuer = rotation
                    .verify_successor(
                        &authenticated.issuer,
                        &scope.instance_id,
                        &scope.account,
                        authenticated.rotation_sequence,
                        now,
                    )
                    .map_err(|_| NativeOAuthError::TrustUnavailable)?;
                authenticated.rotation_sequence = rotation.sequence.get();
                if !seen.insert(authenticated.issuer.key_id.clone()) {
                    return Err(NativeOAuthError::TrustUnavailable);
                }
            }
        }
        if authenticated.issuer != target.issuer
            || authenticated.rotation_sequence != target.rotation_sequence
        {
            return Err(NativeOAuthError::TrustUnavailable);
        }
        let lease = Arc::clone(&self.lease);
        let retained = authenticated.clone();
        tokio::task::spawn_blocking(move || {
            lease
                .lock()
                .map_err(|_| std::io::Error::other("OAuth custody poisoned"))?
                .retain_trust(&retained)
        })
        .await
        .map_err(|_| NativeOAuthError::RecoveryRequired)?
        .map_err(NativeOAuthError::Custody)?;
        self.trust = authenticated;
        Ok(())
    }

    async fn rotate(&mut self) -> Result<Vec<u8>, NativeOAuthError> {
        let lease = Arc::clone(&self.lease);
        let token = tokio::task::spawn_blocking(move || {
            lease
                .lock()
                .map_err(|_| std::io::Error::other("OAuth custody poisoned"))?
                .begin_rotation()
        })
        .await
        .map_err(|_| NativeOAuthError::RecoveryRequired)?;
        let token = token.map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                NativeOAuthError::RecoveryRequired
            }
            _ => NativeOAuthError::Custody(error),
        })?;
        let bytes = self
            .exchange(
                "/token",
                &[
                    ("grant_type", "refresh_token".into()),
                    ("refresh_token", token),
                ],
            )
            .await
            // Ambiguous/lost responses leave the original spent marker in place.
            .map_err(|_| NativeOAuthError::RecoveryRequired)?;
        let response: TokenResponse =
            serde_json::from_slice(&bytes).map_err(|_| NativeOAuthError::InvalidResponse)?;
        if !response.token_type.eq_ignore_ascii_case("DPoP") {
            return Err(NativeOAuthError::InvalidResponse);
        }
        let lease = Arc::clone(&self.lease);
        // Once a success identifies an unsubmitted replacement secret, schedule
        // its durable checkpoint before awaiting admission/issuer verification.
        // The blocking task retains custody even if this future is cancelled.
        tokio::task::spawn_blocking(move || {
            lease
                .lock()
                .map_err(|_| std::io::Error::other("OAuth custody poisoned"))?
                .retain_rotated_token(response.refresh_token)
        })
        .await
        .map_err(|_| NativeOAuthError::RecoveryRequired)?
        .map_err(NativeOAuthError::Custody)?;
        Ok(bytes)
    }

    fn proof(&self, uri: &str, nonce: Option<&str>) -> Result<String, NativeOAuthError> {
        let lease = self
            .lease
            .lock()
            .map_err(|_| NativeOAuthError::RecoveryRequired)?;
        let header = serde_json::json!({"typ":"dpop+jwt","alg":"ES256","jwk":lease.public_jwk()});
        let jwk = format!(
            "{{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"{}\"}}",
            self.auth.session_key
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| NativeOAuthError::Unavailable)?
            .as_secs();
        let mut claims = serde_json::json!({
            "htm":"POST", "htu":uri, "iat":now, "jti":ulid::Ulid::new().to_string(),
            "trellis_runtime_id":self.runtime_id,
            "trellis_nats_jkt":URL_SAFE_NO_PAD.encode(Sha256::digest(jwk.as_bytes()))
        });
        if let Some(nonce) = nonce {
            claims["nonce"] = nonce.into();
        }
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&header).map_err(|_| NativeOAuthError::InvalidResponse)?
            ),
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&claims).map_err(|_| NativeOAuthError::InvalidResponse)?
            )
        );
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(lease.sign_dpop(input.as_bytes()))
        ))
    }

    async fn exchange(
        &self,
        endpoint: &str,
        extra: &[(&str, String)],
    ) -> Result<Vec<u8>, ExchangeError> {
        let scope = self
            .lease
            .lock()
            .map_err(|_| ExchangeError::Failed)?
            .scope()
            .clone();
        let uri = format!("{}{endpoint}", scope.issuer);
        let mut fields = vec![
            ("client_id", scope.client_id),
            ("trellis_runtime_id", self.runtime_id.clone()),
            (
                "trellis_session_nkey",
                self.auth
                    .session_nkey()
                    .map_err(|_| ExchangeError::Failed)?,
            ),
        ];
        fields.extend_from_slice(extra);
        // Nonces are request-local. A standard challenge is explicitly known to
        // precede credential consumption, and is the only automatic token retry.
        let mut nonce = None;
        for attempt in 0..2 {
            let proof = self
                .proof(&uri, nonce.as_deref())
                .map_err(|_| ExchangeError::Failed)?;
            let mut response = self
                .http
                .post(&uri)
                .header("DPoP", proof)
                .form(&fields)
                .send()
                .await
                .map_err(|_| ExchangeError::Failed)?;
            let status = response.status();
            let challenge = if response.headers().get_all("DPoP-Nonce").iter().count() == 1 {
                response
                    .headers()
                    .get("DPoP-Nonce")
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| !value.is_empty() && value.len() <= 1024)
                    .map(str::to_owned)
            } else {
                None
            };
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| ExchangeError::Failed)? {
                if bytes.len() + chunk.len() > 64 * 1024 {
                    return Err(ExchangeError::Failed);
                }
                bytes.extend_from_slice(&chunk);
            }
            if status.is_success() {
                return Ok(bytes);
            }
            let error: OAuthError =
                serde_json::from_slice(&bytes).map_err(|_| ExchangeError::Failed)?;
            if status == reqwest::StatusCode::BAD_REQUEST
                && error.error == "use_dpop_nonce"
                && attempt == 0
            {
                nonce = Some(challenge.ok_or(ExchangeError::Failed)?);
                continue;
            }
            return Err(ExchangeError::OAuth(error.error));
        }
        Err(ExchangeError::Failed)
    }

    async fn install(mut self, bytes: &[u8]) -> Result<NativeOAuthSession, NativeOAuthError> {
        let (admission, details) = self.install_response(bytes).await?;
        Ok(NativeOAuthSession {
            auth: self.auth.clone(),
            runtime_id: self.runtime_id.clone(),
            admission,
            trust: self.trust.clone(),
            authorization_details: details,
            client: self,
        })
    }

    async fn install_response(
        &mut self,
        bytes: &[u8],
    ) -> Result<(SignedOAuthAdmission, serde_json::Value), NativeOAuthError> {
        let response: TokenResponse =
            serde_json::from_slice(bytes).map_err(|_| NativeOAuthError::InvalidResponse)?;
        let scope = self
            .lease
            .lock()
            .map_err(|_| NativeOAuthError::RecoveryRequired)?
            .scope()
            .clone();
        let session_nkey = self
            .auth
            .session_nkey()
            .map_err(|_| NativeOAuthError::InvalidResponse)?;
        let verify = |trust: &RuntimeAuthorizationTrust| {
            verify_oauth_admission(
                response.trellis_admission.as_bytes(),
                &trust.issuer,
                &scope.instance_id,
                &scope.account,
                &session_nkey,
                time::OffsetDateTime::now_utc().unix_timestamp(),
                300,
            )
        };
        let admission = match verify(&self.trust) {
            Ok(admission) => admission,
            Err(_) => {
                self.refresh_trust().await?;
                verify(&self.trust).map_err(|_| NativeOAuthError::InvalidResponse)?
            }
        };
        let jwk = self
            .lease
            .lock()
            .map_err(|_| NativeOAuthError::RecoveryRequired)?
            .public_jwk();
        let canonical = format!(
            "{{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":{},\"y\":{}}}",
            jwk["x"], jwk["y"]
        );
        let jkt = URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()));
        let body = &admission.credential;
        if !response.token_type.eq_ignore_ascii_case("DPoP")
            || body.client_id != scope.client_id
            || body.runtime_id != self.runtime_id
            || body.persistent_dpop_jkt != jkt
            || body.binding
                != (OAuthAdmissionBinding::Native {
                    native_dpop_jkt: jkt,
                })
            || !response.authorization_details.is_array()
        {
            return Err(NativeOAuthError::InvalidResponse);
        }
        let grant = NativeOAuthGrant {
            principal_id: body.principal_id.clone(),
            oauth_grant_id: body.oauth_grant_id.clone(),
            login_session_id: body.login_session_id.clone(),
            refresh_token: response.refresh_token,
        };
        let lease = Arc::clone(&self.lease);
        tokio::task::spawn_blocking(move || {
            lease
                .lock()
                .map_err(|_| std::io::Error::other("OAuth custody poisoned"))?
                .install(grant)
        })
        .await
        .map_err(|_| NativeOAuthError::RecoveryRequired)?
        .map_err(NativeOAuthError::Custody)?;
        Ok((admission, response.authorization_details))
    }
}

enum ExchangeError {
    OAuth(String),
    Failed,
}

fn exchange_error(error: ExchangeError) -> NativeOAuthError {
    match error {
        ExchangeError::OAuth(code) if code == "access_denied" => NativeOAuthError::Denied,
        ExchangeError::OAuth(code) if code == "expired_token" => NativeOAuthError::Expired,
        ExchangeError::OAuth(code) if code == "invalid_grant" => NativeOAuthError::RecoveryRequired,
        _ => NativeOAuthError::Unavailable,
    }
}

impl NativeDeviceAuthorization {
    /// Compare this user code with the actual portal before approving.
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    /// Open this selected-issuer portal URI and enter the displayed code manually.
    pub fn verification_uri(&self) -> &str {
        &self.verification_uri
    }

    /// Poll according to RFC 8628, respecting pending/slow-down and the original
    /// expiry. Dropping this future cancels polling without deleting credentials.
    pub async fn complete(mut self) -> Result<NativeOAuthSession, NativeOAuthError> {
        loop {
            tokio::time::sleep_until((Instant::now() + self.interval).min(self.deadline)).await;
            if Instant::now() >= self.deadline {
                return Err(NativeOAuthError::Expired);
            }
            let result = tokio::time::timeout_at(
                self.deadline,
                self.client.exchange(
                    "/token",
                    &[
                        (
                            "grant_type",
                            "urn:ietf:params:oauth:grant-type:device_code".into(),
                        ),
                        ("device_code", self.device_code.clone()),
                    ],
                ),
            )
            .await
            .map_err(|_| NativeOAuthError::Expired)?;
            match result {
                Ok(bytes) => return self.client.install(&bytes).await,
                Err(ExchangeError::OAuth(code)) if code == "authorization_pending" => {}
                Err(ExchangeError::OAuth(code)) if code == "slow_down" => {
                    self.interval += Duration::from_secs(5)
                }
                Err(error) => return Err(exchange_error(error)),
            }
        }
    }
}
