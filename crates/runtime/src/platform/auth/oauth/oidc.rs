//! Upstream identity is verified by openidconnect, never by unverified JWT
//! projection. Additional claims stay intact for Auth's normal claim evaluator.

use super::super::AuthError;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use openidconnect::OAuth2TokenResponse as _;
use openidconnect::{
    core::*, AccessTokenHash, AdditionalClaims, Client, ClientId, ClientSecret,
    EmptyExtraTokenFields, EndpointMaybeSet, EndpointNotSet, EndpointSet, IdTokenFields, IssuerUrl,
    Nonce, RedirectUrl, StandardErrorResponse, StandardTokenResponse, TokenResponse as _,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, future::Future, pin::Pin, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Claims {
    #[serde(flatten)]
    additional: BTreeMap<String, Value>,
}
impl AdditionalClaims for Claims {}

pub(super) type TokenResponse = StandardTokenResponse<
    IdTokenFields<
        Claims,
        EmptyExtraTokenFields,
        CoreGenderClaim,
        CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
    >,
    CoreTokenType,
>;
pub(super) type ProviderClient<A = EndpointNotSet, T = EndpointNotSet, U = EndpointNotSet> = Client<
    Claims,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    TokenResponse,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    A,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    T,
    U,
>;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct ProviderIdentity {
    pub(super) provider_id: String,
    pub(super) revision: u64,
    pub(super) issuer: String,
    pub(super) client_id: String,
}

/// Discovered keys stay in memory: the library's discovery-document serializer
/// intentionally does not serialize its fetched JWKS.
pub(super) struct ProviderSnapshot {
    pub(super) identity: ProviderIdentity,
    metadata: CoreProviderMetadata,
}

pub(super) struct VerifiedIdentity {
    pub(super) subject: String,
    pub(super) claims: Value,
    pub(super) expires_at: i64,
    pub(super) profile: super::super::profiles::VerifiedProfile,
}

#[derive(Deserialize, Serialize)]
pub(super) struct UpstreamFlow {
    pub(super) account_token: Option<String>,
    /// Device approval page to return to; it has no OAuth transaction.
    #[serde(default)]
    pub(super) device_review: Option<String>,
    pub(super) session_id: String,
    pub(super) authorization_transaction: String,
    pub(super) provider: ProviderIdentity,
    pub(super) nonce: String,
    pub(super) pkce_verifier: String,
    pub(super) expires_at: i64,
    pub(super) request_id: String,
}

/// The upstream providers' HTTP client. openidconnect's own adapter is for an
/// older reqwest, so this one keeps the server on a single HTTP stack.
#[derive(Clone)]
pub(in crate::platform::auth) struct UpstreamHttp(reqwest::Client);

impl<'c> openidconnect::AsyncHttpClient<'c> for UpstreamHttp {
    type Error = openidconnect::HttpClientError<reqwest::Error>;
    type Future =
        Pin<Box<dyn Future<Output = Result<openidconnect::HttpResponse, Self::Error>> + Send + 'c>>;

    fn call(&'c self, request: openidconnect::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let response = self
                .0
                .execute(request.try_into().map_err(Box::new)?)
                .await
                .map_err(Box::new)?;
            let mut builder = openidconnect::http::Response::builder()
                .status(response.status())
                .version(response.version());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            builder
                .body(response.bytes().await.map_err(Box::new)?.to_vec())
                .map_err(openidconnect::HttpClientError::Http)
        })
    }
}

/// One pooled native client; redirects and ambient proxy selection cannot change
/// the configured issuer/TLS boundary. Authentication failures remain private.
/// It trusts the system store, and also `issuer_certificate` (DER) when a
/// provider has one pinned.
pub(in crate::platform::auth) fn http_client(
    issuer_certificate: Option<&[u8]>,
) -> Result<UpstreamHttp, AuthError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15));
    if let Some(certificate) = issuer_certificate {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_der(certificate).map_err(|_| AuthError::Unavailable)?,
        );
    }
    builder
        .build()
        .map(UpstreamHttp)
        .map_err(|_| AuthError::Unavailable)
}

/// The upstream HTTP clients: one on the system trust store for providers
/// without a pin, and one per pinned provider, rebuilt when its revision
/// changes so a new or removed pin takes effect on the next sign-in.
pub(super) struct UpstreamClients {
    system: UpstreamHttp,
    pinned: std::sync::Mutex<std::collections::HashMap<String, (u64, UpstreamHttp)>>,
}

