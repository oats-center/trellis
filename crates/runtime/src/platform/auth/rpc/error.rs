use serde_json::{json, Value};
use ulid::Ulid;

use super::super::AuthorizationStateError;

/// Which generated API owns the failing request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApiDomain {
    Auth,
    Core,
}

const AUTH_ERROR: &str = "trellis.auth@v1::AuthError";
const AUTH_VALIDATION_ERROR: &str = "trellis.auth@v1::ValidationError";
const AUTH_UNEXPECTED_ERROR: &str = "trellis.auth@v1::UnexpectedError";
const CORE_VALIDATION_ERROR: &str = "trellis.core@v1::ValidationError";
const CORE_UNEXPECTED_ERROR: &str = "trellis.core@v1::UnexpectedError";

pub(super) fn public_rpc_error(domain: ApiDomain, error: &AuthorizationStateError) -> Value {
    match domain {
        ApiDomain::Core => {
            let (error_type, code, message) = match error {
                AuthorizationStateError::RevisionConflict { .. } => (
                    CORE_VALIDATION_ERROR,
                    "revision_conflict",
                    "The current revision differs from expectedRevision.",
                ),
                AuthorizationStateError::NotFound => (
                    CORE_VALIDATION_ERROR,
                    "not_found",
                    "The requested resource was not found.",
                ),
                AuthorizationStateError::InvalidRecord(_)
                | AuthorizationStateError::StorageConflict => (
                    CORE_VALIDATION_ERROR,
                    "invalid_request",
                    "The resource request is invalid.",
                ),
                error if error.is_expected_denial() => (
                    CORE_VALIDATION_ERROR,
                    "not_authorized",
                    "The request is not authorized.",
                ),
                _ => (
                    CORE_UNEXPECTED_ERROR,
                    "internal_error",
                    "The request could not be completed.",
                ),
            };
            json!({
                "id": format!("err_{}", Ulid::new()),
                "type": error_type,
                "message": message,
                "context": { "code": code },
            })
        }
        ApiDomain::Auth => {
            let (error_type, code, message) = match error {
                AuthorizationStateError::ApprovalRequired { .. } => (
                    AUTH_ERROR,
                    "approval_required",
                    "Approval of the current deployment consent request is required.",
                ),
                AuthorizationStateError::WrongPrincipalKind => (
                    AUTH_ERROR,
                    "wrong_principal_kind",
                    "This operation requires a user login.",
                ),
                AuthorizationStateError::CurrentIssuerConflict => (
                    AUTH_ERROR,
                    "issuer_current",
                    "Select a replacement signing issuer before revoking this key.",
                ),
                AuthorizationStateError::RevisionConflict { .. } => (
                    AUTH_ERROR,
                    "revision_conflict",
                    "The current revision differs from expectedRevision.",
                ),
                AuthorizationStateError::InvalidRecord(_) => (
                    AUTH_VALIDATION_ERROR,
                    "invalid_request",
                    "The request is invalid.",
                ),
                AuthorizationStateError::PortalPolicyChanged
                | AuthorizationStateError::StorageConflict => (
                    AUTH_ERROR,
                    "conflict",
                    "The request conflicts with current authentication state.",
                ),
                AuthorizationStateError::IdentityMissing => (
                    AUTH_ERROR,
                    "identity_not_found",
                    "The requested identity was not found.",
                ),
                AuthorizationStateError::PrincipalMissing
                | AuthorizationStateError::ParticipantMissing
                | AuthorizationStateError::NotFound
                | AuthorizationStateError::IssuerMissing
                | AuthorizationStateError::SessionMissing
                | AuthorizationStateError::AuthorityMissing => (
                    AUTH_ERROR,
                    "not_found",
                    "The requested authentication record was not found.",
                ),
                error if error.is_expected_denial() => (
                    AUTH_ERROR,
                    "not_authorized",
                    "The request is not authorized.",
                ),
                _ => (
                    AUTH_UNEXPECTED_ERROR,
                    "internal_error",
                    "The request could not be completed.",
                ),
            };
            let mut response = json!({
                "id": format!("err_{}", Ulid::new()),
                "type": error_type,
                "message": message,
                "code": code,
                "field": null,
                "retryable": false,
            });
            if let AuthorizationStateError::ApprovalRequired { consent_request } = error {
                response["consentRequest"] = consent_request.clone();
            }
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_and_principal_kind_denials_are_not_malformed_requests() {
        for (error, expected_type, reason) in [
            (
                AuthorizationStateError::NotAuthorized,
                AUTH_ERROR,
                "not_authorized",
            ),
            (
                AuthorizationStateError::WrongPrincipalKind,
                AUTH_ERROR,
                "wrong_principal_kind",
            ),
            (
                AuthorizationStateError::InvalidRecord("malformed".into()),
                AUTH_VALIDATION_ERROR,
                "invalid_request",
            ),
        ] {
            let response = public_rpc_error(ApiDomain::Auth, &error);
            assert_eq!(response["type"], expected_type);
            assert_eq!(response["code"], reason);
            assert_eq!(response["field"], serde_json::Value::Null);
            assert_eq!(response["retryable"], false);
        }
    }

    #[test]
    fn auth_error_families_decode_with_their_required_code_and_type() {
        // Every family R10 names, at the exact production serializer.
        for (error, expected_code, expected_type) in [
            (
                AuthorizationStateError::InvalidRecord("malformed".to_owned()),
                "invalid_request",
                AUTH_VALIDATION_ERROR,
            ),
            (AuthorizationStateError::NotFound, "not_found", AUTH_ERROR),
            (
                AuthorizationStateError::NotAuthorized,
                "not_authorized",
                AUTH_ERROR,
            ),
            (
                AuthorizationStateError::RevisionConflict {
                    expected: 1,
                    current: 2,
                },
                "revision_conflict",
                AUTH_ERROR,
            ),
            (
                AuthorizationStateError::StorageConflict,
                "conflict",
                AUTH_ERROR,
            ),
            (
                AuthorizationStateError::Storage("boom".to_owned()),
                "internal_error",
                AUTH_UNEXPECTED_ERROR,
            ),
        ] {
            let response = public_rpc_error(ApiDomain::Auth, &error);
            assert_eq!(response["code"], expected_code);
            assert_eq!(response["type"], expected_type);
            let id = response["id"].as_str().expect("error id");
            assert!(id.starts_with("err_"), "id {id}");
            let message = response["message"].as_str().expect("message");
            assert!(!message.is_empty());
            assert!(response.get("field").is_some());
        }

        // Approval-required keeps the Auth family and its consent request.
        let approval = public_rpc_error(
            ApiDomain::Auth,
            &AuthorizationStateError::ApprovalRequired {
                consent_request: serde_json::json!({ "participantId": "acme@v1" }),
            },
        );
        assert_eq!(approval["code"], "approval_required");
        assert_eq!(approval["type"], AUTH_ERROR);
        assert_eq!(approval["consentRequest"]["participantId"], "acme@v1");
    }

    #[test]
    fn auth_errors_carry_the_complete_generated_detail_shape() {
        for error in [
            AuthorizationStateError::StorageConflict,
            AuthorizationStateError::NotFound,
            AuthorizationStateError::InvalidRecord("malformed".into()),
            AuthorizationStateError::Storage("postgres://admin:secret@internal".to_owned()),
        ] {
            let response = public_rpc_error(ApiDomain::Auth, &error);
            assert!(response["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("err_")));
            assert!(response["type"]
                .as_str()
                .is_some_and(|value| value.starts_with("trellis.auth@v1::")));
            assert!(response["message"]
                .as_str()
                .is_some_and(|value| !value.is_empty()));
            assert!(response["code"]
                .as_str()
                .is_some_and(|value| !value.is_empty()));
            assert!(response.get("field").is_some());
            assert_eq!(response["retryable"], false);
            assert!(!serde_json::to_string(&response).unwrap().contains("secret"));
        }
    }

    #[test]
    fn core_errors_use_core_generated_schemas() {
        let validation = public_rpc_error(ApiDomain::Core, &AuthorizationStateError::NotFound);
        assert_eq!(validation["type"], CORE_VALIDATION_ERROR);
        assert_eq!(validation["context"]["code"], "not_found");
        let unexpected = public_rpc_error(
            ApiDomain::Core,
            &AuthorizationStateError::Storage("x".into()),
        );
        assert_eq!(unexpected["type"], CORE_UNEXPECTED_ERROR);
        assert_eq!(unexpected["context"]["code"], "internal_error");
    }

    #[test]
    fn deployment_approval_error_preserves_the_server_consent_request() {
        let consent_request = json!({
            "participantId": "acme.service@v1",
            "installedRevision": "2",
            "expectedGrantRevision": "3",
            "decisionDigest": "digest"
        });
        let response = public_rpc_error(
            ApiDomain::Auth,
            &AuthorizationStateError::ApprovalRequired {
                consent_request: consent_request.clone(),
            },
        );
        assert_eq!(response["type"], AUTH_ERROR);
        assert_eq!(response["code"], "approval_required");
        assert_eq!(response["consentRequest"], consent_request);
    }
}
