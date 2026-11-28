//! Enrollment secrets, self-enrolled service instances, and the retirement of
//! instances that stay offline (design/auth/instance-enrollment.md).

use std::sync::Arc;
use trellis_protocol::model::IdKind;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo},
    apis::trellis_auth_v1::Provider,
};

use super::sqlite::{id, timestamp, Connection};
use super::{
    rpc::{caller, mutation, rpc_error},
    AuthError, EnforcementScope, Mutation, OAuthEngine, SqliteAuthorizationStore,
};
use crate::platform::auth::{blocking, Administration};

/// Every enrolled instance holds this, so it can retire itself at a graceful stop.
pub(super) const RETIRE_OWN_CAPABILITY: &str = "trellis.auth.v1:instances.retireOwn";
const SECRET_PREFIX: &str = "trellis-enroll-";
/// How far a request's send time may be from Auth's clock, either way.
const ENROLLMENT_WINDOW_MS: i64 = 5 * 60 * 1000;
/// After Auth starts, it waits this long before retiring offline instances, so
/// processes that were connected before the restart can reconnect.
pub(crate) const STARTUP_QUIET_SECONDS: i64 = 15 * 60;
/// Each retirement or cleanup transaction handles at most this many instances.
const BATCH: i64 = 1000;

/// Register enrollment-secret, instance-policy, and self-retirement handlers.
pub(super) fn register_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    macro_rules! admin {
        ($registration:ident, $operation:expr) => {{
            let store = Arc::clone(&store);
            provider.$registration(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let identity = mutation(
                        &context,
                        &input.idempotency_key,
                        Some(Administration::ManagePrincipals),
                        &input,
                    )
                    .map_err(rpc_error)?;
                    blocking(move || {
                        ($operation)(&store, &identity, &input, crate::clock::unix_now())
                    })
                    .await
                    .map_err(rpc_error)
                }
            });
        }};
    }
    admin!(register_enrollment_secrets_issue, issue_secret);
    admin!(register_enrollment_secrets_update, update_secret);
    admin!(register_enrollment_secrets_revoke, revoke_secret);
    admin!(register_instance_policy_set, set_policy);
    let lister = Arc::clone(&store);
    provider.register_enrollment_secrets_list(move |context, input| {
        let store = Arc::clone(&lister);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            blocking(move || {
                store.read(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    list_secrets(connection, &input, crate::clock::unix_now())
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let reader = Arc::clone(&store);
    provider.register_instance_policy_get(move |context, input| {
        let store = Arc::clone(&reader);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            blocking(move || {
                store.read(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    read_policy(
                        connection,
                        input
                            .deployment_id
                            .as_ref()
                            .map(ToString::to_string)
                            .as_deref(),
                    )
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_service_instances_retire_own(move |context, _input| {
        let store = Arc::clone(&store);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            let session = context
                .request()
                .verified_request()
                .map(|request| request.caller().authorization_session_id.clone())
                .ok_or_else(|| rpc_error(AuthError::Denied))?;
            blocking(move || {
                store.transaction(|connection| {
                    let now = crate::clock::unix_now();
                    // Only an enrolled instance's key lives in its process
                    // alone; a provisioned instance's seed outlives it.
                    let instance: String = connection
                        .query_row(
                            "SELECT instance_id FROM auth_instances \
                            WHERE principal_id=?1 AND enrollment_secret_id IS NOT NULL \
                            AND state IN ('active','disabled')",
                            [&actor],
                            |row| row.get(0),
                        )
                        .optional()?
                        .ok_or(AuthError::Denied)?;
                    // The caller's own connection is closed after it has the reply.
                    super::revocation::defer_caller_enforcement(
                        connection,
                        &session,
                        &EnforcementScope::Principal(actor.clone()),
                        now,
                    )?;
                    retire_in(connection, &instance, "stopped", now)?;
                    Ok(AuthEmpty {
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
}

fn unix(value: &trellis_rs::generated::Timestamp) -> Result<i64, AuthError> {
    time::OffsetDateTime::parse(
        value.as_str(),
        &time::format_description::well_known::Rfc3339,
    )
    .map(|value| value.unix_timestamp())
    .map_err(|_| AuthError::Invalid("invalid timestamp".into()))
}

fn ulid(value: &str) -> Result<trellis_rs::generated::Ulid, AuthError> {
    value.parse().map_err(|_| AuthError::Unavailable)
}

/// Write one security audit entry. `actor` is `None` for Trellis's own actions.
fn audit(
    connection: &Connection,
    actor: Option<&str>,
    kind: &str,
    target: &str,
    data: &Value,
    now: i64,
) -> Result<(), AuthError> {
    connection.execute(
        "INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,\
        target_id,data_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
        params![id(), actor, kind, target, data.to_string(), now],
    )?;
    Ok(())
}

/// A deployment that holds an administrative capability lets every instance
/// a secret creates hold it too, so issuing one needs `platform.manage`.
fn require_issuer(connection: &Connection, actor: &str, deployment: &str) -> Result<(), AuthError> {
    SqliteAuthorizationStore::require_administration(
        connection,
        actor,
        Administration::ManagePrincipals,
    )?;
    let (kind, required, optional): (String, String, String) = connection
        .query_row(
            "SELECT d.participant_kind,b.required_capabilities_json,b.optional_capabilities_json \
            FROM auth_deployments d JOIN auth_deployment_bindings b USING(deployment_id) \
            WHERE d.deployment_id=?1",
            [deployment],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| AuthError::NotFound(format!("no deployment {deployment}")))?;
    if kind != "service" {
        return Err(AuthError::Invalid(
            "enrollment secrets are for service deployments".into(),
        ));
    }
    let required: Vec<String> = super::sqlite::decode(&required)?;
    let optional: Vec<String> = super::sqlite::decode(&optional)?;
    SqliteAuthorizationStore::require_platform_for(
        connection,
        actor,
        required.iter().chain(&optional).map(String::as_str),
    )
}

fn issue_secret(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthEnrollmentSecretIssueRequest,
    now: i64,
) -> Result<AuthEnrollmentSecretIssueResponse, AuthError> {
    // The text is shown once. The replayable result holds only the record, so
    // the text is never written anywhere.
    let mut text = None;
    let issued = store.mutate(
        identity,
        "rpc.enrollmentSecrets.issue",
        input,
        now,
        |connection| {
            let deployment = input.deployment_id.to_string();
            require_issuer(connection, &identity.actor, &deployment)?;
            let expires = input.expires_at.as_ref().map(unix).transpose()?;
            if expires.is_some_and(|expires| expires <= now) {
                return Err(AuthError::Invalid("expiry must be in the future".into()));
            }
            let (max, rate): (u32, u32) = connection.query_row(
                "SELECT secret_max_instances,secret_enrollments_per_hour \
                FROM auth_instance_policy",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let secret_id = IdKind::EnrollmentSecret.generate();
            let mut random = [0u8; 32];
            getrandom::fill(&mut random).map_err(|_| AuthError::Unavailable)?;
            let issued = format!(
                "{SECRET_PREFIX}{secret_id}-{}",
                URL_SAFE_NO_PAD.encode(random)
            );
            connection.execute(
                "INSERT INTO auth_enrollment_secrets(secret_id,deployment_id,title,secret_hash,\
                expires_at,max_instances,enrollments_per_hour,current_build_only,created_at,\
                created_by,updated_at,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?9,1)",
                params![
                    secret_id,
                    deployment,
                    input.title.trim(),
                    URL_SAFE_NO_PAD.encode(Sha256::digest(issued.as_bytes())),
                    expires,
                    input.max_instances.map_or(max, |value| value.0),
                    input.enrollments_per_hour.map_or(rate, |value| value.0),
                    input.current_build_only.unwrap_or(false),
                    now,
                    identity.actor,
                ],
            )?;
            audit(
                connection,
                Some(&identity.actor),
                "enrollmentSecret.issued",
                &secret_id,
                &json!({"deploymentId": deployment, "secretId": secret_id}),
                now,
            )?;
            text = Some(issued);
            Ok(AuthEnrollmentSecretResponse {
                secret: read_secret(connection, &secret_id, now)?,
                extra: Default::default(),
            })
        },
    )?;
    Ok(AuthEnrollmentSecretIssueResponse {
        secret: issued.secret,
        text,
        extra: Default::default(),
    })
}

fn update_secret(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthEnrollmentSecretUpdateRequest,
    now: i64,
) -> Result<AuthEnrollmentSecretResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.enrollmentSecrets.update",
        input,
        now,
        |connection| {
            let secret = input.secret_id.to_string();
            let (deployment, revoked): (String, Option<i64>) = connection
                .query_row(
                    "SELECT deployment_id,revoked_at FROM auth_enrollment_secrets \
                    WHERE secret_id=?1",
                    [&secret],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| AuthError::NotFound(format!("no enrollment secret {secret}")))?;
            require_issuer(connection, &identity.actor, &deployment)?;
            if revoked.is_some() {
                return Err(AuthError::Invalid(format!(
                    "enrollment secret {secret} is revoked and can't be changed"
                )));
            }
            let expires = input.expires_at.as_ref().map(unix).transpose()?;
            if connection.execute(
                "UPDATE auth_enrollment_secrets SET title=?2,expires_at=?3,max_instances=?4,\
                enrollments_per_hour=?5,current_build_only=?6,updated_at=?7,revision=revision+1 \
                WHERE secret_id=?1 AND revision=?8 AND revoked_at IS NULL",
                params![
                    secret,
                    input.title.trim(),
                    expires,
                    input.max_instances.0,
                    input.enrollments_per_hour.0,
                    input.current_build_only,
                    now,
                    input.expected_revision.0 .0,
                ],
            )? != 1
            {
                return Err(super::sqlite::changed_since(
                    &format!("enrollment secret {secret}"),
                    input.expected_revision.0 .0,
                ));
            }
            audit(
                connection,
                Some(&identity.actor),
                "enrollmentSecret.updated",
                &secret,
                &json!({"deploymentId": deployment, "secretId": secret}),
                now,
            )?;
            Ok(AuthEnrollmentSecretResponse {
                secret: read_secret(connection, &secret, now)?,
                extra: Default::default(),
            })
        },
    )
}

fn revoke_secret(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthEnrollmentSecretRevokeRequest,
    now: i64,
) -> Result<AuthEnrollmentSecretRevokeResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.enrollmentSecrets.revoke",
        input,
        now,
        |connection| {
            let secret = input.secret_id.to_string();
            let (deployment, revoked): (String, Option<i64>) = connection
                .query_row(
                    "SELECT deployment_id,revoked_at FROM auth_enrollment_secrets \
                    WHERE secret_id=?1",
                    [&secret],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| AuthError::NotFound(format!("no enrollment secret {secret}")))?;
            require_issuer(connection, &identity.actor, &deployment)?;
            if revoked.is_some() {
                return Err(AuthError::Invalid(format!(
                    "enrollment secret {secret} is already revoked"
                )));
            }
            if connection.execute(
                "UPDATE auth_enrollment_secrets SET revoked_at=?2,updated_at=?2,\
                revision=revision+1 WHERE secret_id=?1 AND revision=?3 AND revoked_at IS NULL",
                params![secret, now, input.expected_revision.0 .0],
            )? != 1
            {
                return Err(super::sqlite::changed_since(
                    &format!("enrollment secret {secret}"),
                    input.expected_revision.0 .0,
                ));
            }
            let (mut retired, mut closing) = (0u64, 0u64);
            if input.retire_instances == Some(true) {
                let instances = connection
                    .prepare(
                        "SELECT instance_id FROM auth_instances \
                        WHERE enrollment_secret_id=?1 AND state IN ('active','disabled')",
                    )?
                    .query_map([&secret], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for instance in instances {
                    closing += retire_in(connection, &instance, "secretRevoked", now)?;
                    retired += 1;
                }
            }
            audit(
                connection,
                Some(&identity.actor),
                "enrollmentSecret.revoked",
                &secret,
                &json!({
                    "deploymentId": deployment,
                    "secretId": secret,
                    "retiredInstances": retired,
                }),
                now,
            )?;
            Ok(AuthEnrollmentSecretRevokeResponse {
                secret: read_secret(connection, &secret, now)?,
                retired_instances: retired.into(),
                closing_connections: closing.into(),
                extra: Default::default(),
            })
        },
    )
}

/// Retire one instance in the caller's transaction: mark it, revoke its
/// principal and identity key, keep the key's tombstone, and queue the
/// disconnects of its connections through the enforcement pipeline. Returns
/// how many of its connections were open, all of which the pipeline closes.
pub(super) fn retire_in(
    connection: &Connection,
    instance: &str,
    reason: &str,
    now: i64,
) -> Result<u64, AuthError> {
    let (principal, deployment, secret): (String, String, Option<String>) = connection
        .query_row(
            "SELECT principal_id,deployment_id,enrollment_secret_id FROM auth_instances \
            WHERE instance_id=?1 AND state IN ('active','disabled')",
            [instance],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| AuthError::Invalid(format!("instance {instance} is already retired")))?;
    connection.execute(
        "UPDATE auth_instances SET state='retired',retired_at=?2,retired_reason=?3,\
        updated_at=?2,version=version+1 WHERE instance_id=?1",
        params![instance, now, reason],
    )?;
    connection.execute(
        "UPDATE auth_principals SET state='revoked',disabled_at=NULL,revoked_at=?2,\
        updated_at=?2,version=version+1 WHERE principal_id=?1 AND state!='revoked'",
        params![principal, now],
    )?;
    tombstone(connection, instance, "retired", now)?;
    connection.execute(
        "UPDATE auth_provisioned_identities SET state='revoked',revoked_at=?2 \
        WHERE instance_id=?1 AND state='active'",
        params![instance, now],
    )?;
    let open: u64 = connection.query_row(
        "SELECT count(*) FROM auth_attachments a \
        JOIN auth_authorization_sessions s USING(authorization_session_id) \
        WHERE s.principal_id=?1 AND a.state IN ('pending','admitted')",
        [&principal],
        |row| row.get(0),
    )?;
    SqliteAuthorizationStore::policy_changed(
        connection,
        &EnforcementScope::Principal(principal),
        now,
    )?;
    audit(
        connection,
        None,
        "instance.retired",
        instance,
        &json!({
            "deploymentId": deployment,
            "instanceId": instance,
            "secretId": secret,
            "reason": reason,
        }),
        now,
    )?;
    Ok(open)
}

/// Keep what finally became of `instance`'s identity keys, forever.
pub(super) fn tombstone(
    connection: &Connection,
    instance: &str,
    outcome: &str,
    now: i64,
) -> Result<(), AuthError> {
    connection.execute(
        "INSERT OR IGNORE INTO auth_identity_tombstones(identity_key_id,kind,outcome,\
        recorded_at) SELECT identity_key_id,kind,?2,?3 FROM auth_provisioned_identities \
        WHERE instance_id=?1",
        params![instance, outcome, now],
    )?;
    Ok(())
}

pub(super) fn read_secret(
    connection: &Connection,
    secret: &str,
    now: i64,
) -> Result<AuthEnrollmentSecret, AuthError> {
    // deployment, title, expiry, cap, rate, current build only, created,
    // last used, revoked, revision
    type Row = (
        String,
        String,
        Option<i64>,
        u32,
        u32,
        bool,
        i64,
        Option<i64>,
        Option<i64>,
        u64,
    );
    let row: Row = connection
        .query_row(
            "SELECT deployment_id,title,expires_at,max_instances,enrollments_per_hour,\
                current_build_only,created_at,last_used_at,revoked_at,revision \
                FROM auth_enrollment_secrets WHERE secret_id=?1",
            [secret],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| AuthError::NotFound(format!("no enrollment secret {secret}")))?;
    let instances: u64 = connection.query_row(
        "SELECT count(*) FROM auth_instances \
        WHERE enrollment_secret_id=?1 AND state IN ('active','disabled')",
        [secret],
        |row| row.get(0),
    )?;
    let mut alerts = Vec::new();
    let mut statement = connection.prepare(
        "SELECT json_extract(data_json,'$.reason'),count(*),max(created_at) \
        FROM auth_security_audit \
        WHERE target_id=?1 AND kind='instance.enrollmentRefused' AND created_at>?2 \
        GROUP BY 1 ORDER BY 1",
    )?;
    let refusals = statement
        .query_map(params![secret, now - 86_400], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (reason, count, last) in refusals {
        let kind = match reason.as_str() {
            "revoked" => AuthEnrollmentAlertKind::RevokedUsed,
            "expired" => AuthEnrollmentAlertKind::ExpiredUsed,
            "instance_limit" => AuthEnrollmentAlertKind::InstanceLimit,
            "rate_limited" => AuthEnrollmentAlertKind::RateLimited,
            "participant_mismatch" => AuthEnrollmentAlertKind::ParticipantMismatch,
            "build_mismatch" => AuthEnrollmentAlertKind::BuildMismatch,
            _ => continue,
        };
        alerts.push(AuthEnrollmentAlert {
            kind,
            count: count.into(),
            last_at: timestamp(last)?,
            extra: Default::default(),
        });
    }
    Ok(AuthEnrollmentSecret {
        secret_id: ulid(secret)?,
        deployment_id: ulid(&row.0)?,
        title: row.1.into(),
        expires_at: row.2.map(timestamp).transpose()?,
        max_instances: AuthEnrollmentLimit(row.3),
        enrollments_per_hour: AuthEnrollmentLimit(row.4),
        current_build_only: row.5,
        created_at: timestamp(row.6)?,
        last_used_at: row.7.map(timestamp).transpose()?,
        revoked_at: row.8.map(timestamp).transpose()?,
        revision: AuthRevision(row.9.into()),
        instances: instances.into(),
        alerts,
        extra: Default::default(),
    })
}

fn list_secrets(
    connection: &Connection,
    input: &AuthEnrollmentSecretsListRequest,
    now: i64,
) -> Result<AuthEnrollmentSecretsListResponse, AuthError> {
    let limit = input
        .page
        .as_ref()
        .and_then(|page| page.limit)
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    let cursor = input
        .page
        .as_ref()
        .and_then(|page| page.cursor.as_deref())
        .unwrap_or("");
    let mut rows = connection
        .prepare(
            "SELECT secret_id FROM auth_enrollment_secrets \
            WHERE deployment_id=?1 AND secret_id>?2 ORDER BY secret_id LIMIT ?3",
        )?
        .query_map(
            params![input.deployment_id.to_string(), cursor, limit + 1],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (rows.len() > limit as usize).then(|| rows[limit as usize - 1].clone());
    rows.truncate(limit as usize);
    Ok(CursorPage {
        items: rows
            .iter()
            .map(|secret| read_secret(connection, secret, now))
            .collect::<Result<_, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

fn read_policy(
    connection: &Connection,
    deployment: Option<&str>,
) -> Result<AuthInstancePolicyResponse, AuthError> {
    let installation = connection.query_row(
        "SELECT retire_offline,retire_offline_after,retire_provisioned,retired_record_days,\
        first_connect_grace,secret_max_instances,secret_enrollments_per_hour \
        FROM auth_instance_policy",
        [],
        |row| {
            Ok(AuthInstallationInstancePolicy {
                retire_offline: row.get(0)?,
                retire_offline_after_seconds: AuthRetirementSeconds(row.get(1)?),
                retire_provisioned: row.get(2)?,
                retired_record_days: AuthRecordDays(row.get(3)?),
                first_connect_grace_seconds: AuthGraceSeconds(row.get(4)?),
                secret_max_instances: AuthEnrollmentLimit(row.get(5)?),
                secret_enrollments_per_hour: AuthEnrollmentLimit(row.get(6)?),
                extra: Default::default(),
            })
        },
    )?;
    let Some(deployment) = deployment else {
        return Ok(AuthInstancePolicyResponse {
            installation,
            deployment: None,
            effective: None,
            extra: Default::default(),
        });
    };
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_deployments WHERE deployment_id=?1)",
        [deployment],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(AuthError::NotFound(format!("no deployment {deployment}")));
    }
    let own: (Option<bool>, Option<u32>, Option<bool>) = connection
        .query_row(
            "SELECT retire_offline,retire_offline_after,retire_provisioned \
            FROM auth_deployment_instance_policies WHERE deployment_id=?1",
            [deployment],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .unwrap_or_default();
    let effective = AuthEffectiveInstancePolicy {
        retire_offline: own.0.unwrap_or(installation.retire_offline),
        retire_offline_after_seconds: own.1.map_or(
            installation.retire_offline_after_seconds,
            AuthRetirementSeconds,
        ),
        retire_provisioned: own.2.unwrap_or(installation.retire_provisioned),
        extra: Default::default(),
    };
    Ok(AuthInstancePolicyResponse {
        installation,
        deployment: Some(AuthDeploymentInstancePolicy {
            retire_offline: own.0,
            retire_offline_after_seconds: own.1.map(AuthRetirementSeconds),
            retire_provisioned: own.2,
            extra: Default::default(),
        }),
        effective: Some(effective),
        extra: Default::default(),
    })
}

fn set_policy(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthInstancePolicySetRequest,
    now: i64,
) -> Result<AuthInstancePolicyResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.instancePolicy.set",
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_administration(
                connection,
                &identity.actor,
                Administration::ManagePrincipals,
            )?;
            let deployment = input.deployment_id.as_ref().map(ToString::to_string);
            match (&deployment, &input.installation) {
                (None, Some(policy)) => {
                    connection.execute(
                    "UPDATE auth_instance_policy SET retire_offline=?1,retire_offline_after=?2,\
                    retire_provisioned=?3,retired_record_days=?4,first_connect_grace=?5,\
                    secret_max_instances=?6,secret_enrollments_per_hour=?7,revision=revision+1",
                    params![
                        policy.retire_offline,
                        policy.retire_offline_after_seconds.0,
                        policy.retire_provisioned,
                        policy.retired_record_days.0,
                        policy.first_connect_grace_seconds.0,
                        policy.secret_max_instances.0,
                        policy.secret_enrollments_per_hour.0,
                    ],
                )?;
                }
                (Some(deployment), None) => {
                    read_policy(connection, Some(deployment))?;
                    match &input.deployment {
                        None => {
                            connection.execute(
                            "DELETE FROM auth_deployment_instance_policies WHERE deployment_id=?1",
                            [deployment],
                        )?;
                        }
                        Some(policy) => {
                            connection.execute(
                                "INSERT INTO auth_deployment_instance_policies(deployment_id,\
                            retire_offline,retire_offline_after,retire_provisioned,revision) \
                            VALUES(?1,?2,?3,?4,1) ON CONFLICT(deployment_id) DO UPDATE SET \
                            retire_offline=?2,retire_offline_after=?3,retire_provisioned=?4,\
                            revision=revision+1",
                                params![
                                    deployment,
                                    policy.retire_offline,
                                    policy.retire_offline_after_seconds.map(|value| value.0),
                                    policy.retire_provisioned,
                                ],
                            )?;
                        }
                    }
                }
                _ => {
                    return Err(AuthError::Invalid(
                        "give `installation` without a deployment, or `deployment` with one".into(),
                    ))
                }
            }
            audit(
                connection,
                Some(&identity.actor),
                "instancePolicy.changed",
                deployment.as_deref().unwrap_or("installation"),
                &serde_json::to_value(input)?,
                now,
            )?;
            read_policy(connection, deployment.as_deref())
        },
    )
}

/// Why an enrollment was not accepted.
#[derive(Debug)]
enum Refusal {
    /// The body is malformed or not signed by the key it registers.
    Invalid,
    /// The send time is too far from Auth's clock.
    Clock,
    /// No such secret. Nothing more is said, and nothing is audited.
    Unknown,
    /// The key already belongs to another instance or deployment.
    KeyInUse,
    /// A known secret refused, for the named reason. Audited.
    Refused(&'static str),
}

/// `POST /auth/service/enroll`: register a new instance for the key that signed
/// the request, with an enrollment secret.
pub(super) async fn http_enroll(
    axum::extract::State(engine): axum::extract::State<Arc<OAuthEngine>>,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};
    let store = Arc::clone(&engine.server.store().authority);
    let saved = Arc::clone(&store);
    let now = crate::clock::unix_now();
    let enrolled = blocking(move || Ok::<_, AuthError>(enroll(&saved, body, now))).await;
    let refuse = |status: StatusCode, code: &str| {
        (status, axum::Json(json!({"error": {"code": code}}))).into_response()
    };
    // The server failing the enrollment: logged once, under the reference the
    // caller gets in the error envelope.
    let unavailable = |error: &str, cause: &dyn std::fmt::Display| {
        crate::http_failure::record(error, cause);
        let reference = crate::http_failure::reference("unavailable");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": {"code": "unavailable", "reference": reference}})),
        )
            .into_response()
    };
    let response = match enrolled {
        Ok(Ok(Ok(response))) => response,
        Ok(Ok(Err(refusal))) => {
            return match refusal {
                Refusal::Invalid => refuse(StatusCode::BAD_REQUEST, "invalid_request"),
                Refusal::Clock => refuse(StatusCode::BAD_REQUEST, "clock_skew"),
                Refusal::Unknown => refuse(StatusCode::FORBIDDEN, "enrollment_denied"),
                Refusal::KeyInUse => refuse(StatusCode::CONFLICT, "key_in_use"),
                Refusal::Refused(code @ ("instance_limit" | "rate_limited")) => {
                    refuse(StatusCode::TOO_MANY_REQUESTS, code)
                }
                Refusal::Refused(code) => refuse(StatusCode::FORBIDDEN, code),
            }
        }
        Ok(Err(error)) | Err(error) => {
            return unavailable("could not enroll the service instance", &error);
        }
    };
    // The instance's resources are ready before it connects, as for a
    // provisioned one. A retry of a lost response resumes this.
    let Some(operational) = engine.operational() else {
        return unavailable(
            "could not prepare the enrolled instance's resources",
            &"Trellis is not connected to NATS",
        );
    };
    if let Err(error) = super::resources::prepare_for_instance(
        &store,
        operational,
        &response.deployment_id.to_string(),
        now,
    )
    .await
    {
        return unavailable("enrolled instance resources not ready yet", &error);
    }
    axum::Json(response).into_response()
}

fn verify_signed(
    store: &SqliteAuthorizationStore,
    body: &Value,
    public_key: &str,
    key_id: &str,
    status: bool,
    now: i64,
) -> Result<(), Refusal> {
    let raw = URL_SAFE_NO_PAD
        .decode(public_key)
        .map_err(|_| Refusal::Invalid)?;
    if URL_SAFE_NO_PAD.encode(Sha256::digest(&raw)) != key_id {
        return Err(Refusal::Invalid);
    }
    let mut unsigned = body.clone();
    let proof = unsigned
        .as_object_mut()
        .and_then(|request| request.remove("proof"))
        .ok_or(Refusal::Invalid)?;
    let proof: trellis_protocol::SessionProof =
        serde_json::from_value(proof).map_err(|_| Refusal::Invalid)?;
    let input = trellis_protocol::ServiceIdentitySessionProofInput {
        origin: store.settings.oauth_issuer.clone(),
        unsigned_request: unsigned,
    };
    let input = if status {
        trellis_protocol::SessionProofInput::service_identity_status(input)
    } else {
        trellis_protocol::SessionProofInput::service_enrollment(input)
    }
    .map_err(|_| Refusal::Invalid)?;
    let policy =
        trellis_protocol::SessionProofPolicy::new(ENROLLMENT_WINDOW_MS, ENROLLMENT_WINDOW_MS)
            .map_err(|_| Refusal::Invalid)?;
    trellis_protocol::verify_session_proof(&input, &proof, public_key, now * 1000, policy).map_err(
        |error| match error {
            trellis_protocol::ProtocolError::SessionProof {
                code: trellis_protocol::SessionProofErrorCode::ProofIatOutOfRange,
                ..
            } => Refusal::Clock,
            _ => Refusal::Invalid,
        },
    )
}

/// Check, then register, one enrollment in a single transaction. A refusal
/// for a known secret commits its audit entry.
fn enroll(
    store: &SqliteAuthorizationStore,
    body: Value,
    now: i64,
) -> Result<Result<AuthServiceEnrollmentResponse, Refusal>, AuthError> {
    let Ok(input) = serde_json::from_value::<AuthServiceEnrollmentRequest>(body.clone()) else {
        return Ok(Err(Refusal::Invalid));
    };
    if !input.extra.is_empty() || !input.proof.extra.is_empty() || input.title.trim().is_empty() {
        return Ok(Err(Refusal::Invalid));
    }
    // The key proves itself, and the clock is checked, before the secret is.
    if let Err(refusal) = verify_signed(
        store,
        &body,
        &input.identity_public_key,
        &input.identity_key_id,
        false,
        now,
    ) {
        return Ok(Err(refusal));
    }
    let text = input.enrollment_secret.as_str();
    // `trellis-enroll-<secret ID>-<random>`: an ID has no `-`.
    let named = text
        .strip_prefix(SECRET_PREFIX)
        .and_then(|rest| rest.split_once('-'))
        .filter(|(secret, _)| {
            trellis_protocol::model::parse_id(secret, &[IdKind::EnrollmentSecret]).is_ok()
        })
        .map(|(secret, _)| secret.to_owned());
    let Some(named) = named else {
        return Ok(Err(Refusal::Unknown));
    };
    let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(text.as_bytes()));
    let key_id = input.identity_key_id.to_string();
    store.transaction(|connection| {
        type SecretRow = (String, String, Option<i64>, u32, u32, bool, Option<i64>);
        let secret: Option<SecretRow> = connection
            .query_row(
                "SELECT secret_id,deployment_id,expires_at,max_instances,\
                enrollments_per_hour,current_build_only,revoked_at \
                FROM auth_enrollment_secrets WHERE secret_hash=?1",
                [&hash],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((secret, deployment, expires, cap, rate, current_only, revoked)) =
            secret.filter(|secret| secret.0 == named)
        else {
            return Ok(Err(Refusal::Unknown));
        };
        // A retry for a key this secret already enrolled gets its instance
        // back before any limit is checked, so a lost response is recoverable.
        let known: Option<(String, Option<String>, String, String)> = connection
            .query_row(
                "SELECT i.instance_id,i.enrollment_secret_id,p.state,i.title \
                FROM auth_provisioned_identities p JOIN auth_instances i USING(instance_id) \
                WHERE p.identity_key_id=?1",
                [&key_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((instance, enrolled_by, state, title)) = known {
            if enrolled_by.as_deref() == Some(secret.as_str()) && state == "active" {
                return Ok(Ok(AuthServiceEnrollmentResponse {
                    instance_id: ulid(&instance)?,
                    deployment_id: ulid(&deployment)?,
                    title: title.into(),
                    extra: Default::default(),
                }));
            }
            return Ok(Err(Refusal::KeyInUse));
        }
        let elsewhere: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_identity_tombstones WHERE identity_key_id=?1) \
            OR EXISTS(SELECT 1 FROM auth_service_claims WHERE identity_key_id=?1)",
            [&key_id],
            |row| row.get(0),
        )?;
        if elsewhere {
            return Ok(Err(Refusal::KeyInUse));
        }
        let refuse = |reason: &'static str| -> Result<Result<_, Refusal>, AuthError> {
            audit(
                connection,
                None,
                "instance.enrollmentRefused",
                &secret,
                &json!({
                    "reason": reason,
                    "secretId": secret,
                    "deploymentId": deployment,
                    "participantId": input.participant_id,
                    "participantDigest": input.participant_digest.as_str(),
                    "title": input.title.as_str(),
                }),
                now,
            )?;
            Ok(Err(Refusal::Refused(reason)))
        };
        if revoked.is_some() {
            return refuse("revoked");
        }
        if expires.is_some_and(|expires| expires <= now) {
            return refuse("expired");
        }
        let recent: u32 = connection.query_row(
            "SELECT count(*) FROM auth_instances WHERE enrollment_secret_id=?1 AND created_at>?2",
            params![secret, now - 3600],
            |row| row.get(0),
        )?;
        if recent >= rate {
            return refuse("rate_limited");
        }
        let target: Option<(String, String, Option<i64>)> = connection
            .query_row(
                "SELECT d.state,d.participant_id,d.expires_at FROM auth_deployments d \
                WHERE d.deployment_id=?1",
                [&deployment],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((state, participant, deployment_expiry)) = target else {
            return refuse("deployment_disabled");
        };
        if state != "active" || deployment_expiry.is_some_and(|expiry| expiry <= now) {
            return refuse("deployment_disabled");
        }
        if participant != input.participant_id {
            return refuse("participant_mismatch");
        }
        if current_only {
            let installed: String = connection.query_row(
                "SELECT p.participant_digest FROM auth_deployment_bindings b \
                JOIN auth_installed_participants p \
                ON p.participant_id=?2 AND p.revision=b.installed_revision \
                WHERE b.deployment_id=?1",
                params![deployment, participant],
                |row| row.get(0),
            )?;
            if installed != input.participant_digest.as_str() {
                return refuse("build_mismatch");
            }
        }
        // The cap is checked, and a slot made, in the transaction that creates
        // the instance, so two enrollments cannot both take the last slot.
        let live: u32 = connection.query_row(
            "SELECT count(*) FROM auth_instances \
            WHERE enrollment_secret_id=?1 AND state IN ('active','disabled')",
            [&secret],
            |row| row.get(0),
        )?;
        // A cap lowered below the roster admits nothing until the roster
        // drains to it; at the cap a new key replaces one offline instance.
        if live > cap {
            return refuse("instance_limit");
        }
        if live == cap {
            let grace: i64 = connection.query_row(
                "SELECT first_connect_grace FROM auth_instance_policy",
                [],
                |row| row.get(0),
            )?;
            // The instance offline longest, never one that is connected or one
            // enrolled within the grace period that has not connected yet.
            let replaceable: Option<String> = connection
                .query_row(
                    "SELECT i.instance_id FROM auth_instances i \
                    WHERE i.enrollment_secret_id=?1 AND i.state IN ('active','disabled') \
                    AND NOT EXISTS(SELECT 1 FROM auth_attachments a \
                        JOIN auth_authorization_sessions s USING(authorization_session_id) \
                        WHERE s.principal_id=i.principal_id AND a.state IN ('pending','admitted')) \
                    AND NOT (i.last_seen_at IS NULL AND i.created_at>?2) \
                    ORDER BY coalesce(i.last_seen_at,i.created_at),i.instance_id LIMIT 1",
                    params![secret, now - grace],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(replaced) = replaceable else {
                return refuse("instance_limit");
            };
            retire_in(connection, &replaced, "replaced", now)?;
        }
        let created = SqliteAuthorizationStore::provision_instance_in_transaction(
            connection,
            &deployment,
            Some(input.identity_public_key.as_str()),
            input.title.trim(),
            None,
            now,
        )?;
        connection.execute(
            "UPDATE auth_instances SET enrollment_secret_id=?2 WHERE instance_id=?1",
            params![created.instance_id, secret],
        )?;
        let retire_own = super::policy::capability(connection, RETIRE_OWN_CAPABILITY)?;
        connection.execute(
            "INSERT INTO auth_direct_capabilities(grant_id,principal_id,capability_id,\
            capability_generation,revision,expires_at,created_at) VALUES(?1,?2,?3,?4,1,?5,?6)",
            params![
                IdKind::Grant.generate(),
                created.principal_id,
                RETIRE_OWN_CAPABILITY,
                retire_own.identity_generation.get(),
                deployment_expiry,
                now
            ],
        )?;
        connection.execute(
            "UPDATE auth_enrollment_secrets SET last_used_at=?2 WHERE secret_id=?1",
            params![secret, now],
        )?;
        audit(
            connection,
            None,
            "instance.enrolled",
            &created.instance_id,
            &json!({
                "deploymentId": deployment,
                "instanceId": created.instance_id,
                "secretId": secret,
                "identityKeyId": key_id,
                "title": input.title.trim(),
            }),
            now,
        )?;
        Ok(Ok(AuthServiceEnrollmentResponse {
            instance_id: ulid(&created.instance_id)?,
            deployment_id: ulid(&deployment)?,
            title: input.title.trim().to_owned().into(),
            extra: Default::default(),
        }))
    })
}

/// `POST /auth/service/identity`: tell the process holding a key what became
/// of it, so a key-file host can tell a removal from a retirement.
pub(super) async fn http_identity_status(
    axum::extract::State(engine): axum::extract::State<Arc<OAuthEngine>>,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};
    let store = Arc::clone(&engine.server.store().authority);
    let now = crate::clock::unix_now();
    let status = blocking(move || Ok::<_, AuthError>(identity_status(&store, body, now))).await;
    let refuse = |status: StatusCode, code: &str| {
        (status, axum::Json(json!({"error": {"code": code}}))).into_response()
    };
    match status {
        Ok(Ok(Ok(state))) => axum::Json(AuthServiceIdentityStatusResponse {
            state,
            extra: Default::default(),
        })
        .into_response(),
        Ok(Ok(Err(Refusal::Clock))) => refuse(StatusCode::BAD_REQUEST, "clock_skew"),
        Ok(Ok(Err(_))) => refuse(StatusCode::BAD_REQUEST, "invalid_request"),
        Ok(Err(cause)) | Err(cause) => {
            crate::http_failure::record("could not read the service identity's status", cause);
            let reference = crate::http_failure::reference("unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({"error": {"code": "unavailable", "reference": reference}})),
            )
                .into_response()
        }
    }
}

fn identity_status(
    store: &SqliteAuthorizationStore,
    body: Value,
    now: i64,
) -> Result<Result<AuthServiceIdentityState, Refusal>, AuthError> {
    let Ok(input) = serde_json::from_value::<AuthServiceIdentityStatusRequest>(body.clone()) else {
        return Ok(Err(Refusal::Invalid));
    };
    if !input.extra.is_empty() || !input.proof.extra.is_empty() {
        return Ok(Err(Refusal::Invalid));
    }
    if let Err(refusal) = verify_signed(
        store,
        &body,
        &input.identity_public_key,
        &input.identity_key_id,
        true,
        now,
    ) {
        return Ok(Err(refusal));
    }
    let key = input.identity_key_id.to_string();
    store.read(|connection| {
        let current: Option<(String, String, String, String)> = connection
            .query_row(
                "SELECT k.state,i.state,d.state,p.state FROM auth_provisioned_identities k \
                JOIN auth_instances i USING(instance_id) \
                JOIN auth_deployments d ON d.deployment_id=i.deployment_id \
                JOIN auth_principals p ON p.principal_id=i.principal_id \
                WHERE k.identity_key_id=?1 AND k.kind='service'",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((key_state, instance, deployment, principal)) = current {
            return Ok(Ok(match () {
                () if instance == "retired" => AuthServiceIdentityState::Retired,
                () if key_state != "active" || instance == "revoked" || deployment == "revoked" => {
                    AuthServiceIdentityState::Removed
                }
                () if instance == "active" && deployment == "active" && principal == "active" => {
                    AuthServiceIdentityState::Active
                }
                () => AuthServiceIdentityState::Disabled,
            }));
        }
        let outcome: Option<String> = connection
            .query_row(
                "SELECT outcome FROM auth_identity_tombstones \
                WHERE identity_key_id=?1 AND kind='service'",
                [&key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(Ok(match outcome.as_deref() {
            Some("removed") => AuthServiceIdentityState::Removed,
            Some(_) => AuthServiceIdentityState::Retired,
            None => AuthServiceIdentityState::Unregistered,
        }))
    })
}

impl SqliteAuthorizationStore {
    /// Retire service instances that stayed offline past their deployment's
    /// retirement period. Instances of a disabled deployment are skipped, and
    /// a re-enabled deployment's instances count from when it was enabled.
    /// Nothing is retired before `quiet_until`, so after Auth restarts the
    /// processes that were connected have time to come back. Returns how many
    /// were retired.
    pub(crate) fn retire_offline_instances(
        &self,
        now: i64,
        quiet_until: i64,
    ) -> Result<usize, AuthError> {
        if now < quiet_until {
            return Ok(0);
        }
        self.transaction(|connection| {
            let due = connection
                .prepare(
                    "SELECT i.instance_id FROM auth_instances i \
                    JOIN auth_deployments d USING(deployment_id) \
                    JOIN auth_instance_policy g \
                    LEFT JOIN auth_deployment_instance_policies p \
                        ON p.deployment_id=i.deployment_id \
                    WHERE i.state IN ('active','disabled') \
                    AND d.state='active' AND d.participant_kind='service' \
                    AND coalesce(p.retire_offline,g.retire_offline)=1 \
                    AND (i.enrollment_secret_id IS NOT NULL \
                        OR coalesce(p.retire_provisioned,g.retire_provisioned)=1) \
                    AND max(coalesce(i.last_seen_at,i.created_at),coalesce(d.enabled_at,0)) \
                        <=?1-coalesce(p.retire_offline_after,g.retire_offline_after) \
                    AND NOT EXISTS(SELECT 1 FROM auth_attachments a \
                        JOIN auth_authorization_sessions s USING(authorization_session_id) \
                        WHERE s.principal_id=i.principal_id AND a.state IN ('pending','admitted')) \
                    ORDER BY i.instance_id LIMIT ?2",
                )?
                .query_map(params![now, BATCH], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for instance in &due {
                retire_in(connection, instance, "offline", now)?;
            }
            Ok(due.len())
        })
    }

    /// Delete the rows of service instances retired or removed longer ago than
    /// the installation keeps them, with their identity keys and settings
    /// overrides. Each key keeps its tombstone and the principal row stays.
    /// Returns how many instances were deleted.
    pub(crate) fn clean_ended_instances(&self, now: i64) -> Result<usize, AuthError> {
        self.transaction(|connection| {
            let ended = connection
                .prepare(
                    "SELECT i.instance_id,i.state FROM auth_instances i \
                    JOIN auth_deployments d USING(deployment_id) \
                    JOIN auth_instance_policy g \
                    WHERE d.participant_kind='service' \
                    AND ((i.state='retired' AND i.retired_at<=?1-g.retired_record_days*86400) \
                    OR (i.state='revoked' AND i.updated_at<=?1-g.retired_record_days*86400)) \
                    ORDER BY i.instance_id LIMIT ?2",
                )?
                .query_map(params![now, BATCH], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (instance, state) in &ended {
                let outcome = if state == "retired" {
                    "retired"
                } else {
                    "removed"
                };
                tombstone(connection, instance, outcome, now)?;
                connection.execute(
                    "DELETE FROM auth_instances WHERE instance_id=?1",
                    [instance],
                )?;
            }
            Ok(ended.len())
        })
    }
}

/// Record that one of `principal`'s connections was admitted or closed, for
/// the offline retirement clock.
pub(super) fn seen(connection: &Connection, session: &str, now: i64) -> Result<(), AuthError> {
    connection.execute(
        "UPDATE auth_instances SET last_seen_at=?2 WHERE principal_id=(SELECT principal_id \
        FROM auth_authorization_sessions WHERE authorization_session_id=?1)",
        params![session, now],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "instances_tests.rs"]
mod tests;
