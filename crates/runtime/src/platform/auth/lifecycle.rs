use std::collections::{BTreeMap, BTreeSet};

use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use trellis_protocol::{CapabilityAuthority, PlatformPrivilege};

use super::authorization_sessions::GrantConsentBasis;
use super::policy::{capability, entitlements, CapabilitySelection};
use super::revocation::{enqueue_scope, EnforcementScope};
use super::sqlite::{decode, id, json, AuthError, Mutation, SqliteAuthorizationStore};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ClientRegistration {
    pub(crate) client_id: String,
    pub(crate) kind: String,
    pub(crate) display_name: String,
    pub(crate) redirect_uris: Vec<String>,
    pub(crate) development: bool,
    pub(crate) implied_capabilities: Vec<String>,
    pub(crate) eligible_privileges: Vec<PlatformPrivilege>,
    pub(crate) metadata: Value,
    pub(crate) expected_revision: u64,
}

/// Provider configuration contains only a secret already sealed by Auth's
/// configured credential store; plaintext credentials never enter this adapter.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OidcProviderRegistration {
    pub(crate) provider_id: String,
    pub(crate) issuer: String,
    pub(crate) display_name: String,
    pub(crate) client_id: String,
    pub(crate) sealed_client_secret: Option<Vec<u8>>,
    pub(crate) config: Value,
    pub(crate) claim_freshness_seconds: u32,
    pub(crate) expected_revision: u64,
}

/// Only the verified upstream/local credential boundary constructs this input.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedLogin {
    pub(crate) principal_id: String,
    pub(crate) provider_id: Option<String>,
    pub(crate) upstream_issuer: Option<String>,
    pub(crate) upstream_subject: Option<String>,
    pub(crate) verified_claims: Option<Value>,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct GrantApproval {
    pub(crate) presentation: Option<super::oauth::ConsentPresentation>,
    pub(crate) principal_id: String,
    pub(crate) login_session_id: String,
    pub(crate) client_id: String,
    pub(crate) binding_kind: String,
    pub(crate) binding_value: String,
    pub(crate) durable_dpop_jkt: String,
    pub(crate) public_jwk: Value,
    pub(crate) selection: CapabilitySelection,
    pub(crate) approved: Vec<String>,
    /// Approved choices reused without a fresh user decision must still have
    /// valid remembered consent and inherit its optional deadline.
    pub(crate) reused_remembered: Vec<String>,
    pub(crate) declined: Vec<String>,
    pub(crate) privileges: Vec<PlatformPrivilege>,
    pub(crate) required_privileges: Vec<PlatformPrivilege>,
    pub(crate) remember: bool,
    pub(crate) remember_until: Option<i64>,
    pub(crate) expires_at: i64,
}