impl UpstreamClients {
    pub(super) fn new() -> Result<Self, AuthError> {
        Ok(Self {
            system: http_client(None)?,
            pinned: std::sync::Mutex::default(),
        })
    }

    /// The client every HTTPS call for `provider_id` uses.
    pub(super) fn for_provider(
        &self,
        provider_id: &str,
        configured: &ConfiguredProvider,
    ) -> Result<UpstreamHttp, AuthError> {
        let mut pinned = self.pinned.lock().map_err(|_| AuthError::Unavailable)?;
        let Some(certificate) = &configured.issuer_certificate else {
            pinned.remove(provider_id);
            return Ok(self.system.clone());
        };
        if let Some((revision, client)) = pinned.get(provider_id) {
            if *revision == configured.revision {
                return Ok(client.clone());
            }
        }
        let client = http_client(Some(certificate))?;
        pinned.insert(
            provider_id.to_owned(),
            (configured.revision, client.clone()),
        );
        Ok(client)
    }
}

/// What a provider Put does with the issuer certificate pinned for it.
pub(in crate::platform::auth) enum IssuerPin {
    /// Keep whatever the provider has pinned now.
    Keep,
    /// Trust the system store alone.
    Remove,
    /// Pin this DER issuer certificate.
    Set(Vec<u8>),
}

/// Resolves a provider Put's `certificateFingerprint` before anything is
/// stored. A fingerprint must match the issuer that ends the chain `issuer`'s
/// host presents, read without trusting it. A provider left on the system
/// store alone is checked the way a service checks Trellis: when nothing
/// vouches for its certificate but its presented issuer would, the Put is
/// refused with that issuer's fingerprint for the administrator to confirm
/// and pin. An issuer that can't be reached is saved as before. Removing an
/// existing provider's pin is the administrator's decision and is never
/// checked against the issuer: the provider is then reached through the
/// system store alone, even while its issuer still needs the removed pin.
pub(in crate::platform::auth) async fn issuer_pin(
    issuer: &str,
    fingerprint: Option<&trellis_runtime_apis::__types::Nullable<String>>,
    creating: bool,
) -> Result<IssuerPin, AuthError> {
    use trellis_runtime_apis::__types::Nullable;
    let invalid = || {
        AuthError::Invalid(
            "issuer must be an https URL, such as https://example.us.auth0.com/".into(),
        )
    };
    let url = url::Url::parse(issuer)
        .ok()
        .filter(|url| url.scheme() == "https")
        .ok_or_else(invalid)?;
    let host = url
        .host_str()
        .map(|host| host.trim_matches(['[', ']']).to_owned())
        .ok_or_else(invalid)?;
    let port = url.port_or_known_default().ok_or_else(invalid)?;
    let server = format!("{host}:{port}");
    let pin = match fingerprint {
        Some(Nullable::Value(expected)) => {
            let presented = trellis_rs::auth::presented_issuer(&host, port)
                .await
                .ok_or_else(|| {
                    AuthError::Invalid(format!(
                        "could not read the certificate chain {server} presents; to pin its \
                        issuer, the provider must be reachable and present its issuer \
                        certificate after its own"
                    ))
                })?;
            let presented_fingerprint = trellis_rs::auth::certificate_fingerprint(&presented);
            if !trellis_rs::auth::fingerprints_match(expected, &presented_fingerprint) {
                return Err(AuthError::Invalid(format!(
                    "{server} presents issuer fingerprint {presented_fingerprint}, not \
                    {expected}"
                )));
            }
            return Ok(IssuerPin::Set(presented.to_vec()));
        }
        Some(Nullable::Null) if !creating => return Ok(IssuerPin::Remove),
        Some(Nullable::Null) => IssuerPin::Remove,
        None if creating => IssuerPin::Keep,
        None => return Ok(IssuerPin::Keep),
    };
    let issuer_url = IssuerUrl::new(issuer.to_owned()).map_err(|_| invalid())?;
    if CoreProviderMetadata::discover_async(issuer_url.clone(), &http_client(None)?)
        .await
        .is_ok()
    {
        return Ok(pin);
    }
    let Some(presented) = trellis_rs::auth::presented_issuer(&host, port).await else {
        return Ok(pin);
    };
    if CoreProviderMetadata::discover_async(issuer_url, &http_client(Some(&presented))?)
        .await
        .is_ok()
    {
        return Err(AuthError::Invalid(format!(
            "{server} presents a certificate nothing on this server vouches for; its \
            issuer's SHA-256 fingerprint is {}. After confirming it with the provider's \
            operator, save the provider again with this certificate fingerprint",
            trellis_rs::auth::certificate_fingerprint(&presented)
        )));
    }
    Ok(pin)
}

