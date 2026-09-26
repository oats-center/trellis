use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::Digest as _;
use trellis_protocol::{
    GrantSet, ParticipantKind, ParticipantResourceKind, PermissionAction, PermissionTarget,
    PlatformPrivilege,
};
use trellis_runtime_apis::apis::trellis_auth_v1::events::GrantsChanged;
use trellis_runtime_apis::types::{AuthGrantsListRequestOwnerKind, AuthGrantsListRequestState};

use super::super::application::repository::{ActivationReviewClaim, ActivationReviewDecision};
use super::super::authority::{IssuanceConnection, IssuanceCredential};
use super::super::compiled_evidence::{CompiledInstalledEvidence, SemanticJobError};
use super::super::context::{
    load_sql_context_by_digest, revoke_sql_contexts, revoke_sql_contexts_matching,
    AuthorizationContextRecord, AuthorizationContextRevocationReason, AuthorizationContextSelector,
    AuthorizationContextState,
};
use super::super::domain::{require_protocol_timestamp, ApprovedResource, GrantBindingReplacement};
use super::super::evidence::{
    participant_connection_surface_is_covered, ParticipantRuntimeProjection,
};
use super::super::{
    auth_event_subject, AuthorizationStateError, DeviceActivationReviewState,
    DeviceDelegationRecord, DeviceDelegationState, DeviceState, GrantBinding, GrantBindingState,
    GrantOwnerKind, IdempotencyResultRecord, MutationActor, ParticipantBindingRecord,
    PostCommitActionKind, PostCommitActionRecord, SessionRecord,
};
use crate::telemetry::{record_duration, DurationMetric, Outcome};

pub(super) fn require_current_actor(
    connection: &Connection,
    actor: &MutationActor,
    require_admin: bool,
    now: i64,
) -> Result<(), AuthorizationStateError> {
    require_protocol_timestamp("now", now)?;
    let context = load_sql_context_by_digest(connection, &actor.context_digest)?
        .ok_or(AuthorizationStateError::NotAuthorized)?;
    let now_seconds = now.div_euclid(1_000);
    if context.state != AuthorizationContextState::Active
        || context.revoked_at.is_some()
        || context.not_before > now_seconds
        || context.expires_at <= now_seconds
        || context.principal_id != actor.principal_id
        || context.participant_id != actor.participant_id
        || context.owner_kind != actor.owner_kind
        || context.owner_id != actor.owner_id
        || context.grant_revision != actor.grant_revision
        || context.login_session_id != actor.login_session_id
        || context.session_public_key != actor.session_public_key
    {
        return Err(AuthorizationStateError::NotAuthorized);
    }
    let credential = match (&context.login_session_id, &context.identity_key_id) {
        (Some(login_session_id), None) => IssuanceCredential::Login(login_session_id.clone()),
        (None, Some(identity_key_id)) => IssuanceCredential::Native(identity_key_id.clone()),
        _ => return Err(AuthorizationStateError::NotAuthorized),
    };
    let mut snapshot = super::contexts::sqlite_issuance_snapshot(
        connection,
        &IssuanceConnection {
            credential,
            connection_id: context.connection_id.clone(),
            session_public_key: context.session_public_key.clone(),
        },
    )?;
    snapshot.issuer = super::contexts::load_eligible_authorization_issuer(
        connection,
        &context.issuer_key_id,
        now,
    )?;
    let current = super::super::issuance::resolve_snapshot(snapshot, now)?;
    if current.principal_id != context.principal_id
        || current.binding.owner_kind != context.owner_kind
        || current.binding.owner_id != context.owner_id
        || current.participant.participant_id != context.participant_id
        || current.binding.revision != context.grant_revision
        || current.binding.installed_revision != context.installed_revision
        || current.session_public_key != context.session_public_key
        || current.login_session_id != context.login_session_id
    {
        return Err(AuthorizationStateError::NotAuthorized);
    }
    if require_admin
        && !current
            .binding
            .platform_privileges
            .contains(&PlatformPrivilege::Admin)
    {
        return Err(AuthorizationStateError::NotAuthorized);
    }
    Ok(())
}
use super::common::{
    decode_enum, decode_json, encode_enum, encode_json, map_write_error, sql_error, to_sql_version,
};
use super::deployments::{load_deployment_profile, upsert_deployment_profile_evidence};
use super::evidence::load_deployment;
use super::outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay};
use super::principals::load_principal;
use super::validation::next_version;
use super::SqliteAuthorizationStore;

enum CompanionActivationReviewMutation {
    Claim(ActivationReviewClaim),
    Decision(ActivationReviewDecision),
}

pub(in crate::platform::auth) fn load_grant_binding(
    connection: &Connection,
    owner_kind: GrantOwnerKind,
    owner_id: &str,
    participant_id: &str,
) -> Result<Option<GrantBinding>, AuthorizationStateError> {
    let binding = connection
        .query_row(
            "SELECT owner_kind, owner_id, participant_id, installed_revision, grants_json,
                approval_mode, approved_capabilities_json, approved_resources_json,
                delegation_ceiling_json, approval_decision_digest,
                approval_expected_grant_revision, companion_approved,
                platform_privileges_json, revision, state, expires_at,
                provenance_json, created_at, updated_at
         FROM auth_grant_bindings WHERE owner_kind = ?1 AND owner_id = ?2 AND participant_id = ?3",
            params![encode_enum(owner_kind)?, owner_id, participant_id],
            |row| {
                Ok(GrantBinding {
                    owner_kind: decode_enum(row.get::<_, String>(0)?)?,
                    owner_id: row.get(1)?,
                    participant_id: row.get(2)?,
                    installed_revision: row.get(3)?,
                    grants: decode_json(row.get::<_, String>(4)?)?,
                    approval_mode: decode_enum(row.get::<_, String>(5)?)?,
                    approved_capabilities: decode_json(row.get::<_, String>(6)?)?,
                    approved_resources: decode_json(row.get::<_, String>(7)?)?,
                    delegation_ceiling: decode_json(row.get::<_, String>(8)?)?,
                    approval_decision_digest: row.get(9)?,
                    approval_expected_grant_revision: row.get(10)?,
                    companion_approved: row.get(11)?,
                    platform_privileges: decode_json(row.get::<_, String>(12)?)?,
                    revision: row.get(13)?,
                    state: decode_enum(row.get::<_, String>(14)?)?,
                    expires_at: row.get(15)?,
                    provenance: row
                        .get::<_, Option<String>>(16)?
                        .map(decode_json)
                        .transpose()?,
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                })
            },
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(binding) = &binding {
        let mut canonical = binding.clone();
        canonical.validate()?;
        if canonical != *binding {
            return Err(AuthorizationStateError::InvalidRecord(
                "stored grant binding is not canonical".to_owned(),
            ));
        }
    }
    Ok(binding)
}

pub(in crate::platform::auth) fn install_participant(
    connection: &Connection,
    binding: &ParticipantBindingRecord,
    expected_revision: Option<u64>,
) -> Result<u64, AuthorizationStateError> {
    if expected_revision.is_some_and(|revision| revision > super::super::MAX_PROTOCOL_INTEGER) {
        return Err(AuthorizationStateError::InvalidRecord(
            "expectedRevision exceeds safe integer range".to_owned(),
        ));
    }
    let platform_trusted = connection
        .query_row(
            "SELECT platform_trusted FROM auth_package_evidence WHERE package_digest = ?1",
            [&binding.package_digest],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(sql_error)?
        .unwrap_or(false);
    super::super::builtins::validate_binding_namespace(binding, platform_trusted)?;
    binding.resolve()?;
    if let Some((_, installed)) =
        load_installed_participant(connection, &binding.participant_id, None)?
    {
        if installed.participant_kind != binding.participant_kind {
            return Err(AuthorizationStateError::InvalidRecord(
                "installed participant kind cannot change".to_owned(),
            ));
        }
    }
    let current = connection
        .query_row(
            "SELECT revision FROM auth_installed_participants
         WHERE participant_id = ?1 ORDER BY revision DESC LIMIT 1",
            [&binding.participant_id],
            |row| row.get::<_, u64>(0),
        )
        .optional()
        .map_err(sql_error)?;
    let revision = current.unwrap_or(0);
    if let Some(expected) = expected_revision {
        if expected != revision {
            return Err(AuthorizationStateError::RevisionConflict {
                expected,
                current: revision,
            });
        }
    }
    let identical = connection
        .query_row(
            "SELECT revision FROM auth_installed_participants
               WHERE participant_id = ?1 AND participant_kind = ?2 AND participant_digest = ?3
                AND needs_digest = ?4 AND package_digest = ?5 AND evidence_digest = ?6 AND participant_path = ?7
                  AND companion_participant_id IS ?8 AND companion_participant_kind IS ?9
                  AND companion_required = ?10 AND projection_json = ?11
              ORDER BY revision DESC LIMIT 1",
            params![
                binding.participant_id,
                encode_enum(binding.participant_kind)?,
                binding.participant_digest,
                binding.needs_digest,
                binding.package_digest,
                binding.evidence_digest,
                binding.participant_path,
                binding.projection.companion_participant_id,
                binding
                    .projection
                    .companion_participant_kind
                    .map(encode_enum)
                    .transpose()?,
                binding.projection.companion_required,
                encode_json(&binding.projection)?
            ],
            |row| row.get::<_, u64>(0),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(revision) = identical {
        return Ok(revision);
    }

    let revision = revision
        .checked_add(1)
        .filter(|revision| *revision <= super::super::MAX_PROTOCOL_INTEGER)
        .ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("installed revision overflow".to_owned())
        })?;
    connection
        .execute(
            "INSERT INTO auth_installed_participants
                (participant_id, revision, participant_kind, participant_digest, needs_digest,
                 installed_at, package_digest, participant_path, companion_participant_id,
                  evidence_digest, companion_participant_kind, companion_required, projection_json)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                binding.participant_id,
                revision,
                encode_enum(binding.participant_kind)?,
                binding.participant_digest,
                binding.needs_digest,
                binding.resolved_at,
                binding.package_digest,
                binding.participant_path,
                binding.projection.companion_participant_id,
                binding.evidence_digest,
                binding
                    .projection
                    .companion_participant_kind
                    .map(encode_enum)
                    .transpose()?,
                binding.projection.companion_required,
                encode_json(&binding.projection)?
            ],
        )
        .map_err(map_write_error)?;
    Ok(revision)
}

pub(in crate::platform::auth) fn load_installed_participant(
    connection: &Connection,
    participant_id: &str,
    revision: Option<u64>,
) -> Result<Option<(u64, ParticipantBindingRecord)>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT revision, participant_id, participant_kind, participant_digest, needs_digest,
                installed_at, package_digest, participant_path, companion_participant_id,
                evidence_digest, companion_participant_kind, companion_required, projection_json
         FROM auth_installed_participants WHERE participant_id = ?1
           AND (?2 IS NULL OR revision = ?2) ORDER BY revision DESC LIMIT 1",
            params![participant_id, revision],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, bool>(11)?,
                    row.get::<_, String>(12)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .map(
            |(
                revision,
                participant_id,
                participant_kind,
                participant_digest,
                needs_digest,
                resolved_at,
                package_digest,
                participant_path,
                companion_participant_id,
                evidence_digest,
                companion_participant_kind,
                companion_required,
                projection_json,
            )| {
                let projection: super::super::evidence::ParticipantRuntimeProjection =
                    decode_json(projection_json).map_err(sql_error)?;
                if projection.companion_participant_id != companion_participant_id
                    || projection.companion_participant_kind
                        != companion_participant_kind
                            .map(|kind| decode_enum(kind).map_err(sql_error))
                            .transpose()?
                    || projection.companion_required != companion_required
                {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "stored companion projection does not match installed columns".to_owned(),
                    ));
                }
                let binding = ParticipantBindingRecord {
                    participant_id,
                    participant_kind: decode_enum(participant_kind).map_err(sql_error)?,
                    participant_digest,
                    needs_digest,
                    package_digest,
                    evidence_digest,
                    participant_path,
                    projection,
                    resolved_at,
                    state: super::super::ParticipantBindingState::Resolved,
                    error: None,
                };
                binding.resolve()?;
                Ok((revision, binding))
            },
        )
        .transpose()
}

pub(in crate::platform::auth) fn accept_package_evidence(
    connection: &Connection,
    package_digest: &str,
    root_package: &str,
    evidence_json: &str,
    platform_trust: bool,
    actor: Option<&MutationActor>,
    now: i64,
) -> Result<String, AuthorizationStateError> {
    if platform_trust && root_package != "trellis" {
        return Err(AuthorizationStateError::InvalidRecord(
            "platformTrust is only valid for the reserved Trellis package".to_owned(),
        ));
    }
    let internal_trust = platform_trust
        && actor.is_none()
        && super::super::builtins::is_trusted_package_evidence(package_digest, evidence_json);
    let trust_actor = if platform_trust && !internal_trust {
        let actor = actor.ok_or(AuthorizationStateError::NotAuthorized)?;
        require_current_actor(connection, actor, true, now)?;
        Some(actor)
    } else {
        None
    };
    let current = connection
        .query_row(
            "SELECT platform_trusted FROM auth_package_evidence WHERE package_digest = ?1",
            [package_digest],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(trusted) = current {
        if platform_trust && !trusted {
            let trusted_by =
                trust_actor.map_or("trellis.runtime", |actor| actor.principal_id.as_str());
            connection
                .execute(
                    "UPDATE auth_package_evidence SET platform_trusted = 1, trusted_at = ?2, trusted_by = ?3 WHERE package_digest = ?1",
                    params![package_digest, now, trusted_by],
                )
                .map_err(map_write_error)?;
        }
    } else {
        if root_package == "trellis" && !platform_trust {
            return Err(AuthorizationStateError::NotAuthorized);
        }
        connection
            .execute(
                "INSERT INTO auth_package_evidence (package_digest, platform_trusted, accepted_at, trusted_at, trusted_by) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    package_digest,
                    platform_trust,
                    now,
                    platform_trust.then_some(now),
                    platform_trust.then(|| actor.map_or_else(|| "trellis.runtime".to_owned(), |value| value.principal_id.clone())),
                ],
            )
            .map_err(map_write_error)?;
    }
    let evidence_digest = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        sha2::Sha256::digest(evidence_json.as_bytes()),
    );
    let stored = connection
        .query_row(
            "SELECT package_digest, evidence_json FROM auth_package_evidence_documents WHERE evidence_digest = ?1",
            [&evidence_digest],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some((stored_package_digest, stored_json)) = stored {
        if stored_package_digest != package_digest
            || stored_json.as_bytes() != evidence_json.as_bytes()
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "package evidence document disagrees with its immutable digest".to_owned(),
            ));
        }
    } else {
        connection.execute(
            "INSERT INTO auth_package_evidence_documents (evidence_digest, package_digest, evidence_json, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![evidence_digest, package_digest, evidence_json, now],
        ).map_err(map_write_error)?;
    }
    if root_package == "trellis" {
        let trusted = connection
            .query_row(
                "SELECT platform_trusted FROM auth_package_evidence WHERE package_digest = ?1",
                [package_digest],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if !trusted {
            return Err(AuthorizationStateError::NotAuthorized);
        }
    }
    Ok(evidence_digest)
}

/// Whether every ability a retained signed context already possesses is still
/// permitted by the replacement binding and participant installation.
///
/// A connection keeps its physical attachment across a deployment or contract
/// update while its exact effective authority stays covered. Installed revision
/// identity, participant/package/API digests, consent metadata, and added
/// authority are provenance, not revocation conditions: they never invalidate a
/// context the replacement still covers.
fn context_authority_is_covered(
    context: &AuthorizationContextRecord,
    binding: &GrantBinding,
    previous: &ParticipantRuntimeProjection,
    replacement: &ParticipantRuntimeProjection,
) -> Result<bool, AuthorizationStateError> {
    let signed = context.signed_context()?.unsigned;
    if !grant_set_is_covered(binding, replacement, &signed.grants)? {
        return Ok(false);
    }
    if !signed
        .platform_privileges
        .iter()
        .all(|privilege| binding.platform_privileges.contains(privilege))
    {
        return Ok(false);
    }
    if !binding_expiry_covers(binding.expires_at, context.expires_at) {
        return Ok(false);
    }
    participant_connection_surface_is_covered(previous, replacement, &signed.grants)
}

/// Whether the replacement's effective issued authority covers `held`.
///
/// The persisted binding grants are not the whole issued authority: issuance
/// derives event-publish grants for services from their implemented APIs. Both
/// sources must be considered so a service context is not revoked merely because
/// its provider-derived event grants are absent from the grant columns.
fn grant_set_is_covered(
    binding: &GrantBinding,
    replacement: &ParticipantRuntimeProjection,
    held: &GrantSet,
) -> Result<bool, AuthorizationStateError> {
    let mut allowed = binding.grants.permissions().to_vec();
    if binding.owner_kind == GrantOwnerKind::Deployment
        && replacement.participant_kind == ParticipantKind::Service
    {
        allowed.extend(replacement.provider_event_grants()?);
    }
    Ok(held
        .permissions()
        .iter()
        .all(|permission| allowed.contains(permission)))
}

