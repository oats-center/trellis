use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    sign_session_authority, AuthorityIssuerKey, AuthorityIssuerState, AuthorityProvider,
    CapabilityAuthority, CatalogActionIdentity, PlatformPrivilege, PrincipalKind, SessionBinding,
    SignedIssuerRotation, SignedProviderCertificate, SignedSessionAuthority, U64s,
    UnsignedSessionAuthority, ISSUER_ROTATION_FORMAT_V1, PROVIDER_CERTIFICATE_FORMAT_V1,
    SESSION_AUTHORITY_FORMAT_V1,
};

use super::policy::{evaluate, CapabilitySelection};
use super::revocation::{enqueue_effect, retire_session};
use super::sqlite::{decode, digest, id, json, AuthError, Mutation, SqliteAuthorizationStore};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Credential {
    OAuthGrant(String),
    ProvisionedIdentity(String),
}

/// Passed only after the admission boundary authenticates the credential's
/// association with this ephemeral key and the broker's nonce possession proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AdmissionIdentity {
    pub(crate) credential: Credential,
    /// Original verified OAuth admission family, never inferred from a grant.
    pub(crate) refresh_family_id: Option<String>,
    pub(crate) runtime_id: String,
    pub(crate) session_public_key: String,
    pub(crate) inbox_prefix: String,
}

#[derive(Clone, Debug)]
pub(crate) struct IssuedAuthority {
    pub(crate) authority: SignedSessionAuthority,
    pub(crate) digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProviderImplementation {
    pub(crate) api_id: String,
    pub(crate) generation: u64,
    pub(crate) implementation_digest: String,
    pub(crate) implemented_actions: Vec<CatalogActionIdentity>,
}

pub(super) struct CredentialSnapshot {
    pub(super) principal_id: String,
    pub(super) principal_kind: PrincipalKind,
    pub(super) binding: SessionBinding,
    pub(super) login_session_id: Option<String>,
    pub(super) selection: CapabilitySelection,
    pub(super) approved: Option<Vec<CapabilityAuthority>>,
    pub(super) consent_deadlines: BTreeMap<String, i64>,
    pub(super) providers: Vec<AuthorityProvider>,
    pub(super) resource_bindings_digest: Option<String>,
    pub(super) platform_privileges: Vec<PlatformPrivilege>,
    pub(super) platform_privilege_deadline: Option<i64>,
    /// Shared credential lifetime, independent of selected permission limits.
    pub(super) not_after: i64,
}

/// Grant-local approval supersedes remembered choices observed at creation.
/// Only later shared decisions constrain it; its own optional lifetime remains.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrantConsentBasis {
    pub(super) revision: u64,
    pub(super) expires_at: Option<i64>,
}

