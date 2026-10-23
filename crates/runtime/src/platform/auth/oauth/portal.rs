//! Hashed login-cookie custody for the isolated authorization portal.

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::super::revocation::{enqueue_scope, EnforcementScope};
use super::super::sqlite::{digest, id, AuthError, Mutation, SqliteAuthorizationStore};

pub(crate) struct PortalCookie {
    pub(crate) session_id: String,
    pub(crate) cookie: String,
    pub(crate) csrf: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PortalLogin {
    pub(crate) session_id: String,
    pub(crate) principal_id: String,
    pub(crate) login_session_id: String,
    pub(crate) expires_at: i64,
}

#[derive(Serialize, Deserialize)]
pub(super) struct PortalAuthorization {
    pub(super) session_id: String,
    pub(super) request: ConsentRequest,
    pub(super) public_jwk: serde_json::Value,
    pub(super) expires_at: i64,
    pub(super) presentation: Option<ConsentPresentation>,
}

/// Server-owned meaning of one displayed decision. Kept only in the bounded
/// pending authorization artifact, never carried in runtime authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConsentPresentation {
    pub(crate) id: String,
    pub(crate) principal_id: String,
    pub(crate) login_session_id: String,
    pub(crate) client_id: String,
    pub(crate) binding_kind: String,
    pub(crate) binding_value: String,
    pub(crate) selection: super::super::policy::CapabilitySelection,
    pub(crate) capabilities:
        std::collections::BTreeMap<String, Option<trellis_protocol::CapabilityAuthority>>,
    pub(crate) api_generations: std::collections::BTreeMap<String, u64>,
    pub(crate) required_privileges: Vec<trellis_protocol::PlatformPrivilege>,
    pub(crate) optional_privileges: Vec<trellis_protocol::PlatformPrivilege>,
    // Authenticated custody evidence filled when reading the sealed artifact.
    #[serde(skip)]
    pub(crate) lookup_hash: String,
    #[serde(skip)]
    pub(crate) sealed_digest: String,
}

#[derive(Debug)]
pub(crate) enum ApprovalError {
    ConsentChanged,
    Protocol(oauth_as::error::ErrorResponse),
}

impl From<oauth_as::error::ErrorResponse> for ApprovalError {
    fn from(error: oauth_as::error::ErrorResponse) -> Self {
        Self::Protocol(error)
    }
}

impl From<AuthError> for ApprovalError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::ConsentChanged => Self::ConsentChanged,
            _ => Self::Protocol(oauth_as::error::ErrorResponse::new(
                oauth_as::error::ErrorCode::AccessDenied,
            )),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) enum ConsentRequest {
    Authorization(Vec<(String, String)>),
    Device { device_code: String },
}

pub(super) struct ConsentIntent {
    pub(super) client_id: oauth_as::client::ClientId,
    pub(super) redirect_uri: Option<String>,
    pub(super) device_user_code: Option<String>,
    pub(super) dpop_jkt: String,
    pub(super) authorization_details: String,
}

impl super::OAuthEngine {
    /// Both standard grant flows present the same authority-selection model.
    pub(super) async fn consent_intent(
        &self,
        flow: &ConsentRequest,
    ) -> Result<ConsentIntent, oauth_as::error::ErrorResponse> {
        use oauth_as::{
            authorization::AuthorizationRequest,
            device::DeviceGrantState,
            error::{ErrorCode, ErrorResponse},
            store::Storage,
        };
        let invalid = || ErrorResponse::new(ErrorCode::InvalidRequest);
        match flow {
            ConsentRequest::Authorization(pairs) => {
                let request = AuthorizationRequest::from_pairs(
                    pairs
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                );
                let validated = self
                    .server
                    .validate_authorization_request(&request)
                    .await
                    .map_err(|_| invalid())?;
                Ok(ConsentIntent {
                    client_id: validated.client_id,
                    redirect_uri: Some(validated.redirect_uri),
                    device_user_code: None,
                    dpop_jkt: validated.dpop_jkt.ok_or_else(invalid)?.into_string(),
                    authorization_details: request
                        .authorization_details
                        .ok_or_else(invalid)?
                        .into_owned(),
                })
            }
            ConsentRequest::Device { device_code } => {
                let device = self
                    .server
                    .store()
                    .get_device_grant(device_code)
                    .await
                    .map_err(|_| invalid())?
                    .ok_or_else(invalid)?;
                if device.expires_at <= std::time::SystemTime::now()
                    || device.state != DeviceGrantState::Pending
                {
                    return Err(invalid());
                }
                let (store, code) = (self.server.store().clone(), device_code.clone());
                let selection =
                    super::super::sqlite::blocking(move || store.device_selection(&code))
                        .await
                        .map_err(|_| invalid())?;
                Ok(ConsentIntent {
                    client_id: device.client_id,
                    redirect_uri: None,
                    device_user_code: Some(device.user_code),
                    dpop_jkt: selection.persistent_dpop_jkt,
                    authorization_details: selection.authorization_details,
                })
            }
        }
    }
}

