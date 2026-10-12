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
use std::{collections::BTreeMap, time::Duration};

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
}

#[derive(Deserialize, Serialize)]
pub(super) struct UpstreamFlow {
    pub(super) account_token: Option<String>,
    pub(super) session_id: String,
    pub(super) authorization_transaction: String,
    pub(super) provider: ProviderIdentity,
    pub(super) nonce: String,
    pub(super) pkce_verifier: String,
    pub(super) expires_at: i64,
    pub(super) request_id: String,
}

/// One pooled native client; redirects and ambient proxy selection cannot change
/// the configured issuer/TLS boundary. Authentication failures remain private.
pub(super) fn http_client(roots: &[String]) -> Result<openidconnect::reqwest::Client, AuthError> {
    let mut builder = openidconnect::reqwest::Client::builder()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .https_only(true)
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15));
    for root in roots {
        builder = builder.add_root_certificate(
            openidconnect::reqwest::Certificate::from_pem(root.as_bytes())
                .map_err(|_| AuthError::Denied)?,
        );
    }
    builder.build().map_err(|_| AuthError::Unavailable)
}

impl ProviderSnapshot {
    pub(super) async fn discover(
        provider_id: String,
        revision: u64,
        issuer: String,
        client_id: String,
        http: &openidconnect::reqwest::Client,
    ) -> Result<Self, AuthError> {
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer.clone()).map_err(|_| AuthError::Denied)?,
            http,
        )
        .await
        .map_err(|_| AuthError::Unavailable)?;
        let token = metadata.token_endpoint().ok_or(AuthError::Denied)?;
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
        let id_token = token.id_token().ok_or(AuthError::Denied)?;
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
        Ok(VerifiedIdentity {
            subject: claims.subject().as_str().into(),
            claims: serde_json::to_value(claims)?,
            expires_at: claims.expiration().timestamp(),
        })
    }
}

pub(super) struct ConfiguredProvider {
    pub(super) issuer: String,
    pub(super) client_id: String,
    pub(super) revision: u64,
    pub(super) secret: Option<Vec<u8>>,
    pub(super) scopes: Vec<String>,
}

impl super::super::SqliteAuthorizationStore {
    pub(super) fn upstream_provider(
        &self,
        provider: &str,
    ) -> Result<ConfiguredProvider, AuthError> {
        let connection = self.connection.lock().map_err(|_| AuthError::Unavailable)?;
        let (issuer,client_id,revision,secret,config):(String,String,u64,Option<Vec<u8>>,String)=connection.query_row("SELECT issuer,client_id,revision,sealed_client_secret,config_json FROM auth_oidc_providers WHERE provider_id=?1 AND state='active'",[provider],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?.ok_or(AuthError::Denied)?;
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
            issuer,
            client_id,
            revision,
            secret,
            scopes: options.scopes,
        })
    }

    /// The verified upstream subject resolves only through an administrator's
    /// existing identity link; matching an email never selects another user.
    pub(super) fn portal_upstream_login(
        &self,
        flow: &UpstreamFlow,
        identity: &VerifiedIdentity,
        cookie: &str,
        csrf: &str,
        replacement: &str,
        now: i64,
    ) -> Result<super::portal::PortalLogin, AuthError> {
        let principal = {
            let connection = self.connection.lock().map_err(|_| AuthError::Unavailable)?;
            connection.query_row("SELECT principal_id FROM auth_provider_identities WHERE provider=?1 AND provider_subject=?2",params![flow.provider.provider_id,identity.subject],|row|row.get::<_,String>(0)).optional()?.ok_or(AuthError::Denied)?
        };
        let input = (
            &flow.session_id,
            &flow.provider.provider_id,
            flow.provider.revision,
            &identity.subject,
            &identity.claims,
        );
        let mutation = super::super::Mutation {
            actor: principal.clone(),
            request_id: flow.request_id.clone(),
            request_digest: super::super::sqlite::digest(&input)?,
        };
        let cookie_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(cookie.as_bytes()));
        let csrf_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.as_bytes()));
        let replacement_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(replacement.as_bytes()));
        self.mutate(&mutation,"login.oidc",&input,now,|connection| {
            let current:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oidc_providers WHERE provider_id=?1 AND issuer=?2 AND client_id=?3 AND revision=?4 AND state='active')",params![flow.provider.provider_id,flow.provider.issuer,flow.provider.client_id,flow.provider.revision],|row|row.get(0))?;
            if !current {return Err(AuthError::Denied);}
            let expires_at=identity.expires_at.min(now.saturating_add(i64::from(self.settings.login_lifetime_seconds)));
            let login=super::super::VerifiedLogin {principal_id:principal.clone(),provider_id:Some(flow.provider.provider_id.clone()),upstream_issuer:Some(flow.provider.issuer.clone()),upstream_subject:Some(identity.subject.clone()),verified_claims:Some(identity.claims.clone()),expires_at};
            let login_session_id=Self::record_verified_login_in(connection,&login,now)?;
            if connection.execute("UPDATE auth_portal_sessions SET cookie_hash=?1,login_session_id=?2,expires_at=?3 WHERE portal_session_id=?4 AND cookie_hash=?5 AND csrf_hash=?6 AND expires_at>?7 AND login_session_id IS NULL",params![replacement_hash,login_session_id,expires_at,flow.session_id,cookie_hash,csrf_hash,now])?!=1 {return Err(AuthError::Denied);}
            Ok(super::portal::PortalLogin {session_id:flow.session_id.clone(),principal_id:principal.clone(),login_session_id,expires_at})
        })
    }
}