/// Whether a replacement binding expiry still covers a signed context expiry.
///
/// Grant-binding expiry is milliseconds; authorization-context expiry is seconds.
fn binding_expiry_covers(binding_expires_at: Option<i64>, context_expires_at: i64) -> bool {
    binding_expires_at.is_none_or(|expires_at| expires_at.div_euclid(1_000) >= context_expires_at)
}

pub(in crate::platform::auth) fn replace_grant_binding(
    connection: &Connection,
    replacement: GrantBindingReplacement,
    now: i64,
) -> Result<(GrantBinding, Vec<PostCommitActionRecord>), AuthorizationStateError> {
    if replacement.expected_revision > super::super::MAX_PROTOCOL_INTEGER {
        return Err(AuthorizationStateError::InvalidRecord(
            "expectedRevision exceeds safe integer range".to_owned(),
        ));
    }
    let current = load_grant_binding(
        connection,
        replacement.owner_kind,
        &replacement.owner_id,
        &replacement.participant_id,
    )?;
    let revision = current.as_ref().map_or(0, |binding| binding.revision);
    if revision != replacement.expected_revision {
        return Err(AuthorizationStateError::RevisionConflict {
            expected: replacement.expected_revision,
            current: revision,
        });
    }
    if let Some(expected) = replacement.expected_current_installed_revision {
        let current_installed_revision = connection
            .query_row(
                "SELECT revision FROM auth_installed_participants
                 WHERE participant_id = ?1 ORDER BY revision DESC LIMIT 1",
                [&replacement.participant_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()
            .map_err(sql_error)?
            .unwrap_or(0);
        if current_installed_revision != expected {
            return Err(AuthorizationStateError::RevisionConflict {
                expected,
                current: current_installed_revision,
            });
        }
    }
    let (installed_revision, participant) = load_installed_participant(
        connection,
        &replacement.participant_id,
        Some(replacement.installed_revision),
    )?
    .ok_or(AuthorizationStateError::ParticipantMissing)?;
    let mut binding = GrantBinding {
        owner_kind: replacement.owner_kind,
        owner_id: replacement.owner_id,
        participant_id: replacement.participant_id,
        installed_revision,
        grants: replacement.grants,
        approval_mode: replacement.approval_mode,
        approved_capabilities: replacement.approved_capabilities,
        approved_resources: replacement.approved_resources,
        delegation_ceiling: replacement.delegation_ceiling,
        approval_decision_digest: replacement.approval_decision_digest,
        approval_expected_grant_revision: replacement.expected_revision,
        companion_approved: replacement.companion_approved,
        platform_privileges: replacement.platform_privileges,
        revision: revision
            .checked_add(1)
            .filter(|revision| *revision <= super::super::MAX_PROTOCOL_INTEGER)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("grant revision overflow".to_owned())
            })?,
        state: replacement.state,
        expires_at: replacement.expires_at,
        provenance: replacement.provenance,
        created_at: current.as_ref().map_or(now, |binding| binding.created_at),
        updated_at: current.as_ref().map_or(now, |binding| binding.updated_at),
    };
    binding.validate()?;
    match binding.owner_kind {
        GrantOwnerKind::Deployment => {
            let deployment = load_deployment(connection, &binding.owner_id)?
                .ok_or(AuthorizationStateError::DeploymentInactive)?;
            if deployment.participant_id != binding.participant_id {
                return Err(AuthorizationStateError::InvalidRecord(
                    "deployment is assigned to another participant".to_owned(),
                ));
            }
            let profile = load_deployment_profile(connection, &binding.owner_id)?
                .ok_or(AuthorizationStateError::DeploymentInactive)?;
            if !matches!(
                (profile.kind, participant.participant_kind),
                (
                    super::super::PrincipalKind::Service,
                    ParticipantKind::Service
                ) | (super::super::PrincipalKind::Device, ParticipantKind::Device)
            ) {
                return Err(AuthorizationStateError::InvalidRecord(
                    "deployment and installed participant kinds differ".to_owned(),
                ));
            }
        }
        GrantOwnerKind::User => {
            let principal = load_principal(connection, &binding.owner_id)?
                .ok_or(AuthorizationStateError::PrincipalMissing)?;
            if principal.kind != super::super::PrincipalKind::User
                || !matches!(
                    participant.participant_kind,
                    ParticipantKind::App | ParticipantKind::Agent
                )
            {
                return Err(AuthorizationStateError::InvalidRecord(
                    "user grant owner is not a user".to_owned(),
                ));
            }
        }
    }
    let resolved_participant = participant.resolve()?;
    let allowed = resolved_participant
        .required_grants
        .permissions()
        .iter()
        .chain(
            resolved_participant
                .optional_grant_bundles
                .values()
                .flat_map(|grant| grant.permissions()),
        );
    let allowed = allowed.collect::<Vec<_>>();
    if binding
        .grants
        .permissions()
        .iter()
        .any(|permission| !allowed.contains(&permission))
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "grant contains a permission outside the installed participant definitions".to_owned(),
        ));
    }
    let resources = super::contexts::load_resource_bindings(
        connection,
        binding.owner_kind,
        &binding.owner_id,
        &binding.participant_id,
        binding.installed_revision,
    )?;
    let authority = super::super::policy::resolve_authority(
        &participant,
        binding.approval_mode,
        &binding.approved_capabilities,
        &binding.approved_resources,
        &binding.platform_privileges,
        &binding.delegation_ceiling,
        (&resources, true),
    )?;
    if binding.approval_mode == super::super::ApprovalMode::Capabilities {
        binding.grants = authority.exact_grants;
        binding.platform_privileges = authority.platform_privileges;
    } else if authority.exact_grants != binding.grants
        || authority.platform_privileges != binding.platform_privileges
    {
        tracing::warn!(
            owner_kind = ?binding.owner_kind,
            owner_id = %binding.owner_id,
            participant_id = %binding.participant_id,
            revision = binding.revision,
            "stored grant binding differs from resolved authority"
        );
        return Err(AuthorizationStateError::InvalidRecord(
            "grant binding does not match resolved authority".to_owned(),
        ));
    }
    for permission in binding.grants.permissions() {
        if matches!(
            permission.target(),
            PermissionTarget::ParticipantResource {
                resource: ParticipantResourceKind::Kv | ParticipantResourceKind::Store,
                ..
            }
        ) {
            let paired_action = match permission.action() {
                PermissionAction::Write => PermissionAction::Delete,
                PermissionAction::Delete => PermissionAction::Write,
                _ => continue,
            };
            if !binding.grants.permissions().iter().any(|paired| {
                paired.target() == permission.target() && paired.action() == paired_action
            }) {
                return Err(AuthorizationStateError::InvalidRecord(
                    "direct KV/store write and delete permissions must be granted together"
                        .to_owned(),
                ));
            }
        }
    }
    if current.as_ref().is_some_and(|current| {
        current.owner_kind == GrantOwnerKind::User
            && current
                .platform_privileges
                .contains(&PlatformPrivilege::Admin)
            && (binding.state != GrantBindingState::Active
                || !binding
                    .platform_privileges
                    .contains(&PlatformPrivilege::Admin))
    }) && connection
        .query_row(
            "SELECT 1 FROM auth_bootstrap_administrator WHERE singleton = 1 AND principal_id = ?1",
            [&binding.owner_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(sql_error)?
        .is_some()
    {
        return Err(AuthorizationStateError::NotAuthorized);
    }
    if let Some(current) = current.as_ref().filter(|current| {
        binding.provenance.is_some()
            && current.owner_kind == GrantOwnerKind::User
            && current.owner_id == binding.owner_id
            && current.participant_id == binding.participant_id
            && current.installed_revision == binding.installed_revision
            && current.grants == binding.grants
            && current.approval_mode == binding.approval_mode
            && current.approved_capabilities == binding.approved_capabilities
            && current.approved_resources == binding.approved_resources
            && current.delegation_ceiling == binding.delegation_ceiling
            && current.companion_approved == binding.companion_approved
            && current.platform_privileges == binding.platform_privileges
            && current.state == binding.state
            && current.expires_at == binding.expires_at
            && current.provenance == binding.provenance
    }) {
        return Ok((current.clone(), Vec::new()));
    }
    binding.updated_at = now;
    binding.validate()?;
    connection.execute(
        "INSERT INTO auth_grant_bindings (owner_kind, owner_id, participant_id, installed_revision,
             grants_json, approval_mode, approved_capabilities_json, approved_resources_json,
             delegation_ceiling_json, approval_decision_digest,
             approval_expected_grant_revision, companion_approved,
             platform_privileges_json, revision, state, expires_at,
             provenance_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(owner_kind, owner_id, participant_id) DO UPDATE SET
             installed_revision = excluded.installed_revision, grants_json = excluded.grants_json,
             approval_mode = excluded.approval_mode,
             approved_capabilities_json = excluded.approved_capabilities_json,
             approved_resources_json = excluded.approved_resources_json,
             delegation_ceiling_json = excluded.delegation_ceiling_json,
             approval_decision_digest = excluded.approval_decision_digest,
             approval_expected_grant_revision = excluded.approval_expected_grant_revision,
             companion_approved = excluded.companion_approved,
             platform_privileges_json = excluded.platform_privileges_json, revision = excluded.revision,
              state = excluded.state, expires_at = excluded.expires_at, provenance_json = excluded.provenance_json,
               updated_at = excluded.updated_at",
        params![encode_enum(binding.owner_kind)?, binding.owner_id, binding.participant_id,
            binding.installed_revision, encode_json(&binding.grants)?, encode_enum(binding.approval_mode)?,
            encode_json(&binding.approved_capabilities)?, encode_json(&binding.approved_resources)?,
            encode_json(&binding.delegation_ceiling)?, binding.approval_decision_digest,
            binding.approval_expected_grant_revision, binding.companion_approved,
            encode_json(&binding.platform_privileges)?, binding.revision, encode_enum(binding.state)?, binding.expires_at,
            binding.provenance.as_ref().map(encode_json).transpose()?, binding.created_at, binding.updated_at],
    ).map_err(map_write_error)?;
    if binding.owner_kind == GrantOwnerKind::User && binding.state == GrantBindingState::Active {
        if let Some(previous) = current.as_ref() {
            connection
                .execute(
                    "UPDATE auth_device_delegations
                     SET child_grant_revision = ?1
                     WHERE child_grant_revision = ?2
                       AND companion_participant_id = ?3
                       AND user_login_session_id IN (
                           SELECT session_id FROM auth_sessions
                           WHERE principal_id = ?4 AND participant_id = ?3
                       )",
                    params![
                        binding.revision,
                        previous.revision,
                        binding.participant_id,
                        binding.owner_id,
                    ],
                )
                .map_err(map_write_error)?;
        }
    }
    // Installed revision identity alone is never a revocation condition. Each
    // live context is judged against the participant revision it was actually
    // issued under, and is revoked only when the replacement no longer covers
    // its exact effective authority. Increasing authority keeps every narrower
    // context attached and is picked up at the next ordinary refresh.
    let revoked_contexts = if binding.state == GrantBindingState::Active {
        let now_seconds = now.div_euclid(1_000);
        let replacement = &participant.projection;
        let mut previous_projections: BTreeMap<u64, Option<ParticipantRuntimeProjection>> =
            BTreeMap::new();
        revoke_sql_contexts_matching(
            connection,
            &AuthorizationContextSelector::Grant(
                binding.owner_kind,
                binding.owner_id.clone(),
                binding.participant_id.clone(),
            ),
            AuthorizationContextRevocationReason::AuthorityChanged,
            now_seconds,
            |context| {
                // Already-expired history is not retroactively invalidated, and
                // only contexts still within their signed lease are candidates.
                if context.state != AuthorizationContextState::Active
                    || context.expires_at <= now_seconds
                {
                    return Ok(false);
                }
                let previous = match previous_projections.entry(context.installed_revision) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                        load_installed_participant(
                            connection,
                            &binding.participant_id,
                            Some(context.installed_revision),
                        )?
                        .map(|(_, record)| record.projection),
                    ),
                };
                let Some(previous) = previous.as_ref() else {
                    // The historical installation that issued this context can no
                    // longer be resolved, so its retained authority cannot be
                    // proven covered. Revoke only this context; the legitimate
                    // replacement itself still commits.
                    return Ok(true);
                };
                Ok(!context_authority_is_covered(
                    context,
                    &binding,
                    previous,
                    replacement,
                )?)
            },
        )?
    } else {
        revoke_sql_contexts(
            connection,
            &AuthorizationContextSelector::Grant(
                binding.owner_kind,
                binding.owner_id.clone(),
                binding.participant_id.clone(),
            ),
            AuthorizationContextRevocationReason::AuthorityRevoked,
            now.div_euclid(1_000),
        )?
    };
    let mut event_payload = json!({
        "eventType": "Auth.Grants.Changed",
        "eventId": ulid::Ulid::new().to_string(), "occurredAt": now, "binding": binding,
    });
    event_payload["eventSubject"] = json!(auth_event_subject::<GrantsChanged>(&event_payload)?);
    let event = PostCommitActionRecord {
        predecessor_action_id: None,
        action_id: trellis_protocol::digest_json(&json!({
            "event": "Auth.Grants.Changed", "ownerKind": binding.owner_kind,
            "ownerId": binding.owner_id, "participantId": binding.participant_id,
            "revision": binding.revision,
        }))
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        kind: PostCommitActionKind::Event,
        payload: event_payload,
        created_at: now,
        attempts: 0,
        next_attempt_at: now,
        claimed_until: None,
        last_error: None,
    };
    let mut actions = vec![event];
    for context in revoked_contexts {
        actions.push(PostCommitActionRecord {
            predecessor_action_id: Some(super::super::context::context_revocation_action_id(
                &context,
            )?),
            action_id: trellis_protocol::digest_json(&json!({
                "grantKick": context.connection_id,
                "participantId": binding.participant_id,
                "revision": binding.revision,
            }))
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
            kind: PostCommitActionKind::Kick,
            payload: json!({
                "connectionId": context.connection_id,
                "reason": "grant_replaced",
            }),
            created_at: now,
            attempts: 0,
            next_attempt_at: now,
            claimed_until: None,
            last_error: None,
        });
    }
    actions.extend(super::resources::reconcile_sql_resource_catalog(
        connection, &binding, now,
    )?);
    Ok((binding, actions))
}

fn write_companion_activation(
    connection: &Connection,
    review: &super::super::DeviceActivationReviewRecord,
    delegation: &DeviceDelegationRecord,
    session: Option<&SessionRecord>,
    now: i64,
) -> Result<(), AuthorizationStateError> {
    let session = session.ok_or_else(|| {
        AuthorizationStateError::InvalidRecord(
            "companion activation requires a login session".to_owned(),
        )
    })?;
    if delegation.user_login_session_id.as_deref() != Some(&session.session_id) {
        return Err(AuthorizationStateError::InvalidRecord(
            "companion delegation and session do not match".to_owned(),
        ));
    }
    super::sessions::insert_sql_session(connection, session)?;
    connection
        .execute(
            "INSERT INTO auth_device_delegations (principal_id, deployment_id, companion_participant_id, user_login_session_id, installation_public_key, device_grant_revision, child_grant_revision, required, state, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(principal_id, deployment_id) DO UPDATE SET companion_participant_id = excluded.companion_participant_id, user_login_session_id = excluded.user_login_session_id, installation_public_key = excluded.installation_public_key, device_grant_revision = excluded.device_grant_revision, child_grant_revision = excluded.child_grant_revision, required = excluded.required, state = excluded.state, expires_at = excluded.expires_at",
            params![
                delegation.principal_id,
                delegation.deployment_id,
                delegation.companion_participant_id,
                delegation.user_login_session_id,
                delegation.installation_public_key,
                delegation.device_grant_revision,
                delegation.child_grant_revision,
                delegation.required,
                encode_enum(delegation.state)?,
                delegation.expires_at,
            ],
        )
        .map_err(map_write_error)?;
    let changed = connection
        .execute(
            "UPDATE auth_devices SET state = ?1, updated_at = ?2, version = version + 1 WHERE principal_id = ?3 AND deployment_id = ?4",
            params![
                encode_enum(DeviceState::Active)?,
                now,
                review.principal_id,
                review.deployment_id,
            ],
        )
        .map_err(map_write_error)?;
    if changed != 1 {
        return Err(AuthorizationStateError::StorageConflict);
    }
    Ok(())
}