pub(super) fn credential_snapshot(
    connection: &Connection,
    credential: &Credential,
    now: i64,
) -> Result<CredentialSnapshot, AuthError> {
    match credential {
        Credential::OAuthGrant(grant) => {
            let record=connection.query_row(
                "SELECT g.principal_id,g.login_session_id,g.client_id,g.binding_kind,g.binding_value,g.durable_dpop_jkt,g.required_capabilities_json,g.optional_capabilities_json,g.approved_capabilities_json,min(g.expires_at,l.expires_at),g.revision FROM auth_oauth_grants g JOIN auth_login_sessions l ON l.login_session_id=g.login_session_id AND l.principal_id=g.principal_id JOIN auth_principals p ON p.principal_id=g.principal_id WHERE g.oauth_grant_id=?1 AND g.revoked_at IS NULL AND l.revoked_at IS NULL AND g.expires_at>?2 AND l.expires_at>?2 AND p.state='active'",params![grant,now],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,row.get::<_,String>(7)?,row.get::<_,String>(8)?,row.get::<_,i64>(9)?,row.get::<_,i64>(10)?))).optional()?;
            let (
                principal,
                login,
                client,
                kind,
                value,
                jkt,
                required,
                optional,
                approved,
                deadline,
                _,
            ) = record.ok_or(AuthError::Denied)?;
            let client_state: Option<String> = connection
                .query_row(
                    "SELECT state FROM auth_oauth_clients WHERE client_id=?1",
                    [&client],
                    |row| row.get(0),
                )
                .optional()?;
            let validated_cimd: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_cimd_clients WHERE client_id=?1)",
                [&client],
                |row| row.get(0),
            )?;
            if client_state
                .as_deref()
                .is_some_and(|state| state != "active")
                || (client_state.is_none() && !validated_cimd)
            {
                return Err(AuthError::Denied);
            }
            let provider_active:bool=connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM auth_login_sessions l JOIN auth_oidc_providers p USING(provider_id) WHERE l.login_session_id=?1 AND p.state!='active')",[&login],|row|row.get(0))?;
            if !provider_active {
                return Err(AuthError::Denied);
            }
            // A revoked refresh family is a hard grant barrier, not a policy
            // reduction that can be escaped by creating another logical session.
            let families:(i64,Option<i64>)=connection.query_row("SELECT count(*),max(CASE WHEN revoked_at IS NULL AND expires_at>?2 THEN expires_at END) FROM auth_oauth_families WHERE oauth_grant_id=?1",params![grant,now],|row|Ok((row.get(0)?,row.get(1)?)))?;
            if families.0 > 0 && families.1.is_none() {
                return Err(AuthError::Denied);
            }
            let deadline = families.1.map_or(deadline, |expiry| deadline.min(expiry));
            let mut approved: Vec<CapabilityAuthority> = decode(&approved)?;
            let (requested,basis):(String,String)=connection.query_row("SELECT approved_privileges_json,consent_basis_json FROM auth_oauth_grants WHERE oauth_grant_id=?1",[grant],|row|Ok((row.get(0)?,row.get(1)?)))?;
            let basis: BTreeMap<String, GrantConsentBasis> = decode(&basis)?;
            let mut consent_deadlines = BTreeMap::new();
            let mut retained = Vec::new();
            for cap in approved.drain(..) {
                let approval = basis.get(&cap.capability_id).ok_or(AuthError::Denied)?;
                let consent:Option<(String,Option<i64>,u64)>=connection.query_row("SELECT decision,expires_at,revision FROM auth_remembered_consent WHERE principal_id=?1 AND client_id=?2 AND binding_kind=?3 AND binding_value=?4 AND capability_id=?5 AND capability_generation=?6 AND consent_revision=?7",params![principal,client,kind,value,cap.capability_id,cap.identity_generation.get(),cap.consent_revision.get()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
                let expires = if let Some((decision, expires, _)) =
                    consent.filter(|(_, _, revision)| *revision > approval.revision)
                {
                    if decision != "approved" {
                        continue;
                    }
                    expires
                } else {
                    approval.expires_at
                };
                if expires.is_some_and(|expiry| expiry <= now) {
                    continue;
                }
                if let Some(expires) = expires {
                    consent_deadlines.insert(cap.capability_id.clone(), expires);
                }
                retained.push(cap);
            }
            let requested: Vec<PlatformPrivilege> = decode(&requested)?;
            let mut privileges = Vec::new();
            let eligibility:Option<String>=connection.query_row("SELECT requested_privileges_json FROM auth_oauth_clients WHERE client_id=?1 AND state='active'",[&client],|row|row.get(0)).optional()?;
            let delegation:Option<(String,Option<i64>)>=connection.query_row("SELECT approved_privileges_json,expires_at FROM auth_platform_delegations WHERE principal_id=?1 AND client_id=?2 AND binding_kind=?3 AND binding_value=?4 AND (expires_at IS NULL OR expires_at>?5)",params![principal,client,kind,value,now],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            let mut platform_privilege_deadline = None;
            if let (Some(eligibility), Some((delegation, expires))) = (eligibility, delegation) {
                let eligibility: Vec<PlatformPrivilege> = decode(&eligibility)?;
                let delegation: Vec<PlatformPrivilege> = decode(&delegation)?;
                for privilege in requested {
                    let assigned:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_platform_privileges WHERE principal_id=?1 AND privilege=?2)",params![principal,serde_json::to_value(privilege)?.as_str()],|row|row.get(0))?;
                    if assigned
                        && eligibility.contains(&privilege)
                        && delegation.contains(&privilege)
                    {
                        privileges.push(privilege);
                    }
                }
                if !privileges.is_empty() {
                    platform_privilege_deadline = expires;
                }
            }
            privileges
                .sort_by_key(|privilege| serde_json::to_string(privilege).unwrap_or_default());
            privileges.dedup();
            let required_privileges: String = connection.query_row(
                "SELECT required_privileges_json FROM auth_oauth_grants WHERE oauth_grant_id=?1",
                [grant],
                |row| row.get(0),
            )?;
            let required_privileges: Vec<PlatformPrivilege> = decode(&required_privileges)?;
            if !required_privileges
                .iter()
                .all(|privilege| privileges.contains(privilege))
            {
                return Err(AuthError::RequiredMissing);
            }
            let binding = match kind.as_str() {
                "browser" => SessionBinding::Browser {
                    client_id: client,
                    origin: value,
                },
                "native" if value == jkt => SessionBinding::Native {
                    client_id: client,
                    durable_dpop_jkt: jkt,
                },
                _ => return Err(AuthError::Denied),
            };
            Ok(CredentialSnapshot {
                principal_id: principal,
                principal_kind: PrincipalKind::User,
                binding,
                login_session_id: Some(login),
                selection: CapabilitySelection {
                    required: decode(&required)?,
                    optional: decode(&optional)?,
                },
                approved: Some(retained),
                consent_deadlines,
                providers: vec![],
                resource_bindings_digest: None,
                platform_privileges: privileges,
                platform_privilege_deadline,
                not_after: deadline,
            })
        }
        Credential::ProvisionedIdentity(identity) => {
            let row:Option<(String,String,String,String,String,Option<i64>)>=connection.query_row(
                  "SELECT i.principal_id,i.deployment_id,i.instance_id,i.participant_id,i.kind,d.expires_at FROM auth_provisioned_identities i JOIN auth_principals p ON p.principal_id=i.principal_id JOIN auth_deployments d ON d.deployment_id=i.deployment_id AND d.participant_id=i.participant_id JOIN auth_instances r ON r.instance_id=i.instance_id AND r.deployment_id=i.deployment_id AND r.principal_id=i.principal_id JOIN auth_deployment_bindings b ON b.deployment_id=i.deployment_id WHERE i.identity_key_id=?1 AND i.state='active' AND p.state='active' AND p.kind=i.kind AND d.state='active' AND r.state='active' AND (d.expires_at IS NULL OR d.expires_at>?2)",params![identity,now],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).optional()?;
            let (principal, deployment, instance, participant, kind, deadline) =
                row.ok_or(AuthError::Denied)?;
            let projection =
                super::native_projection::project_instance(connection, &instance, now)?;
            let (principal_kind, binding) = match kind.as_str() {
                "service" => (
                    PrincipalKind::Service,
                    SessionBinding::Service {
                        deployment_id: deployment.clone(),
                        instance_id: instance,
                        participant_id: participant,
                    },
                ),
                "device" => {
                    let active:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_devices WHERE principal_id=?1 AND deployment_id=?2 AND state='active')",params![principal,deployment],|row|row.get(0))?;
                    if !active {
                        return Err(AuthError::Denied);
                    }
                    (
                        PrincipalKind::Device,
                        SessionBinding::Device {
                            deployment_id: deployment.clone(),
                            instance_id: instance,
                            participant_id: participant,
                        },
                    )
                }
                _ => return Err(AuthError::Denied),
            };
            let provided = projection.provided;
            let providers = provided
                .into_iter()
                .map(|provider| AuthorityProvider {
                    api_id: provider.api_id,
                    generation: U64s::new(provider.generation),
                    deployment_id: deployment.clone(),
                    implementation_digest: provider.implementation_digest,
                    provider_certificate_digest: String::new(),
                })
                .collect();
            let resources = projection.resources;
            super::deployments::native_health_capability(connection)?;
            let mut required = projection.required;
            if principal_kind == PrincipalKind::Device
                && resources
                    .get("state")
                    .and_then(Value::as_object)
                    .is_some_and(|state| !state.is_empty())
            {
                for capability in [
                    "trellis.state@v1::read",
                    "trellis.state@v1::write",
                    "trellis.state@v1::delete",
                ] {
                    if !required.iter().any(|id| id == capability) {
                        required.push(capability.into());
                    }
                }
            }
            if principal_kind == PrincipalKind::Service
                && resources
                    .get("jobs")
                    .and_then(|jobs| jobs.get("queues"))
                    .and_then(Value::as_object)
                    .is_some_and(|queues| !queues.is_empty())
                && !required
                    .iter()
                    .any(|id| id == super::deployments::JOB_HISTORY_CAPABILITY)
            {
                required.push(super::deployments::JOB_HISTORY_CAPABILITY.into());
            }
            if principal_kind == PrincipalKind::Service
                && resources
                    .get("eventConsumers")
                    .and_then(Value::as_object)
                    .is_some_and(|consumers| !consumers.is_empty())
                && !required
                    .iter()
                    .any(|id| id == super::deployments::CONSUMER_RUNTIME_CAPABILITY)
            {
                required.push(super::deployments::CONSUMER_RUNTIME_CAPABILITY.into());
            }
            if !required
                .iter()
                .any(|id| id == super::deployments::SELF_HEALTH_CAPABILITY)
            {
                required.push(super::deployments::SELF_HEALTH_CAPABILITY.into());
            }
            Ok(CredentialSnapshot {
                principal_id: principal,
                principal_kind,
                binding,
                login_session_id: None,
                selection: CapabilitySelection {
                    required,
                    optional: projection.optional,
                },
                approved: None,
                consent_deadlines: BTreeMap::new(),
                providers,
                resource_bindings_digest: Some(digest(&resources)?),
                platform_privileges: vec![],
                platform_privilege_deadline: None,
                not_after: deadline.unwrap_or(i64::MAX),
            })
        }
    }
}