impl ProviderSnapshot {
    pub(super) async fn discover(
        provider_id: String,
        revision: u64,
        issuer: String,
        client_id: String,
        http: &UpstreamHttp,
    ) -> Result<Self, AuthError> {
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer.clone()).map_err(|error| {
                tracing::warn!(%provider_id, %issuer, %error, "upstream OIDC issuer is not a URL");
                AuthError::Denied
            })?,
            http,
        )
        .await
        .map_err(|error| {
            // An issuer must match the discovery document's `issuer` exactly,
            // including any trailing slash.
            tracing::warn!(
                %provider_id,
                %issuer,
                error = %error_chain(&error),
                "upstream OIDC discovery failed"
            );
            AuthError::Unavailable
        })?;
        let token = metadata.token_endpoint().ok_or_else(|| {
            tracing::warn!(%provider_id, %issuer, "upstream OIDC discovery lists no token endpoint");
            AuthError::Denied
        })?;
        for endpoint in [
            metadata.authorization_endpoint().as_str(),
            token.as_str(),
            metadata.jwks_uri().as_str(),
        ] {
            let endpoint = url::Url::parse(endpoint).map_err(|_| AuthError::Denied)?;
            if endpoint.scheme() != "https"
                || endpoint.host_str().is_none()
                || !endpoint.username().is_empty()
                || endpoint.password().is_some()
                || endpoint.fragment().is_some()
            {
                tracing::warn!(
                    %provider_id,
                    %issuer,
                    %endpoint,
                    "upstream OIDC discovery lists an endpoint that is not plain HTTPS"
                );
                return Err(AuthError::Denied);
            }
        }
        Ok(Self {
            identity: ProviderIdentity {
                provider_id,
                revision,
                issuer,
                client_id,
            },
            metadata,
        })
    }

    pub(super) fn client(
        &self,
        secret: Option<ClientSecret>,
        redirect: String,
    ) -> Result<ProviderClient<EndpointSet, EndpointMaybeSet, EndpointMaybeSet>, AuthError> {
        Ok(ProviderClient::from_provider_metadata(
            self.metadata.clone(),
            ClientId::new(self.identity.client_id.clone()),
            secret,
        )
        .set_redirect_uri(RedirectUrl::new(redirect).map_err(|_| AuthError::Denied)?))
    }

    /// The same client/nonce that initiated the upstream code flow authenticates
    /// issuer, audience, algorithm, signature and expiry before any role claim is
    /// returned to policy. If present, the access-token hash is checked too.
    pub(super) fn verify(
        &self,
        client: &ProviderClient<EndpointSet, EndpointMaybeSet, EndpointMaybeSet>,
        token: &TokenResponse,
        nonce: &str,
    ) -> Result<VerifiedIdentity, AuthError> {
        let id_token = token.id_token().ok_or_else(|| {
            tracing::warn!(
                provider_id = %self.identity.provider_id,
                "upstream OIDC token response has no ID token; is the openid scope allowed?"
            );
            AuthError::Denied
        })?;
        let verifier = client.id_token_verifier();
        let claims = id_token
            .claims(&verifier, &Nonce::new(nonce.into()))
            .map_err(|error| {
                use openidconnect::{ClaimsVerificationError as C,SignatureVerificationError as S};
                let reason=match error {
                    C::Expired(_)=>"expired",
                    C::InvalidAudience(_)=>"audience",
                    C::InvalidAuthContext(_)=>"authentication_context",
                    C::InvalidAuthTime(_)=>"authentication_time",
                    C::InvalidIssuer(_)=>"issuer",
                    C::InvalidNonce(_)=>"nonce",
                    C::InvalidSubject(_)=>"subject",
                    C::SignatureVerification(S::NoMatchingKey)=>"signing_key_unavailable",
                    C::SignatureVerification(_)=>"signature",
                    _=>"unsupported_claims",
                };
                tracing::warn!(provider_id=%self.identity.provider_id,reason,"upstream OIDC claims rejected");
                AuthError::Denied
            })?;
        if let Some(expected) = claims.access_token_hash() {
            let actual = AccessTokenHash::from_token(
                token.access_token(),
                id_token.signing_alg().map_err(|_| AuthError::Denied)?,
                id_token
                    .signing_key(&verifier)
                    .map_err(|_| AuthError::Denied)?,
            )
            .map_err(|_| AuthError::Denied)?;
            if &actual != expected {
                tracing::warn!(provider_id=%self.identity.provider_id,"upstream OIDC access-token hash rejected");
                return Err(AuthError::Denied);
            }
        }
        let json = serde_json::to_value(claims)?;
        Ok(VerifiedIdentity {
            subject: claims.subject().as_str().into(),
            profile: super::super::profiles::VerifiedProfile::from_claims(&json),
            claims: json,
            expires_at: claims.expiration().timestamp(),
        })
    }

    /// Best-effort standard profile enrichment; authentication has already
    /// completed. Cross-origin discovery cannot redirect bearer credentials to
    /// private destinations; an explicitly configured issuer keeps its existing
    /// operator-selected TLS boundary, including approved private installations.
    pub(super) async fn enrich(
        &self,
        identity: &mut VerifiedIdentity,
        token: &TokenResponse,
        http: &UpstreamHttp,
    ) {
        if identity.profile.name.is_some()
            && identity.profile.given_name.is_some()
            && identity.profile.family_name.is_some()
            && identity.profile.email.is_some()
            && identity.profile.email_verified.is_some()
            && identity.profile.picture.is_some()
        {
            return;
        }
        let Some(endpoint) = self.metadata.userinfo_endpoint() else {
            return;
        };
        let Ok(url) = url::Url::parse(endpoint.as_str()) else {
            return;
        };
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return;
        }
        let Ok(issuer) = url::Url::parse(&self.identity.issuer) else {
            return;
        };
        // The issuer's own origin is reached the way discovery reached it,
        // trusting the provider's pinned issuer too.
        let client = if url.origin() == issuer.origin() {
            http.0.clone()
        } else {
            if !super::super::remote_http::public_url(&url) {
                return;
            }
            let Ok(client) = super::super::remote_http::client()
                .timeout(Duration::from_secs(3))
                .connect_timeout(Duration::from_secs(3))
                .build()
            else {
                return;
            };
            client
        };
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let mut response = client
                .get(url)
                .bearer_auth(token.access_token().secret())
                .timeout(Duration::from_secs(3))
                .send()
                .await
                .ok()?;
            if !response.status().is_success()
                || response
                    .content_length()
                    .is_some_and(|length| length > 32768)
            {
                return None;
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.ok()? {
                if bytes.len() + chunk.len() > 32768 {
                    return None;
                }
                bytes.extend_from_slice(&chunk);
            }
            let claims: Value = serde_json::from_slice(&bytes).ok()?;
            (claims.get("sub").and_then(Value::as_str) == Some(identity.subject.as_str()))
                .then_some(claims)
        })
        .await;
        if let Ok(Some(claims)) = result {
            identity
                .profile
                .fill(super::super::profiles::VerifiedProfile::from_claims(
                    &claims,
                ));
        }
    }
}