impl SqliteAuthorizationStore {
    pub(crate) async fn user_is_admin(
        &self,
        user_id: String,
    ) -> Result<bool, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT platform_privileges_json FROM auth_grant_bindings
                     WHERE owner_kind = 'user' AND owner_id = ?1 AND state = 'active'",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map([user_id], |row| row.get::<_, String>(0))
                .map_err(sql_error)?;
            for row in rows {
                let privileges: Vec<PlatformPrivilege> =
                    decode_json(row.map_err(sql_error)?).map_err(sql_error)?;
                if privileges.contains(&PlatformPrivilege::Admin) {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .await
    }

    pub(crate) async fn get_installed_participant_record(
        &self,
        participant_id: String,
        revision: Option<u64>,
    ) -> Result<Option<(u64, ParticipantBindingRecord)>, AuthorizationStateError> {
        self.run_read(move |connection| {
            load_installed_participant(connection, &participant_id, revision)
        })
        .await
    }

    pub(crate) async fn is_companion_participant(
        &self,
        participant_id: String,
    ) -> Result<bool, AuthorizationStateError> {
        self.run_read(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM auth_installed_participants parent
                        WHERE parent.companion_participant_id = ?1
                          AND parent.revision = (
                            SELECT MAX(current.revision) FROM auth_installed_participants current
                            WHERE current.participant_id = parent.participant_id
                          )
                    )",
                    [participant_id],
                    |row| row.get(0),
                )
                .map_err(sql_error)
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn get_installed_package_evidence(
        &self,
        evidence_digest: &str,
    ) -> Result<Option<trellis_idl::PackageEvidence>, AuthorizationStateError> {
        let evidence_digest = evidence_digest.to_owned();
        self.run_read(move |connection| {
            connection
                .query_row(
                    "SELECT evidence_json FROM auth_package_evidence_documents WHERE evidence_digest = ?1",
                    [&evidence_digest],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_error)?
                .map(|json| {
                    serde_json::from_str(&json)
                        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
                })
                .transpose()
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn get_api_binding(
        &self,
        participant_id: &str,
        api_id: &str,
    ) -> Result<Option<String>, AuthorizationStateError> {
        let participant_id = participant_id.to_owned();
        let api_id = api_id.to_owned();
        self.run_read(move |connection| {
            connection
                .query_row(
                    "SELECT provider_deployment_id FROM auth_api_bindings WHERE participant_id = ?1 AND api_id = ?2",
                    rusqlite::params![participant_id, api_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_error)
        })
        .await
    }

    pub(crate) async fn put_api_binding(
        &self,
        participant_id: &str,
        api_id: &str,
        provider_deployment_id: &str,
    ) -> Result<(), AuthorizationStateError> {
        let participant_id = participant_id.to_owned();
        let api_id = api_id.to_owned();
        let provider_deployment_id = provider_deployment_id.to_owned();
        self.run(move |connection| {
            connection.execute(
                "INSERT INTO auth_api_bindings (participant_id, api_id, provider_deployment_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT(participant_id, api_id) DO UPDATE SET provider_deployment_id = excluded.provider_deployment_id",
                rusqlite::params![participant_id, api_id, provider_deployment_id],
            ).map_err(sql_error)?;
            Ok(())
        }).await
    }

    pub(crate) async fn get_api_bindings(
        &self,
        participant_id: &str,
    ) -> Result<BTreeMap<String, String>, AuthorizationStateError> {
        let participant_id = participant_id.to_owned();
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT api_id, provider_deployment_id FROM auth_api_bindings
                     WHERE participant_id = ?1 ORDER BY api_id",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map([&participant_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            Ok(rows.into_iter().collect())
        })
        .await
    }

    #[tracing::instrument(
        name = "trellis.contract.compare_selected",
        skip_all,
        fields(trellis.surface = "contract", trellis.operation = "compare_selected")
    )]
    pub(crate) async fn compare_installed_selection(
        &self,
        consumer: Arc<CompiledInstalledEvidence>,
        selection: trellis_idl::InteractionSelection,
        provider: Arc<CompiledInstalledEvidence>,
    ) -> Result<Arc<trellis_idl::CompatibilityReport>, AuthorizationStateError> {
        let total_started = std::time::Instant::now();
        let cpu = self.compiled_evidence.cpu_semaphore();
        let consumer_graph = Arc::clone(&consumer.graph);
        let provider_graph = Arc::clone(&provider.graph);
        let job_selection = selection.clone();
        let result = self
            .compiled_evidence
            .compare_selection(&consumer, &selection, &provider, move || async move {
                let cpu_started = std::time::Instant::now();
                let cpu_permit = Arc::clone(&cpu).acquire_owned().await;
                record_duration(
                    DurationMetric::ContractAnalysis,
                    cpu_started.elapsed(),
                    "contract",
                    "compare_selected",
                    "cpu_wait",
                    if cpu_permit.is_ok() {
                        Outcome::Ok
                    } else {
                        Outcome::Error
                    },
                );
                let permit = cpu_permit.map_err(|_| {
                    SemanticJobError::Storage("semantic CPU pool closed".to_owned())
                })?;
                let submitted = std::time::Instant::now();
                let (report, blocking_queue, compare) = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let blocking_queue = submitted.elapsed();
                    let compute_started = std::time::Instant::now();
                    let report = trellis_idl::compare_selected(
                        &consumer_graph,
                        &job_selection,
                        &provider_graph,
                    );
                    (report, blocking_queue, compute_started.elapsed())
                })
                .await
                .map_err(|error| SemanticJobError::Worker(error.to_string()))?;
                record_duration(
                    DurationMetric::ContractAnalysis,
                    blocking_queue,
                    "contract",
                    "compare_selected",
                    "blocking_queue",
                    Outcome::Ok,
                );
                record_duration(
                    DurationMetric::ContractAnalysis,
                    compare,
                    "contract",
                    "compare_selected",
                    "compare",
                    Outcome::Ok,
                );
                Ok(report)
            })
            .await
            .map_err(SemanticJobError::into_state_error);
        record_duration(
            DurationMetric::ContractAnalysis,
            total_started.elapsed(),
            "contract",
            "compare_selected",
            "total",
            if result.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Error
            },
        );
        result
    }

    pub(crate) async fn put_participant_binding(
        &self,
        participant: ParticipantBindingRecord,
    ) -> Result<u64, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let evidence_json =
                super::super::builtins::trusted_package_evidence_json(&participant.package_digest)?
                    .ok_or(AuthorizationStateError::NotAuthorized)?;
            let evidence_digest = accept_package_evidence(
                &transaction,
                &participant.package_digest,
                "trellis",
                &evidence_json,
                true,
                None,
                participant.resolved_at,
            )?;
            if evidence_digest != participant.evidence_digest {
                return Err(AuthorizationStateError::InvalidRecord(
                    "participant evidence digest does not match accepted document".to_owned(),
                ));
            }
            let revision = install_participant(&transaction, &participant, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(revision)
        })
        .await
    }

    /// Inspect the exact retained installed snapshot, or the latest revision.
    pub(crate) async fn get_installed_participant(
        &self,
        participant_id: String,
        revision: Option<u64>,
    ) -> Result<Value, AuthorizationStateError> {
        if revision
            .is_some_and(|revision| revision == 0 || revision > super::super::MAX_PROTOCOL_INTEGER)
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "invalid installed revision".to_owned(),
            ));
        }
        self.run_read(move |connection| {
            let (revision, binding) = load_installed_participant(connection, &participant_id, revision)?
                .ok_or(AuthorizationStateError::ParticipantMissing)?;
            let resolved = binding.resolve()?;
            let package_evidence: Value = connection.query_row(
                "SELECT evidence_json FROM auth_package_evidence_documents WHERE evidence_digest = ?1",
                [&binding.evidence_digest],
                |row| row.get::<_, String>(0),
            ).map_err(sql_error).and_then(|value| decode_json(value).map_err(sql_error))?;
            Ok(json!({"participant": {
                "participantId": binding.participant_id, "participantKind": binding.participant_kind,
                 "revision": revision, "participantDigest": binding.participant_digest, "installedAt": binding.resolved_at,
                 "packageDigest": binding.package_digest, "evidenceDigest": binding.evidence_digest,
                 "participantPath": binding.participant_path,
                "companionRequired": resolved.companion_required,
                "packageEvidence": package_evidence,
                "requiredGrants": resolved.required_grants,
                "optionalBundles": resolved.optional_grant_bundles.iter().map(|(id, grant)| json!({
                    "id": id, "permissions": grant.permissions(),
                })).collect::<Vec<_>>(),
            }}))
          }).await
    }

    pub(crate) async fn list_installed_participants(
        &self,
        request: trellis_runtime_apis::types::AuthParticipantsListRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let mut query = serde_json::to_value(&request)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        query.as_object_mut().map(|query| query.remove("page"));
        let digest = trellis_protocol::pagination_query_digest("auth.Participants.List", &query)
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
        let limit = request
            .page
            .as_ref()
            .and_then(|page| page.limit)
            .unwrap_or(50);
        if limit == 0 || limit > 200 {
            return Err(AuthorizationStateError::InvalidRecord(
                "limit must be between 1 and 200".to_owned(),
            ));
        }
        let after = request
            .page
            .and_then(|page| page.cursor)
            .map(|cursor| trellis_protocol::decode_pagination_cursor::<String>(&cursor, &digest))
            .transpose()
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
        let kind = request.kind.map(encode_enum).transpose()?;
        self.run_read(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT participant_id FROM auth_installed_participants p
                     WHERE revision = (SELECT MAX(revision) FROM auth_installed_participants WHERE participant_id = p.participant_id)
                       AND (?1 IS NULL OR participant_kind = ?1) AND (?2 IS NULL OR participant_id > ?2)
                     ORDER BY participant_id LIMIT ?3"
                ).map_err(sql_error)?;
                let ids = statement.query_map(params![kind, after, i64::from(limit) + 1], |row| row.get::<_, String>(0))
                    .map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
                let next_cursor = (ids.len() > limit as usize).then(|| {
                    trellis_protocol::encode_pagination_cursor(&digest, &ids[limit as usize - 1])
                        .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))
                }).transpose()?;
                let items = ids.into_iter().take(limit as usize).map(|id| {
                    let (revision, binding) = load_installed_participant(connection, &id, None)?
                        .ok_or(AuthorizationStateError::StorageConflict)?;
                    Ok(json!({
                        "participantId": binding.participant_id,
                        "participantKind": binding.participant_kind,
                        "revision": revision,
                        "packageDigest": binding.package_digest,
                        "participantPath": binding.participant_path,
                        "participantDigest": binding.participant_digest,
                        "installedAt": binding.resolved_at,
                        "companionParticipantId": binding.projection.companion_participant_id,
                        "companionRequired": binding.projection.companion_required,
                    }))
                }).collect::<Result<Vec<_>, AuthorizationStateError>>()?;
                Ok(json!({"items": items, "page": {"nextCursor": next_cursor}}))
            }).await
    }

    /// List one caller-scoped page in stable owner/participant tuple order.
    pub(crate) async fn list_grant_bindings(
        &self,
        request: trellis_runtime_apis::types::AuthGrantsListRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let mut query = serde_json::to_value(&request)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        query.as_object_mut().map(|query| query.remove("page"));
        let query_digest = trellis_protocol::pagination_query_digest("auth.Grants.List", &query)
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
        let owner_kind = request
            .owner_kind
            .map(|kind| match kind {
                AuthGrantsListRequestOwnerKind::User => Ok(GrantOwnerKind::User),
                AuthGrantsListRequestOwnerKind::Deployment => Ok(GrantOwnerKind::Deployment),
                AuthGrantsListRequestOwnerKind::Unknown(_) => Err(
                    AuthorizationStateError::InvalidRecord("unknown grant owner kind".to_owned()),
                ),
            })
            .transpose()?;
        let state = request
            .state
            .map(|state| match state {
                AuthGrantsListRequestState::Active => Ok(GrantBindingState::Active),
                AuthGrantsListRequestState::Revoked => Ok(GrantBindingState::Revoked),
                AuthGrantsListRequestState::Unknown(_) => Err(
                    AuthorizationStateError::InvalidRecord("unknown grant state".to_owned()),
                ),
            })
            .transpose()?;
        let limit = i64::from(
            request
                .page
                .as_ref()
                .and_then(|page| page.limit)
                .unwrap_or(50),
        );
        if !(1..=200).contains(&limit) {
            return Err(AuthorizationStateError::InvalidRecord(
                "limit must be between 1 and 200".to_owned(),
            ));
        }
        let after = request
            .page
            .and_then(|page| page.cursor)
            .map(|cursor| {
                trellis_protocol::decode_pagination_cursor::<(String, String, String)>(
                    &cursor,
                    &query_digest,
                )
            })
            .transpose()
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
        self.run_read(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sql_error)?;
            let keys = {
                let mut statement = transaction
                    .prepare(
                        "SELECT owner_kind, owner_id, participant_id FROM auth_grant_bindings
                     WHERE (?1 IS NULL OR owner_kind = ?1) AND (?2 IS NULL OR owner_id = ?2)
                             AND (?3 IS NULL OR participant_id = ?3) AND (?4 IS NULL OR state = ?4)
                             AND (?5 IS NULL OR owner_kind > ?5 OR (owner_kind = ?5 AND (owner_id > ?6 OR (owner_id = ?6 AND participant_id > ?7))))
                           ORDER BY owner_kind, owner_id, participant_id LIMIT ?8",
                    )
                    .map_err(sql_error)?;
                let keys = statement
                    .query_map(
                        params![
                            owner_kind.map(encode_enum).transpose()?,
                            request.owner_id.map(|owner| owner.0),
                            request.participant_id.map(|participant| participant.0),
                                  state.map(encode_enum).transpose()?,
                                  after.as_ref().map(|after| after.0.as_str()),
                                  after.as_ref().map(|after| after.1.as_str()),
                                  after.as_ref().map(|after| after.2.as_str()),
                                  limit + 1
                        ],
                        |row| {
                            Ok((
                                decode_enum(row.get::<_, String>(0)?)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .map_err(sql_error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(sql_error)?;
                keys
            };
                  let next_cursor = (keys.len() > limit as usize).then(|| {
                      let (kind, owner, participant) = &keys[limit as usize - 1];
                      trellis_protocol::encode_pagination_cursor(
                          &query_digest,
                          &(encode_enum(*kind)?, owner, participant),
                      ).map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))
                  }).transpose()?;
                  let entries = keys
                .into_iter()
                .take(limit as usize)
                .map(|(kind, owner, participant)| {
                    load_grant_binding(&transaction, kind, &owner, &participant)?
                        .ok_or(AuthorizationStateError::StorageConflict)
                })
                .collect::<Result<Vec<_>, _>>()?;
            transaction.commit().map_err(sql_error)?;
                  let page = next_cursor.map_or_else(|| json!({}), |cursor| json!({"nextCursor": cursor}));
            Ok(json!({
                "items": entries,
                "page": page,
            }))
        })
        .await
    }

    /// Atomically install definitions and apply explicit deployment permissions.
    pub(crate) async fn apply_deployment(
        &self,
        actor: MutationActor,
        deployment_id: String,
        deployment_evidence: (
            ParticipantBindingRecord,
            String,
            String,
            Option<super::super::ephemeral::ConsentApproval>,
        ),
        revision: (u64, IdempotencyResultRecord),
    ) -> Result<Value, AuthorizationStateError> {
        let (expected_revision, mut idempotency) = revision;
        let (participant, root_package, evidence_json, approval) = deployment_evidence;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            require_current_actor(&transaction, &actor, true, idempotency.created_at)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let evidence_digest = accept_package_evidence(&transaction, &participant.package_digest, &root_package, &evidence_json, false, None, idempotency.created_at)?;
            if evidence_digest != participant.evidence_digest {
                return Err(AuthorizationStateError::InvalidRecord(
                    "participant evidence digest does not match accepted document".to_owned(),
                ));
            }
            let mut deployment = load_deployment_profile(&transaction, &deployment_id)?
                .ok_or(AuthorizationStateError::DeploymentInactive)?;
            let matching_kind = matches!(
                (deployment.kind, participant.participant_kind),
                (super::super::PrincipalKind::Service, trellis_protocol::ParticipantKind::Service)
                    | (super::super::PrincipalKind::Device, trellis_protocol::ParticipantKind::Device)
            );
            if !matching_kind || deployment.participant_id.as_ref().is_some_and(|id| id != &participant.participant_id) {
                return Err(AuthorizationStateError::InvalidRecord(
                    "participant must match the deployment kind and existing assignment".to_owned(),
                ));
            }
            let current = load_grant_binding(&transaction, GrantOwnerKind::Deployment, &deployment_id, &participant.participant_id)?;
            let current_revision = current.as_ref().map_or(0, |binding| binding.revision);
            if current_revision != expected_revision {
                return Err(AuthorizationStateError::RevisionConflict { expected: expected_revision, current: current_revision });
            }
            let ceiling = super::super::policy::participant_delegation_ceiling(&participant)?;
            let installed_revision = install_participant(&transaction, &participant, None)?;
            let companion = participant
                .resolve()?
                .companion_participant_id
                .as_deref()
                .map(|participant_id| load_installed_participant(&transaction, participant_id, None))
                .transpose()?
                .flatten();
            let consent = super::super::policy::consent_request(
                &participant,
                installed_revision,
                current.as_ref(),
                &ceiling,
                &[],
                companion
                    .as_ref()
                    .map(|(_, child)| (child, None, [].as_slice(), &ceiling)),
            )?;
            let Some(approval) = approval else {
                return Err(AuthorizationStateError::ApprovalRequired {
                    consent_request: serde_json::to_value(&consent)
                        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
                });
            };
            let approved_capabilities = approval.approved_capabilities.iter().collect::<std::collections::BTreeSet<_>>();
            let approved_resources = approval.approved_resources.iter().collect::<std::collections::BTreeSet<_>>();
            if approval.mode != super::super::ApprovalMode::Capabilities
                || approval.installed_revision != installed_revision
                || approval.expected_grant_revision != current_revision
                || approval.decision_digest != consent.decision_digest
                || approval.delegation_ceiling.as_ref().is_some_and(|submitted| {
                    submitted.capabilities != ceiling.capabilities
                        || submitted.exact_restrictions != ceiling.exact_restrictions
                })
                || approved_capabilities.len() != approval.approved_capabilities.len()
                || approved_resources.len() != approval.approved_resources.len()
                || approval.approved_capabilities.iter().any(|approved| !consent.capabilities.iter().any(|capability| capability.eligible && capability.id == approved.id && capability.consent_digest == approved.consent_digest))
                || approval.approved_resources.iter().any(|approved| !consent.resources.iter().any(|resource| resource.eligible && resource.kind == approved.kind && resource.name == approved.name && resource.requested_commitment == approved.commitment))
                || consent.capabilities.iter().any(|capability| capability.required && capability.eligible && !approved_capabilities.iter().any(|approved| approved.id == capability.id && approved.consent_digest == capability.consent_digest))
                || consent.resources.iter().any(|resource| resource.required && !approved_resources.contains(&ApprovedResource { kind: resource.kind, name: resource.name.clone(), commitment: resource.requested_commitment.clone() }))
                || consent.companion.as_ref().is_some_and(|companion| companion.required && !approval.companion_approved)
                || (approval.companion_approved && consent.companion.is_none())
            {
                return Err(AuthorizationStateError::ApprovalRequired {
                    consent_request: serde_json::to_value(&consent)
                        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
                });
            }
            let authority = super::super::policy::resolve_authority(&participant, approval.mode, &approval.approved_capabilities, &approval.approved_resources, &current.as_ref().map_or_else(Vec::new, |binding| binding.platform_privileges.clone()), &ceiling, (&[], approval.companion_approved))?;
            if deployment.participant_id.is_none() {
                deployment.participant_id = Some(participant.participant_id.clone());
                deployment.updated_at = idempotency.created_at;
                deployment.version = super::validation::next_version(deployment.version)?;
                transaction.execute(
                    "UPDATE auth_deployment_profiles SET participant_id = ?1, updated_at = ?2, version = ?3
                     WHERE deployment_id = ?4 AND participant_id IS NULL",
                    params![deployment.participant_id, deployment.updated_at, deployment.version, deployment_id],
                ).map_err(map_write_error)?;
            }
            upsert_deployment_profile_evidence(&transaction, &deployment)?;
            let (binding, actions) = replace_grant_binding(&transaction, GrantBindingReplacement {
                owner_kind: GrantOwnerKind::Deployment,
                owner_id: deployment_id,
                participant_id: participant.participant_id.clone(),
                installed_revision,
                grants: authority.exact_grants,
                approval_mode: approval.mode,
                approved_capabilities: approval.approved_capabilities,
                approved_resources: approval.approved_resources,
                delegation_ceiling: ceiling,
                approval_decision_digest: approval.decision_digest,
                companion_approved: approval.companion_approved,
                platform_privileges: current.as_ref().map_or_else(Vec::new, |binding| binding.platform_privileges.clone()),
                expected_revision,
                expected_current_installed_revision: None,
                state: GrantBindingState::Active,
                expires_at: current.as_ref().and_then(|binding| binding.expires_at),
                provenance: None,
            }, idempotency.created_at)?;
            let principal = super::principals::load_principal(&transaction, &deployment.deployment_id)?
                .ok_or(AuthorizationStateError::PrincipalMissing)?;
            let mut deployment_value = serde_json::to_value(&deployment)
                .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
            if deployment.state == super::super::DeploymentProfileState::Removed {
                deployment_value["state"] = json!("revoked");
            }
            deployment_value["disabledAt"] = json!(principal.disabled_at);
            deployment_value["revokedAt"] = json!(principal.revoked_at);
            idempotency.result = json!({
                "deployment": deployment_value,
                "binding": binding,
                "consentRequest": consent,
            });
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        }).await
    }

    /// Read the retained current binding for an exact owner and participant.
    pub async fn get_grant_binding(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    ) -> Result<Option<GrantBinding>, AuthorizationStateError> {
        self.run_read(move |connection| {
            load_grant_binding(connection, owner_kind, &owner_id, &participant_id)
        })
        .await
    }

    /// Replace one administratively authorized binding with revision and replay fences.
    pub(crate) async fn set_grant_binding(
        &self,
        replacement: GrantBindingReplacement,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let (binding, actions) =
                replace_grant_binding(&transaction, replacement, idempotency.created_at)?;
            idempotency.result = json!({"binding": binding});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        })
        .await
    }

    pub(crate) async fn apply_companion_activation_claim(
        &self,
        replacement: Option<GrantBindingReplacement>,
        authority: Option<super::super::ConsentAuthorityPreconditions>,
        command: ActivationReviewClaim,
    ) -> Result<Value, AuthorizationStateError> {
        self.apply_companion_activation(
            replacement,
            authority,
            CompanionActivationReviewMutation::Claim(command),
        )
        .await
    }

    pub(crate) async fn apply_companion_activation_decision(
        &self,
        replacement: Option<GrantBindingReplacement>,
        authority: Option<super::super::ConsentAuthorityPreconditions>,
        command: ActivationReviewDecision,
    ) -> Result<Value, AuthorizationStateError> {
        self.apply_companion_activation(
            replacement,
            authority,
            CompanionActivationReviewMutation::Decision(command),
        )
        .await
    }

    async fn apply_companion_activation(
        &self,
        replacement: Option<GrantBindingReplacement>,
        authority: Option<super::super::ConsentAuthorityPreconditions>,
        mutation: CompanionActivationReviewMutation,
    ) -> Result<Value, AuthorizationStateError> {
        match &mutation {
            CompanionActivationReviewMutation::Claim(command) => {
                super::super::application::validation::validate_idempotency_and_actions(
                    &command.idempotency,
                    &command.actions,
                )?;
            }
            CompanionActivationReviewMutation::Decision(command) => {
                super::super::application::validation::validate_activation_decision(
                    command.state,
                    command.decided_at,
                    &command.decided_by,
                )?;
                super::super::application::validation::validate_idempotency_and_actions(
                    &command.idempotency,
                    &command.actions,
                )?;
            }
        }
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(authority) = &authority {
                verify_consent_authority_preconditions(&transaction, authority)?;
            }
            let idempotency = match &mutation {
                CompanionActivationReviewMutation::Claim(command) => &command.idempotency,
                CompanionActivationReviewMutation::Decision(command) => &command.idempotency,
            };
            if let Some(result) = sqlite_idempotency_replay(&transaction, idempotency)? {
                return Ok(result);
            }
            let (review_id, expected_version, now) = match &mutation {
                CompanionActivationReviewMutation::Claim(command) => {
                    (&command.review_id, command.expected_version, command.now)
                }
                CompanionActivationReviewMutation::Decision(command) => {
                    (&command.review_id, command.expected_version, command.decided_at)
                }
            };
            let current = super::provisioning::load_activation_review(&transaction, review_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.version != expected_version || current.expires_at <= now {
                return Err(AuthorizationStateError::StorageConflict);
            }
            match &mutation {
                CompanionActivationReviewMutation::Claim(command)
                    if current.state != DeviceActivationReviewState::Approved
                        || current
                            .activated_by_user_principal_id
                            .as_deref()
                            .is_some_and(|principal| {
                                principal != command.activated_by_user_principal_id
                            }) =>
                {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                CompanionActivationReviewMutation::Decision(command)
                    if current.state != DeviceActivationReviewState::Pending
                        || command.decided_at < current.requested_at =>
                {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                _ => {}
            }
            let (expected_owner, delegation) = match &mutation {
                CompanionActivationReviewMutation::Claim(command) => (
                    command.activated_by_user_principal_id.as_str(),
                    command.delegation.as_ref(),
                ),
                CompanionActivationReviewMutation::Decision(command) => {
                    (command.decided_by.as_str(), command.delegation.as_ref())
                }
            };
            let (binding, mut actions) = if let Some(replacement) = replacement {
                if replacement.owner_kind != GrantOwnerKind::User
                    || replacement.owner_id != expected_owner
                {
                    return Err(AuthorizationStateError::NotAuthorized);
                }
                replace_grant_binding(&transaction, replacement, now)?
            } else {
                let delegation = delegation.ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(
                        "companion activation requires delegation".to_owned(),
                    )
                })?;
                let participant_id = delegation
                    .companion_participant_id
                    .as_deref()
                    .ok_or_else(|| {
                        AuthorizationStateError::InvalidRecord(
                            "companion activation requires participant identity".to_owned(),
                        )
                    })?;
                let binding = load_grant_binding(
                    &transaction,
                    GrantOwnerKind::User,
                    expected_owner,
                    participant_id,
                )?
                .filter(|binding| {
                    binding.state == GrantBindingState::Active
                        && delegation.child_grant_revision == Some(binding.revision)
                })
                .ok_or(AuthorizationStateError::StorageConflict)?;
                (binding, Vec::new())
            };
            match &mutation {
                CompanionActivationReviewMutation::Claim(command) => {
                    let delegation = command.delegation.as_ref().ok_or_else(|| {
                        AuthorizationStateError::InvalidRecord(
                            "companion activation claim requires delegation".to_owned(),
                        )
                    })?;
                    if delegation.principal_id != current.principal_id
                        || delegation.deployment_id != current.deployment_id
                        || delegation.state != DeviceDelegationState::Active
                        || delegation.child_grant_revision != Some(binding.revision)
                    {
                        return Err(AuthorizationStateError::InvalidRecord(
                            "activation claim delegation does not match approved review".to_owned(),
                        ));
                    }
                    write_companion_activation(
                        &transaction,
                        &current,
                        delegation,
                        command.companion_session.as_ref(),
                        command.now,
                    )?;
                    let changed = transaction
                        .execute(
                            "UPDATE auth_device_activation_reviews SET activated_by_user_principal_id = ?1, version = ?2 WHERE review_id = ?3 AND version = ?4 AND expires_at > ?5 AND (activated_by_user_principal_id IS NULL OR activated_by_user_principal_id = ?1)",
                            params![
                                command.activated_by_user_principal_id,
                                to_sql_version(next_version(command.expected_version)?)?,
                                command.review_id,
                                to_sql_version(command.expected_version)?,
                                command.now,
                            ],
                        )
                        .map_err(map_write_error)?;
                    if changed != 1 {
                        return Err(AuthorizationStateError::StorageConflict);
                    }
                    actions.extend(command.actions.iter().cloned());
                }
                CompanionActivationReviewMutation::Decision(command) => {
                    let delegation = command.delegation.as_ref().ok_or_else(|| {
                        AuthorizationStateError::InvalidRecord(
                            "companion activation decision requires delegation".to_owned(),
                        )
                    })?;
                    if !command.activate_device
                        || command.state != DeviceActivationReviewState::Approved
                        || delegation.principal_id != current.principal_id
                        || delegation.deployment_id != current.deployment_id
                        || delegation.child_grant_revision != Some(binding.revision)
                    {
                        return Err(AuthorizationStateError::InvalidRecord(
                            "activation decision delegation does not match review".to_owned(),
                        ));
                    }
                    write_companion_activation(
                        &transaction,
                        &current,
                        delegation,
                        command.companion_session.as_ref(),
                        command.decided_at,
                    )?;
                    let changed = transaction
                        .execute(
                            "UPDATE auth_device_activation_reviews SET state = ?1, activated_by_user_principal_id = ?3, decided_at = ?2, decided_by = ?3, reason = ?4, version = ?5 WHERE review_id = ?6 AND version = ?7 AND state = 'pending' AND expires_at > ?2",
                            params![
                                encode_enum(command.state)?,
                                command.decided_at,
                                command.decided_by,
                                command.reason,
                                to_sql_version(next_version(command.expected_version)?)?,
                                command.review_id,
                                to_sql_version(command.expected_version)?,
                            ],
                        )
                        .map_err(map_write_error)?;
                    if changed != 1 {
                        return Err(AuthorizationStateError::StorageConflict);
                    }
                    actions.extend(command.actions.iter().cloned());
                }
            }
            let mut idempotency = idempotency.clone();
            idempotency.result = json!({"reviewId": review_id});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        })
        .await
    }

    pub(crate) async fn admin_set_grant_binding(
        &self,
        actor: MutationActor,
        replacement: GrantBindingReplacement,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            require_current_actor(&transaction, &actor, true, idempotency.created_at)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let (binding, actions) =
                replace_grant_binding(&transaction, replacement, idempotency.created_at)?;
            idempotency.result = json!({"binding": binding});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        })
        .await
    }

    /// Revoke effective authority while retaining its current revisioned row.
    pub(crate) async fn revoke_grant_binding(
        &self,
        actor: MutationActor,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
        expected_revision: u64,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            require_current_actor(
                &transaction,
                &actor,
                owner_kind != GrantOwnerKind::User || owner_id != actor.principal_id,
                idempotency.created_at,
            )?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let binding = load_grant_binding(&transaction, owner_kind, &owner_id, &participant_id)?
                .ok_or(AuthorizationStateError::AuthorityMissing)?;
            let (binding, actions) = replace_grant_binding(
                &transaction,
                GrantBindingReplacement {
                    owner_kind,
                    owner_id,
                    participant_id,
                    installed_revision: binding.installed_revision,
                    grants: trellis_protocol::GrantSet::new(Vec::new()),
                    approval_mode: super::super::ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: super::super::DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(trellis_protocol::GrantSet::new(Vec::new())),
                        platform_privileges: Vec::new(),
                    },
                    approval_decision_digest: idempotency.request_digest.clone(),
                    companion_approved: false,
                    platform_privileges: Vec::new(),
                    state: GrantBindingState::Revoked,
                    expires_at: binding.expires_at,
                    provenance: binding.provenance,
                    expected_revision,
                    expected_current_installed_revision: None,
                },
                idempotency.created_at,
            )?;
            idempotency.result = json!({"binding": binding});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        })
        .await
    }

    /// Install canonical definitions without granting access or rotating credentials.
    pub(crate) async fn install_participant(
        &self,
        actor: MutationActor,
        binding: ParticipantBindingRecord,
        root_package: String,
        evidence_json: String,
        platform_trust: bool,
        revision: (u64, IdempotencyResultRecord),
    ) -> Result<Value, AuthorizationStateError> {
        let (expected_revision, mut idempotency) = revision;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            require_current_actor(&transaction, &actor, true, idempotency.created_at)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let evidence_digest = accept_package_evidence(&transaction, &binding.package_digest, &root_package, &evidence_json, platform_trust, Some(&actor), idempotency.created_at)?;
            if evidence_digest != binding.evidence_digest {
                return Err(AuthorizationStateError::InvalidRecord(
                    "participant evidence digest does not match accepted document".to_owned(),
                ));
            }
            let revision = install_participant(&transaction, &binding, Some(expected_revision))?;
            let (_, installed) = load_installed_participant(&transaction, &binding.participant_id, Some(revision))?
                .ok_or(AuthorizationStateError::ParticipantMissing)?;
            let mut participant = json!({
                "participantId": installed.participant_id, "participantKind": installed.participant_kind,
                "revision": revision, "participantDigest": installed.participant_digest, "installedAt": installed.resolved_at,
                "packageDigest": installed.package_digest, "evidenceDigest": installed.evidence_digest,
                "participantPath": installed.participant_path,
                "companionRequired": installed.projection.companion_required,
            });
            if let Some(companion_id) = installed.projection.companion_participant_id {
                participant["companionParticipantId"] = json!(companion_id);
            }
            idempotency.result = json!({"participant": participant});
            let actions = Vec::new();
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        }).await
    }
}

