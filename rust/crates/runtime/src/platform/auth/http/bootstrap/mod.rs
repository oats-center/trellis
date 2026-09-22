use std::collections::BTreeMap;

use super::super::{AuthorizationContextBundle, AuthorizationContextIssueRequest};
use super::*;
use crate::platform::auth::{
    IssuanceConnection, IssuanceCredential, ProvisionedIdentityKind, ProvisionedIdentityState,
};

mod enroll;
pub(super) use enroll::device_enroll;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeBootstrapRequest {
    package_evidence: trellis_runtime_apis::types::AuthPackageEvidence,
    participant_path: String,
    package_digest: String,
    identity_key_id: String,
    session_key: String,
    connection_id: String,
    request_id: String,
    #[serde(rename = "iat")]
    _iat: i64,
    name: Option<String>,
    companion: Option<CompanionBootstrapRequest>,
    proof: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompanionBootstrapRequest {
    connection_id: String,
    request_id: String,
    issued_at: i64,
    session_key: String,
    proof: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BootstrapResponse {
    server_now: i64,
    authorization_context: AuthorizationContextBundle,
    routing: BootstrapRouting,
    runtime: BootstrapRuntime,
    api_bindings: BTreeMap<String, trellis_rs::client::AuthorizationApiBinding>,
    transports: BootstrapTransports,
    authorization: BootstrapAuthorization,
    #[serde(skip_serializing_if = "Option::is_none")]
    companion: Option<CompanionBootstrapResponse>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompanionBootstrapResponse {
    participant_id: String,
    login_session_id: String,
    required: bool,
    installation: Box<BootstrapResponse>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapRouting {
    bootstrap_jwt: String,
    bootstrap_jwt_expires_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapRuntime {
    connection_id: String,
    login_session_id: Option<String>,
    participant_id: String,
    inbox_prefix: String,
}

#[derive(Serialize)]
struct BootstrapTransports {
    #[serde(skip_serializing_if = "Option::is_none")]
    native: Option<BootstrapTransport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    websocket: Option<BootstrapTransport>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapTransport {
    nats_servers: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapAuthorization {
    participant_id: String,
    participant_digest: String,
    resource_runtime: ServiceResourceBindings,
}

pub(super) async fn service_bootstrap<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Json(raw): Json<Value>,
) -> Result<Json<BootstrapResponse>, HttpError>
where
    R: AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + GrantRepository
        + ProvisioningRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    bootstrap(&state, raw, ProvisionedIdentityKind::Service)
        .await
        .map(Json)
}

pub(super) async fn device_bootstrap<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Json(raw): Json<Value>,
) -> Result<Json<BootstrapResponse>, HttpError>
where
    R: AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + GrantRepository
        + ProvisioningRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    bootstrap(&state, raw, ProvisionedIdentityKind::Device)
        .await
        .map(Json)
}

async fn bootstrap<R, E>(
    state: &AuthHttpState<R, E>,
    raw: Value,
    expected_kind: ProvisionedIdentityKind,
) -> Result<BootstrapResponse, HttpError>
where
    R: AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + GrantRepository
        + ProvisioningRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let request: NativeBootstrapRequest = serde_json::from_value(raw.clone())
        .map_err(|_| HttpError::bad_request("invalid_native_bootstrap"))?;
    if request
        .name
        .as_ref()
        .is_some_and(|name| name.chars().count() > 128)
    {
        return Err(HttpError::bad_request("invalid_native_bootstrap"));
    }
    let mut unsigned_request = raw.clone();
    unsigned_request
        .as_object_mut()
        .ok_or_else(|| HttpError::bad_request("invalid_native_bootstrap"))?
        .remove("proof");
    let proof_input = match expected_kind {
        ProvisionedIdentityKind::Service => {
            SessionProofInput::service_bootstrap(NativeBootstrapSessionProofInput {
                origin: state.public_origin.clone(),
                unsigned_request,
            })
        }
        ProvisionedIdentityKind::Device => {
            SessionProofInput::device_bootstrap(NativeBootstrapSessionProofInput {
                origin: state.public_origin.clone(),
                unsigned_request,
            })
        }
    }
    .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
    let identity = state
        .service
        .repository()
        .get_provisioned_identity(&request.identity_key_id)
        .await?
        .ok_or_else(|| HttpError::unauthorized("identity_not_found"))?;
    if identity.kind != expected_kind || identity.state != ProvisionedIdentityState::Active {
        return Err(HttpError::unauthorized("identity_inactive"));
    }
    let assigned_participant = state
        .service
        .repository()
        .get_credential_participant_assignment(request.identity_key_id.clone())
        .await?
        .ok_or_else(|| HttpError::unauthorized("participant_assignment_missing"))?;
    if assigned_participant
        != format!(
            "{}.{}",
            request.package_evidence.root_package, request.participant_path
        )
    {
        return Err(HttpError::unauthorized("participant_assignment_mismatch"));
    }
    let proof = parse_session_proof(&request.proof)
        .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
    verify_session_proof(
        &proof_input,
        &proof,
        &identity.identity_public_key,
        now_ms()?,
        state.proof_policy,
    )
    .map_err(|_| HttpError::unauthorized("invalid_proof"))?;

    let evidence = crate::platform::auth::evidence::PackageEvidenceInput::from_generated_wire(
        request.package_evidence,
        request.participant_path,
        request.package_digest,
    )?;
    let now = now_ms()?;
    let presented = state
        .service
        .repository()
        .accept_presented_package(evidence, now)
        .await?;
    let (_, installed) = state
        .service
        .repository()
        .get_installed_participant_record(presented.participant_id.clone(), None)
        .await?
        .ok_or(AuthorizationStateError::ParticipantMissing)?;
    if presented.package_digest != installed.package_digest
        || presented.participant_path != installed.participant_path
        || presented.participant_digest != installed.participant_digest
    {
        return Err(HttpError::conflict("participant_evidence_mismatch"));
    }
    let installed_projection = installed.resolve()?;
    let companion_response = if expected_kind == ProvisionedIdentityKind::Device {
        let delegation = state
            .service
            .repository()
            .get_device_delegation(&identity.principal_id, &identity.deployment_id)
            .await?;
        match (
            installed_projection.companion_participant_id.as_deref(),
            delegation,
            request.companion,
        ) {
            (None, None, None) => None,
            (Some(_), None, _) if !installed_projection.companion_required => None,
            (Some(_), Some(_), None) if !installed_projection.companion_required => None,
            (Some(_), Some(delegation), _)
                if !installed_projection.companion_required
                    && delegation.state != crate::platform::auth::DeviceDelegationState::Active =>
            {
                None
            }
            (Some(_), None, _) => return Err(HttpError::conflict("companion_delegation_missing")),
            (Some(expected), Some(delegation), Some(companion))
                if delegation.state == crate::platform::auth::DeviceDelegationState::Active
                    && delegation.companion_participant_id.as_deref() == Some(expected) =>
            {
                let installation_public_key = delegation
                    .installation_public_key
                    .ok_or_else(|| HttpError::conflict("companion_delegation_incomplete"))?;
                let login_session_id = delegation
                    .user_login_session_id
                    .ok_or_else(|| HttpError::conflict("companion_delegation_incomplete"))?;
                ulid::Ulid::from_string(&companion.connection_id)
                    .map_err(|_| HttpError::bad_request("invalid_companion_connection_id"))?;
                let digest = trellis_protocol::digest_json(&serde_json::json!({
                    "format": "trellis.device.user-companion.v1",
                    "origin": state.public_origin,
                    "identityKeyId": request.identity_key_id,
                    "participantId": expected,
                    "connectionId": companion.connection_id,
                    "requestId": companion.request_id,
                    "issuedAt": companion.issued_at,
                    "sessionKey": companion.session_key,
                }))
                .map_err(|_| HttpError::bad_request("invalid_companion_bootstrap"))?;
                verify_detached_companion_proof(
                    &installation_public_key,
                    &digest,
                    &companion.proof,
                )?;
                let installation = issue_bootstrap(
                    state,
                    IssuanceConnection {
                        credential: IssuanceCredential::Login(login_session_id.clone()),
                        connection_id: companion.connection_id,
                        session_public_key: companion.session_key,
                    },
                    companion.request_id,
                    digest,
                    now,
                )
                .await;
                match installation {
                    Ok(installation) => Some(CompanionBootstrapResponse {
                        participant_id: expected.to_owned(),
                        login_session_id,
                        required: delegation.required,
                        installation: Box::new(installation),
                    }),
                    Err(error) if delegation.required => return Err(error),
                    Err(_) => None,
                }
            }
            _ => return Err(HttpError::conflict("companion_bootstrap_mismatch")),
        }
    } else {
        None
    };
    let connection = IssuanceConnection {
        credential: IssuanceCredential::Native(request.identity_key_id),
        connection_id: request.connection_id,
        session_public_key: request.session_key.clone(),
    };
    let mut response = issue_bootstrap(
        state,
        connection,
        request.request_id,
        proof_request_digest(&raw)
            .map_err(|_| HttpError::bad_request("invalid_native_bootstrap"))?,
        now,
    )
    .await?;
    response.companion = companion_response;
    Ok(response)
}

fn verify_detached_companion_proof(
    public_key: &str,
    digest: &str,
    signature: &str,
) -> Result<(), HttpError> {
    crate::platform::auth::verify_detached_ed25519_proof(public_key, digest, signature)
        .map_err(|_| HttpError::unauthorized("invalid_companion_proof"))
}

#[tracing::instrument(
    name = "trellis.auth.issue_bootstrap",
    skip_all,
    fields(trellis.surface = "auth", trellis.operation = "issue_bootstrap")
)]
pub(super) async fn issue_bootstrap<R, E>(
    state: &AuthHttpState<R, E>,
    connection: IssuanceConnection,
    request_id: String,
    request_digest: String,
    now: i64,
) -> Result<BootstrapResponse, HttpError>
where
    R: AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + GrantRepository
        + ProvisioningRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let total_started = std::time::Instant::now();
    let result = async {
        let session_key = connection.session_public_key.clone();
        let context_issue_started = std::time::Instant::now();
        let context_issue = state
            .authorization_contexts
            .issue_with_state(
                AuthorizationContextIssueRequest {
                    connection,
                    request_id,
                    request_digest,
                },
                now / 1_000,
            )
            .await;
        crate::telemetry::record_duration(
            crate::telemetry::DurationMetric::AuthFlow,
            context_issue_started.elapsed(),
            "auth",
            "issue_bootstrap",
            "context_issue",
            if context_issue.is_ok() {
                crate::telemetry::Outcome::Ok
            } else {
                crate::telemetry::Outcome::Error
            },
        );
        let (authorization_context, issuance) = context_issue.map_err(map_issuance_error)?;
        let bootstrap_jwt_expires_at = authorization_context
            .context
            .get("expiresAt")
            .and_then(Value::as_i64)
            .ok_or_else(|| HttpError::internal("issued_context_invalid"))?;
        let route = state.issuer.deny_all_user_jwt(
            &session_public_key_to_user_nkey(&session_key)?,
            bootstrap_jwt_expires_at,
            now / 1_000,
        )?;
        let transports = BootstrapTransports {
            native: (!state.native_nats_servers.is_empty()).then(|| BootstrapTransport {
                nats_servers: state.native_nats_servers.clone(),
            }),
            websocket: (!state.websocket_nats_servers.is_empty()).then(|| BootstrapTransport {
                nats_servers: state.websocket_nats_servers.clone(),
            }),
        };
        if transports.native.is_none() && transports.websocket.is_none() {
            return Err(HttpError::internal("transport_unavailable"));
        }
        if issuance
            .participant
            .projection
            .implemented_apis
            .values()
            .any(|api| {
                api.actions.values().any(|action| {
                    action.kind == crate::platform::auth::evidence::RuntimeActionKind::Operation
                })
            })
        {
            let deployment_id = issuance
                .deployment_id
                .as_deref()
                .ok_or_else(|| HttpError::internal("provider_deployment_missing"))?;
            let operation_store_started = std::time::Instant::now();
            let operation_store = crate::platform::auth::resources::ensure_operation_store(
                &state.nats,
                deployment_id,
            )
            .await;
            crate::telemetry::record_duration(
                crate::telemetry::DurationMetric::AuthFlow,
                operation_store_started.elapsed(),
                "auth",
                "issue_bootstrap",
                "operation_store",
                if operation_store.is_ok() {
                    crate::telemetry::Outcome::Ok
                } else {
                    crate::telemetry::Outcome::Error
                },
            );
            operation_store?;
        }
        let api_bindings_started = std::time::Instant::now();
        let api_bindings = crate::platform::auth::current_api_bindings(
            state.service.repository(),
            &issuance.participant,
            issuance.deployment_id.as_deref(),
        )
        .await;
        crate::telemetry::record_duration(
            crate::telemetry::DurationMetric::AuthFlow,
            api_bindings_started.elapsed(),
            "auth",
            "issue_bootstrap",
            "api_bindings",
            if api_bindings.is_ok() {
                crate::telemetry::Outcome::Ok
            } else {
                crate::telemetry::Outcome::Error
            },
        );
        let api_bindings = api_bindings?;
        Ok(BootstrapResponse {
            server_now: now,
            authorization_context,
            routing: BootstrapRouting {
                bootstrap_jwt: route.jwt,
                bootstrap_jwt_expires_at: route.expires_at,
            },
            runtime: BootstrapRuntime {
                connection_id: issuance.connection_id,
                login_session_id: issuance.login_session_id,
                participant_id: issuance.participant.participant_id.clone(),
                inbox_prefix: issuance.inbox_prefix,
            },
            api_bindings,
            transports,
            authorization: BootstrapAuthorization {
                participant_id: issuance.participant.participant_id.clone(),
                participant_digest: issuance.participant.participant_digest.clone(),
                resource_runtime: project_service_resource_bindings(
                    &issuance.participant.projection,
                    &issuance.resource_bindings,
                    &issuance.participant.participant_id,
                )?,
            },
            companion: None,
        })
    }
    .await;
    crate::telemetry::record_duration(
        crate::telemetry::DurationMetric::AuthFlow,
        total_started.elapsed(),
        "auth",
        "issue_bootstrap",
        "total",
        if result.is_ok() {
            crate::telemetry::Outcome::Ok
        } else {
            crate::telemetry::Outcome::Error
        },
    );
    result
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::verify_detached_companion_proof;

    #[test]
    fn companion_proof_covers_the_domain_separated_digest() {
        let key = SigningKey::from_bytes(&[11; 32]);
        let digest = [17; 32];
        let public_key = URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes());
        let signature = URL_SAFE_NO_PAD.encode(key.sign(&digest).to_bytes());
        assert!(verify_detached_companion_proof(
            &public_key,
            &URL_SAFE_NO_PAD.encode(digest),
            &signature,
        )
        .is_ok());
        assert!(verify_detached_companion_proof(
            &public_key,
            &URL_SAFE_NO_PAD.encode([18; 32]),
            &signature,
        )
        .is_err());
        let substituted_key = SigningKey::from_bytes(&[12; 32]);
        assert!(verify_detached_companion_proof(
            &URL_SAFE_NO_PAD.encode(substituted_key.verifying_key().to_bytes()),
            &URL_SAFE_NO_PAD.encode(digest),
            &signature,
        )
        .is_err());
    }
}