impl SqliteAuthorizationStore {
    pub(super) fn bootstrap_administrator_in_transaction(
        connection: &rusqlite::Connection,
        principal: &str,
        username: &str,
        password: Option<&str>,
        now: i64,
    ) -> Result<(), AuthError> {
        if username.trim() != username
            || username.is_empty()
            || password.is_some_and(|password| password.len() < 12)
        {
            return Err(AuthError::Invalid(
                "invalid administrator credentials".into(),
            ));
        }
        let hash = if let Some(password) = password {
            let mut salt = [0_u8; 16];
            getrandom::fill(&mut salt).map_err(|_| AuthError::Unavailable)?;
            let salt = SaltString::encode_b64(&salt).map_err(|_| AuthError::Unavailable)?;
            Some(
                Argon2::default()
                    .hash_password(password.as_bytes(), &salt)
                    .map_err(|_| AuthError::Unavailable)?
                    .to_string(),
            )
        } else {
            None
        };
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator)",
            [],
            |row| row.get(0),
        )?;
        if exists {
            return Err(AuthError::Conflict);
        }
        connection.execute(
            "INSERT INTO auth_principals(principal_id,kind,state,created_at,\
            updated_at,version) \
            VALUES(?1,'user','active',?2,?2,1)",
            params![principal, now],
        )?;
        connection.execute(
            "INSERT INTO auth_local_credentials(principal_id,normalized_username,\
            password_hash,hash_profile,password_changed_at,updated_at,version) \
            VALUES(?1,?2,?3,1,?4,?4,1)",
            params![principal, username.to_lowercase(), hash, now],
        )?;
        connection.execute(
            "INSERT INTO auth_bootstrap_administrator VALUES(1,?1,?2)",
            params![principal, now],
        )?;
        connection.execute(
            "INSERT INTO auth_platform_assignments VALUES(?1,?2,1,?3)",
            params![id(), principal, now],
        )?;
        for privilege in [
            PlatformPrivilege::PrincipalsManage,
            PlatformPrivilege::RolesManage,
            PlatformPrivilege::ApisAccept,
            PlatformPrivilege::ApisForceReplace,
            PlatformPrivilege::ClientsManage,
            PlatformPrivilege::PrivilegesManage,
        ] {
            connection.execute(
                "INSERT INTO auth_platform_privileges VALUES(?1,?2)",
                params![principal, serde_json::to_value(privilege)?.as_str()],
            )?;
        }
        connection.execute(
            "INSERT INTO auth_security_audit VALUES(?1,?2,'administrator.bootstrap',?2,'{}',?3)",
            params![id(), principal, now],
        )?;
        Ok(())
    }

    pub(super) fn register_client_in_transaction(
        &self,
        connection: &rusqlite::Connection,
        identity: &Mutation,
        registration: &ClientRegistration,
        now: i64,
    ) -> Result<u64, AuthError> {
        self.mutate_in_transaction(
            connection,
            identity,
            "client.register",
            registration,
            now,
            |connection| {
                Self::require_privilege(connection, &identity.actor, "clients.manage")?;
                if let Some(grants) = registration.metadata.get("grant_types") {
                    let grants = grants
                        .as_array()
                        .ok_or_else(|| AuthError::Invalid("invalid grant types".into()))?;
                    if grants.is_empty()
                        || grants.iter().any(|grant| {
                            !matches!(
                                grant.as_str(),
                                Some(
                                    "authorization_code"
                                        | "refresh_token"
                                        | "urn:ietf:params:oauth:grant-type:device_code"
                                )
                            )
                        })
                    {
                        return Err(AuthError::Invalid("unsupported grant types".into()));
                    }
                }
                let callback_free_device = registration.kind == "native"
                    && registration
                        .metadata
                        .get("grant_types")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|grants| {
                            grants.iter().any(|grant| {
                                grant.as_str()
                                    == Some("urn:ietf:params:oauth:grant-type:device_code")
                            }) && grants.iter().all(|grant| {
                                matches!(
                                    grant.as_str(),
                                    Some(
                                        "urn:ietf:params:oauth:grant-type:device_code"
                                            | "refresh_token"
                                    )
                                )
                            })
                        });
                if !["browser", "native"].contains(&registration.kind.as_str())
                    || registration.client_id.is_empty()
                    || registration.display_name.is_empty()
                    || (registration.redirect_uris.is_empty() && !callback_free_device)
                {
                    return Err(AuthError::Invalid("invalid public client".into()));
                }
                for redirect in &registration.redirect_uris {
                    let uri = url::Url::parse(redirect)
                        .map_err(|_| AuthError::Invalid("invalid redirect".into()))?;
                    let loopback = match uri.host() {
                        Some(url::Host::Domain("localhost")) => true,
                        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                        _ => false,
                    };
                    if uri.origin().ascii_serialization() == self.settings.oauth_origin {
                        return Err(AuthError::Invalid("reserved_oauth_origin".into()));
                    }
                    // A plaintext loopback browser callback is development-only;
                    // HTTPS authenticates a loopback callback like any other host.
                    if uri.host_str().is_none()
                        || uri.fragment().is_some()
                        || !uri.username().is_empty()
                        || uri.password().is_some()
                        || (uri.scheme() != "https"
                            && !(uri.scheme() == "http"
                                && loopback
                                && (registration.development || registration.kind == "native")))
                        || (registration.kind == "browser"
                            && loopback
                            && uri.scheme() == "http"
                            && !registration.development)
                    {
                        return Err(AuthError::Invalid("unsafe redirect".into()));
                    }
                    super::warn_oauth_cookie_isolation(
                        &url::Url::parse(&self.settings.oauth_origin)
                            .map_err(|_| AuthError::Denied)?,
                        &uri,
                    );
                }
                // Eligibility is assigned explicitly by a privilege manager; a
                // loopback callback or development mode never implies it.
                let existing_eligibility: Option<String> = connection
                    .query_row(
                        "SELECT requested_privileges_json \
                    FROM auth_oauth_clients \
                    WHERE client_id=?1",
                        [&registration.client_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let existing_eligibility: Vec<PlatformPrivilege> = existing_eligibility
                    .as_deref()
                    .map(decode)
                    .transpose()?
                    .unwrap_or_default();
                if existing_eligibility.iter().collect::<BTreeSet<_>>()
                    != registration
                        .eligible_privileges
                        .iter()
                        .collect::<BTreeSet<_>>()
                {
                    Self::require_privilege(connection, &identity.actor, "privileges.manage")?;
                }
                for name in &registration.implied_capabilities {
                    capability(connection, name)?;
                }
                let current: Option<(u64, String)> = connection
                    .query_row(
                        "SELECT revision,kind FROM auth_oauth_clients WHERE client_id=?1",
                        [&registration.client_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if current.as_ref().map_or(0, |(revision, _)| *revision)
                    != registration.expected_revision
                {
                    return Err(AuthError::Conflict);
                }
                if current
                    .as_ref()
                    .is_some_and(|(_, kind)| kind != &registration.kind)
                {
                    return Err(AuthError::Invalid(
                        "client binding kind cannot change".into(),
                    ));
                }
                connection.execute(
                    "INSERT INTO auth_oauth_clients \
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'active',?9,?10,?10) \
                ON CONFLICT(client_id) \
                DO UPDATE SET kind=excluded.kind,display_name=excluded.display_name,\
                redirect_uris_json=excluded.redirect_uris_json,\
                development=excluded.development,\
                implied_capabilities_json=excluded.implied_capabilities_json,\
                requested_privileges_json=excluded.requested_privileges_json,\
                metadata_json=excluded.metadata_json,revision=excluded.revision,\
                updated_at=excluded.updated_at",
                    params![
                        registration.client_id,
                        registration.kind,
                        registration.display_name,
                        json(&registration.redirect_uris)?,
                        registration.development,
                        json(&registration.implied_capabilities)?,
                        json(&registration.eligible_privileges)?,
                        json(&registration.metadata)?,
                        registration.expected_revision + 1,
                        now
                    ],
                )?;
                enqueue_scope(
                    connection,
                    &EnforcementScope::Client(registration.client_id.clone()),
                    now,
                )?;
                Ok(registration.expected_revision + 1)
            },
        )
    }

    pub(super) fn record_verified_login_in(
        connection: &rusqlite::Connection,
        login: &VerifiedLogin,
        now: i64,
    ) -> Result<String, AuthError> {
        let active: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_principals \
            WHERE principal_id=?1 \
            AND kind='user' AND state='active')",
            [&login.principal_id],
            |row| row.get(0),
        )?;
        if !active || login.expires_at <= now {
            return Err(AuthError::Denied);
        }
        let mut fresh_until = None;
        if let Some(provider) = &login.provider_id {
            let (issuer, freshness): (String, i64) = connection
                .query_row(
                    "SELECT issuer,claim_freshness_seconds \
                    FROM auth_oidc_providers \
                    WHERE provider_id=?1 \
                    AND state='active'",
                    [provider],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(AuthError::Denied)?;
            if login.upstream_issuer.as_deref() != Some(&issuer)
                || login.upstream_subject.as_ref().is_none_or(String::is_empty)
                || login
                    .verified_claims
                    .as_ref()
                    .is_none_or(|claims| !claims.is_object())
            {
                return Err(AuthError::Denied);
            }
            fresh_until = Some(now.saturating_add(freshness).min(login.expires_at));
            let mapped: Option<String> = connection
                .query_row(
                    "SELECT principal_id \
                    FROM auth_provider_identities \
                    WHERE provider=?1 AND provider_subject=?2",
                    params![provider, login.upstream_subject],
                    |row| row.get(0),
                )
                .optional()?;
            if mapped.as_deref() != Some(&login.principal_id) {
                return Err(AuthError::Denied);
            }
        } else if login.upstream_issuer.is_some()
            || login.upstream_subject.is_some()
            || login.verified_claims.is_some()
        {
            return Err(AuthError::Denied);
        }
        let order: u64 = connection.query_row(
            "SELECT coalesce(max(observation_order),0)+1 \
            FROM auth_login_sessions \
            WHERE principal_id=?1",
            [&login.principal_id],
            |row| row.get(0),
        )?;
        let session = id();
        connection.execute(
            "INSERT INTO auth_login_sessions(login_session_id,principal_id,method,\
            provider_id,upstream_issuer,upstream_subject,verified_claims_json,\
            verified_roles_json,observed_at,observation_order,fresh_until,\
            expires_at,created_at,revision) \
            VALUES(?1,?2,?3,?4,?5,?6,?7,'[]',?8,?9,?10,?11,?8,1)",
            params![
                session,
                login.principal_id,
                if login.provider_id.is_some() {
                    "oidc"
                } else {
                    "local"
                },
                login.provider_id,
                login.upstream_issuer,
                login.upstream_subject,
                login.verified_claims.as_ref().map(json).transpose()?,
                now,
                order,
                fresh_until,
                login.expires_at
            ],
        )?;
        if login.provider_id.is_some() {
            enqueue_scope(
                connection,
                &EnforcementScope::Principal(login.principal_id.clone()),
                now,
            )?;
        }
        Ok(session)
    }

    pub(super) fn register_oidc_provider_in_transaction(
        &self,
        connection: &rusqlite::Connection,
        identity: &Mutation,
        registration: &OidcProviderRegistration,
        now: i64,
    ) -> Result<u64, AuthError> {
        self.mutate_in_transaction(
            connection,
            identity,
            "oidc.register",
            registration,
            now,
            |connection| {
                Self::require_privilege(connection, &identity.actor, "principals.manage")?;
                let issuer =
                    url::Url::parse(&registration.issuer).map_err(|_| AuthError::Denied)?;
                if issuer.scheme() != "https"
                    || issuer.host_str().is_none()
                    || !issuer.username().is_empty()
                    || issuer.password().is_some()
                    || issuer.query().is_some()
                    || issuer.fragment().is_some()
                    || ulid::Ulid::from_string(&registration.provider_id).is_err()
                    || registration.client_id.is_empty()
                    || registration.display_name.is_empty()
                    || !registration.config.is_object()
                    || registration.claim_freshness_seconds == 0
                {
                    return Err(AuthError::Invalid("invalid upstream provider".into()));
                }
                let current: Option<u64> = connection
                    .query_row(
                        "SELECT revision FROM auth_oidc_providers WHERE provider_id=?1",
                        [&registration.provider_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if current.unwrap_or(0) != registration.expected_revision {
                    return Err(AuthError::Conflict);
                }
                let old_issuer: Option<String> = connection
                    .query_row(
                        "SELECT issuer FROM auth_oidc_providers WHERE provider_id=?1",
                        [&registration.provider_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if old_issuer
                    .as_ref()
                    .is_some_and(|issuer| issuer != &registration.issuer)
                {
                    return Err(AuthError::Invalid(
                        "provider identity cannot change issuer".into(),
                    ));
                }
                connection.execute(
                    "INSERT INTO auth_oidc_providers \
                    VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8,?9,?9) \
                    ON CONFLICT(provider_id) \
                    DO UPDATE SET display_name=excluded.display_name,\
                    client_id=excluded.client_id,\
                    sealed_client_secret=excluded.sealed_client_secret,\
                    config_json=excluded.config_json,\
                    claim_freshness_seconds=excluded.claim_freshness_seconds,\
                    revision=excluded.revision,updated_at=excluded.updated_at",
                    params![
                        registration.provider_id,
                        registration.issuer,
                        registration.display_name,
                        registration.client_id,
                        registration.sealed_client_secret,
                        json(&registration.config)?,
                        registration.claim_freshness_seconds,
                        registration.expected_revision + 1,
                        now
                    ],
                )?;
                enqueue_scope(
                    connection,
                    &EnforcementScope::OidcProvider(registration.provider_id.clone()),
                    now,
                )?;
                Ok(registration.expected_revision + 1)
            },
        )
    }

    /// Persist a verified upstream link inside its owning account transaction.
    /// A subject already owned by another user cannot be reassigned.
    pub(super) fn link_oidc_identity_in(
        connection: &Connection,
        principal: &str,
        provider: &str,
        subject: &str,
        now: i64,
    ) -> Result<bool, AuthError> {
        let valid: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_principals p,auth_oidc_providers o \
            WHERE p.principal_id=?1 \
            AND p.kind='user' AND p.state='active' \
            AND o.provider_id=?2 \
            AND o.state='active')",
            params![principal, provider],
            |row| row.get(0),
        )?;
        if !valid || subject.is_empty() {
            return Err(AuthError::Denied);
        }
        let existing: Option<String> = connection
            .query_row(
                "SELECT principal_id \
                FROM auth_provider_identities \
                WHERE provider=?1 AND provider_subject=?2",
                params![provider, subject],
                |row| row.get(0),
            )
            .optional()?;
        if existing.as_ref().is_some_and(|owner| owner != principal) {
            return Err(AuthError::Conflict);
        }
        if existing.is_some() {
            return Ok(true);
        }
        connection.execute(
            "INSERT INTO auth_provider_identities(identity_id,principal_id,\
            provider,provider_subject,linked_at,last_seen_at) \
            VALUES(?1,?2,?3,?4,?5,?5)",
            params![id(), principal, provider, subject, now],
        )?;
        Ok(false)
    }

    /// The OAuth adapter supplies authenticated client/binding and DPoP proof.
    /// Auth rereads all principal, entitlement, consent and privilege policy here.
    pub(crate) fn approve_grant(
        &self,
        identity: &Mutation,
        approval: &GrantApproval,
        now: i64,
    ) -> Result<String, AuthError> {
        self.mutate(identity, "oauth.approve", approval, now, |connection| {
            if identity.actor != approval.principal_id
                || approval.expires_at <= now
                || approval.remember_until.is_some_and(|expiry| expiry <= now)
            {
                return Err(AuthError::Denied);
            }
            super::oauth::enrollment::validate_selection(
                connection,
                &approval.client_id,
                &approval.selection.required,
                &approval.selection.optional,
            )?;
            let login_expiry: Option<i64> = connection
                .query_row(
                    "SELECT l.expires_at \
                    FROM auth_login_sessions l \
                    JOIN auth_principals p USING(principal_id) \
                    WHERE l.login_session_id=?1 \
                    AND l.principal_id=?2 \
                    AND l.revoked_at IS NULL \
                    AND l.expires_at>?3 \
                    AND p.state='active'",
                    params![approval.login_session_id, approval.principal_id, now],
                    |row| row.get(0),
                )
                .optional()?;
            let login_expiry = login_expiry.ok_or(AuthError::Denied)?;
            if let Some(presentation) = &approval.presentation {
                let sealed: Option<Vec<u8>> = connection
                    .query_row(
                        "SELECT sealed_record \
                        FROM auth_oauth_artifacts \
                        WHERE kind='authorization' \
                        AND lookup_hash=?1 AND expires_at>?2",
                        params![presentation.lookup_hash, now],
                        |row| row.get(0),
                    )
                    .optional()?;
                if sealed
                    .as_ref()
                    .map(|bytes| URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
                    != Some(presentation.sealed_digest.clone())
                {
                    return Err(AuthError::ConsentChanged);
                }
                if presentation.principal_id != approval.principal_id
                    || presentation.login_session_id != approval.login_session_id
                    || presentation.client_id != approval.client_id
                    || presentation.binding_kind != approval.binding_kind
                    || presentation.binding_value != approval.binding_value
                    || presentation.selection.required != approval.selection.required
                    || presentation.selection.optional != approval.selection.optional
                    || presentation.required_privileges != approval.required_privileges
                    || !approval.privileges.iter().all(|privilege| {
                        presentation.required_privileges.contains(privilege)
                            || presentation.optional_privileges.contains(privilege)
                    })
                {
                    return Err(AuthError::Denied);
                }
                for (name, displayed) in &presentation.capabilities {
                    let current = match capability(connection, name) {
                        Ok(value) => Some(value),
                        Err(AuthError::NotFound) => None,
                        Err(error) => return Err(error),
                    };
                    if &current != displayed {
                        return Err(AuthError::ConsentChanged);
                    }
                }
                for (api, generation) in &presentation.api_generations {
                    let current: Option<u64> = connection
                        .query_row(
                            "SELECT generation FROM auth_accepted_apis WHERE api_id=?1",
                            [api],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if current != Some(*generation) {
                        return Err(AuthError::ConsentChanged);
                    }
                }
            }
            let public = approval.public_jwk.as_object().ok_or(AuthError::Denied)?;
            if public.contains_key("d")
                || public.get("kty").and_then(Value::as_str) != Some("EC")
                || public.get("crv").and_then(Value::as_str) != Some("P-256")
            {
                return Err(AuthError::Denied);
            }
            let thumbprint = serde_json::json!({
                "crv":"P-256",
                "kty":"EC",
                "x":public.get("x").ok_or(AuthError::Denied)?,
                "y":public.get("y").ok_or(AuthError::Denied)?
            });
            if URL_SAFE_NO_PAD.encode(Sha256::digest(json(&thumbprint)?.as_bytes()))
                != approval.durable_dpop_jkt
            {
                return Err(AuthError::Denied);
            }
            if approval.binding_kind == "native"
                && approval.binding_value != approval.durable_dpop_jkt
            {
                return Err(AuthError::Denied);
            }
            if !["native", "browser"].contains(&approval.binding_kind.as_str()) {
                return Err(AuthError::Denied);
            }
            if approval.binding_kind == "browser" {
                let origin =
                    url::Url::parse(&approval.binding_value).map_err(|_| AuthError::Denied)?;
                let loopback =
                    matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
                if origin.origin().ascii_serialization() != approval.binding_value
                    || (origin.scheme() != "https" && !(origin.scheme() == "http" && loopback))
                {
                    return Err(AuthError::Denied);
                }
            }
            let client: Option<(String, String, String, String)> = connection
                .query_row(
                    "SELECT state,kind,implied_capabilities_json,requested_privileges_json \
                    FROM auth_oauth_clients \
                    WHERE client_id=?1",
                    [&approval.client_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let (kind, implied, eligible) = match client {
                Some((state, kind, implied, eligible)) if state == "active" => {
                    (kind, implied, eligible)
                }
                Some(_) => return Err(AuthError::Denied),
                None => {
                    let kind: Option<String> = connection
                        .query_row(
                            "SELECT kind FROM auth_cimd_clients WHERE client_id=?1",
                            [&approval.client_id],
                            |row| row.get(0),
                        )
                        .optional()?;
                    (kind.ok_or(AuthError::Denied)?, "[]".into(), "[]".into())
                }
            };
            if kind != approval.binding_kind {
                return Err(AuthError::Denied);
            }
            // Registered browser trust cannot stand in for a native user's
            // explicit approval, including Device Authorization Grant consent.
            let implied: Vec<String> = if kind == "browser" {
                decode(&implied)?
            } else {
                Vec::new()
            };
            let eligible: Vec<PlatformPrivilege> = decode(&eligible)?;
            let entitled = entitlements(
                connection,
                &approval.principal_id,
                Some(&approval.login_session_id),
                now,
            )?;
            if approval.presentation.is_some()
                && approval
                    .selection
                    .required
                    .iter()
                    .any(|name| !entitled.contains_key(name))
            {
                return Err(AuthError::ConsentChanged);
            }
            let selected: BTreeSet<_> = approval
                .selection
                .required
                .iter()
                .chain(&approval.selection.optional)
                .cloned()
                .collect();
            let declined: BTreeSet<_> = approval.declined.iter().cloned().collect();
            let choices: BTreeSet<_> = approval.approved.iter().cloned().collect();
            let reused: BTreeSet<_> = approval.reused_remembered.iter().cloned().collect();
            if !choices.is_subset(&selected)
                || !declined.is_subset(&selected)
                || !choices.is_disjoint(&declined)
                || !reused.is_subset(&choices)
            {
                return Err(AuthError::Denied);
            }
            let mut approved: Vec<CapabilityAuthority> = Vec::new();
            let mut consent_basis = BTreeMap::new();
            for name in &selected {
                // Implied choices are still explicit members of this request;
                // original declines always win and are retained in the grant.
                let accepted = (choices.contains(name) || implied.contains(name))
                    && !declined.contains(name)
                    && entitled.contains_key(name);
                if !accepted && approval.selection.required.contains(name) {
                    return Err(AuthError::RequiredMissing);
                }
                let cap = match capability(connection, name) {
                    Ok(cap) => cap,
                    Err(AuthError::NotFound) if !approval.selection.required.contains(name) => {
                        continue
                    }
                    Err(error) => return Err(error),
                };
                if approval.remember && !reused.contains(name) {
                    connection.execute(
                        "INSERT INTO auth_remembered_consent \
                        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10) \
                        ON CONFLICT(principal_id,client_id,binding_kind,binding_value,\
                        capability_id,capability_generation,consent_revision) \
                        DO UPDATE SET decision=excluded.decision,\
                        expires_at=excluded.expires_at,revision=revision+1,\
                        updated_at=excluded.updated_at",
                        params![
                            approval.principal_id,
                            approval.client_id,
                            approval.binding_kind,
                            approval.binding_value,
                            name,
                            cap.identity_generation.get(),
                            cap.consent_revision.get(),
                            if accepted { "approved" } else { "declined" },
                            approval.remember_until,
                            now
                        ],
                    )?;
                }
                if accepted {
                    let consent: Option<(String, Option<i64>, u64)> = connection
                        .query_row(
                            "SELECT decision,expires_at,revision \
                            FROM auth_remembered_consent \
                            WHERE principal_id=?1 \
                            AND client_id=?2 AND binding_kind=?3 \
                            AND binding_value=?4 \
                            AND capability_id=?5 \
                            AND capability_generation=?6 \
                            AND consent_revision=?7",
                            params![
                                approval.principal_id,
                                approval.client_id,
                                approval.binding_kind,
                                approval.binding_value,
                                name,
                                cap.identity_generation.get(),
                                cap.consent_revision.get()
                            ],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .optional()?;
                    let (revision, expires_at) = if reused.contains(name) {
                        let (decision, expires, revision) = consent.ok_or(AuthError::Denied)?;
                        if decision != "approved" || expires.is_some_and(|expiry| expiry <= now) {
                            return Err(AuthError::Denied);
                        }
                        (revision, expires)
                    } else {
                        (
                            consent.map_or(0, |(_, _, revision)| revision),
                            if approval.remember {
                                approval.remember_until
                            } else {
                                None
                            },
                        )
                    };
                    consent_basis.insert(
                        name.clone(),
                        GrantConsentBasis {
                            revision,
                            expires_at,
                        },
                    );
                    approved.push(cap);
                }
            }
            if !approval
                .required_privileges
                .iter()
                .all(|privilege| approval.privileges.contains(privilege))
            {
                return Err(AuthError::RequiredMissing);
            }
            for privilege in &approval.privileges {
                let assigned: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 \
                    FROM auth_platform_privileges \
                    WHERE principal_id=?1 \
                    AND privilege=?2)",
                    params![
                        approval.principal_id,
                        serde_json::to_value(privilege)?.as_str()
                    ],
                    |row| row.get(0),
                )?;
                if !assigned || !eligible.contains(privilege) {
                    return Err(AuthError::Denied);
                }
            }
            if !approval.privileges.is_empty() {
                connection.execute(
            "INSERT INTO auth_platform_delegations \
            VALUES(?1,?2,?3,?4,?5,?6,1) \
            ON CONFLICT(principal_id,client_id,binding_kind,binding_value) \
            DO UPDATE SET approved_privileges_json=excluded.approved_privileges_json,expires_at=excluded.expires_at,revision=revision+1",
            params![
                approval.principal_id,
                approval.client_id,
                approval.binding_kind,
                approval.binding_value,
                json(&approval.privileges)?,
                approval.remember_until
            ],
                )?;
            }
            let grant = id();
            connection.execute(
                "INSERT INTO auth_oauth_grants(oauth_grant_id,principal_id,\
                login_session_id,client_id,binding_kind,binding_value,\
                durable_dpop_jkt,public_jwk_json,required_capabilities_json,\
                optional_capabilities_json,approved_capabilities_json,\
                approved_privileges_json,required_privileges_json,consent_basis_json,\
                created_at,expires_at,revoked_at,revision) \
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,NULL,1)",
                params![
                    grant,
                    approval.principal_id,
                    approval.login_session_id,
                    approval.client_id,
                    approval.binding_kind,
                    approval.binding_value,
                    approval.durable_dpop_jkt,
                    json(&approval.public_jwk)?,
                    json(&approval.selection.required)?,
                    json(&approval.selection.optional)?,
                    json(&approved)?,
                    json(&approval.privileges)?,
                    json(&approval.required_privileges)?,
                    json(&consent_basis)?,
                    now,
                    approval.expires_at.min(login_expiry)
                ],
            )?;
            if approval.remember || !approval.privileges.is_empty() {
                enqueue_scope(
                    connection,
                    &EnforcementScope::Principal(approval.principal_id.clone()),
                    now,
                )?;
            }
            if let Some(presentation) = &approval.presentation {
                connection.execute(
                    "DELETE FROM auth_oauth_artifacts \
                    WHERE kind='authorization' \
                    AND lookup_hash=?1",
                    [&presentation.lookup_hash],
                )?;
            }
            Ok(grant)
        })
    }
}