#[async_trait::async_trait]
impl super::super::GrantRepository for SqliteAuthorizationStore {
    async fn get_installed_participant_record(
        &self,
        participant_id: String,
        revision: Option<u64>,
    ) -> Result<Option<(u64, ParticipantBindingRecord)>, AuthorizationStateError> {
        SqliteAuthorizationStore::get_installed_participant_record(self, participant_id, revision)
            .await
    }

    async fn is_companion_participant(
        &self,
        participant_id: String,
    ) -> Result<bool, AuthorizationStateError> {
        SqliteAuthorizationStore::is_companion_participant(self, participant_id).await
    }

    async fn get_installed_package_evidence(
        &self,
        evidence_digest: &str,
    ) -> Result<Option<trellis_idl::PackageEvidence>, AuthorizationStateError> {
        SqliteAuthorizationStore::get_installed_package_evidence(self, evidence_digest).await
    }

    async fn get_api_binding(
        &self,
        participant_id: &str,
        api_id: &str,
    ) -> Result<Option<String>, AuthorizationStateError> {
        SqliteAuthorizationStore::get_api_binding(self, participant_id, api_id).await
    }

    async fn put_api_binding(
        &self,
        participant_id: &str,
        api_id: &str,
        provider_deployment_id: &str,
    ) -> Result<(), AuthorizationStateError> {
        SqliteAuthorizationStore::put_api_binding(
            self,
            participant_id,
            api_id,
            provider_deployment_id,
        )
        .await
    }