pub(super) fn session_credential(
    connection: &Connection,
    session: &str,
) -> Result<Credential, AuthError> {
    let (kind,grant,identity):(String,Option<String>,Option<String>)=connection.query_row("SELECT credential_kind,oauth_grant_id,identity_key_id FROM auth_authorization_sessions WHERE authorization_session_id=?1",[session],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?.ok_or(AuthError::NotFound)?;
    match (kind.as_str(), grant, identity) {
        ("oauthGrant", Some(grant), None) => Ok(Credential::OAuthGrant(grant)),
        ("provisionedIdentity", None, Some(identity)) => {
            Ok(Credential::ProvisionedIdentity(identity))
        }
        _ => Err(AuthError::Denied),
    }
}

/// Compare every still-usable context, not the last issuance alone. A narrower
/// renewal can never hide an earlier live provider/capability/platform right.
pub(super) fn loses_authority(
    connection: &Connection,
    session: &str,
    current: &super::policy::Evaluation,
    now: i64,
) -> Result<bool, AuthError> {
    // Family invalidation is a session-root reduction, not a grant-wide policy
    // change: another family of this same grant cannot keep this session live.
    let family_unavailable:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_authorization_sessions s JOIN auth_oauth_families f ON f.family_lookup_hash=s.refresh_family_lookup_hash AND f.oauth_grant_id=s.oauth_grant_id WHERE s.authorization_session_id=?1 AND (f.revoked_at IS NOT NULL OR f.expires_at<=?2))",params![session,now],|row|row.get(0))?;
    if family_unavailable {
        return Ok(true);
    }
    let mut statement=connection.prepare("SELECT signed_bytes FROM auth_session_authorities WHERE authorization_session_id=?1 AND maximum_acceptance_deadline>?2")?;
    for row in statement.query_map(params![session, now], |row| row.get::<_, Vec<u8>>(0))? {
        let old = trellis_protocol::parse_session_authority(&row?, 1_048_576)?;
        let old = &old.unsigned;
        if old.expires_at > current.root_not_after
            || old.capabilities.iter().any(|capability| {
                !current.capabilities.contains(capability)
                    || current
                        .capability_deadlines
                        .get(&capability.capability_id)
                        .is_some_and(|deadline| old.expires_at > *deadline)
            })
            || (!old.platform_privileges.is_empty()
                && current
                    .platform_privilege_deadline
                    .is_some_and(|deadline| old.expires_at > deadline))
            || old
                .platform_privileges
                .iter()
                .any(|privilege| !current.platform_privileges.contains(privilege))
            || old.apis.iter().any(|api| {
                current
                    .apis
                    .iter()
                    .find(|next| next.api_id == api.api_id)
                    .is_none_or(|next| next.generation != api.generation)
            })
            || old.provider_bindings.iter().any(|provider| {
                !current.providers.iter().any(|next| {
                    next.api_id == provider.api_id
                        && next.generation == provider.generation
                        && next.deployment_id == provider.deployment_id
                        && next.implementation_digest == provider.implementation_digest
                })
            })
            || old.resource_bindings_digest != current.resource_bindings_digest
        {
            return Ok(true);
        }
    }
    Ok(false)
}

