use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use super::super::AuthorizationStateError;

#[derive(Debug)]
pub(crate) struct HttpError {
    pub(super) status: StatusCode,
    pub(super) code: &'static str,
}

impl HttpError {
    pub(super) fn bad_request(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
        }
    }

    pub(super) fn unauthorized(code: &'static str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code,
        }
    }

    pub(super) fn forbidden(code: &'static str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code,
        }
    }

    pub(super) fn not_found(code: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
        }
    }

    pub(super) fn conflict(code: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
        }
    }

    pub(super) fn gone(code: &'static str) -> Self {
        Self {
            status: StatusCode::GONE,
            code,
        }
    }

    pub(super) fn bad_gateway(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code,
        }
    }

    pub(super) fn unavailable(code: &'static str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
        }
    }

    pub(super) fn internal(code: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
        }
    }
}

impl From<AuthorizationStateError> for HttpError {
    fn from(error: AuthorizationStateError) -> Self {
        let server_error = matches!(
            &error,
            AuthorizationStateError::RequiredDependencyUnavailable(_)
                | AuthorizationStateError::RequiredResourceUnavailable(_)
                | AuthorizationStateError::AuthorityStale
                | AuthorizationStateError::MaterializationStale
                | AuthorizationStateError::ContextLifetimeUnavailable
                | AuthorizationStateError::ContextSnapshotChanged
                | AuthorizationStateError::Storage(_)
        );
        if server_error {
            tracing::warn!(%error, "auth HTTP domain operation failed");
        } else {
            tracing::debug!(%error, "auth HTTP request denied");
        }
        match error {
            AuthorizationStateError::ApprovalRequired { .. } => Self::conflict("approval_required"),
            AuthorizationStateError::NotAuthorized => Self::forbidden("not_authorized"),
            AuthorizationStateError::WrongPrincipalKind => Self::forbidden("wrong_principal_kind"),
            AuthorizationStateError::InvalidRecord(message)
                if message == "new password must differ from current password" =>
            {
                Self::bad_request("password_unchanged")
            }
            AuthorizationStateError::InvalidRecord(_) => Self::bad_request("invalid_request"),
            AuthorizationStateError::SessionMissing => Self::not_found("session_not_found"),
            AuthorizationStateError::SessionExpired => Self::unauthorized("session_expired"),
            AuthorizationStateError::SessionRevoked => Self::unauthorized("session_revoked"),
            AuthorizationStateError::PrincipalMissing => Self::not_found("user_not_found"),
            AuthorizationStateError::NotFound => Self::not_found("not_found"),
            AuthorizationStateError::PrincipalInactive => Self::forbidden("user_inactive"),
            AuthorizationStateError::IdentityMissing => Self::not_found("identity_not_found"),
            AuthorizationStateError::ParticipantMissing => Self::not_found("participant_not_found"),
            AuthorizationStateError::ParticipantDigestMismatch => {
                Self::conflict("participant_changed")
            }
            AuthorizationStateError::NeedsDigestMismatch => Self::conflict("contract_changed"),
            AuthorizationStateError::AuthorityMissing => Self::not_found("authority_not_found"),
            AuthorizationStateError::AuthorityPending => Self::conflict("approval_required"),
            AuthorizationStateError::AuthorityRejected => Self::forbidden("authority_rejected"),
            AuthorizationStateError::AuthorityRevoked => Self::forbidden("authority_revoked"),
            AuthorizationStateError::AuthorityExpired => Self::forbidden("authority_expired"),
            AuthorizationStateError::DeploymentInactive => Self::forbidden("deployment_inactive"),
            AuthorizationStateError::InstanceInactive => Self::forbidden("instance_inactive"),
            AuthorizationStateError::DeviceInactive => Self::forbidden("device_inactive"),
            AuthorizationStateError::ActivationMissing => Self::forbidden("activation_required"),
            AuthorizationStateError::DelegationExpired => Self::forbidden("delegation_expired"),
            AuthorizationStateError::RequiredDependencyUnavailable(_) => {
                Self::unavailable("dependency_pending")
            }
            AuthorizationStateError::RequiredResourceUnavailable(_) => {
                Self::unavailable("resource_pending")
            }
            AuthorizationStateError::AuthorityStale
            | AuthorizationStateError::MaterializationStale
            | AuthorizationStateError::ContextLifetimeUnavailable
            | AuthorizationStateError::ContextSnapshotChanged => {
                Self::unavailable("authorization_pending")
            }
            AuthorizationStateError::PortalPolicyChanged => Self::conflict("portal_policy_changed"),
            AuthorizationStateError::StorageConflict => Self::conflict("storage_conflict"),
            AuthorizationStateError::RevisionConflict { .. } => Self::conflict("revision_conflict"),
            AuthorizationStateError::CurrentIssuerConflict => Self::conflict("issuer_current"),
            AuthorizationStateError::IssuerMissing => Self::not_found("issuer_key_not_found"),
            AuthorizationStateError::Storage(_) => Self::internal("internal_error"),
        }
    }
}

pub(super) fn map_issuance_error(error: AuthorizationStateError) -> HttpError {
    let server_error = matches!(
        &error,
        AuthorizationStateError::AuthorityStale
            | AuthorizationStateError::MaterializationStale
            | AuthorizationStateError::ContextSnapshotChanged
            | AuthorizationStateError::RequiredDependencyUnavailable(_)
            | AuthorizationStateError::RequiredResourceUnavailable(_)
            | AuthorizationStateError::ContextLifetimeUnavailable
            | AuthorizationStateError::Storage(_)
    );
    if server_error {
        tracing::warn!(%error, "auth issuance denied");
    } else {
        tracing::debug!(%error, "auth issuance rejected");
    }
    match error {
        AuthorizationStateError::AuthorityStale
        | AuthorizationStateError::MaterializationStale
        | AuthorizationStateError::ContextSnapshotChanged => {
            HttpError::unavailable("authorization_pending")
        }
        AuthorizationStateError::SessionMissing => HttpError::unauthorized("session_not_found"),
        AuthorizationStateError::SessionExpired => HttpError::unauthorized("session_expired"),
        AuthorizationStateError::SessionRevoked => HttpError::unauthorized("session_revoked"),
        AuthorizationStateError::PrincipalMissing => HttpError::unauthorized("user_not_found"),
        AuthorizationStateError::PrincipalInactive => HttpError::unauthorized("user_inactive"),
        AuthorizationStateError::AuthorityMissing => HttpError::unauthorized("authority_not_found"),
        AuthorizationStateError::AuthorityPending => HttpError::unauthorized("approval_required"),
        AuthorizationStateError::AuthorityRejected => HttpError::unauthorized("authority_rejected"),
        AuthorizationStateError::AuthorityRevoked => HttpError::unauthorized("authority_revoked"),
        AuthorizationStateError::AuthorityExpired => HttpError::unauthorized("authority_expired"),
        AuthorizationStateError::DeploymentInactive => {
            HttpError::unauthorized("deployment_inactive")
        }
        AuthorizationStateError::InstanceInactive => HttpError::unauthorized("instance_inactive"),
        AuthorizationStateError::DeviceInactive => HttpError::unauthorized("device_inactive"),
        AuthorizationStateError::ActivationMissing => {
            HttpError::unauthorized("activation_required")
        }
        AuthorizationStateError::DelegationExpired => HttpError::unauthorized("delegation_expired"),
        error => error.into(),
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(CONTENT_TYPE, "application/json")],
            Json(json!({ "error": { "code": self.code } })),
        )
            .into_response()
    }
}