    async fn compiled_installed_evidence(
        &self,
        evidence_digest: &str,
    ) -> Result<Arc<CompiledInstalledEvidence>, AuthorizationStateError> {
        SqliteAuthorizationStore::compiled_installed_evidence(self, evidence_digest).await
    }

    async fn compare_installed_selection(
        &self,
        consumer: Arc<CompiledInstalledEvidence>,
        selection: trellis_idl::InteractionSelection,
        provider: Arc<CompiledInstalledEvidence>,
    ) -> Result<Arc<trellis_idl::CompatibilityReport>, AuthorizationStateError> {
        SqliteAuthorizationStore::compare_installed_selection(self, consumer, selection, provider)
            .await
    }

    async fn get_api_bindings(
        &self,
        participant_id: &str,
    ) -> Result<BTreeMap<String, String>, AuthorizationStateError> {
        SqliteAuthorizationStore::get_api_bindings(self, participant_id).await
    }

    async fn accept_presented_package(
        &self,
        input: super::super::evidence::PackageEvidenceInput,
        now: i64,
    ) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
        self.run(move |connection| {
            let (binding, evidence_json) =
                ParticipantBindingRecord::from_package_evidence(&input, now)?;
            let evidence_digest = accept_package_evidence(
                connection,
                &binding.package_digest,
                &input.package_evidence.root_package,
                &evidence_json,
                false,
                None,
                now,
            )?;
            debug_assert_eq!(evidence_digest, binding.evidence_digest);
            Ok(binding)
        })
        .await
    }

    async fn get_credential_participant_assignment(
        &self,
        identity_key_id: String,
    ) -> Result<Option<String>, AuthorizationStateError> {
        self.run_read(move |connection| {
            connection
                .query_row(
                    "SELECT participant_id FROM auth_provisioned_identities
                     WHERE identity_key_id = ?1",
                    [identity_key_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_error)
        })
        .await
    }

    async fn get_grant_binding(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    ) -> Result<Option<GrantBinding>, AuthorizationStateError> {
        SqliteAuthorizationStore::get_grant_binding(self, owner_kind, owner_id, participant_id)
            .await
    }

    async fn consent_resource_actuals(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    ) -> Result<Vec<super::super::ephemeral::ConsentResourceActualEntry>, AuthorizationStateError>
    {
        SqliteAuthorizationStore::consent_resource_actuals(
            self,
            owner_kind,
            owner_id,
            participant_id,
        )
        .await
    }

    async fn set_grant_binding(
        &self,
        replacement: GrantBindingReplacement,
        idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        SqliteAuthorizationStore::set_grant_binding(self, replacement, idempotency).await
    }

    async fn set_portal_grant_binding(
        &self,
        replacement: GrantBindingReplacement,
        policy: super::super::PortalPolicySnapshot,
        idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            verify_portal_policy_snapshot(&transaction, &policy)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let (binding, actions) =
                replace_grant_binding(&transaction, replacement, idempotency.created_at)?;
            let mut idempotency = idempotency;
            idempotency.result = json!({"binding": binding});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        })
        .await
    }

    async fn set_consent_grant_binding(
        &self,
        replacement: GrantBindingReplacement,
        authority: super::super::ConsentAuthorityPreconditions,
        idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            verify_consent_authority_preconditions(&transaction, &authority)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let (binding, actions) =
                replace_grant_binding(&transaction, replacement, idempotency.created_at)?;
            let mut idempotency = idempotency;
            idempotency.result = json!({"binding": binding});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        })
        .await
    }

    async fn revoke_portal_grant_binding(
        &self,
        owner_id: String,
        participant_id: String,
        expected_revision: u64,
        policy: super::super::PortalPolicySnapshot,
        mut idempotency: IdempotencyResultRecord,
    ) -> Result<Value, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            verify_portal_policy_snapshot(&transaction, &policy)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &idempotency)? {
                return Ok(result);
            }
            let binding = load_grant_binding(
                &transaction,
                GrantOwnerKind::User,
                &owner_id,
                &participant_id,
            )?
            .ok_or(AuthorizationStateError::AuthorityMissing)?;
            let (binding, actions) = replace_grant_binding(
                &transaction,
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id,
                    participant_id,
                    installed_revision: binding.installed_revision,
                    grants: trellis_protocol::GrantSet::new(Vec::new()),
                    approval_mode: super::super::ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: Vec::new(),
                    delegation_ceiling: super::super::DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(trellis_protocol::GrantSet::new(Vec::new())),
                        platform_privileges: Vec::new(),
                    },
                    approval_decision_digest: idempotency.request_digest.clone(),
                    companion_approved: false,
                    platform_privileges: Vec::new(),
                    state: GrantBindingState::Revoked,
                    expires_at: binding.expires_at,
                    provenance: binding.provenance,
                    expected_revision,
                    expected_current_installed_revision: None,
                },
                idempotency.created_at,
            )?;
            idempotency.result = json!({"binding": binding});
            insert_sql_idempotency_and_actions(&transaction, &idempotency, &actions)?;
            transaction.commit().map_err(sql_error)?;
            Ok(idempotency.result)
        })
        .await
    }
}

fn verify_portal_policy_snapshot(
    connection: &Connection,
    snapshot: &super::super::PortalPolicySnapshot,
) -> Result<(), AuthorizationStateError> {
    if snapshot.portal_id.is_empty()
        || snapshot.participant_id.is_empty()
        || snapshot.policy_version.is_some() != snapshot.policy_fingerprint.is_some()
        || snapshot.capability_group_versions.len() != snapshot.capability_group_fingerprints.len()
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "portal policy snapshot is malformed".to_owned(),
        ));
    }
    let group_fingerprints = snapshot
        .capability_group_fingerprints
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();
    if group_fingerprints.len() != snapshot.capability_group_fingerprints.len()
        || snapshot
            .capability_group_versions
            .windows(2)
            .any(|pair| pair[0].0 >= pair[1].0)
        || snapshot
            .capability_group_fingerprints
            .windows(2)
            .any(|pair| pair[0].0 >= pair[1].0)
        || group_fingerprints.keys().ne(snapshot
            .capability_group_versions
            .iter()
            .map(|(group_key, _)| group_key))
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "portal policy snapshot capability-group evidence is malformed".to_owned(),
        ));
    }
    let Some((portal, settings)) =
        super::accounts::load_login_portal(connection, &snapshot.portal_id)?
    else {
        return Err(AuthorizationStateError::PortalPolicyChanged);
    };
    if portal.version != snapshot.portal_version
        || super::super::policy::policy_record_fingerprint(&portal)? != snapshot.portal_fingerprint
        || settings.version != snapshot.login_settings_version
        || super::super::policy::policy_record_fingerprint(&settings)?
            != snapshot.login_settings_fingerprint
    {
        return Err(AuthorizationStateError::PortalPolicyChanged);
    }
    let policy = super::policy::load_portal_grant_overrides(
        connection,
        Some(&snapshot.portal_id),
        Some(&snapshot.participant_id),
    )?
    .into_iter()
    .next();
    match (
        policy.as_ref(),
        snapshot.policy_version,
        snapshot.policy_fingerprint.as_ref(),
    ) {
        (None, None, None) => {}
        (Some(policy), Some(version), Some(fingerprint))
            if policy.version == version
                && super::super::policy::policy_record_fingerprint(policy)? == *fingerprint => {}
        _ => return Err(AuthorizationStateError::PortalPolicyChanged),
    }
    for (group_key, version) in &snapshot.capability_group_versions {
        let Some(group) = super::policy::load_capability_group(connection, group_key)? else {
            return Err(AuthorizationStateError::PortalPolicyChanged);
        };
        if group.version != *version
            || super::super::policy::policy_record_fingerprint(&group)?
                != group_fingerprints[group_key]
        {
            return Err(AuthorizationStateError::PortalPolicyChanged);
        }
    }
    Ok(())
}

fn verify_consent_authority_preconditions(
    transaction: &Connection,
    authority: &super::super::ConsentAuthorityPreconditions,
) -> Result<(), AuthorizationStateError> {
    if let Some(policy) = &authority.policy {
        verify_portal_policy_snapshot(transaction, policy)?;
    }
    let now = i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| AuthorizationStateError::InvalidRecord("current time overflow".to_owned()))?;
    for expected in &authority.bindings {
        let current = load_grant_binding(
            transaction,
            expected.owner_kind,
            &expected.owner_id,
            &expected.participant_id,
        )?
        .ok_or(AuthorizationStateError::StorageConflict)?;
        if current.revision != expected.revision
            || current.state != super::super::GrantBindingState::Active
            || current.expires_at != expected.expires_at
            || current.expires_at.is_some_and(|expiry| expiry <= now)
            || current.delegation_ceiling != expected.delegation_ceiling
            || current.provenance != expected.provenance
        {
            return Err(AuthorizationStateError::StorageConflict);
        }
    }
    Ok(())
}

#[cfg(test)]
mod package_evidence_tests {
    use super::*;
    use crate::platform::auth::authority::AuthorityEvidenceRepository;
    use crate::platform::auth::evidence::{PackageEvidenceInput, ParticipantRuntimeProjection};
    use crate::platform::auth::policy::resolve_authority;
    use crate::platform::auth::sqlite::deployments::insert_deployment_profile;
    use crate::platform::auth::{
        portal_policy_snapshot, resolve_api_bindings, resolve_portal_authority_selection,
        ApprovalMode, ConsentAuthorityPreconditions, ConsentBindingPrecondition, DelegationCeiling,
        DeploymentProfileRecord, DeploymentProfileState, GrantRepository, LoginPortalMutation,
        LoginPortalRecord, LoginSettingsRecord, OutboxRepository, ParticipantBindingState,
        PortalGrantOverrideRecord, PortalGrantProvenance, PortalRepository, PrincipalKind,
        ProviderLoginAttributes, ProvisionedIdentityKind, ProvisionedIdentityRecord,
        ProvisionedIdentityState, SessionRepository,
    };
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use trellis_idl::project::{GenerateConfig, PackageManifest, PackageMetadata};
    use trellis_idl::{
        canonical_package, compile_project, CanonicalMode, PackageEvidence, PackageSourceEvidence,
        SourceUnit,
    };