pub(super) struct ConfiguredProvider {
    pub(super) issuer: String,
    pub(super) client_id: String,
    pub(super) revision: u64,
    pub(super) secret: Option<Vec<u8>>,
    /// The DER issuer certificate pinned for this provider, if any.
    pub(super) issuer_certificate: Option<Vec<u8>>,
    pub(super) scopes: Vec<String>,
}

impl super::super::SqliteAuthorizationStore {
    pub(super) fn upstream_provider(
        &self,
        provider: &str,
    ) -> Result<ConfiguredProvider, AuthError> {
        let connection = self.reader()?;
        let (configured, config) = connection
            .query_row(
                "SELECT issuer,client_id,revision,sealed_client_secret,config_json,\
                issuer_certificate \
                FROM auth_oidc_providers \
                WHERE provider_id=?1 \
                AND state='active'",
                [provider],
                |row| {
                    Ok((
                        ConfiguredProvider {
                            issuer: row.get(0)?,
                            client_id: row.get(1)?,
                            revision: row.get(2)?,
                            secret: row.get(3)?,
                            issuer_certificate: row.get(5)?,
                            scopes: Vec::new(),
                        },
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AuthError::Denied)?;
        #[derive(Deserialize)]
        struct Options {
            #[serde(default)]
            scopes: Vec<String>,
        }
        let options: Options = serde_json::from_str(&config)?;
        if options.scopes.len() > 32
            || options.scopes.iter().any(|scope| {
                scope.is_empty()
                    || scope.len() > 256
                    || scope
                        .bytes()
                        .any(|byte| !(0x21..=0x7e).contains(&byte) || byte == b'"' || byte == b'\\')
            })
        {
            return Err(AuthError::Denied);
        }
        Ok(ConfiguredProvider {
            scopes: options.scopes,
            ..configured
        })
    }

    /// The verified upstream subject resolves only through an existing identity
    /// link; matching an email never selects another user. With `sign_up`, an
    /// unlinked subject gets a new account linked to it instead of a refusal.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn portal_upstream_login(
        &self,
        flow: &UpstreamFlow,
        identity: &VerifiedIdentity,
        cookie: &str,
        csrf: &str,
        replacement: &str,
        sign_up: bool,
        now: i64,
    ) -> Result<super::portal::PortalLogin, AuthError> {
        let linked = {
            let connection = self.reader()?;
            connection
                .query_row(
                    "SELECT principal_id \
                    FROM auth_provider_identities \
                    WHERE provider=?1 AND provider_subject=?2",
                    params![flow.provider.provider_id, identity.subject],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        };
        let created = linked.is_none();
        let principal = match linked {
            Some(principal) => principal,
            None if sign_up => super::super::sqlite::id(),
            // No account has this provider identity, and this app does not let
            // people create one by signing in.
            None => return Err(AuthError::NotFound),
        };
        let input = (
            &flow.session_id,
            &flow.provider.provider_id,
            flow.provider.revision,
            &identity.subject,
            &identity.claims,
            &identity.profile,
        );
        let mutation = super::super::Mutation {
            actor: principal.clone(),
            request_id: flow.request_id.clone(),
            request_digest: super::super::sqlite::digest(&input)?,
            session: None,
        };
        let cookie_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(cookie.as_bytes()));
        let csrf_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.as_bytes()));
        let replacement_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(replacement.as_bytes()));
        self.mutate(&mutation, "login.oidc", &input, now, |connection| {
            let current: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_oidc_providers \
                WHERE provider_id=?1 \
                AND issuer=?2 AND client_id=?3 \
                AND revision=?4 AND state='active')",
                params![
                    flow.provider.provider_id,
                    flow.provider.issuer,
                    flow.provider.client_id,
                    flow.provider.revision
                ],
                |row| row.get(0),
            )?;
            // The provider was edited or turned off, or company sign-in was.
            if !current || !super::super::sign_in::read(connection)?.providers {
                return Err(AuthError::Conflict);
            }
            if created {
                super::super::rpc_users::insert_user(connection, &principal, now)?;
                // The primary key refuses a subject another sign-up linked first.
                connection.execute(
                    "INSERT INTO auth_provider_identities(identity_id,provider,\
                    provider_subject,principal_id,linked_at,last_seen_at) \
                    VALUES(?1,?2,?3,?4,?5,?5)",
                    params![
                        super::super::sqlite::id(),
                        flow.provider.provider_id,
                        identity.subject,
                        principal,
                        now
                    ],
                )?;
            }
            let expires_at = identity
                .expires_at
                .min(now.saturating_add(i64::from(self.settings.login_lifetime_seconds)));
            let login = super::super::VerifiedLogin {
                principal_id: principal.clone(),
                provider_id: Some(flow.provider.provider_id.clone()),
                upstream_issuer: Some(flow.provider.issuer.clone()),
                upstream_subject: Some(identity.subject.clone()),
                verified_claims: Some(identity.claims.clone()),
                expires_at,
            };
            let login_session_id = Self::record_verified_login_in(connection, &login, now)?;
            super::super::profiles::ingest(
                connection,
                &principal,
                &flow.provider.provider_id,
                &identity.subject,
                &identity.profile,
                now,
            )?;
            if connection.execute(
                "UPDATE auth_portal_sessions \
                SET cookie_hash=?1,login_session_id=?2,expires_at=?3 \
                WHERE portal_session_id=?4 \
                AND cookie_hash=?5 AND csrf_hash=?6 \
                AND expires_at>?7 AND login_session_id IS NULL",
                params![
                    replacement_hash,
                    login_session_id,
                    expires_at,
                    flow.session_id,
                    cookie_hash,
                    csrf_hash,
                    now
                ],
            )? != 1
            {
                return Err(AuthError::Denied);
            }
            Ok(super::portal::PortalLogin {
                session_id: flow.session_id.clone(),
                principal_id: principal.clone(),
                login_session_id,
                expires_at,
            })
        })
    }
}

/// An error and its sources on one line, for a log an operator reads: the
/// OIDC library's top-level message often omits the cause, such as the
/// mismatched issuer or the provider's refusal.
pub(super) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}
