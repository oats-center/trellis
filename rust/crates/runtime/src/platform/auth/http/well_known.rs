use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use trellis_protocol::{
    parse_session_proof, verify_session_proof, AuthorizationContextRefreshSessionProofInput,
    SessionProofInput,
};

use super::super::context::AuthorizationContextRepository;
use super::super::ephemeral::AuthEphemeralRepository;
use super::super::{
    AuthorityEvidenceRepository, GrantRepository, IssuanceConnection, IssuanceCredential,
};
use super::{
    bootstrap, now_ms, proof_request_digest, AuthHttpState, ContextRepository,
    DeploymentRepository, HttpError, ProvisioningRepository, SessionRepository,
};

pub(super) async fn issuer_key<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(key_id): Path<String>,
) -> Result<impl axum::response::IntoResponse, HttpError>
where
    R: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    let key = state
        .authorization_contexts
        .issuer_key(key_id, now_ms()?)
        .await?
        .ok_or_else(|| HttpError::not_found("issuer_key_not_found"))?;
    Ok(([(axum::http::header::CACHE_CONTROL, "no-store")], Json(key)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ContextRefreshRequest {
    login_session_id: String,
    connection_id: String,
    session_key: String,
    current_context_digest: RequiredNullableString,
    request_id: String,
    #[serde(rename = "issuedAt")]
    _issued_at: i64,
    name: Option<String>,
    proof: Value,
}

#[derive(Deserialize)]
pub(super) struct RequiredNullableString(Option<String>);

#[tracing::instrument(
    name = "trellis.auth.context_refresh",
    skip_all,
    fields(trellis.surface = "auth", trellis.operation = "context_refresh")
)]
pub(super) async fn refresh_context<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Json(raw): Json<Value>,
) -> Result<Json<bootstrap::BootstrapResponse>, HttpError>
where
    R: AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + GrantRepository
        + ProvisioningRepository
        + SessionRepository
        + AuthorizationContextRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let total_started = std::time::Instant::now();
    let result = async {
        let request: ContextRefreshRequest = serde_json::from_value(raw.clone())
            .map_err(|_| HttpError::bad_request("invalid_context_refresh"))?;
        if request
            .name
            .as_ref()
            .is_some_and(|name| name.chars().count() > 128)
        {
            return Err(HttpError::bad_request("invalid_context_refresh"));
        }
        let session = state
            .service
            .repository()
            .get_session(&request.login_session_id)
            .await?
            .ok_or_else(|| HttpError::unauthorized("login_not_found"))?;
        let mut unsigned_request = raw.clone();
        unsigned_request
            .as_object_mut()
            .ok_or_else(|| HttpError::bad_request("invalid_context_refresh"))?
            .remove("proof");
        let proof_input = SessionProofInput::authorization_context_refresh(
            AuthorizationContextRefreshSessionProofInput {
                origin: state.public_origin.clone(),
                session_public_key: session.session_public_key.clone(),
                unsigned_request,
            },
        )
        .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
        verify_session_proof(
            &proof_input,
            &parse_session_proof(&request.proof)
                .map_err(|_| HttpError::unauthorized("invalid_proof"))?,
            &session.session_public_key,
            now_ms()?,
            state.proof_policy,
        )
        .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
        if let Some(digest) = &request.current_context_digest.0 {
            let current = state
                .service
                .repository()
                .get_context_by_digest(digest)
                .await?
                .ok_or_else(|| HttpError::unauthorized("context_not_found"))?;
            if current.login_session_id.as_deref() != Some(request.login_session_id.as_str())
                || current.connection_id != request.connection_id
                || current.session_public_key != request.session_key
            {
                return Err(HttpError::unauthorized("context_owner_mismatch"));
            }
        }
        let now = now_ms()?;
        Ok(Json(
            bootstrap::issue_bootstrap(
                &state,
                IssuanceConnection {
                    credential: IssuanceCredential::Login(request.login_session_id),
                    connection_id: request.connection_id,
                    session_public_key: request.session_key,
                },
                request.request_id,
                proof_request_digest(&raw)
                    .map_err(|_| HttpError::bad_request("invalid_context_refresh"))?,
                now,
            )
            .await?,
        ))
    }
    .await;
    crate::telemetry::record_duration(
        crate::telemetry::DurationMetric::AuthFlow,
        total_started.elapsed(),
        "auth",
        "context_refresh",
        "total",
        if result.is_ok() {
            crate::telemetry::Outcome::Ok
        } else {
            crate::telemetry::Outcome::Error
        },
    );
    result
}