    fn binding(display_name: &str, resolved_at: i64) -> ParticipantBindingRecord {
        let projection = ParticipantRuntimeProjection {
            participant_id: "example.Service".to_owned(),
            participant_kind: ParticipantKind::Service,
            display_name: display_name.to_owned(),
            implemented_apis: BTreeMap::new(),
            referenced_apis: BTreeMap::new(),
            resources: BTreeMap::new(),
            required_grants: trellis_protocol::GrantSet::new(Vec::new()),
            optional_grant_bundles: BTreeMap::new(),
            required_capabilities: Vec::new(),
            optional_capability_definitions: BTreeMap::new(),
            companion_participant_id: None,
            companion_participant_kind: None,
            companion_required: false,
        };
        ParticipantBindingRecord {
            participant_id: projection.participant_id.clone(),
            participant_kind: projection.participant_kind,
            participant_digest: "a".repeat(43),
            needs_digest: trellis_protocol::digest_json(
                &serde_json::to_value(&projection).expect("projection value"),
            )
            .expect("projection digest"),
            package_digest: "p".repeat(43),
            evidence_digest: URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(b"{}")),
            participant_path: "Service".to_owned(),
            projection,
            resolved_at,
            state: ParticipantBindingState::Resolved,
            error: None,
        }
    }

    fn package_evidence(source: &str) -> PackageEvidence {
        package_evidence_version(source, "1.0.0")
    }

    fn package_evidence_version(source: &str, version: &str) -> PackageEvidence {
        let manifest = PackageManifest {
            package: PackageMetadata {
                name: "binding-test".into(),
                version: version.parse().expect("version"),
            },
            sources: BTreeMap::from([("contract".into(), "contract.trellis".into())]),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        };
        let graph = compile_project(
            &manifest,
            vec![SourceUnit {
                alias: "contract".into(),
                path: PathBuf::from("contract.trellis"),
                source: source.into(),
            }],
            BTreeMap::new(),
        )
        .expect("compile package");
        PackageEvidence {
            root_package: "binding-test".into(),
            root_digest: graph.root_digest().into(),
            packages: vec![PackageSourceEvidence {
                name: "binding-test".into(),
                version: version.parse().expect("version"),
                digest: graph.root_digest().into(),
                source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation)
                    .expect("canonical package"),
            }],
        }
    }

    #[tokio::test]
    async fn presentation_only_evidence_change_gets_a_new_installed_revision() {
        let source = "service Service {}";
        let first_evidence = package_evidence_version(source, "1.0.0");
        let second_evidence = package_evidence_version(source, "1.0.1");
        assert_eq!(first_evidence.root_digest, second_evidence.root_digest);
        let (first, first_json) = installed(first_evidence, "Service");
        let (mut second, second_json) = installed(second_evidence, "Service");
        second.resolved_at = 2;
        assert_ne!(first.evidence_digest, second.evidence_digest);

        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        store
            .run(move |connection| {
                let transaction = connection.transaction().map_err(sql_error)?;
                accept_package_evidence(
                    &transaction,
                    &first.package_digest,
                    "binding-test",
                    &first_json,
                    false,
                    None,
                    1,
                )?;
                assert_eq!(install_participant(&transaction, &first, None)?, 1);
                accept_package_evidence(
                    &transaction,
                    &second.package_digest,
                    "binding-test",
                    &second_json,
                    false,
                    None,
                    2,
                )?;
                assert_eq!(install_participant(&transaction, &second, None)?, 2);
                transaction.commit().map_err(sql_error)
            })
            .await
            .unwrap();
    }

    fn installed(
        evidence: PackageEvidence,
        participant_path: &str,
    ) -> (ParticipantBindingRecord, String) {
        ParticipantBindingRecord::from_package_evidence(
            &PackageEvidenceInput {
                package_digest: evidence.root_digest.clone(),
                package_evidence: evidence,
                participant_path: participant_path.to_owned(),
            },
            1,
        )
        .expect("installed participant")
    }

    fn deployment(
        deployment_id: &str,
        participant_id: &str,
        state: DeploymentProfileState,
    ) -> DeploymentProfileRecord {
        DeploymentProfileRecord {
            deployment_id: deployment_id.to_owned(),
            kind: PrincipalKind::Service,
            display_name: deployment_id.to_owned(),
            participant_id: Some(participant_id.to_owned()),
            portal_id: None,
            review_mode: None,
            requires_device_delegation: false,
            expires_at: None,
            state,
            created_at: 1,
            updated_at: 1,
            version: 1,
        }
    }

    #[tokio::test]
    async fn stale_companion_review_rolls_back_grant_replacement() {
        const NOW: i64 = 1_700_000_000_000;
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, NOW,
            )
            .await
            .unwrap();
        let participant =
            crate::platform::auth::builtins::console_participant_binding(NOW).unwrap();
        let participant_id = participant.participant_id.clone();
        store
            .put_participant_binding(participant.clone())
            .await
            .unwrap();
        let grants = participant.projection.required_grants.clone();
        let replacement = |expected_revision, digest| GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: actor.principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: grants.clone(),
            approval_mode: super::super::super::ApprovalMode::Exact,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: super::super::super::DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(grants.clone()),
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: digest,
            companion_approved: false,
            platform_privileges: Vec::new(),
            expected_revision,
            expected_current_installed_revision: Some(1),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
        };
        let idempotency = |purpose: &str, digest: &str| IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!([purpose])).unwrap(),
            purpose: purpose.to_owned(),
            signer_id: actor.principal_id.clone(),
            request_id: "review-stale".to_owned(),
            request_digest: digest.to_owned(),
            result: Value::Null,
            created_at: NOW,
            expires_at: NOW + 60_000,
        };
        store
            .set_grant_binding(
                replacement(0, "A".repeat(43)),
                idempotency("binding.initial", &"B".repeat(43)),
            )
            .await
            .unwrap();
        store
            .run(|connection| {
                connection
                    .execute(
                        "INSERT INTO auth_principals (principal_id, kind, state, created_at, updated_at, version) VALUES ('device-stale', 'device', 'active', ?1, ?1, 1)",
                        [NOW],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_deployments (deployment_id, participant_id, participant_kind, state) VALUES ('deployment-stale', 'device-participant', 'device', 'active')",
                        [],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_instances (instance_id, deployment_id, principal_id, state, created_at, updated_at, version) VALUES ('instance-stale', 'deployment-stale', 'device-stale', 'active', ?1, ?1, 1)",
                        [NOW],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_devices (principal_id, deployment_id, state, created_at, updated_at, version) VALUES ('device-stale', 'deployment-stale', 'pending', ?1, ?1, 1)",
                        [NOW],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_device_activation_reviews (review_id, principal_id, deployment_id, instance_id, request_digest, payload_json, state, requested_at, expires_at, version) VALUES ('review-stale', 'device-stale', 'deployment-stale', 'instance-stale', ?1, '{}', 'pending', ?2, ?3, 1)",
                        params!["C".repeat(43), NOW - 2, NOW - 1],
                    )
                    .map(|_| ())
                    .map_err(sql_error)
            })
            .await
            .unwrap();
        let stale_digest = trellis_protocol::digest_json(&json!(["stale-request"])).unwrap();
        assert_eq!(
            store
                .apply_companion_activation_decision(
                    Some(replacement(1, "E".repeat(43))),
                    None,
                    ActivationReviewDecision {
                        review_id: "review-stale".to_owned(),
                        expected_version: 1,
                        state: DeviceActivationReviewState::Approved,
                        decided_at: NOW,
                        decided_by: actor.principal_id.clone(),
                        reason: None,
                        delegation: None,
                        companion_session: None,
                        activate_device: true,
                        idempotency: idempotency(
                            "device.companion-activation.approve",
                            &stale_digest,
                        ),
                        actions: Vec::new(),
                    },
                )
                .await,
            Err(AuthorizationStateError::StorageConflict)
        );
        let unchanged = store
            .get_grant_binding(
                GrantOwnerKind::User,
                actor.principal_id.clone(),
                participant_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.revision, 1);
        assert_eq!(unchanged.approval_decision_digest, "A".repeat(43));
        assert!(store
            .get_idempotency_result(
                "device.companion-activation.approve",
                &actor.principal_id,
                "review-stale",
            )
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn portal_consent_is_bounded_and_policy_changes_preserve_provenance() {
        const NOW: i64 = 1_700_000_000_000;
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, NOW,
            )
            .await
            .unwrap();
        let evidence = package_evidence(
            r#"
model Empty {}
api access@v1 {
  title "Access";
  description "Access API.";
  rpc A { input Empty; output Empty; }
  rpc B { input Empty; output Empty; }
  rpc Public { input Empty; output Empty; }
  capabilities {
    public { allows { rpc Public; } }
    capability a {
      title "A";
      description "Use A.";
      consequence "A is used.";
      allows { rpc A; }
    }
    capability b {
      title "B";
      description "Use B.";
      consequence "B is used.";
      allows { rpc B; }
    }
  }
}
app Client { use access { rpc A; rpc B; rpc Public; optional capability b; } }
device Device { app Companion { use access { rpc B; optional capability b; } } }
"#,
        );
        let (participant, evidence_json) = installed(evidence.clone(), "Client");
        let participant_id = participant.participant_id.clone();
        store
            .install_participant(
                actor.clone(),
                participant.clone(),
                "binding-test".to_owned(),
                evidence_json,
                false,
                (0, proof("participant.consent.install", NOW)),
            )
            .await
            .unwrap();

        let (device, _) = installed(evidence, "Device");
        assert!(device.projection.companion_participant_id.is_some());
        assert!(
            crate::platform::auth::policy::participant_delegation_ceiling(&device)
                .unwrap()
                .capabilities
                .is_empty()
        );

        let portal = LoginPortalRecord {
            portal_id: "portal-consent".to_owned(),
            display_name: "Portal".to_owned(),
            entry_url: None,
            builtin: false,
            disabled: false,
            removed: false,
            local_registration_enabled: false,
            provider_ids: vec!["local".to_owned()],
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        let settings = LoginSettingsRecord {
            portal_id: portal.portal_id.clone(),
            default_provider_id: Some("local".to_owned()),
            local_login_enabled: true,
            federated_registration_enabled: false,
            provider_selection_enabled: false,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_login_portal(LoginPortalMutation {
                portal: portal.clone(),
                settings: settings.clone(),
                expected_version: None,
                idempotency: proof("portal.consent.create", NOW),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        let capability_a = participant
            .projection
            .referenced_apis
            .values()
            .flat_map(|api| api.capabilities.keys())
            .find(|id| id.ends_with("::a"))
            .unwrap()
            .clone();
        let policy = PortalGrantOverrideRecord {
            portal_id: portal.portal_id.clone(),
            participant_id: participant_id.clone(),
            direct_capabilities: vec![capability_a],
            capability_group_keys: Vec::new(),
            role_mappings: Vec::new(),
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_portal_grant_override(policy.clone(), None, proof("portal.consent.policy", NOW))
            .await
            .unwrap();
        let selection = resolve_portal_authority_selection(
            &policy,
            &BTreeMap::new(),
            &participant,
            &ProviderLoginAttributes {
                provider_id: "local".to_owned(),
                roles: Vec::new(),
            },
        )
        .unwrap();
        let resolved = resolve_authority(
            &participant,
            ApprovalMode::Capabilities,
            &selection.ceiling.capabilities,
            &[],
            &[],
            &selection.ceiling,
            (&[], true),
        )
        .unwrap();
        let granted_actions = resolved
            .exact_grants
            .permissions()
            .iter()
            .filter_map(|permission| {
                permission
                    .target()
                    .as_api_surface()
                    .map(|(_, _, name)| name)
            })
            .collect::<Vec<_>>();
        assert!(granted_actions.contains(&"A"));
        assert!(granted_actions.contains(&"Public"));
        assert!(!granted_actions.contains(&"B"));

        let a_atom = resolved
            .exact_grants
            .permissions()
            .iter()
            .find(|permission| {
                permission
                    .target()
                    .as_api_surface()
                    .is_some_and(|(_, _, name)| name == "A")
            })
            .unwrap()
            .clone();
        let exact = resolve_authority(
            &participant,
            ApprovalMode::Exact,
            &[],
            &[],
            &[],
            &DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(trellis_protocol::GrantSet::new(vec![a_atom])),
                platform_privileges: Vec::new(),
            },
            (&[], true),
        )
        .unwrap();
        assert_eq!(exact.exact_grants.permissions().len(), 1);

        let snapshot = portal_policy_snapshot(
            &portal,
            &settings,
            &participant_id,
            Some(&policy),
            &BTreeMap::new(),
        )
        .unwrap();
        let provenance = PortalGrantProvenance {
            portal_id: portal.portal_id.clone(),
            provider_id: "local".to_owned(),
            roles: Vec::new(),
            effective_policy_digest: selection.effective_policy_digest,
        };
        let replacement = GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: actor.principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: trellis_protocol::GrantSet::new(Vec::new()),
            approval_mode: ApprovalMode::Capabilities,
            approved_capabilities: selection.ceiling.capabilities.clone(),
            approved_resources: Vec::new(),
            delegation_ceiling: selection.ceiling,
            approval_decision_digest: trellis_protocol::digest_json(&json!(["consent"])).unwrap(),
            companion_approved: false,
            platform_privileges: Vec::new(),
            expected_revision: 0,
            expected_current_installed_revision: Some(1),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: Some(provenance.clone()),
        };
        store
            .set_portal_grant_binding(
                replacement.clone(),
                snapshot.clone(),
                proof("portal.consent.accept", NOW),
            )
            .await
            .unwrap();
        let mut repeated = replacement.clone();
        repeated.expected_revision = 1;
        repeated.approval_decision_digest =
            trellis_protocol::digest_json(&json!(["repeated-consent"])).unwrap();
        let (unchanged, actions) = store
            .run(move |connection| replace_grant_binding(connection, repeated, NOW + 1))
            .await
            .unwrap();
        assert_eq!(unchanged.revision, 1);
        assert!(actions.is_empty());

        let mut reduced_policy = policy;
        reduced_policy.direct_capabilities.clear();
        reduced_policy.updated_at += 1;
        reduced_policy.version += 1;
        store
            .put_portal_grant_override(
                reduced_policy,
                Some(1),
                proof("portal.consent.reduce", NOW + 1),
            )
            .await
            .unwrap();
        let mut stale_replacement = replacement;
        stale_replacement.expected_revision = 1;
        stale_replacement.provenance = None;
        assert_eq!(
            store
                .set_portal_grant_binding(
                    stale_replacement,
                    snapshot,
                    proof("portal.consent.stale", NOW + 2),
                )
                .await,
            Err(AuthorizationStateError::PortalPolicyChanged)
        );
        let unchanged = store
            .get_grant_binding(GrantOwnerKind::User, actor.principal_id, participant_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.revision, 1);
        assert_eq!(unchanged.provenance, Some(provenance));
    }

    #[tokio::test]
    async fn non_reducing_grant_replacement_keeps_live_contexts_while_a_reduction_kicks() {
        use trellis_protocol::GrantSet;

        let now: i64 = 1_700_000_000_000;
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, now,
            )
            .await
            .unwrap();
        let current = store
            .get_grant_binding(
                actor.owner_kind,
                actor.owner_id.clone(),
                actor.participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();

        // A replacement that does not reduce authority must leave the active
        // context alone: no revocation, no kick.
        let mut additive = binding_replacement_from(&current);
        additive.approval_decision_digest =
            trellis_protocol::digest_json(&json!(["additive"])).unwrap();
        let (additive_binding, actions) = store
            .run(move |connection| replace_grant_binding(connection, additive, now + 1))
            .await
            .unwrap();
        assert_eq!(additive_binding.revision, current.revision + 1);
        assert!(
            !actions
                .iter()
                .any(|action| action.kind == PostCommitActionKind::Kick),
            "adding authority must not kick live connections"
        );

        let mut reduction = binding_replacement_from(&additive_binding);
        reduction.grants = GrantSet::new(Vec::new());
        reduction.delegation_ceiling.exact_restrictions = Some(GrantSet::new(Vec::new()));
        let (_, actions) = store
            .run(move |connection| replace_grant_binding(connection, reduction, now + 2))
            .await
            .unwrap();
        assert!(
            actions
                .iter()
                .any(|action| action.kind == PostCommitActionKind::Kick),
            "removing authority must revoke and kick live connections"
        );
    }

    fn binding_replacement_from(binding: &GrantBinding) -> GrantBindingReplacement {
        GrantBindingReplacement {
            owner_kind: binding.owner_kind,
            owner_id: binding.owner_id.clone(),
            participant_id: binding.participant_id.clone(),
            installed_revision: binding.installed_revision,
            grants: binding.grants.clone(),
            approval_mode: binding.approval_mode,
            approved_capabilities: binding.approved_capabilities.clone(),
            approved_resources: binding.approved_resources.clone(),
            delegation_ceiling: binding.delegation_ceiling.clone(),
            approval_decision_digest: "A".repeat(43),
            companion_approved: binding.companion_approved,
            platform_privileges: binding.platform_privileges.clone(),
            state: binding.state,
            expires_at: binding.expires_at,
            provenance: binding.provenance.clone(),
            expected_revision: binding.revision,
            expected_current_installed_revision: Some(binding.installed_revision),
        }
    }

    #[tokio::test]
    async fn default_portal_consent_is_fenced_by_a_later_override() {
        const NOW: i64 = 1_700_000_000_000;
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, NOW,
            )
            .await
            .unwrap();
        let evidence = package_evidence(
            r#"
model Empty {}
api access@v1 {
  title "Access";
  description "Access API.";
  rpc A { input Empty; output Empty; }
  rpc Public { input Empty; output Empty; }
  capabilities {
    public { allows { rpc Public; } }
    capability a {
      title "A";
      description "Use A.";
      consequence "A is used.";
      allows { rpc A; }
    }
  }
}
app Client { use access { rpc A; rpc Public; } }
"#,
        );
        let (participant, evidence_json) = installed(evidence, "Client");
        let participant_id = participant.participant_id.clone();
        store
            .install_participant(
                actor.clone(),
                participant.clone(),
                "binding-default".to_owned(),
                evidence_json,
                false,
                (0, proof("participant.default.install", NOW)),
            )
            .await
            .unwrap();
        let portal = LoginPortalRecord {
            portal_id: "portal-default-consent".to_owned(),
            display_name: "Portal".to_owned(),
            entry_url: None,
            builtin: false,
            disabled: false,
            removed: false,
            local_registration_enabled: false,
            provider_ids: vec!["local".to_owned()],
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        let settings = LoginSettingsRecord {
            portal_id: portal.portal_id.clone(),
            default_provider_id: Some("local".to_owned()),
            local_login_enabled: true,
            federated_registration_enabled: false,
            provider_selection_enabled: false,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_login_portal(LoginPortalMutation {
                portal: portal.clone(),
                settings: settings.clone(),
                expected_version: None,
                idempotency: proof("portal.default.create", NOW),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        // No override: the decision is prepared against the default selection
        // and records the default policy snapshot (absent version/fingerprint).
        let selection = crate::platform::auth::policy::default_portal_authority_selection(
            &portal.portal_id,
            &participant,
        )
        .unwrap();
        let snapshot =
            portal_policy_snapshot(&portal, &settings, &participant_id, None, &BTreeMap::new())
                .unwrap();
        assert_eq!(snapshot.policy_version, None);
        assert_eq!(snapshot.policy_fingerprint, None);
        let resolved = resolve_authority(
            &participant,
            ApprovalMode::Capabilities,
            &selection.ceiling.capabilities,
            &[],
            &[],
            &selection.ceiling,
            (&[], true),
        )
        .unwrap();
        let replacement = GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: actor.principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: resolved.exact_grants,
            approval_mode: ApprovalMode::Capabilities,
            approved_capabilities: selection.ceiling.capabilities.clone(),
            approved_resources: Vec::new(),
            delegation_ceiling: selection.ceiling.clone(),
            approval_decision_digest: trellis_protocol::digest_json(&json!(["default-consent"]))
                .unwrap(),
            companion_approved: false,
            platform_privileges: Vec::new(),
            expected_revision: 0,
            expected_current_installed_revision: Some(1),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: Some(PortalGrantProvenance {
                portal_id: portal.portal_id.clone(),
                provider_id: "local".to_owned(),
                roles: Vec::new(),
                effective_policy_digest: selection.effective_policy_digest.clone(),
            }),
        };
        // An operator narrows the policy before the prepared decision commits.
        store
            .put_portal_grant_override(
                PortalGrantOverrideRecord {
                    portal_id: portal.portal_id.clone(),
                    participant_id: participant_id.clone(),
                    direct_capabilities: Vec::new(),
                    capability_group_keys: Vec::new(),
                    role_mappings: Vec::new(),
                    created_at: NOW,
                    updated_at: NOW,
                    version: 1,
                },
                None,
                proof("portal.default.override", NOW + 1),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .set_consent_grant_binding(
                    replacement,
                    crate::platform::auth::ConsentAuthorityPreconditions {
                        policy: Some(snapshot),
                        bindings: Vec::new(),
                    },
                    proof("portal.default.stale", NOW + 2),
                )
                .await,
            Err(AuthorizationStateError::PortalPolicyChanged)
        );
        assert!(store
            .get_grant_binding(GrantOwnerKind::User, actor.principal_id, participant_id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn carried_source_lifetime_is_revalidated_at_commit() {
        const NOW: i64 = 1_700_000_000_000;
        let wall_clock = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, NOW,
            )
            .await
            .unwrap();
        let evidence = package_evidence(
            r#"
model Empty {}
api access@v1 {
  title "Access";
  description "Access API.";
  rpc A { input Empty; output Empty; }
  rpc Public { input Empty; output Empty; }
  capabilities {
    public { allows { rpc Public; } }
    capability a {
      title "A";
      description "Use A.";
      consequence "A is used.";
      allows { rpc A; }
    }
  }
}
app Client { use access { rpc A; rpc Public; } }
"#,
        );
        let (participant, evidence_json) = installed(evidence, "Client");
        let participant_id = participant.participant_id.clone();
        store
            .install_participant(
                actor.clone(),
                participant.clone(),
                "binding-carried".to_owned(),
                evidence_json,
                false,
                (0, proof("participant.carried.install", NOW)),
            )
            .await
            .unwrap();
        let portal = LoginPortalRecord {
            portal_id: "portal-carried".to_owned(),
            display_name: "Portal".to_owned(),
            entry_url: None,
            builtin: false,
            disabled: false,
            removed: false,
            local_registration_enabled: false,
            provider_ids: vec!["local".to_owned()],
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        let settings = LoginSettingsRecord {
            portal_id: portal.portal_id.clone(),
            default_provider_id: Some("local".to_owned()),
            local_login_enabled: true,
            federated_registration_enabled: false,
            provider_selection_enabled: false,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_login_portal(LoginPortalMutation {
                portal: portal.clone(),
                settings: settings.clone(),
                expected_version: None,
                idempotency: proof("portal.carried.create", NOW),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        let selection = crate::platform::auth::policy::default_portal_authority_selection(
            &portal.portal_id,
            &participant,
        )
        .unwrap();
        let mut selection = selection;
        selection.ceiling.platform_privileges = vec![PlatformPrivilege::Admin];
        let resolved = resolve_authority(
            &participant,
            ApprovalMode::Capabilities,
            &selection.ceiling.capabilities,
            &[],
            &[PlatformPrivilege::Admin],
            &selection.ceiling,
            (&[], true),
        )
        .unwrap();
        let expires_at = wall_clock + 3_600_000;
        let source = GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: actor.principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: resolved.exact_grants.clone(),
            approval_mode: ApprovalMode::Capabilities,
            approved_capabilities: selection.ceiling.capabilities.clone(),
            approved_resources: Vec::new(),
            delegation_ceiling: selection.ceiling.clone(),
            approval_decision_digest: trellis_protocol::digest_json(&json!(["carried-source"]))
                .unwrap(),
            companion_approved: false,
            platform_privileges: vec![PlatformPrivilege::Admin],
            state: GrantBindingState::Active,
            expires_at: Some(expires_at),
            provenance: Some(PortalGrantProvenance {
                portal_id: portal.portal_id.clone(),
                provider_id: "local".to_owned(),
                roles: Vec::new(),
                effective_policy_digest: selection.effective_policy_digest.clone(),
            }),
            expected_revision: 0,
            expected_current_installed_revision: Some(1),
        };
        store
            .set_grant_binding(source.clone(), proof("carried.source.create", NOW))
            .await
            .unwrap();
        let current = store
            .get_grant_binding(
                GrantOwnerKind::User,
                actor.principal_id.clone(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        // The production authority construction carries the finite source
        // lifetime and its binding precondition even with portal provenance.
        let authority = crate::platform::auth::policy::consent_authority(
            crate::platform::auth::policy::ConsentAuthoritySource::Portal(Box::new(
                crate::platform::auth::policy::PortalConsentAuthority {
                    selection: selection.clone(),
                    snapshot: portal_policy_snapshot(
                        &portal,
                        &settings,
                        &participant_id,
                        None,
                        &BTreeMap::new(),
                    )
                    .unwrap(),
                    provenance: current.provenance.clone().unwrap(),
                    source: Some(&current),
                    retained_target: None,
                },
            )),
            wall_clock,
        )
        .unwrap();
        assert_eq!(authority.expires_at, Some(expires_at));
        assert_eq!(authority.preconditions.bindings.len(), 1);
        let mut repeated = source.clone();
        repeated.expected_revision = 1;
        repeated.approval_decision_digest =
            trellis_protocol::digest_json(&json!(["repeated-consent"])).unwrap();
        let stored = store
            .set_consent_grant_binding(
                repeated.clone(),
                authority.preconditions.clone(),
                proof("carried.repeated", NOW + 1),
            )
            .await
            .unwrap();
        assert_eq!(stored["binding"]["expiresAt"].as_i64(), Some(expires_at));

        // Preparing again and changing the source before commit is rejected.
        let prepared = store
            .get_grant_binding(
                GrantOwnerKind::User,
                actor.principal_id.clone(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        let stale_authority = {
            let prepared = &prepared;
            crate::platform::auth::policy::consent_authority(
                crate::platform::auth::policy::ConsentAuthoritySource::Portal(Box::new(
                    crate::platform::auth::policy::PortalConsentAuthority {
                        selection: selection.clone(),
                        snapshot: portal_policy_snapshot(
                            &portal,
                            &settings,
                            &participant_id,
                            None,
                            &BTreeMap::new(),
                        )
                        .unwrap(),
                        provenance: prepared.provenance.clone().unwrap(),
                        source: Some(prepared),
                        retained_target: None,
                    },
                )),
                wall_clock + 2,
            )
            .unwrap()
        };
        let mut changed_source = source.clone();
        changed_source.expected_revision = prepared.revision;
        changed_source.expires_at = Some(expires_at + 60_000);
        changed_source.approval_decision_digest =
            trellis_protocol::digest_json(&json!(["changed-source"])).unwrap();
        store
            .set_grant_binding(changed_source, proof("carried.source.change", NOW + 2))
            .await
            .unwrap();
        assert_eq!(
            store
                .set_consent_grant_binding(
                    {
                        let mut stale = repeated;
                        stale.expected_revision = prepared.revision + 1;
                        stale
                    },
                    stale_authority.preconditions,
                    proof("carried.stale", NOW + 3),
                )
                .await,
            Err(AuthorizationStateError::StorageConflict)
        );
    }

    fn proof(purpose: &str, now: i64) -> IdempotencyResultRecord {
        IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!([purpose])).unwrap(),
            purpose: purpose.to_owned(),
            signer_id: "test".to_owned(),
            request_id: purpose.to_owned(),
            request_digest: trellis_protocol::digest_json(&json!({ "purpose": purpose })).unwrap(),
            result: Value::Null,
            created_at: now,
            expires_at: now + 60_000,
        }
    }

    #[tokio::test]
    async fn repeated_browser_replacement_retains_resource_grants() {
        const NOW: i64 = 1_700_000_000_000;
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, NOW,
            )
            .await
            .unwrap();
        let evidence = package_evidence(
            r#"
model Value { value: string; }
app Companion {
  kv cache {
    title "Cache";
    description "Cache.";
    schema Value;
    history 1;
    ttl 5m;
  }
}
"#,
        );
        let (participant, evidence_json) = installed(evidence, "Companion");
        let participant_id = participant.participant_id.clone();
        store
            .install_participant(
                actor.clone(),
                participant.clone(),
                "binding-test".to_owned(),
                evidence_json,
                false,
                (
                    0,
                    IdempotencyResultRecord {
                        scope_key: trellis_protocol::digest_json(&json!(["resource-install"]))
                            .unwrap(),
                        purpose: "participant.install".to_owned(),
                        signer_id: actor.principal_id.clone(),
                        request_id: "resource-install".to_owned(),
                        request_digest: "E".repeat(43),
                        result: Value::Null,
                        created_at: NOW,
                        expires_at: NOW + 60_000,
                    },
                ),
            )
            .await
            .unwrap();
        let approved_resources =
            crate::platform::auth::policy::participant_resource_commitments(&participant).unwrap();
        let resource_evidence: super::super::super::ResourceBindingEvidence =
            serde_json::from_value(json!({
            "resourceKind": "kv",
            "localName": "cache",
            "bindingId": "resource-cache",
            "ownerParticipantId": participant_id,
            "providerIdentity": {"kind": "kv", "bucket": "resource-cache"},
            "actual": {"kind": "kv", "history": 1, "ttl_ms": 300000, "max_value_bytes": null},
            "state": "available",
            "materializedAt": NOW,
            "error": null,
            }))
            .unwrap();
        store
            .replace_resource_bindings(
                GrantOwnerKind::User,
                actor.principal_id.clone(),
                participant_id.clone(),
                1,
                vec![resource_evidence.clone()],
            )
            .await
            .unwrap();
        let ceiling =
            crate::platform::auth::policy::participant_delegation_ceiling(&participant).unwrap();
        let replacement = GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: actor.principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: trellis_protocol::GrantSet::new(Vec::new()),
            approval_mode: super::super::super::ApprovalMode::Capabilities,
            approved_capabilities: ceiling.capabilities.clone(),
            approved_resources: approved_resources.clone(),
            delegation_ceiling: ceiling,
            approval_decision_digest: trellis_protocol::digest_json(&json!(["browser-approval"]))
                .unwrap(),
            companion_approved: false,
            platform_privileges: Vec::new(),
            expected_revision: 0,
            expected_current_installed_revision: Some(1),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
        };
        let idempotency = IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!(["browser-resource"])).unwrap(),
            purpose: "browser.grant.accept".to_owned(),
            signer_id: "browser".to_owned(),
            request_id: "browser-resource".to_owned(),
            request_digest: trellis_protocol::digest_json(&json!(["browser-request"])).unwrap(),
            result: Value::Null,
            created_at: NOW,
            expires_at: NOW + 60_000,
        };
        let applied = store
            .set_grant_binding(replacement.clone(), idempotency.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .set_grant_binding(replacement, idempotency)
                .await
                .unwrap(),
            applied
        );
        let binding = store
            .get_grant_binding(GrantOwnerKind::User, actor.principal_id, participant_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(binding.approved_resources, approved_resources);
        assert!(binding
            .grants
            .permissions()
            .iter()
            .any(|permission| matches!(
                permission.target(),
                PermissionTarget::ParticipantResource { name, .. } if name == "cache"
            )));
        store
            .run(|connection| {
                connection
                    .execute("DELETE FROM auth_post_commit_actions", [])
                    .map(|_| ())
                    .map_err(sql_error)
            })
            .await
            .unwrap();

        let portal = LoginPortalRecord {
            portal_id: "portal-resource".to_owned(),
            display_name: "Portal".to_owned(),
            entry_url: None,
            builtin: false,
            disabled: false,
            removed: false,
            local_registration_enabled: false,
            provider_ids: vec!["local".to_owned()],
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        let settings = LoginSettingsRecord {
            portal_id: portal.portal_id.clone(),
            default_provider_id: Some("local".to_owned()),
            local_login_enabled: true,
            federated_registration_enabled: false,
            provider_selection_enabled: false,
            updated_at: NOW,
            version: 1,
        };
        let proof = |purpose: &str| IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!([purpose])).unwrap(),
            purpose: purpose.to_owned(),
            signer_id: "portal".to_owned(),
            request_id: purpose.to_owned(),
            request_digest: trellis_protocol::digest_json(&json!({"purpose": purpose})).unwrap(),
            result: Value::Null,
            created_at: NOW,
            expires_at: NOW + 60_000,
        };
        store
            .put_login_portal(LoginPortalMutation {
                portal: portal.clone(),
                settings: settings.clone(),
                expected_version: None,
                idempotency: proof("portal.resource.create"),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        let policy = PortalGrantOverrideRecord {
            portal_id: portal.portal_id.clone(),
            participant_id: binding.participant_id.clone(),
            direct_capabilities: Vec::new(),
            capability_group_keys: Vec::new(),
            role_mappings: Vec::new(),
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        store
            .put_portal_grant_override(policy.clone(), None, proof("portal.resource.policy"))
            .await
            .unwrap();
        let snapshot = crate::platform::auth::portal_policy_snapshot(
            &portal,
            &settings,
            &binding.participant_id,
            Some(&policy),
            &BTreeMap::new(),
        )
        .unwrap();
        store
            .replace_resource_bindings(
                GrantOwnerKind::User,
                binding.owner_id.clone(),
                binding.participant_id.clone(),
                binding.installed_revision,
                vec![resource_evidence.clone()],
            )
            .await
            .unwrap();
        store
            .set_portal_grant_binding(
                GrantBindingReplacement {
                    owner_kind: binding.owner_kind,
                    owner_id: binding.owner_id.clone(),
                    participant_id: binding.participant_id.clone(),
                    installed_revision: binding.installed_revision,
                    grants: trellis_protocol::GrantSet::new(Vec::new()),
                    approval_mode: binding.approval_mode,
                    approved_capabilities: Vec::new(),
                    approved_resources: binding.approved_resources.clone(),
                    delegation_ceiling: binding.delegation_ceiling.clone(),
                    approval_decision_digest: trellis_protocol::digest_json(&json!([
                        "portal-approval"
                    ]))
                    .unwrap(),
                    companion_approved: false,
                    platform_privileges: Vec::new(),
                    expected_revision: binding.revision,
                    expected_current_installed_revision: Some(binding.installed_revision),
                    state: GrantBindingState::Active,
                    expires_at: None,
                    provenance: None,
                },
                snapshot,
                proof("portal.resource.replace"),
            )
            .await
            .unwrap();
        let portal_binding = store
            .get_grant_binding(
                GrantOwnerKind::User,
                binding.owner_id,
                binding.participant_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(portal_binding.approved_resources, approved_resources);
        assert!(portal_binding
            .grants
            .permissions()
            .iter()
            .any(|permission| matches!(
                permission.target(),
                PermissionTarget::ParticipantResource { name, .. } if name == "cache"
            )));
        store
            .run(|connection| {
                connection
                    .execute("DELETE FROM auth_post_commit_actions", [])
                    .map(|_| ())
                    .map_err(sql_error)
            })
            .await
            .unwrap();

        let installation_key = SigningKey::from_bytes(&[9; 32]);
        let installation_public_key = URL_SAFE_NO_PAD.encode(installation_key.verifying_key());
        store
            .run(|connection| {
                connection
                    .execute(
                        "INSERT INTO auth_principals (principal_id, kind, state, created_at, updated_at, version) VALUES ('device-resource', 'device', 'active', ?1, ?1, 1)",
                        [NOW],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_deployments (deployment_id, participant_id, participant_kind, state) VALUES ('deployment-resource', 'device-participant', 'device', 'active')",
                        [],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_instances (instance_id, deployment_id, principal_id, state, created_at, updated_at, version) VALUES ('instance-resource', 'deployment-resource', 'device-resource', 'active', ?1, ?1, 1)",
                        [NOW],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_devices (principal_id, deployment_id, state, created_at, updated_at, version) VALUES ('device-resource', 'deployment-resource', 'pending', ?1, ?1, 1)",
                        [NOW],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_device_activation_reviews (review_id, principal_id, deployment_id, instance_id, request_digest, payload_json, state, requested_at, expires_at, version) VALUES ('review-resource', 'device-resource', 'deployment-resource', 'instance-resource', ?1, '{}', 'pending', ?2, ?3, 1)",
                        params![trellis_protocol::digest_json(&json!(["activation-request"])).unwrap(), NOW - 1, NOW + 60_000],
                    )
                    .map(|_| ())
                    .map_err(sql_error)
            })
            .await
            .unwrap();
        let session_id = "01J00000000000000000000000".to_owned();
        let command = ActivationReviewDecision {
            review_id: "review-resource".to_owned(),
            expected_version: 1,
            state: DeviceActivationReviewState::Approved,
            decided_at: NOW,
            decided_by: portal_binding.owner_id.clone(),
            reason: None,
            delegation: Some(DeviceDelegationRecord {
                principal_id: "device-resource".to_owned(),
                deployment_id: "deployment-resource".to_owned(),
                companion_participant_id: Some(portal_binding.participant_id.clone()),
                user_login_session_id: Some(session_id.clone()),
                installation_public_key: Some(installation_public_key.clone()),
                device_grant_revision: Some(1),
                child_grant_revision: Some(portal_binding.revision + 1),
                required: true,
                state: DeviceDelegationState::Active,
                expires_at: None,
            }),
            companion_session: Some(SessionRecord {
                session_id,
                principal_id: portal_binding.owner_id.clone(),
                participant_id: portal_binding.participant_id.clone(),
                participant_kind: ParticipantKind::App,
                session_key_id: crate::platform::auth::validate_ed25519_public_key(
                    "installationPublicKey",
                    &installation_public_key,
                )
                .unwrap(),
                session_public_key: installation_public_key,
                state: super::super::super::SessionState::Active,
                created_at: NOW,
                last_authenticated_at: NOW,
                expires_at: None,
                revoked_at: None,
                version: 1,
            }),
            activate_device: true,
            idempotency: proof("device.companion-activation.approve"),
            actions: Vec::new(),
        };
        let aggregate_replacement = GrantBindingReplacement {
            owner_kind: portal_binding.owner_kind,
            owner_id: portal_binding.owner_id.clone(),
            participant_id: portal_binding.participant_id.clone(),
            installed_revision: portal_binding.installed_revision,
            grants: trellis_protocol::GrantSet::new(Vec::new()),
            approval_mode: portal_binding.approval_mode,
            approved_capabilities: portal_binding.approved_capabilities.clone(),
            approved_resources: portal_binding.approved_resources.clone(),
            delegation_ceiling: portal_binding.delegation_ceiling.clone(),
            approval_decision_digest: trellis_protocol::digest_json(&json!(["companion-approval"]))
                .unwrap(),
            companion_approved: false,
            platform_privileges: Vec::new(),
            expected_revision: portal_binding.revision,
            expected_current_installed_revision: Some(portal_binding.installed_revision),
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
        };
        store
            .replace_resource_bindings(
                GrantOwnerKind::User,
                portal_binding.owner_id.clone(),
                portal_binding.participant_id.clone(),
                portal_binding.installed_revision,
                vec![resource_evidence],
            )
            .await
            .unwrap();
        let applied = store
            .apply_companion_activation_decision(Some(aggregate_replacement), None, command.clone())
            .await
            .unwrap();
        let established = store
            .get_device_delegation("device-resource", "deployment-resource")
            .await
            .unwrap()
            .unwrap();
        let established_session = store
            .get_session(established.user_login_session_id.as_deref().unwrap())
            .await
            .unwrap();
        assert_eq!(
            store
                .apply_companion_activation_decision(None, None, command.clone())
                .await
                .unwrap(),
            applied
        );
        assert_eq!(
            store
                .get_device_delegation("device-resource", "deployment-resource")
                .await
                .unwrap(),
            Some(established)
        );
        assert_eq!(
            store
                .get_session("01J00000000000000000000000")
                .await
                .unwrap(),
            established_session
        );
        let mut collision = command;
        collision.idempotency.request_digest =
            trellis_protocol::digest_json(&json!(["changed-companion-approval"])).unwrap();
        assert_eq!(
            store
                .apply_companion_activation_decision(None, None, collision)
                .await,
            Err(AuthorizationStateError::StorageConflict)
        );
        let replayed_binding = store
            .get_grant_binding(
                GrantOwnerKind::User,
                portal_binding.owner_id.clone(),
                portal_binding.participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replayed_binding.revision, 3);
        assert_eq!(replayed_binding.approved_resources, approved_resources);
        store
            .set_grant_binding(
                GrantBindingReplacement {
                    owner_kind: replayed_binding.owner_kind,
                    owner_id: replayed_binding.owner_id,
                    participant_id: replayed_binding.participant_id,
                    installed_revision: replayed_binding.installed_revision,
                    grants: replayed_binding.grants,
                    approval_mode: replayed_binding.approval_mode,
                    approved_capabilities: replayed_binding.approved_capabilities,
                    approved_resources: replayed_binding.approved_resources,
                    delegation_ceiling: replayed_binding.delegation_ceiling,
                    approval_decision_digest: replayed_binding.approval_decision_digest,
                    companion_approved: replayed_binding.companion_approved,
                    platform_privileges: replayed_binding.platform_privileges,
                    expected_revision: replayed_binding.revision,
                    expected_current_installed_revision: Some(replayed_binding.installed_revision),
                    state: replayed_binding.state,
                    expires_at: replayed_binding.expires_at,
                    provenance: replayed_binding.provenance,
                },
                IdempotencyResultRecord {
                    scope_key: trellis_protocol::digest_json(&json!(["delegation-cascade"]))
                        .unwrap(),
                    purpose: "grant.delegation-cascade".to_owned(),
                    signer_id: "cascade".to_owned(),
                    request_id: "delegation-cascade".to_owned(),
                    request_digest: trellis_protocol::digest_json(&json!(["cascade-request"]))
                        .unwrap(),
                    result: Value::Null,
                    created_at: NOW,
                    expires_at: NOW + 60_000,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .get_device_delegation("device-resource", "deployment-resource")
                .await
                .unwrap()
                .unwrap()
                .child_grant_revision,
            Some(4)
        );
    }

    #[tokio::test]
    async fn bootstrap_binding_stays_sticky_then_reselects_compatible_provider() {
        const API: &str = r#"
model Input { value: string; }
model Output { value: string; }
api orders@v1 {
  title "Orders"; description "Orders."; version "1.0.0";
  rpc Required { input Input; output Output; }
  capabilities { public { allows { rpc Required; } } }
}
"#;
        let consumer = installed(
            package_evidence(&format!(
                "{API}\nservice Consumer {{ use orders {{ rpc Required; }} }}"
            )),
            "Consumer",
        );
        let provider_z = installed(
            package_evidence(&format!(
                "{API}\nservice ProviderZ {{ implements orders; }}"
            )),
            "ProviderZ",
        );
        let provider_a = installed(
            package_evidence(&format!(
                "{API}\nservice ProviderA {{ implements orders; }}"
            )),
            "ProviderA",
        );
        let missing = installed(
            package_evidence(
                r#"
model Input { value: string; }
model Output { value: string; }
api orders@v1 {
  title "Orders"; description "Orders."; version "1.0.1";
  rpc Other { input Input; output Output; }
  capabilities { public { allows { rpc Other; } } }
}
service Missing { implements orders; }
"#,
            ),
            "Missing",
        );
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        for (record, evidence_json) in [&consumer, &provider_z, &provider_a, &missing] {
            store
                .run({
                    let record = record.clone();
                    let evidence_json = evidence_json.clone();
                    move |connection| {
                        accept_package_evidence(
                            connection,
                            &record.package_digest,
                            "binding-test",
                            &evidence_json,
                            false,
                            None,
                            1,
                        )?;
                        install_participant(connection, &record, Some(0))?;
                        Ok(())
                    }
                })
                .await
                .expect("install participant");
        }
        store
            .run({
                let z = deployment(
                    "provider-z",
                    &provider_z.0.participant_id,
                    DeploymentProfileState::Active,
                );
                let a = deployment(
                    "provider-a",
                    &provider_a.0.participant_id,
                    DeploymentProfileState::Active,
                );
                let absent_action = deployment(
                    "provider-missing",
                    &missing.0.participant_id,
                    DeploymentProfileState::Active,
                );
                move |connection| {
                    for id in ["provider-z", "provider-a", "provider-missing"] {
                        connection
                            .execute(
                                "INSERT INTO auth_principals (principal_id, kind, state, created_at, updated_at, version) VALUES (?1, 'service', 'active', 1, 1, 1)",
                                [id],
                            )
                            .map_err(map_write_error)?;
                    }
                    insert_deployment_profile(connection, &z)?;
                    insert_deployment_profile(connection, &a)?;
                    insert_deployment_profile(connection, &absent_action)?;
                    Ok(())
                }
            })
            .await
            .expect("initial providers");

        let api = "binding-test.orders@v1";
        let selected = resolve_api_bindings(&store, &consumer.0, Some("consumer-one"))
            .await
            .expect("initial binding");
        assert_eq!(selected[api].provider_deployment_id, "provider-a");

        store
            .run({
                let lower = deployment(
                    "provider-0",
                    &provider_a.0.participant_id,
                    DeploymentProfileState::Active,
                );
                move |connection| {
                    connection
                        .execute(
                            "INSERT INTO auth_principals (principal_id, kind, state, created_at, updated_at, version) VALUES ('provider-0', 'service', 'active', 1, 1, 1)",
                            [],
                        )
                        .map_err(map_write_error)?;
                    insert_deployment_profile(connection, &lower)
                }
            })
            .await
            .expect("lower provider");
        let refreshed = resolve_api_bindings(&store, &consumer.0, Some("consumer-one"))
            .await
            .expect("sticky refresh");
        assert_eq!(refreshed[api].provider_deployment_id, "provider-a");

        store
            .run(|connection| {
                connection
                    .execute(
                        "UPDATE auth_deployment_profiles SET state = 'disabled' WHERE deployment_id = 'provider-a'",
                        [],
                    )
                    .map_err(map_write_error)?;
                Ok(())
            })
            .await
            .expect("disable provider A");
        let reselected = resolve_api_bindings(&store, &consumer.0, Some("consumer-one"))
            .await
            .expect("safe reselection");
        assert_eq!(reselected[api].provider_deployment_id, "provider-0");
        assert!(!reselected
            .values()
            .any(|binding| binding.provider_deployment_id == "provider-a"));
        assert_eq!(
            store
                .get_api_binding("consumer-one", api)
                .await
                .expect("persisted binding"),
            Some("provider-0".to_owned())
        );
        assert_eq!(
            store
                .get_api_binding("consumer-two", api)
                .await
                .expect("other consumer binding"),
            None
        );
    }

    #[tokio::test]
    async fn api_bindings_are_scoped_to_consumer_deployment_and_api() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        store
            .put_api_binding("consumer-z", "orders@v1", "provider-z")
            .await
            .expect("initial binding");
        store
            .put_api_binding("consumer-a", "orders@v1", "provider-a")
            .await
            .expect("other consumer binding");
        store
            .put_api_binding("consumer-z", "billing@v1", "provider-b")
            .await
            .expect("other API binding");

        assert_eq!(
            store
                .get_api_binding("consumer-z", "orders@v1")
                .await
                .expect("consumer binding"),
            Some("provider-z".to_owned())
        );
        assert_eq!(
            store
                .get_api_binding("consumer-a", "orders@v1")
                .await
                .expect("other consumer binding"),
            Some("provider-a".to_owned())
        );
        assert_eq!(
            store
                .get_api_binding("consumer-z", "billing@v1")
                .await
                .expect("other API binding"),
            Some("provider-b".to_owned())
        );
    }

    #[tokio::test]
    async fn fresh_schema_retains_native_revision_history_with_cas() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        store
            .run(|connection| {
                accept_package_evidence(
                    connection,
                    &"p".repeat(43),
                    "example",
                    "{}",
                    false,
                    None,
                    1,
                )?;
                assert_eq!(
                    install_participant(connection, &binding("v1", 1), Some(0))?,
                    1
                );
                assert_eq!(
                    install_participant(connection, &binding("v2", 2), Some(1))?,
                    2
                );
                assert_eq!(
                    install_participant(connection, &binding("v3", 3), Some(1)),
                    Err(AuthorizationStateError::RevisionConflict {
                        expected: 1,
                        current: 2,
                    })
                );
                assert_eq!(
                    load_installed_participant(connection, "example.Service", Some(1))?
                        .expect("revision one")
                        .1
                        .projection
                        .display_name,
                    "v1"
                );
                assert!(connection
                    .execute(
                        "UPDATE auth_installed_participants SET installed_at = 3
                         WHERE participant_id = 'example.Service' AND revision = 1",
                        [],
                    )
                    .is_err());
                Ok(())
            })
            .await
            .expect("native revision history");
    }

    #[tokio::test]
    async fn installed_native_projection_cannot_be_tampered_or_removed() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        store
            .run(|connection| {
                let binding = binding("valid", 1);
                accept_package_evidence(
                    connection,
                    &binding.package_digest,
                    "example",
                    "{}",
                    false,
                    None,
                    1,
                )?;
                install_participant(connection, &binding, Some(0))?;
                assert!(connection
                    .execute(
                        "UPDATE auth_installed_participants SET projection_json = '{}'",
                        [],
                    )
                    .is_err());
                assert!(connection
                    .execute("DELETE FROM auth_installed_participants", [])
                    .is_err());
                assert_eq!(
                    load_installed_participant(connection, "example.Service", None)?
                        .expect("installed participant")
                        .1
                        .projection,
                    binding.projection
                );
                Ok(())
            })
            .await
            .expect("immutable installed projection");
    }

    #[tokio::test]
    async fn package_evidence_documents_deduplicate_exact_bytes_and_allow_semantic_peers() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        store
            .run(|connection| {
                let first = accept_package_evidence(
                    connection,
                    &"p".repeat(43),
                    "example",
                    "{\"root\":1}",
                    false,
                    None,
                    1,
                )?;
                let duplicate = accept_package_evidence(
                    connection,
                    &"p".repeat(43),
                    "example",
                    "{\"root\":1}",
                    false,
                    None,
                    2,
                )?;
                let second = accept_package_evidence(
                    connection,
                    &"p".repeat(43),
                    "example",
                    "{\"root\":2}",
                    false,
                    None,
                    3,
                )?;
                assert_eq!(first, duplicate);
                assert_ne!(first, second);
                assert_eq!(connection.query_row(
                    "SELECT COUNT(*) FROM auth_package_evidence_documents WHERE package_digest = ?1",
                    [&"p".repeat(43)],
                    |row| row.get::<_, u64>(0),
                ).map_err(sql_error)?, 2);
                assert!(connection
                    .execute("DELETE FROM auth_package_evidence", [])
                    .is_err());
                Ok(())
            })
            .await
            .expect("immutable package evidence documents");
    }

    #[tokio::test]
    async fn credential_assignment_is_server_owned() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        store
            .run(|connection| {
                connection.execute_batch(
                    "INSERT INTO auth_principals VALUES ('deployment', 'service', 'active', 1, 1, 1, NULL, NULL);
                     INSERT INTO auth_deployments VALUES ('deployment', 'example.Service', 'service', 'active', NULL);
                     INSERT INTO auth_deployment_profiles VALUES ('deployment', 'service', 'Service', 'example.Service', NULL, 0, NULL, 'active', 1, 1, 1, NULL);
                     INSERT INTO auth_instances VALUES ('instance', 'deployment', 'deployment', 'active', 1, 1, 1);",
                ).map_err(sql_error)?;
                super::super::provisioning::insert_sql_provisioned_identity(
                    connection,
                    &ProvisionedIdentityRecord {
                        identity_key_id: "k".repeat(43),
                        identity_public_key: "p".repeat(43),
                        principal_id: "deployment".to_owned(),
                        deployment_id: "deployment".to_owned(),
                        instance_id: "instance".to_owned(),
                        kind: ProvisionedIdentityKind::Service,
                        state: ProvisionedIdentityState::Active,
                        created_at: 1,
                        revoked_at: None,
                    },
                )?;
                connection
                    .execute(
                        "UPDATE auth_deployment_profiles SET participant_id = 'other.Service'
                         WHERE deployment_id = 'deployment'",
                        [],
                    )
                    .map_err(sql_error)?;
                Ok(())
            })
            .await
            .expect("assignment fixture");
        assert_eq!(
            store
                .get_credential_participant_assignment("k".repeat(43))
                .await
                .expect("assignment lookup"),
            Some("example.Service".to_owned())
        );
    }

    #[tokio::test]
    async fn platform_trust_requires_an_admin_actor() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        assert_eq!(
            store
                .run(|connection| {
                    accept_package_evidence(
                        connection,
                        &"p".repeat(43),
                        "trellis",
                        "{}",
                        true,
                        None,
                        1,
                    )
                })
                .await,
            Err(AuthorizationStateError::NotAuthorized)
        );
    }

    #[tokio::test]
    async fn installed_namespace_uses_exact_stored_platform_trust() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        store
            .run(|connection| {
                let mut participant =
                    super::super::super::builtins::auth_runtime_participant_binding(1)?;
                participant.package_digest = "x".repeat(43);
                connection
                    .execute(
                        "INSERT INTO auth_package_evidence
                         (package_digest, platform_trusted, accepted_at, trusted_at, trusted_by)
                         VALUES (?1, 1, 1, 1, 'admin')",
                        [&participant.package_digest],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_package_evidence_documents
                         (evidence_digest, package_digest, evidence_json, created_at)
                         VALUES (?1, ?2, '{}', 1)",
                        params![participant.evidence_digest, participant.package_digest],
                    )
                    .map_err(sql_error)?;
                assert_eq!(install_participant(connection, &participant, Some(0))?, 1);
                Ok(())
            })
            .await
            .expect("trusted Trellis participant installation");
    }

    #[tokio::test]
    async fn platform_trust_rejects_non_trellis_packages() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        assert!(matches!(
            store
                .run(|connection| {
                    accept_package_evidence(
                        connection,
                        &"p".repeat(43),
                        "example",
                        "{}",
                        true,
                        None,
                        1,
                    )
                })
                .await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
    }

    #[tokio::test]
    async fn consent_authority_preconditions_fence_expiry_and_revision() {
        const NOW: i64 = 1_700_000_000_000;
        const FUTURE: i64 = 9_000_000_000_000;
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, NOW,
            )
            .await
            .unwrap();
        let participant =
            crate::platform::auth::builtins::console_participant_binding(NOW).unwrap();
        let participant_id = participant.participant_id.clone();
        store
            .put_participant_binding(participant.clone())
            .await
            .unwrap();
        let grants = participant.projection.required_grants.clone();
        let replacement = |expected_revision, expires_at| GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: actor.principal_id.clone(),
            participant_id: participant_id.clone(),
            installed_revision: 1,
            grants: grants.clone(),
            approval_mode: ApprovalMode::Exact,
            approved_capabilities: Vec::new(),
            approved_resources: Vec::new(),
            delegation_ceiling: DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: Some(grants.clone()),
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: "A".repeat(43),
            companion_approved: false,
            platform_privileges: Vec::new(),
            expected_revision,
            expected_current_installed_revision: Some(1),
            state: GrantBindingState::Active,
            expires_at,
            provenance: None,
        };
        let idempotency = |purpose: &str, digest: &str| IdempotencyResultRecord {
            scope_key: trellis_protocol::digest_json(&json!([purpose])).unwrap(),
            purpose: purpose.to_owned(),
            signer_id: actor.principal_id.clone(),
            request_id: purpose.to_owned(),
            request_digest: digest.to_owned(),
            result: Value::Null,
            created_at: NOW,
            expires_at: NOW + 60_000,
        };
        let load = || {
            let (owner, participant) = (actor.principal_id.clone(), participant_id.clone());
            let store = store.clone();
            async move {
                store
                    .get_grant_binding(GrantOwnerKind::User, owner, participant)
                    .await
                    .unwrap()
                    .unwrap()
            }
        };

        store
            .set_grant_binding(
                replacement(0, Some(FUTURE)),
                idempotency("consent.finite", &"B".repeat(43)),
            )
            .await
            .unwrap();
        let binding = load().await;
        assert_eq!(binding.expires_at, Some(FUTURE));

        // Matching source revision and finite expiry commit and preserve the expiry.
        store
            .set_consent_grant_binding(
                replacement(binding.revision, Some(FUTURE)),
                ConsentAuthorityPreconditions {
                    policy: None,
                    bindings: vec![ConsentBindingPrecondition::from(&binding)],
                },
                idempotency("consent.keep", &"C".repeat(43)),
            )
            .await
            .unwrap();
        let preserved = load().await;
        assert_eq!(preserved.expires_at, Some(FUTURE));

        // A stale source revision is rejected inside the commit transaction.
        assert!(matches!(
            store
                .set_consent_grant_binding(
                    replacement(preserved.revision, Some(FUTURE)),
                    ConsentAuthorityPreconditions {
                        policy: None,
                        bindings: vec![ConsentBindingPrecondition {
                            revision: preserved.revision + 1,
                            ..ConsentBindingPrecondition::from(&preserved)
                        }],
                    },
                    idempotency("consent.stale", &"D".repeat(43)),
                )
                .await,
            Err(AuthorizationStateError::StorageConflict)
        ));
        assert_eq!(load().await.revision, preserved.revision);

        // An expired source cannot be extended into a fresh binding.
        store
            .set_grant_binding(
                replacement(preserved.revision, Some(1)),
                idempotency("consent.expired", &"E".repeat(43)),
            )
            .await
            .unwrap();
        let expired = load().await;
        assert!(matches!(
            store
                .set_consent_grant_binding(
                    replacement(expired.revision, Some(FUTURE)),
                    ConsentAuthorityPreconditions {
                        policy: None,
                        bindings: vec![ConsentBindingPrecondition::from(&expired)],
                    },
                    idempotency("consent.expired-reject", &"F".repeat(43)),
                )
                .await,
            Err(AuthorizationStateError::StorageConflict)
        ));
        assert_eq!(load().await.revision, expired.revision);
    }
}