impl SqliteAuthorizationStore {
    pub(super) fn issue_authority_in_transaction(
        &self,
        connection: &Connection,
        admission: &AdmissionIdentity,
        renew_session: Option<&str>,
        now: i64,
    ) -> Result<Result<IssuedAuthority, AuthError>, AuthError> {
        self.current_signer(connection)?;
        if let Some(session) = renew_session {
            let exact: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_authorization_sessions WHERE authorization_session_id=?1 AND state='active' AND runtime_id=?2 AND session_public_key=?3)",
                params![session, admission.runtime_id, admission.session_public_key],
                |row| row.get(0),
            )?;
            if !exact {
                return Err(AuthError::Retired);
            }
            if session_credential(connection, session)? != admission.credential {
                return Err(AuthError::Denied);
            }
            let original:Option<String>=connection.query_row("SELECT original_refresh_family_id FROM auth_authorization_sessions WHERE authorization_session_id=?1",[session],|row|row.get(0))?;
            if original != admission.refresh_family_id {
                return Err(AuthError::Denied);
            }
        }
        let family_deadline = match (&admission.credential, &admission.refresh_family_id) {
            (Credential::OAuthGrant(grant), Some(family)) => {
                match self.oauth_family_deadline_in(connection, grant, family, now) {
                    Ok(deadline) => deadline,
                    Err(AuthError::Denied) => {
                        if let Some(session) = renew_session {
                            retire_session(
                                self,
                                connection,
                                session,
                                "oauth_family_unavailable",
                                true,
                                None,
                                now,
                            )?;
                            return Ok(Err(AuthError::Retired));
                        }
                        return Err(AuthError::Denied);
                    }
                    Err(error) => return Err(error),
                }
            }
            (Credential::ProvisionedIdentity(_), None) => i64::MAX,
            _ => return Err(AuthError::Denied),
        };
        let current = match evaluate(connection, &admission.credential, now) {
            Ok(current) => current,
            Err(error @ (AuthError::Denied | AuthError::RequiredMissing | AuthError::NotFound)) => {
                if let Some(session) = renew_session {
                    retire_session(
                        self,
                        connection,
                        session,
                        "credential_unavailable",
                        matches!(&error, AuthError::Denied | AuthError::NotFound),
                        None,
                        now,
                    )?;
                    // The outer transaction must commit the cutoff and ordered
                    // kicks even though renewal returns no replacement authority.
                    return Ok(Err(AuthError::Retired));
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let binding_key = digest(&(&current.principal_id, &current.binding))?;
        let session = if let Some(session) = renew_session {
            let exact:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_authorization_sessions WHERE authorization_session_id=?1 AND state='active' AND binding_key=?2 AND runtime_id=?3 AND session_public_key=?4)",params![session,binding_key,admission.runtime_id,admission.session_public_key],|row|row.get(0))?;
            if !exact {
                return Err(AuthError::Retired);
            }
            Some(session.to_owned())
        } else {
            connection.query_row("SELECT authorization_session_id FROM auth_authorization_sessions WHERE binding_key=?1 AND runtime_id=?2 AND session_public_key=?3 AND state='active'",params![binding_key,admission.runtime_id,admission.session_public_key],|row|row.get(0)).optional()?
        };
        let mut session = session;
        if let Some(existing) = &session {
            if session_credential(connection, existing)? != admission.credential {
                return Err(AuthError::Denied);
            }
            let original:Option<String>=connection.query_row("SELECT original_refresh_family_id FROM auth_authorization_sessions WHERE authorization_session_id=?1",[existing],|row|row.get(0))?;
            if original != admission.refresh_family_id {
                return Err(AuthError::Denied);
            }
            let expired:bool=connection.query_row("SELECT hard_deadline<=?2 FROM auth_authorization_sessions WHERE authorization_session_id=?1",params![existing,now],|row|row.get(0))?;
            if expired
                || !current.required_missing.is_empty()
                || loses_authority(connection, existing, &current, now)?
            {
                retire_session(
                    self,
                    connection,
                    existing,
                    if expired {
                        "authorization_expired"
                    } else {
                        "policy_reduction"
                    },
                    false,
                    None,
                    now,
                )?;
                if renew_session.is_some() {
                    return Ok(Err(AuthError::Retired));
                }
                session = None;
            }
        }
        if !current.required_missing.is_empty() {
            return Ok(Err(AuthError::RequiredMissing));
        }
        let session = session.unwrap_or_else(id);
        let (kind, grant, identity) = match &admission.credential {
            Credential::OAuthGrant(grant) => ("oauthGrant", Some(grant.as_str()), None),
            Credential::ProvisionedIdentity(identity) => {
                ("provisionedIdentity", None, Some(identity.as_str()))
            }
        };
        let existing:Option<i64>=connection.query_row("SELECT hard_deadline FROM auth_authorization_sessions WHERE authorization_session_id=?1",[&session],|row|row.get(0)).optional()?;
        let hard_deadline = current
            .root_not_after
            .min(family_deadline)
            .min(existing.unwrap_or(i64::MAX));
        let expires_at = current
            .not_after
            .min(hard_deadline)
            .min(now.saturating_add(i64::from(self.settings.authority_lifetime_seconds)));
        if expires_at <= now {
            return Err(AuthError::Denied);
        }
        let maximum_acceptance =
            expires_at.saturating_add(i64::from(self.settings.clock_skew_seconds));
        let binding_kind = match &current.binding {
            SessionBinding::Browser { .. } => "browser",
            SessionBinding::Native { .. } => "native",
            SessionBinding::Service { .. } => "service",
            SessionBinding::Device { .. } => "device",
        };
        let session_hard_deadline = if hard_deadline == i64::MAX {
            9_007_199_254_740_991_i64
        } else {
            hard_deadline
        };
        let family_hash = admission
            .refresh_family_id
            .as_ref()
            .map(|family| URL_SAFE_NO_PAD.encode(Sha256::digest(family.as_bytes())));
        connection.execute("INSERT INTO auth_authorization_sessions(authorization_session_id,principal_id,binding_kind,binding_key,binding_json,runtime_id,session_public_key,credential_kind,oauth_grant_id,identity_key_id,selected_capabilities_json,approved_capabilities_json,approved_privileges_json,issued_at,hard_deadline,latest_context_expiry,original_refresh_family_id,refresh_family_lookup_hash,state,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,'active',1) ON CONFLICT(authorization_session_id) DO UPDATE SET hard_deadline=min(hard_deadline,excluded.hard_deadline),latest_context_expiry=max(latest_context_expiry,excluded.latest_context_expiry),approved_capabilities_json=excluded.approved_capabilities_json,approved_privileges_json=excluded.approved_privileges_json,revision=revision+1",params![session,current.principal_id,binding_kind,binding_key,json(&current.binding)?,admission.runtime_id,admission.session_public_key,kind,grant,identity,json(&current.selection)?,json(&current.capabilities)?,json(&current.platform_privileges)?,now,session_hard_deadline,expires_at,admission.refresh_family_id,family_hash])?;
        let mut providers = current.providers.clone();
        if let SessionBinding::Service {
            deployment_id,
            instance_id,
            ..
        }
        | SessionBinding::Device {
            deployment_id,
            instance_id,
            ..
        } = &current.binding
        {
            let provided =
                super::native_projection::project_instance(connection, instance_id, now)?.provided;
            for provider in &mut providers {
                let implementation = provided
                    .iter()
                    .find(|implementation| {
                        implementation.api_id == provider.api_id
                            && implementation.generation == provider.generation.get()
                    })
                    .ok_or(AuthError::Denied)?;
                let catalog = self.catalog_in_transaction(connection, &provider.api_id)?;
                if implementation.implemented_actions.iter().any(|action| {
                    !catalog
                        .actions
                        .iter()
                        .any(|entry| &entry.identity == action)
                }) {
                    return Err(AuthError::Denied);
                }
                let certificate = SignedProviderCertificate {
                    format: PROVIDER_CERTIFICATE_FORMAT_V1.into(),
                    issuer_key_id: self.current_issuer(connection)?.key_id,
                    trellis_instance_id: self.settings.instance_id.clone(),
                    audience_nats_account: self.settings.account.clone(),
                    principal_id: current.principal_id.clone(),
                    deployment_id: deployment_id.clone(),
                    instance_id: instance_id.clone(),
                    session_public_key: admission.session_public_key.clone(),
                    api_id: provider.api_id.clone(),
                    generation: provider.generation,
                    implementation_digest: provider.implementation_digest.clone(),
                    implemented_actions: implementation.implemented_actions.clone(),
                    issued_at: now,
                    not_before: now,
                    expires_at,
                    extensions: Default::default(),
                    critical: vec![],
                    signature: String::new(),
                }
                .sign(&self.current_signer(connection)?)?;
                provider.provider_certificate_digest = certificate.digest()?;
                connection.execute("INSERT OR IGNORE INTO auth_provider_certificates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![certificate.digest()?,deployment_id,instance_id,current.principal_id,provider.api_id,provider.generation.get(),provider.implementation_digest,self.current_issuer(connection)?.key_id,json(&certificate.implemented_actions)?,json(&certificate)?.as_bytes(),now,expires_at])?;
                enqueue_effect(
                    connection,
                    "authority_publish",
                    &serde_json::json!({"kind":"provider","digest":certificate.digest()?}),
                    None,
                    now,
                )?;
            }
        }
        let mut extensions = serde_json::Map::new();
        if let SessionBinding::Service {
            deployment_id,
            instance_id,
            participant_id,
        }
        | SessionBinding::Device {
            deployment_id,
            instance_id,
            participant_id,
        } = &current.binding
        {
            let (revision, participant_digest): (u64,String) = connection.query_row("SELECT i.installed_revision,p.participant_digest FROM auth_instances i JOIN auth_installed_participants p ON p.participant_id=?3 AND p.revision=i.installed_revision WHERE i.instance_id=?1 AND i.deployment_id=?2 AND i.principal_id=?4",params![instance_id,deployment_id,participant_id,current.principal_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
            extensions.insert("trellis.nativeParticipant.v1".into(), serde_json::json!({"participantId":participant_id,"installedRevision":revision.to_string(),"participantDigest":participant_digest}));
        }
        let authority = sign_session_authority(
            UnsignedSessionAuthority {
                format: SESSION_AUTHORITY_FORMAT_V1.into(),
                issuer_key_id: self.current_issuer(connection)?.key_id,
                trellis_instance_id: self.settings.instance_id.clone(),
                audience_nats_account: self.settings.account.clone(),
                principal_id: current.principal_id.clone(),
                principal_kind: current.principal_kind,
                binding: current.binding,
                authorization_session_id: session.clone(),
                runtime_id: admission.runtime_id.clone(),
                session_public_key: admission.session_public_key.clone(),
                inbox_prefix: admission.inbox_prefix.clone(),
                login_session_id: current.login_session_id,
                oauth_grant_id: grant.map(str::to_owned),
                capabilities: current.capabilities,
                apis: current.apis,
                provider_bindings: providers,
                resource_bindings_digest: current.resource_bindings_digest,
                platform_privileges: current.platform_privileges,
                issued_at: now,
                not_before: now,
                expires_at,
                extensions,
                critical: vec![],
            },
            &self.current_signer(connection)?,
        )?;
        let digest = authority.digest()?;
        // Validate the actual signed first-response envelope before committing
        // either the authority or the attachment that can make it usable. Only
        // the fresh nonce and reply suffix are unknown here; reserve their full
        // supported lengths, rather than copying catalog history into bootstrap.
        trellis_protocol::sign_admission_context(
            trellis_protocol::AdmissionContext {
                format: trellis_protocol::ADMISSION_GET_DOMAIN_V1.into(),
                issuer_key_id: self.current_issuer(connection)?.key_id,
                trellis_instance_id: self.settings.instance_id.clone(),
                audience_nats_account: self.settings.account.clone(),
                runtime_id: admission.runtime_id.clone(),
                request_nonce: URL_SAFE_NO_PAD.encode([0u8; 16]),
                request_subject: trellis_protocol::admission_get_subject(
                    &admission.session_public_key,
                )?,
                reply_subject: format!(
                    "{}.{:A<width$}",
                    admission.inbox_prefix,
                    "",
                    width = 511usize.saturating_sub(admission.inbox_prefix.len())
                ),
                session_authority: json(&authority)?,
                registry: self.admission_registry(connection)?,
                issued_at: now,
                expires_at: expires_at.min(now.saturating_add(30)),
            },
            &self.current_signer(connection)?,
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO auth_session_authorities VALUES(?1,?2,?3,?4,?5,?6,?6,?7,?8)",
            params![
                digest,
                session,
                self.current_issuer(connection)?.key_id,
                json(&authority)?.as_bytes(),
                json(&authority.unsigned.apis)?,
                now,
                expires_at,
                maximum_acceptance
            ],
        )?;
        connection.execute("UPDATE auth_authorization_issuers SET maximum_acceptance_deadline=max(maximum_acceptance_deadline,?1) WHERE key_id=?2",params![maximum_acceptance,self.current_issuer(connection)?.key_id])?;
        enqueue_effect(
            connection,
            "authority_publish",
            &serde_json::json!({"kind":"authority","digest":digest}),
            None,
            now,
        )?;
        Ok(Ok(IssuedAuthority { authority, digest }))
    }

    pub(super) fn catalog_in_transaction(
        &self,
        connection: &Connection,
        api: &str,
    ) -> Result<trellis_protocol::SignedCatalogSnapshot, AuthError> {
        let bytes:Vec<u8>=connection.query_row("SELECT s.signed_bytes FROM auth_accepted_apis a JOIN auth_api_verification_snapshots s ON s.api_id=a.api_id AND s.generation=a.generation AND s.accepted_revision=a.accepted_revision WHERE a.api_id=?1",[api],|row|row.get(0))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Read the persisted logical lifecycle and its immutable retirement evidence
    /// in one snapshot for the ordinary typed Authority.Status RPC.
    pub(crate) fn authorization_session_status(
        &self,
        session: trellis_rs::generated::Ulid,
    ) -> Result<trellis_runtime_apis::types::AuthAuthorityStatusResponse, AuthError> {
        self.transaction(|connection| Self::authorization_session_status_in(connection, session))
    }

    pub(super) fn authorization_session_status_in(
        connection: &Connection,
        session: trellis_rs::generated::Ulid,
    ) -> Result<trellis_runtime_apis::types::AuthAuthorityStatusResponse, AuthError> {
        use trellis_runtime_apis::types::{AuthAuthorityStatusResponse, AuthSignedBlob};
        let (state,latest,cutoff,reason,revocation):(String,i64,Option<i64>,Option<String>,Option<Vec<u8>>)=connection.query_row(
                "SELECT s.state,s.latest_context_expiry,r.effective_cutoff,r.reason,r.signed_bytes FROM auth_authorization_sessions s LEFT JOIN auth_authorization_revocations r USING(authorization_session_id) WHERE s.authorization_session_id=?1",
                [session.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?.ok_or(AuthError::NotFound)?;
        let mut timestamps = Vec::new();
        for value in [Some(latest), cutoff].into_iter().flatten() {
            let value = time::OffsetDateTime::from_unix_timestamp(value)
                .map_err(|_| AuthError::Unavailable)?
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| AuthError::Unavailable)?;
            timestamps.push(
                trellis_rs::generated::Timestamp::parse(&value)
                    .map_err(|_| AuthError::Unavailable)?,
            );
        }
        Ok(AuthAuthorityStatusResponse {
            authorization_session_id: session,
            state: serde_json::from_value(serde_json::Value::String(state))?,
            maximum_context_expires_at: Some(timestamps.remove(0)),
            cutoff: timestamps.pop(),
            reason: match reason.as_deref() {
                Some("session_revoked") => {
                    Some(trellis_runtime_apis::types::AuthRevocationReason::ExplicitRevoke)
                }
                Some("policy_reduction" | "admission_reduction") => {
                    Some(trellis_runtime_apis::types::AuthRevocationReason::PolicyReduced)
                }
                // Preserve the exact internal classification in signed evidence;
                // do not invent a narrower public reason for a generic cutoff.
                _ => None,
            },
            revocation: revocation.map(|bytes| AuthSignedBlob(bytes.into())),
            enforcement: None,
            extra: Default::default(),
        })
    }

    /// Immutable verification material is resolved by content identity. Current
    /// catalog selection remains separate; history is never a routing fallback.
    pub(crate) fn resolve_verification_material(
        &self,
        kind: &str,
        key: &str,
    ) -> Result<Vec<u8>, AuthError> {
        let query = match kind {
            "authority" => {
                "SELECT signed_bytes FROM auth_session_authorities WHERE context_digest=?1"
            }
            "catalog" => {
                "SELECT signed_bytes FROM auth_api_verification_snapshots WHERE snapshot_digest=?1"
            }
            "provider" => {
                "SELECT signed_bytes FROM auth_provider_certificates WHERE certificate_digest=?1"
            }
            "rotation" => "SELECT signed_bytes FROM auth_issuer_rotations WHERE sequence=?1",
            _ => return Err(AuthError::Invalid("unknown verification material".into())),
        };
        self.transaction(|connection| {
            connection
                .query_row(query, [key], |row| row.get(0))
                .optional()?
                .ok_or(AuthError::NotFound)
        })
    }

    /// Persist successor custody before atomically advancing issuer trust.
    pub(crate) fn rotate_issuer(
        &self,
        identity: &Mutation,
        input: &trellis_runtime_apis::types::AuthIssuerRotateRequest,
        now: i64,
    ) -> Result<SignedIssuerRotation, AuthError> {
        // Authorization applies to replays too; an old request is not a privilege.
        self.transaction(|connection| {
            Self::require_privilege(connection, &identity.actor, "privileges.manage")?;
            self.mutate_in_transaction(
                connection,
                identity,
                "issuer.rotate",
                input,
                now,
                |connection| {
                    let signer = self.current_signer(connection)?;
                    let previous_key_id = self.current_issuer(connection)?.key_id;
                    if previous_key_id != input.expected_current_key_id.as_ref() {
                        return Err(AuthError::Conflict);
                    }
                    let mut seed = [0u8; 32];
                    getrandom::fill(&mut seed).map_err(|_| AuthError::Unavailable)?;
                    let next = SigningKey::from_bytes(&seed);
                    let next_public_key = URL_SAFE_NO_PAD.encode(next.verifying_key().as_bytes());
                    let next_key_id = URL_SAFE_NO_PAD.encode(Sha256::digest(next.verifying_key().as_bytes()));
                    self.persist_successor(&next_key_id, &seed)?;
                    let sequence: u64 = connection.query_row(
                        "SELECT coalesce(max(sequence),0)+1 FROM auth_issuer_rotations",
                        [],
                        |row| row.get(0),
                    )?;
                    let rotation = SignedIssuerRotation {
                        format: ISSUER_ROTATION_FORMAT_V1.into(),
                        trellis_instance_id: self.settings.instance_id.clone(),
                        audience_nats_account: self.settings.account.clone(),
                        sequence: U64s::new(sequence),
                        previous_key_id,
                        next_key_id: next_key_id.clone(),
                        next_public_key: next_public_key.clone(),
                        activated_at: now,
                        previous_retired_at: now,
                        extensions: Default::default(),
                        critical: vec![],
                        signature: String::new(),
                    }.sign(&signer)?;
                    connection.execute("UPDATE auth_authorization_issuers SET is_current=0,state='retired',retired_at=?1 WHERE is_current=1", [now])?;
                    connection.execute("INSERT INTO auth_authorization_issuers(key_id,public_key,is_current,state,activated_at,maximum_acceptance_deadline,created_at) VALUES(?1,?2,1,'active',?3,?3,?3)", params![next_key_id, next_public_key, now])?;
                    connection.execute("INSERT INTO auth_issuer_rotations VALUES(?1,?2,?3,?4,?5,?6,?7)", params![sequence, rotation.previous_key_id, rotation.next_key_id, self.settings.instance_id, self.settings.account, json(&rotation)?.as_bytes(), now])?;
                    enqueue_effect(
                        connection,
                        "authority_publish",
                        &serde_json::json!({"kind": "rotation", "sequence": sequence}),
                        None,
                        now,
                    )?;
                    Ok(rotation)
                },
            )
        })
    }

    pub(super) fn resolve_issuer_in(
        connection: &rusqlite::Connection,
        key_id: &str,
    ) -> Result<AuthorityIssuerKey, AuthError> {
        let (public, state): (String, String) = connection
            .query_row(
                "SELECT public_key,state FROM auth_authorization_issuers WHERE key_id=?1",
                [key_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(AuthError::NotFound)?;
        Ok(AuthorityIssuerKey {
            key_id: key_id.into(),
            public_key: public,
            state: match state.as_str() {
                "active" => AuthorityIssuerState::Active,
                "retired" => AuthorityIssuerState::Retired,
                _ => AuthorityIssuerState::Revoked,
            },
        })
    }
}