impl SqliteAuthorizationStore {
    /// Resolve a live portal cookie for its pending authorization flow.
    pub(crate) fn portal_session_identity(
        &self,
        cookie: &str,
        csrf: &str,
        now: i64,
    ) -> Result<String, AuthError> {
        if cookie.len() > 128 || csrf.len() > 128 {
            return Err(AuthError::Denied);
        }
        let connection = self.reader()?;
        connection
            .query_row(
                "SELECT portal_session_id \
                FROM auth_portal_sessions \
                WHERE cookie_hash=?1 \
                AND csrf_hash=?2 AND expires_at>?3",
                params![
                    URL_SAFE_NO_PAD.encode(Sha256::digest(cookie.as_bytes())),
                    URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.as_bytes())),
                    now
                ],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(AuthError::Denied)
    }
    /// Closes the authenticated login and commits its enforcement work. Browser
    /// cookie deletion is cosmetic; retained cookies and grants fail immediately.
    pub(crate) fn portal_logout(
        &self,
        identity: &Mutation,
        login: &PortalLogin,
        now: i64,
    ) -> Result<(), AuthError> {
        self.mutate(
            identity,
            "login.logout",
            &login.login_session_id,
            now,
            |connection| {
                if identity.actor != login.principal_id {
                    return Err(AuthError::Denied);
                }
                let changed = connection.execute(
                    "UPDATE auth_login_sessions \
                    SET revoked_at=?1,revision=revision+1 \
                    WHERE login_session_id=?2 \
                    AND principal_id=?3 \
                    AND revoked_at IS NULL",
                    params![now, login.login_session_id, identity.actor],
                )?;
                if changed != 1 {
                    return Err(AuthError::Denied);
                }
                connection.execute(
                    "DELETE FROM auth_portal_sessions WHERE login_session_id=?1",
                    [&login.login_session_id],
                )?;
                enqueue_scope(
                    connection,
                    &EnforcementScope::Login(login.login_session_id.clone()),
                    now,
                )?;
                Ok(())
            },
        )
    }

    /// Creates a guest session with separate cookie and CSRF secrets. Only their
    /// hashes survive in the database; neither secret is logged or serialized.
    pub(crate) fn create_portal_session(&self, now: i64) -> Result<PortalCookie, AuthError> {
        let mut secrets = [0u8; 64];
        getrandom::fill(&mut secrets).map_err(|_| AuthError::Unavailable)?;
        let cookie = URL_SAFE_NO_PAD.encode(&secrets[..32]);
        let csrf = URL_SAFE_NO_PAD.encode(&secrets[32..]);
        let session_id = id();
        let cookie_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(cookie.as_bytes()));
        let csrf_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.as_bytes()));
        self.transaction(|connection| {
            connection.execute(
                "DELETE FROM auth_portal_sessions WHERE expires_at<=?1",
                [now],
            )?;
            connection.execute(
                "INSERT INTO auth_portal_sessions(portal_session_id,cookie_hash,\
                csrf_hash,created_at,expires_at) \
                VALUES(?1,?2,?3,?4,?5)",
                params![session_id, cookie_hash, csrf_hash, now, now + 600],
            )?;
            Ok(())
        })?;
        Ok(PortalCookie {
            session_id,
            cookie,
            csrf,
        })
    }

    /// Verifies cookie possession before password hashing, then rereads the
    /// credential in the committing transaction. Authentication rotates the
    /// bearer without changing the pending authorization's portal identity.
    pub(crate) fn portal_local_login(
        &self,
        cookie: &str,
        csrf: &str,
        replacement_cookie: &str,
        credentials: (&str, &str),
        request_id: &str,
        now: i64,
    ) -> Result<PortalLogin, AuthError> {
        let (username, password) = credentials;
        if username.len() > 256 || password.len() > 4096 || replacement_cookie.len() != 43 {
            return Err(AuthError::Denied);
        }
        let cookie_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(cookie.as_bytes()));
        let csrf_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.as_bytes()));
        let replacement_hash =
            URL_SAFE_NO_PAD.encode(Sha256::digest(replacement_cookie.as_bytes()));
        let (session_id, principal, hash, version): (String, String, String, u64) = {
            let connection = self.reader()?;
            let session: String = connection
                .query_row(
                    "SELECT portal_session_id \
                    FROM auth_portal_sessions \
                    WHERE cookie_hash=?1 \
                    AND csrf_hash=?2 AND expires_at>?3 \
                    AND login_session_id IS NULL",
                    params![cookie_hash, csrf_hash, now],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::Denied)?;
            let credentials = connection
                .query_row(
                    "SELECT c.principal_id,c.password_hash,c.version \
                    FROM auth_local_credentials c \
                    JOIN auth_principals p ON p.principal_id=c.principal_id \
                    WHERE c.normalized_username=?1 \
                    AND c.password_hash IS NOT NULL \
                    AND p.state='active'",
                    [username.to_lowercase()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, u64>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((principal, hash, version)) = credentials else {
                drop(connection);
                // Perform the normal hash work for absent/disabled users too.
                // This zero-result hash is not a credential or a fallback login.
                let dummy = PasswordHash::new(
                    "$argon2id$v=19$m=19456,t=2,\
                    p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                )
                .map_err(|_| AuthError::Unavailable)?;
                let _ = Argon2::default().verify_password(password.as_bytes(), &dummy);
                return Err(AuthError::Denied);
            };
            (session, principal, hash, version)
        };
        let password_hash = PasswordHash::new(&hash).map_err(|_| AuthError::Unavailable)?;
        Argon2::default()
            .verify_password(password.as_bytes(), &password_hash)
            .map_err(|_| AuthError::Denied)?;
        let input = (&session_id, &principal, version);
        let mutation = Mutation {
            actor: principal.clone(),
            request_id: request_id.into(),
            request_digest: digest(&input)?,
        };
        self.mutate(&mutation, "login.local", &input, now, |connection| {
            let valid: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_local_credentials c \
                JOIN auth_principals p ON p.principal_id=c.principal_id \
                WHERE c.principal_id=?1 \
                AND c.version=?2 AND c.password_hash=?3 \
                AND p.state='active')",
                params![principal, version, hash],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(AuthError::Denied);
            }
            let login_session_id = id();
            let expires_at = now + i64::from(self.settings.login_lifetime_seconds);
            connection.execute(
                "INSERT INTO auth_login_sessions(login_session_id,principal_id,method,\
                verified_roles_json,observed_at,observation_order,expires_at,\
                created_at,revision) \
                VALUES(?1,?2,'local','[]',?3,1,?4,?3,1)",
                params![login_session_id, principal, now, expires_at],
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
                    session_id,
                    cookie_hash,
                    csrf_hash,
                    now
                ],
            )? != 1
            {
                return Err(AuthError::Denied);
            }
            Ok(PortalLogin {
                session_id: session_id.clone(),
                principal_id: principal.clone(),
                login_session_id,
                expires_at,
            })
        })
    }

    /// Reads a current authenticated portal session. Logout and principal
    /// disablement invalidate cookies without relying on browser cleanup.
    pub(crate) fn portal_login(
        &self,
        cookie: &str,
        csrf: &str,
        now: i64,
    ) -> Result<PortalLogin, AuthError> {
        let cookie_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(cookie.as_bytes()));
        let csrf_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.as_bytes()));
        let connection = self.reader()?;
        connection
            .query_row(
                "SELECT p.portal_session_id,l.principal_id,l.login_session_id,\
                l.expires_at \
                FROM auth_portal_sessions p \
                JOIN auth_login_sessions l ON l.login_session_id=p.login_session_id \
                JOIN auth_principals u ON u.principal_id=l.principal_id \
                WHERE p.cookie_hash=?1 \
                AND p.csrf_hash=?2 AND p.expires_at>?3 \
                AND l.expires_at>?3 \
                AND l.revoked_at IS NULL \
                AND u.state='active'",
                params![cookie_hash, csrf_hash, now],
                |row| {
                    Ok(PortalLogin {
                        session_id: row.get(0)?,
                        principal_id: row.get(1)?,
                        login_session_id: row.get(2)?,
                        expires_at: row.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or(AuthError::Denied)
    }
}
