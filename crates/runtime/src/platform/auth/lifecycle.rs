use std::collections::{BTreeMap, BTreeSet};

use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, OptionalExtension};
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
    pub(crate) remember: bool,
    pub(crate) remember_until: Option<i64>,
    pub(crate) expires_at: i64,
}

impl SqliteAuthorizationStore {
    /// Initial administrator provisioning is a single aggregate transaction.
    /// There is no application participant or blanket wildcard authority.
    pub(crate) fn bootstrap_administrator(
        &self,
        username: &str,
        password: &str,
        now: i64,
    ) -> Result<String, AuthError> {
        if username.trim() != username || username.is_empty() || password.len() < 12 {
            return Err(AuthError::Invalid(
                "invalid administrator credentials".into(),
            ));
        }
        let mut salt = [0_u8; 16];
        getrandom::fill(&mut salt).map_err(|_| AuthError::Unavailable)?;
        let salt = SaltString::encode_b64(&salt).map_err(|_| AuthError::Unavailable)?;
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| AuthError::Unavailable)?
            .to_string();
        self.transaction(|connection| {
            let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator)",[],|row|row.get(0))?;
            if exists {return Err(AuthError::Conflict);}
            let principal=id();
            connection.execute("INSERT INTO auth_principals(principal_id,kind,state,created_at,updated_at,version) VALUES(?1,'user','active',?2,?2,1)",params![principal,now])?;
            connection.execute("INSERT INTO auth_local_credentials(principal_id,normalized_username,password_hash,hash_profile,password_changed_at,updated_at,version) VALUES(?1,?2,?3,1,?4,?4,1)",params![principal,username.to_lowercase(),hash,now])?;
            connection.execute("INSERT INTO auth_bootstrap_administrator VALUES(1,?1,?2)",params![principal,now])?;
            for privilege in [PlatformPrivilege::PrincipalsManage,PlatformPrivilege::RolesManage,PlatformPrivilege::ApisAccept,PlatformPrivilege::ApisForceReplace,PlatformPrivilege::ClientsManage,PlatformPrivilege::PrivilegesManage] {
                connection.execute("INSERT INTO auth_platform_privileges VALUES(?1,?2,1,?3)",params![principal,serde_json::to_value(privilege)?.as_str(),now])?;
            }
            connection.execute("INSERT INTO auth_security_audit VALUES(?1,?2,'administrator.bootstrap',?2,'{}',?3)",params![id(),principal,now])?;
            Ok(principal)
        })
    }

    pub(crate) fn create_principal(
        &self,
        identity: &Mutation,
        kind: &str,
        now: i64,
    ) -> Result<String, AuthError> {
        self.mutate(identity,"principal.create",&kind,now,|connection| {
            Self::require_privilege(connection,&identity.actor,"principals.manage")?;
            if !["user","service","device"].contains(&kind) {return Err(AuthError::Invalid("invalid principal kind".into()));}
            let principal=id();
            connection.execute("INSERT INTO auth_principals(principal_id,kind,state,created_at,updated_at,version) VALUES(?1,?2,'active',?3,?3,1)",params![principal,kind,now])?;
            Ok(principal)
        })
    }

    pub(crate) fn register_client(
        &self,
        identity: &Mutation,
        registration: &ClientRegistration,
        now: i64,
    ) -> Result<u64, AuthError> {
        self.mutate(identity,"client.register",registration,now,|connection| {
            Self::require_privilege(connection,&identity.actor,"clients.manage")?;
            if !["browser","native"].contains(&registration.kind.as_str()) || registration.client_id.is_empty() || registration.display_name.is_empty() {return Err(AuthError::Invalid("invalid public client".into()));}
            for redirect in &registration.redirect_uris {
                let uri=url::Url::parse(redirect).map_err(|_|AuthError::Invalid("invalid redirect".into()))?;
                let loopback=matches!(uri.host_str(),Some("127.0.0.1"|"[::1]"|"::1"));
                if uri.fragment().is_some() || !uri.username().is_empty() || uri.password().is_some()
                    || (uri.scheme()!="https" && !(uri.scheme()=="http" && (registration.development || (registration.kind=="native" && loopback)))) {return Err(AuthError::Invalid("unsafe redirect".into()));}
            }
            // Administrative eligibility is never available to CIMD or a
            // development registration, even with administrator registration.
            if !registration.eligible_privileges.is_empty() && (registration.development || registration.client_id.starts_with("https://")) {return Err(AuthError::Denied);}
            for name in &registration.implied_capabilities {capability(connection,name)?;}
            let current:Option<u64>=connection.query_row("SELECT revision FROM auth_oauth_clients WHERE client_id=?1 AND state!='deleted'",[&registration.client_id],|row|row.get(0)).optional()?;
            if current.unwrap_or(0)!=registration.expected_revision {return Err(AuthError::Conflict);}
            connection.execute("INSERT INTO auth_oauth_clients VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'active',?9,?10,?10) ON CONFLICT(client_id) DO UPDATE SET kind=excluded.kind,display_name=excluded.display_name,redirect_uris_json=excluded.redirect_uris_json,development=excluded.development,implied_capabilities_json=excluded.implied_capabilities_json,requested_privileges_json=excluded.requested_privileges_json,metadata_json=excluded.metadata_json,revision=excluded.revision,updated_at=excluded.updated_at",params![registration.client_id,registration.kind,registration.display_name,json(&registration.redirect_uris)?,registration.development,json(&registration.implied_capabilities)?,json(&registration.eligible_privileges)?,json(&registration.metadata)?,registration.expected_revision+1,now])?;
            enqueue_scope(connection,&EnforcementScope::Client(registration.client_id.clone()),now)?;
            Ok(registration.expected_revision+1)
        })
    }

    pub(crate) fn record_verified_login(
        &self,
        login: &VerifiedLogin,
        now: i64,
    ) -> Result<String, AuthError> {
        self.transaction(|connection| {
            let active:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_principals WHERE principal_id=?1 AND kind='user' AND state='active')",[&login.principal_id],|row|row.get(0))?;
            if !active || login.expires_at<=now {return Err(AuthError::Denied);}
            let mut fresh_until=None;
            if let Some(provider)=&login.provider_id {
                let (issuer,freshness):(String,i64)=connection.query_row("SELECT issuer,claim_freshness_seconds FROM auth_oidc_providers WHERE provider_id=?1 AND state='active'",[provider],|row|Ok((row.get(0)?,row.get(1)?))).optional()?.ok_or(AuthError::Denied)?;
                if login.upstream_issuer.as_deref()!=Some(&issuer) || login.upstream_subject.as_ref().is_none_or(String::is_empty) || login.verified_claims.as_ref().is_none_or(|claims|!claims.is_object()) {return Err(AuthError::Denied);}
                fresh_until=Some(now.saturating_add(freshness).min(login.expires_at));
                let mapped:Option<String>=connection.query_row("SELECT principal_id FROM auth_provider_identities WHERE provider=?1 AND provider_subject=?2",params![provider,login.upstream_subject],|row|row.get(0)).optional()?;
                if mapped.as_deref()!=Some(&login.principal_id) {return Err(AuthError::Denied);}
            } else if login.upstream_issuer.is_some() || login.upstream_subject.is_some() || login.verified_claims.is_some() {return Err(AuthError::Denied);}
            let order:u64=connection.query_row("SELECT coalesce(max(observation_order),0)+1 FROM auth_login_sessions WHERE principal_id=?1",[&login.principal_id],|row|row.get(0))?;
            let session=id();
            connection.execute("INSERT INTO auth_login_sessions(login_session_id,principal_id,method,provider_id,upstream_issuer,upstream_subject,verified_claims_json,verified_roles_json,observed_at,observation_order,fresh_until,expires_at,created_at,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,'[]',?8,?9,?10,?11,?8,1)",params![session,login.principal_id,if login.provider_id.is_some(){"oidc"}else{"local"},login.provider_id,login.upstream_issuer,login.upstream_subject,login.verified_claims.as_ref().map(json).transpose()?,now,order,fresh_until,login.expires_at])?;
            if login.provider_id.is_some() {enqueue_scope(connection,&EnforcementScope::Principal(login.principal_id.clone()),now)?;}
            Ok(session)
        })
    }

    pub(crate) fn register_oidc_provider(
        &self,
        identity: &Mutation,
        registration: &OidcProviderRegistration,
        now: i64,
    ) -> Result<u64, AuthError> {
        self.mutate(identity, "oidc.register", registration, now, |connection| {
            Self::require_privilege(connection, &identity.actor, "principals.manage")?;
            let issuer = url::Url::parse(&registration.issuer).map_err(|_| AuthError::Denied)?;
            if issuer.scheme() != "https" || issuer.host_str().is_none()
                || !issuer.username().is_empty() || issuer.password().is_some()
                || issuer.query().is_some() || issuer.fragment().is_some()
                || ulid::Ulid::from_string(&registration.provider_id).is_err() || registration.client_id.is_empty()
                || registration.display_name.is_empty() || !registration.config.is_object()
                || registration.claim_freshness_seconds == 0 {
                return Err(AuthError::Invalid("invalid upstream provider".into()));
            }
            let current: Option<u64> = connection.query_row(
                "SELECT revision FROM auth_oidc_providers WHERE provider_id=?1",
                [&registration.provider_id], |row| row.get(0),
            ).optional()?;
            if current.unwrap_or(0) != registration.expected_revision { return Err(AuthError::Conflict); }
            let old_issuer: Option<String> = connection.query_row(
                "SELECT issuer FROM auth_oidc_providers WHERE provider_id=?1",
                [&registration.provider_id], |row| row.get(0),
            ).optional()?;
            if old_issuer.as_ref().is_some_and(|issuer| issuer != &registration.issuer) {
                return Err(AuthError::Invalid("provider identity cannot change issuer".into()));
            }
            connection.execute(
                "INSERT INTO auth_oidc_providers VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8,?9,?9) ON CONFLICT(provider_id) DO UPDATE SET display_name=excluded.display_name,client_id=excluded.client_id,sealed_client_secret=excluded.sealed_client_secret,config_json=excluded.config_json,claim_freshness_seconds=excluded.claim_freshness_seconds,revision=excluded.revision,updated_at=excluded.updated_at",
                params![registration.provider_id,registration.issuer,registration.display_name,registration.client_id,registration.sealed_client_secret,json(&registration.config)?,registration.claim_freshness_seconds,registration.expected_revision+1,now],
            )?;
            enqueue_scope(connection, &EnforcementScope::OidcProvider(registration.provider_id.clone()), now)?;
            Ok(registration.expected_revision + 1)
        })
    }

    pub(crate) fn link_oidc_identity(
        &self,
        identity: &Mutation,
        principal: &str,
        provider: &str,
        subject: &str,
        now: i64,
    ) -> Result<(), AuthError> {
        self.mutate(identity, "oidc.identity.link", &(principal,provider,subject), now, |connection| {
            Self::require_privilege(connection, &identity.actor, "principals.manage")?;
            let valid: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_principals p,auth_oidc_providers o WHERE p.principal_id=?1 AND p.kind='user' AND p.state='active' AND o.provider_id=?2 AND o.state='active')",
                params![principal,provider], |row| row.get(0),
            )?;
            if !valid || subject.is_empty() {return Err(AuthError::Denied);}
            connection.execute(
                "INSERT INTO auth_provider_identities(principal_id,provider,provider_subject,linked_at,last_seen_at) VALUES(?1,?2,?3,?4,?4)",
                params![principal,provider,subject,now],
            )?;
            Ok(())
        })
    }

    /// The OAuth adapter supplies authenticated client/binding and DPoP proof.
    /// Auth rereads all principal, entitlement, consent and privilege policy here.
    pub(crate) fn approve_grant(
        &self,
        identity: &Mutation,
        approval: &GrantApproval,
        now: i64,
    ) -> Result<String, AuthError> {
        self.mutate(identity,"oauth.approve",approval,now,|connection| {
            if identity.actor!=approval.principal_id || approval.expires_at<=now || approval.remember_until.is_some_and(|expiry|expiry<=now) {return Err(AuthError::Denied);}
            let login_expiry:Option<i64>=connection.query_row("SELECT l.expires_at FROM auth_login_sessions l JOIN auth_principals p USING(principal_id) WHERE l.login_session_id=?1 AND l.principal_id=?2 AND l.revoked_at IS NULL AND l.expires_at>?3 AND p.state='active'",params![approval.login_session_id,approval.principal_id,now],|row|row.get(0)).optional()?;
            let login_expiry=login_expiry.ok_or(AuthError::Denied)?;
            let public=approval.public_jwk.as_object().ok_or(AuthError::Denied)?;
            if public.contains_key("d") || public.get("kty").and_then(Value::as_str)!=Some("EC") || public.get("crv").and_then(Value::as_str)!=Some("P-256") {return Err(AuthError::Denied);}
            let thumbprint=serde_json::json!({"crv":"P-256","kty":"EC","x":public.get("x").ok_or(AuthError::Denied)?,"y":public.get("y").ok_or(AuthError::Denied)?});
            if URL_SAFE_NO_PAD.encode(Sha256::digest(json(&thumbprint)?.as_bytes()))!=approval.durable_dpop_jkt {return Err(AuthError::Denied);}
            if approval.binding_kind=="native" && approval.binding_value!=approval.durable_dpop_jkt {return Err(AuthError::Denied);}
            if !["native","browser"].contains(&approval.binding_kind.as_str()) {return Err(AuthError::Denied);}
            if approval.binding_kind=="browser" {
                let origin=url::Url::parse(&approval.binding_value).map_err(|_|AuthError::Denied)?;
                if origin.origin().ascii_serialization()!=approval.binding_value || origin.scheme()!="https" {return Err(AuthError::Denied);}
            }
            let client:Option<(String,String,String)>=connection.query_row("SELECT kind,implied_capabilities_json,requested_privileges_json FROM auth_oauth_clients WHERE client_id=?1 AND state='active'",[&approval.client_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            let (kind,implied,eligible)=client.ok_or(AuthError::Denied)?;
            if kind!=approval.binding_kind {return Err(AuthError::Denied);}
            let implied:Vec<String>=decode(&implied)?;
            let eligible:Vec<PlatformPrivilege>=decode(&eligible)?;
            let entitled=entitlements(connection,&approval.principal_id,Some(&approval.login_session_id),now)?;
            let selected:BTreeSet<_>=approval.selection.required.iter().chain(&approval.selection.optional).cloned().collect();
            let declined:BTreeSet<_>=approval.declined.iter().cloned().collect();
            let choices:BTreeSet<_>=approval.approved.iter().cloned().collect();
            let reused:BTreeSet<_>=approval.reused_remembered.iter().cloned().collect();
            if !choices.is_subset(&selected) || !declined.is_subset(&selected) || !choices.is_disjoint(&declined) || !reused.is_subset(&choices) {return Err(AuthError::Denied);}
            let mut approved:Vec<CapabilityAuthority>=Vec::new();
            let mut consent_basis = BTreeMap::new();
            for name in &selected {
                // Implied choices are still explicit members of this request;
                // original declines always win and are retained in the grant.
                let accepted=(choices.contains(name)||implied.contains(name)) && !declined.contains(name) && entitled.contains_key(name);
                if !accepted && approval.selection.required.contains(name) {return Err(AuthError::RequiredMissing);}
                let cap=match capability(connection,name) {
                    Ok(cap)=>cap,
                    Err(AuthError::NotFound) if !approval.selection.required.contains(name)=>continue,
                    Err(error)=>return Err(error),
                };
                if approval.remember && !reused.contains(name) {
                    connection.execute("INSERT INTO auth_remembered_consent VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10) ON CONFLICT(principal_id,client_id,binding_kind,binding_value,capability_id,capability_generation,consent_revision) DO UPDATE SET decision=excluded.decision,expires_at=excluded.expires_at,revision=revision+1,updated_at=excluded.updated_at",params![approval.principal_id,approval.client_id,approval.binding_kind,approval.binding_value,name,cap.identity_generation.get(),cap.consent_revision.get(),if accepted{"approved"}else{"declined"},approval.remember_until,now])?;
                }
                if accepted {
                    let consent:Option<(String,Option<i64>,u64)>=connection.query_row("SELECT decision,expires_at,revision FROM auth_remembered_consent WHERE principal_id=?1 AND client_id=?2 AND binding_kind=?3 AND binding_value=?4 AND capability_id=?5 AND capability_generation=?6 AND consent_revision=?7",params![approval.principal_id,approval.client_id,approval.binding_kind,approval.binding_value,name,cap.identity_generation.get(),cap.consent_revision.get()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
                    let (revision,expires_at) = if reused.contains(name) {
                        let (decision,expires,revision) = consent.ok_or(AuthError::Denied)?;
                        if decision != "approved" || expires.is_some_and(|expiry|expiry <= now) {return Err(AuthError::Denied);}
                        (revision,expires)
                    } else {
                        (consent.map_or(0, |(_, _, revision)|revision), if approval.remember {approval.remember_until} else {None})
                    };
                    consent_basis.insert(name.clone(), GrantConsentBasis {revision,expires_at});
                    approved.push(cap);
                }
            }
            for privilege in &approval.privileges {
                let assigned:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_platform_privileges WHERE principal_id=?1 AND privilege=?2)",params![approval.principal_id,serde_json::to_value(privilege)?.as_str()],|row|row.get(0))?;
                if !assigned || !eligible.contains(privilege) {return Err(AuthError::Denied);}
            }
            if !approval.privileges.is_empty() {
                connection.execute("INSERT INTO auth_platform_delegations VALUES(?1,?2,?3,?4,?5,?6,1) ON CONFLICT(principal_id,client_id,binding_kind,binding_value) DO UPDATE SET approved_privileges_json=excluded.approved_privileges_json,expires_at=excluded.expires_at,revision=revision+1",params![approval.principal_id,approval.client_id,approval.binding_kind,approval.binding_value,json(&approval.privileges)?,approval.remember_until])?;
            }
            let grant=id();
            connection.execute("INSERT INTO auth_oauth_grants(oauth_grant_id,principal_id,login_session_id,client_id,binding_kind,binding_value,durable_dpop_jkt,public_jwk_json,required_capabilities_json,optional_capabilities_json,approved_capabilities_json,approved_privileges_json,consent_basis_json,created_at,expires_at,revoked_at,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,NULL,1)",params![grant,approval.principal_id,approval.login_session_id,approval.client_id,approval.binding_kind,approval.binding_value,approval.durable_dpop_jkt,json(&approval.public_jwk)?,json(&approval.selection.required)?,json(&approval.selection.optional)?,json(&approved)?,json(&approval.privileges)?,json(&consent_basis)?,now,approval.expires_at.min(login_expiry)])?;
            if approval.remember || !approval.privileges.is_empty() {
                enqueue_scope(connection, &EnforcementScope::Principal(approval.principal_id.clone()), now)?;
            }
            Ok(grant)
        })
    }
}
