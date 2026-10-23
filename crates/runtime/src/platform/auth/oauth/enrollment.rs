//! Device enrollment and companion association share Auth's authoritative store.
use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trellis_runtime_apis::__types::trellis::AuthDeviceEnrollmentRequest;
use trellis_runtime_apis::types::*;

use super::super::sqlite::{digest, id};

pub(crate) fn register_rpc<P: trellis_rs::generated::ParticipantDescriptor>(
    provider: &mut trellis_runtime_apis::apis::trellis_auth_v1::Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
    operational: async_nats::Client,
) {
    use super::super::rpc::{caller, mutation, rpc_error};
    use trellis_protocol::PlatformPrivilege;
    let get = store.clone();
    provider.register_devices_reviews_get(move |context, input| {
        let store = get.clone();
        async move {
            caller(&context, None).map_err(rpc_error)?;
            tokio::task::spawn_blocking(move || {
                store.transaction(|connection| {
                    confirm(
                        connection,
                        &input.review_id.to_string(),
                        &input.confirmation_code,
                    )?;
                    Ok(AuthDeviceReviewResponse {
                        review: read_review(connection, &input.review_id.to_string())?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
    let list = store.clone();
    provider.register_devices_reviews_list(move |context, input| {
        let store = list.clone();
        async move {
            let actor =
                caller(&context, Some(PlatformPrivilege::PrincipalsManage)).map_err(rpc_error)?;
            tokio::task::spawn_blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_privilege(
                        connection,
                        &actor,
                        "principals.manage",
                    )?;
                    let limit = input
                        .page
                        .as_ref()
                        .and_then(|page| page.limit)
                        .unwrap_or(50);
                    if !(1..=200).contains(&limit) {
                        return Err(AuthError::Denied);
                    }
                    let cursor = input
                        .page
                        .as_ref()
                        .and_then(|page| page.cursor.as_deref())
                        .unwrap_or("");
                    let mut query = connection.prepare(
                        "SELECT review_id FROM auth_device_activation_reviews \
                        WHERE review_id>?1 ORDER BY review_id \
                        LIMIT ?2",
                    )?;
                    let mut ids = query
                        .query_map(params![cursor, limit + 1], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let next_cursor =
                        (ids.len() > limit as usize).then(|| ids[limit as usize - 1].clone());
                    ids.truncate(limit as usize);
                    Ok(trellis_runtime_apis::__types::CursorPage {
                        items: ids
                            .iter()
                            .map(|id| read_review(connection, id))
                            .collect::<Result<Vec<_>, _>>()?,
                        page: trellis_runtime_apis::__types::CursorPageInfo { next_cursor },
                    })
                })
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
    let decisions = store.clone();
    provider.register_devices_reviews_decide(move |context, input| {
        let store = decisions.clone();
        let operational = operational.clone();
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(PlatformPrivilege::PrincipalsManage),
                &input,
            )
            .map_err(rpc_error)?;
            let transaction_store = store.clone();
            let response = tokio::task::spawn_blocking(move || {
                decide(
                    &transaction_store,
                    &identity,
                    &input,
                    crate::clock::unix_now(),
                )
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)?;
            if response.review.state == AuthDeviceReviewState::Approved {
                super::super::resources::materialize_native_kv(
                    &store,
                    &operational,
                    &response.review.deployment_id.to_string(),
                    crate::clock::unix_now(),
                )
                .await
                .map_err(rpc_error)?;
            }
            Ok(response)
        }
    });
    let associations = store.clone();
    provider.register_device_companions_attach(move |context, input| {
        let store = associations.clone();
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            let verified = context
                .request()
                .verified_request()
                .ok_or_else(|| rpc_error(AuthError::Denied))?
                .caller()
                .clone();
            tokio::task::spawn_blocking(move || {
                attach(
                    &store,
                    &identity,
                    &verified,
                    &input,
                    crate::clock::unix_now(),
                )
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
    provider.register_device_companions_revoke(move |context, input| {
        let store = store.clone();
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            let delegated_admin =
                caller(&context, Some(PlatformPrivilege::PrincipalsManage)).is_ok();
            tokio::task::spawn_blocking(move || {
                revoke(
                    &store,
                    &identity,
                    delegated_admin,
                    &input,
                    crate::clock::unix_now(),
                )
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
}

fn confirm(connection: &Connection, review: &str, code: &str) -> Result<(), AuthError> {
    let expected: String = connection
        .query_row(
            "SELECT json_extract(payload_json,'$.confirmationHash') \
            FROM auth_device_activation_reviews \
            WHERE review_id=?1",
            [review],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    if expected != digest(&code)? {
        return Err(AuthError::Denied);
    }
    Ok(())
}

fn read_review(connection: &Connection, review: &str) -> Result<AuthDeviceReview, AuthError> {
    let (instance, deployment, state, expires, revision, payload): (
        String,
        String,
        String,
        i64,
        u64,
        String,
    ) = connection
        .query_row(
            "SELECT instance_id,deployment_id,state,expires_at,version,payload_json \
            FROM auth_device_activation_reviews \
            WHERE review_id=?1",
            [review],
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
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let payload: Value = serde_json::from_str(&payload)?;
    let client = format!("trellis.device.{deployment}.companion");
    let request: Option<String> = connection
        .query_row(
            "SELECT json_extract(metadata_json,'$.trellis_companion.requestId') \
            FROM auth_oauth_clients \
            WHERE client_id=?1 AND state='active'",
            [&client],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let required = request
        .as_ref()
        .map(|_| companion_required(connection, &deployment))
        .transpose()?;
    let state = if state == "pending" && expires <= crate::clock::unix_now() {
        "expired"
    } else {
        &state
    };
    Ok(serde_json::from_value(json!({
        "reviewId":review,
        "instanceId":instance,
        "deploymentId":deployment,
        "participantId":payload["participantId"],
        "state":state,
        "expiresAt":super::super::sqlite::timestamp(expires)?,
        "revision":revision.to_string(),
        "companionClientId":request.as_ref().map(|_| &client),
        "companionRequestId":request,
        "companionRequired":required
    }))?)
}

fn companion_required(connection: &Connection, deployment: &str) -> Result<bool, AuthError> {
    connection
        .query_row(
            "SELECT p.companion_required \
            FROM auth_deployment_bindings b \
            JOIN auth_deployments d USING(deployment_id) \
            JOIN auth_installed_participants p ON \
            p.participant_id=d.participant_id \
            AND p.revision=b.installed_revision \
            WHERE b.deployment_id=?1",
            [deployment],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn decide(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthDeviceReviewDecideRequest,
    now: i64,
) -> Result<AuthDeviceReviewResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.devices.reviews.decide",
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "principals.manage",
            )?;
            let id = input.review_id.to_string();
            confirm(connection, &id, &input.confirmation_code)?;
            let review = read_review(connection, &id)?;
            if review.state != AuthDeviceReviewState::Pending
                || review.revision != input.expected_revision
            {
                return Err(AuthError::Conflict);
            }
            let current: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_device_activation_reviews r \
                JOIN auth_instances i USING(instance_id) \
                JOIN auth_deployments d ON d.deployment_id=i.deployment_id \
                JOIN auth_deployment_bindings b ON b.deployment_id=d.deployment_id \
                JOIN auth_devices v ON v.principal_id=i.principal_id \
                JOIN auth_principals p ON p.principal_id=i.principal_id \
                WHERE r.review_id=?1 \
                AND i.installed_revision=b.installed_revision \
                AND i.installed_revision=json_extract(r.payload_json,\
                '$.installedRevision') \
                AND i.state IN ('active','disabled') \
                AND v.state IN ('pending','active') \
                AND p.state='active' \
                AND d.state='active' \
                AND (d.expires_at IS NULL \
                OR d.expires_at>?2))",
                params![id, now],
                |row| row.get(0),
            )?;
            if !current {
                return Err(AuthError::Conflict);
            }
            let state = match input.decision {
                AuthDecision::Approved => "approved",
                AuthDecision::Declined => "rejected",
                _ => return Err(AuthError::Denied),
            };
            connection.execute(
                "UPDATE auth_device_activation_reviews \
                SET state=?2,decided_at=?3,decided_by=?4,version=version+1 \
                WHERE review_id=?1 AND state='pending'",
                params![id, state, now, identity.actor],
            )?;
            if state == "approved" {
                connection.execute(
                    "UPDATE auth_devices \
                    SET state='active',version=version+1,updated_at=?2 \
                    WHERE principal_id=(SELECT principal_id \
                    FROM auth_device_activation_reviews \
                    WHERE review_id=?1)",
                    params![id, now],
                )?;
                connection.execute(
                    "UPDATE auth_instances \
                    SET state='active',version=version+1,updated_at=?2 \
                    WHERE instance_id=?1",
                    params![review.instance_id.to_string(), now],
                )?;
            }
            Ok(AuthDeviceReviewResponse {
                review: read_review(connection, &id)?,
                extra: Default::default(),
            })
        },
    )
}

fn attach(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    caller: &trellis_protocol::AuthenticatedCaller,
    input: &AuthDeviceCompanionAttachRequest,
    now: i64,
) -> Result<AuthDeviceCompanion, AuthError> {
    let grant = caller.oauth_grant_id.as_ref().ok_or(AuthError::Denied)?;
    store.mutate(
        identity,
        "rpc.deviceCompanions.attach",
        input,
        now,
        |connection| {
            let review_id = input.review_id.to_string();
            confirm(connection, &review_id, &input.confirmation_code)?;
            let review = read_review(connection, &review_id)?;
            if review.state != AuthDeviceReviewState::Approved
                || review.revision != input.expected_revision
            {
                return Err(AuthError::Conflict);
            }
            let instance = review.instance_id.to_string();
            let client = review
                .companion_client_id
                .as_ref()
                .ok_or(AuthError::Denied)?;
            let request = review
                .companion_request_id
                .as_ref()
                .ok_or(AuthError::Denied)?;
            let live: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_device_activation_reviews r \
                JOIN auth_instances i USING(instance_id) \
                JOIN auth_deployment_bindings b ON b.deployment_id=i.deployment_id \
                JOIN auth_deployments d ON d.deployment_id=i.deployment_id \
                WHERE r.review_id=?1 \
                AND r.expires_at>?2 \
                AND i.state='active' \
                AND d.state='active' \
                AND (d.expires_at IS NULL \
                OR d.expires_at>?2) \
                AND i.installed_revision=b.installed_revision \
                AND i.installed_revision=json_extract(r.payload_json,\
                '$.installedRevision'))",
                params![review_id, now],
                |row| row.get(0),
            )?;
            if !live {
                return Err(AuthError::Denied);
            }
            let (required, optional, approved, expiry): (String, String, String, i64) = connection
                .query_row(
                    "SELECT g.required_capabilities_json,g.optional_capabilities_json,\
                    g.approved_capabilities_json,g.expires_at \
                    FROM auth_oauth_grants g \
                    JOIN auth_login_sessions l USING(login_session_id) \
                    JOIN auth_principals p ON p.principal_id=g.principal_id \
                    WHERE g.oauth_grant_id=?1 \
                    AND g.principal_id=?2 \
                    AND g.client_id=?3 AND g.binding_kind='native' \
                    AND g.revoked_at IS NULL \
                    AND g.expires_at>?4 \
                    AND l.revoked_at IS NULL \
                    AND l.expires_at>?4 \
                    AND p.state='active' \
                    AND EXISTS(SELECT 1 \
                    FROM auth_oauth_families f \
                    WHERE f.oauth_grant_id=g.oauth_grant_id \
                    AND f.revoked_at IS NULL \
                    AND f.expires_at>?4)",
                    params![grant, identity.actor, client.as_ref(), now],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
                .ok_or(AuthError::Denied)?;
            validate_selection(
                connection,
                client.as_ref(),
                &serde_json::from_str::<Vec<String>>(&required)?,
                &serde_json::from_str::<Vec<String>>(&optional)?,
            )?;
            let current = super::super::policy::evaluate(
                connection,
                &super::super::Credential::OAuthGrant(grant.clone()),
                now,
            )?;
            if !current.required_missing.is_empty() {
                return Err(AuthError::RequiredMissing);
            }
            let old: Option<(String, u64)> = connection
                .query_row(
                    "SELECT state,revision FROM auth_device_companions WHERE instance_id=?1",
                    [&instance],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if old.as_ref().is_some_and(|(state, _)| state == "active") {
                return Err(AuthError::Conflict);
            }
            let revision = old.map_or(1, |(_, revision)| revision + 1);
            connection.execute(
                "INSERT INTO auth_device_companions(instance_id,required,\
                companion_request_id,oauth_grant_id,approved_capabilities_json,state,\
                expires_at,revision) \
                VALUES(?1,?2,?3,?4,?5,'active',?6,?7) \
                ON CONFLICT(instance_id) \
                DO UPDATE SET required=excluded.required,\
                companion_request_id=excluded.companion_request_id,\
                oauth_grant_id=excluded.oauth_grant_id,\
                approved_capabilities_json=excluded.approved_capabilities_json,\
                state='active',expires_at=excluded.expires_at,\
                revision=excluded.revision",
                params![
                    instance,
                    review.companion_required.unwrap_or(false),
                    request,
                    grant,
                    approved,
                    expiry,
                    revision
                ],
            )?;
            if connection.execute(
                "UPDATE auth_device_activation_reviews \
                SET activated_by_user_principal_id=?2,version=version+1 \
                WHERE review_id=?1 AND activated_by_user_principal_id IS NULL",
                params![review_id, identity.actor],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            Ok(AuthDeviceCompanion {
                instance_id: review.instance_id,
                client_id: client.clone(),
                request_id: request.clone(),
                oauth_grant_id: grant.parse().map_err(|_| AuthError::Unavailable)?,
                revision: AuthRevision(revision.into()),
                extra: Default::default(),
            })
        },
    )
}

pub(in crate::platform::auth) fn validate_selection(
    connection: &Connection,
    client: &str,
    required: &[String],
    optional: &[String],
) -> Result<(), AuthError> {
    let metadata: Option<String> = connection
        .query_row(
            "SELECT metadata_json FROM auth_oauth_clients WHERE client_id=?1",
            [client],
            |row| row.get(0),
        )
        .optional()?;
    let Some(metadata) = metadata else {
        // Only managed clients have Auth-owned companion bindings. A validated
        // CIMD client may request ordinary consent, never assert such a binding.
        let known: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_cimd_clients WHERE client_id=?1)",
            [client],
            |row| row.get(0),
        )?;
        return if known {
            Ok(())
        } else {
            Err(AuthError::Denied)
        };
    };
    let metadata: Value = serde_json::from_str(&metadata)?;
    let Some(deployment) = metadata["trellis_companion"]["deploymentId"].as_str() else {
        return Ok(());
    };
    let (participant, evidence, metadata): (String, String, String) = connection
        .query_row(
            "SELECT d.participant_id,p.projection_json,c.metadata_json \
            FROM auth_deployments d \
            JOIN auth_deployment_bindings b USING(deployment_id) \
            JOIN auth_installed_participants p ON \
            p.participant_id=d.participant_id \
            AND p.revision=b.installed_revision \
            JOIN auth_oauth_clients c ON c.client_id=?2 \
            WHERE d.deployment_id=?1 \
            AND d.state='active' \
            AND c.state='active' \
            AND (d.expires_at IS NULL \
            OR d.expires_at>?3)",
            params![deployment, client, crate::clock::unix_now()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(AuthError::Denied)?;
    let graph = trellis_idl::compile_evidence(serde_json::from_str(&evidence)?)
        .map_err(|_| AuthError::Denied)?;
    let companion = graph
        .packages()
        .values()
        .flat_map(|package| package.participants().values())
        .find(|value| value.identity().as_str() == participant)
        .and_then(|value| value.companion())
        .ok_or(AuthError::Denied)?;
    let request = graph
        .packages()
        .values()
        .find_map(|package| package.app_requests().get(&companion.request))
        .ok_or(AuthError::Denied)?;
    let metadata: Value = serde_json::from_str(&metadata)?;
    if metadata["trellis_companion"]["requestId"].as_str() != Some(companion.request.as_str())
        || request.kind() != trellis_idl::AppKind::Native
    {
        return Err(AuthError::Denied);
    }
    let mut expected = request
        .uses()
        .values()
        .flat_map(|selection| selection.required_capabilities.iter())
        .map(ToString::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    if !request.states().is_empty() {
        expected.extend(
            [
                "trellis.state@v1::read",
                "trellis.state@v1::write",
                "trellis.state@v1::delete",
            ]
            .map(str::to_owned),
        );
    }
    let allowed = request
        .uses()
        .values()
        .flat_map(|selection| selection.optional_capabilities.iter())
        .map(ToString::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    if required
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != expected
        || optional.iter().any(|id| !allowed.contains(id))
    {
        return Err(AuthError::Denied);
    }
    Ok(())
}

fn revoke(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    delegated_admin: bool,
    input: &AuthDeviceCompanionRevokeRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        "rpc.deviceCompanions.revoke",
        input,
        now,
        |connection| {
            let instance = input.instance_id.to_string();
            let (grant, owner, revision, revoked): (String, String, u64, Option<i64>) = connection
                .query_row(
                    "SELECT c.oauth_grant_id,g.principal_id,c.revision,g.revoked_at \
                    FROM auth_device_companions c \
                    JOIN auth_oauth_grants g USING(oauth_grant_id) \
                    WHERE c.instance_id=?1 \
                    AND c.state='active'",
                    [&instance],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            if owner != identity.actor {
                if !delegated_admin {
                    return Err(AuthError::Denied);
                }
                SqliteAuthorizationStore::require_privilege(
                    connection,
                    &identity.actor,
                    "principals.manage",
                )?;
            }
            if revision != input.expected_revision.0 .0 {
                return Err(AuthError::Conflict);
            }
            if revoked.is_none() {
                let scope = super::super::EnforcementScope::Grant(grant);
                let mutation = Mutation {
                    request_digest: digest(&scope)?,
                    ..identity.clone()
                };
                store.revoke_root_in_transaction(connection, &mutation, &scope, now)?;
            }
            connection.execute(
                "UPDATE auth_device_companions \
                SET state='revoked',revision=revision+1 \
                WHERE instance_id=?1",
                [instance],
            )?;
            Ok(AuthMutationResult {
                revision: AuthRevision((revision + 1).into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) async fn http_enroll(
    axum::extract::State(engine): axum::extract::State<Arc<OAuthEngine>>,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let result = tokio::task::spawn_blocking(move || {
        enroll(
            &engine.server.store().authority,
            body,
            crate::clock::unix_now(),
        )
    })
    .await;
    match result {
        Ok(Ok(value)) => axum::Json(value).into_response(),
        Ok(Err(AuthError::Unavailable)) | Err(_) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error":{"code":"unavailable"}})),
        )
            .into_response(),
        _ => (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(json!({"error":{"code":"device_enrollment_denied"}})),
        )
            .into_response(),
    }
}

fn enroll(store: &SqliteAuthorizationStore, mut body: Value, now: i64) -> Result<Value, AuthError> {
    let input: AuthDeviceEnrollmentRequest = serde_json::from_value(body.clone())?;
    if !input.extra.is_empty()
        || !input.proof.extra.is_empty()
        || !input.session_proof.extra.is_empty()
    {
        return Err(AuthError::Denied);
    }
    let identity = input.identity_public_key.as_ref();
    let raw = URL_SAFE_NO_PAD
        .decode(identity)
        .map_err(|_| AuthError::Denied)?;
    if URL_SAFE_NO_PAD.encode(Sha256::digest(&raw)) != input.identity_key_id.as_ref() {
        return Err(AuthError::Denied);
    }
    let request = body.as_object_mut().ok_or(AuthError::Denied)?;
    let proof = serde_json::from_value(request.remove("proof").ok_or(AuthError::Denied)?)?;
    let session_proof =
        serde_json::from_value(request.remove("sessionProof").ok_or(AuthError::Denied)?)?;
    let make = |request| {
        trellis_protocol::SessionProofInput::device_enrollment(
            trellis_protocol::DeviceEnrollmentSessionProofInput {
                origin: store.settings.oauth_origin.clone(),
                unsigned_request: request,
            },
        )
        .map_err(|_| AuthError::Denied)
    };
    let policy =
        trellis_protocol::SessionProofPolicy::new(60_000, 5_000).map_err(|_| AuthError::Denied)?;
    trellis_protocol::verify_session_proof(
        &make(body.clone())?,
        &proof,
        identity,
        now * 1000,
        policy,
    )
    .map_err(|_| AuthError::Denied)?;
    let mut session_body = body.clone();
    let session_raw = URL_SAFE_NO_PAD
        .decode(input.session_key.as_ref())
        .map_err(|_| AuthError::Denied)?;
    session_body["identityKeyId"] = URL_SAFE_NO_PAD.encode(Sha256::digest(session_raw)).into();
    trellis_protocol::verify_session_proof(
        &make(session_body)?,
        &session_proof,
        input.session_key.as_ref(),
        now * 1000,
        policy,
    )
    .map_err(|_| AuthError::Denied)?;
    let attempt = digest(&(
        input.identity_key_id.as_ref(),
        input.challenge_digest.as_ref(),
        &input.participant_id,
    ))?;
    store.transaction(|connection| {
        let known: Option<String> = connection
            .query_row(
                "SELECT instance_id FROM auth_provisioned_identities \
                WHERE identity_key_id=?1 \
                AND state='active'",
                [input.identity_key_id.as_ref()],
                |row| row.get(0),
            )
            .optional()?;
        let instance = match known {
            Some(instance) => instance,
            None => {
                let secret = input
                    .provisioning_secret
                    .as_ref()
                    .ok_or(AuthError::Denied)?;
                let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()));
                let (secret_id, instance): (String, String) = connection
                    .query_row(
                        "SELECT secret_id,instance_id \
                        FROM auth_device_provisioning_secrets \
                        WHERE secret_hash=?1 \
                        AND state='pending' \
                        AND expires_at>?2",
                        params![hash, now],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .ok_or(AuthError::Denied)?;
                let (principal, deployment, participant): (String, String, String) = connection
                    .query_row(
                        "SELECT i.principal_id,i.deployment_id,d.participant_id \
                        FROM auth_instances i \
                        JOIN auth_deployments d USING(deployment_id) \
                        JOIN auth_devices v ON v.principal_id=i.principal_id \
                        WHERE i.instance_id=?1 \
                        AND v.state='pending' \
                        AND d.state='active' \
                        AND (d.expires_at IS NULL \
                        OR d.expires_at>?2)",
                        params![instance, now],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                    .ok_or(AuthError::Denied)?;
                if participant != input.participant_id {
                    return Err(AuthError::Denied);
                }
                connection.execute(
                    "INSERT INTO auth_provisioned_identities(identity_key_id,principal_id,\
                    deployment_id,instance_id,participant_id,kind,identity_public_key,\
                    state,created_at) \
                    VALUES(?1,?2,?3,?4,?5,'device',?6,'active',?7)",
                    params![
                        input.identity_key_id.as_ref(),
                        principal,
                        deployment,
                        instance,
                        participant,
                        identity,
                        now
                    ],
                )?;
                if connection.execute(
                    "UPDATE auth_device_provisioning_secrets \
                    SET state='consumed',consumed_at=?2,version=version+1 \
                    WHERE secret_id=?1 AND state='pending'",
                    params![secret_id, now],
                )? != 1
                {
                    return Err(AuthError::Conflict);
                }
                instance
            }
        };
        let (principal, deployment, participant, revision): (String, String, String, u64) =
            connection
                .query_row(
                    "SELECT i.principal_id,i.deployment_id,d.participant_id,\
                    i.installed_revision \
                    FROM auth_instances i \
                    JOIN auth_deployments d USING(deployment_id) \
                    JOIN auth_devices v ON v.principal_id=i.principal_id \
                    JOIN auth_deployment_bindings b USING(deployment_id) \
                    WHERE i.instance_id=?1 \
                    AND i.state IN ('active','disabled') \
                    AND v.state IN ('pending','active') \
                    AND d.state='active' \
                    AND i.installed_revision=b.installed_revision \
                    AND (d.expires_at IS NULL \
                    OR d.expires_at>?2)",
                    params![instance, now],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
                .ok_or(AuthError::Denied)?;
        if participant != input.participant_id {
            return Err(AuthError::Denied);
        }
        let existing: Option<(String, String, i64)> = connection
            .query_row(
                "SELECT review_id,state,expires_at \
                FROM auth_device_activation_reviews \
                WHERE instance_id=?1 \
                AND request_digest=?2 \
                ORDER BY requested_at DESC \
                LIMIT 1",
                params![instance, attempt],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (review, state, expiry) = if let Some(existing) = existing {
            existing
        } else {
            let pending: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_device_activation_reviews \
                WHERE instance_id=?1 \
                AND state='pending' \
                AND expires_at>?2)",
                params![instance, now],
                |row| row.get(0),
            )?;
            if pending {
                return Err(AuthError::Conflict);
            }
            let review = id();
            let payload = json!({
                "participantId":participant,
                "installedRevision":revision,
                "identityPublicKey":identity,
                "sessionKey":input.session_key.as_ref(),
                "confirmationHash":digest(&input.confirmation_code)?
            });
            connection.execute(
                "INSERT INTO auth_device_activation_reviews(review_id,principal_id,\
                deployment_id,instance_id,request_digest,payload_json,state,\
                requested_at,expires_at,version) \
                VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?8,1)",
                params![
                    review,
                    principal,
                    deployment,
                    instance,
                    attempt,
                    payload.to_string(),
                    now,
                    now + 900
                ],
            )?;
            (review, "pending".to_owned(), now + 900)
        };
        let state = if expiry <= now && state == "pending" {
            "expired"
        } else {
            &state
        };
        Ok(json!({
            "serverNow":now*1000,
            "state":state,
            "activation": {
                "state":state,
                "reviewId":review,
                "principalId":principal,
                "deploymentId":deployment,
                "instanceId":instance,
                "activationUrl":format!("{}/device-enrollment?reviewId={review}",store.settings.oauth_origin),
                "expiresAt":expiry*1000,
                "retryAfterMs":1000
                    }
        }))
    })
}
use super::super::{AuthError, Mutation, SqliteAuthorizationStore};
use super::{OAuthEngine, OAuthStorage};

pub(crate) fn provision(
    storage: &OAuthStorage,
    identity: &Mutation,
    input: &AuthInstanceProvisionRequest,
    now: i64,
) -> Result<AuthInstanceProvisionResponse, AuthError> {
    let lookup = digest(&(
        identity.actor.as_str(),
        identity.request_id.as_str(),
        "device.provision",
    ))?;
    let sealed: Vec<u8> =
        storage
            .authority
            .mutate(identity, "rpc.device.provision", input, now, |connection| {
                SqliteAuthorizationStore::require_privilege(
                    connection,
                    &identity.actor,
                    "principals.manage",
                )?;
                let deployment = input.deployment_id.to_string();
                let kind: String = connection.query_row(
                    "SELECT participant_kind FROM auth_deployments WHERE deployment_id=?1",
                    [&deployment],
                    |row| row.get(0),
                )?;
                if kind != "device" {
                    return Err(AuthError::Denied);
                }
                let result = SqliteAuthorizationStore::provision_instance_in_transaction(
                    connection,
                    &deployment,
                    input.identity_public_key.as_ref().map(|key| key.as_ref()),
                    &input.title,
                    now,
                )?;
                let secret = if input.identity_public_key.is_none() {
                    let mut bytes = [0u8; 32];
                    getrandom::fill(&mut bytes).map_err(|_| AuthError::Unavailable)?;
                    let secret = URL_SAFE_NO_PAD.encode(bytes);
                    let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()));
                    connection.execute(
                        "INSERT INTO auth_device_provisioning_secrets(secret_id,instance_id,\
                        secret_hash,state,created_at,expires_at,version) \
                        VALUES(?1,?2,?3,'pending',?4,?5,1)",
                        params![id(), result.instance_id, hash, now, now + 86400],
                    )?;
                    Some(secret)
                } else {
                    None
                };
                let response = AuthInstanceProvisionResponse {
                    instance: super::super::rpc_deployments::read_instance(
                        connection,
                        &result.instance_id,
                    )?,
                    provisioning_secret: secret,
                    extra: Default::default(),
                };
                storage
                    .encode("device-provision-response", &lookup, &response)
                    .map_err(|_| AuthError::Unavailable)
            })?;
    storage
        .decode("device-provision-response", &lookup, &sealed)
        .map_err(|_| AuthError::Unavailable)
}
