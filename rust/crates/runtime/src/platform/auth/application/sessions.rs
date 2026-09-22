use ulid::Ulid;

use super::super::*;
use trellis_protocol::ParticipantKind;

/// Verified interactive authentication input for a user installation login.
#[derive(Clone, Debug)]
pub struct CreateSessionInput {
    /// Stable authenticated user principal ID.
    pub principal_id: String,
    /// Stable installed participant ID.
    pub participant_id: String,
    /// App or agent participant class.
    pub participant_kind: ParticipantKind,
    /// Canonical Ed25519 installation public key.
    pub session_public_key: String,
    /// Interactive authentication time in Unix milliseconds.
    pub created_at: i64,
    /// Durable proof claim, committed with the selected login ID.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

impl<R: SessionRepository + Clone> AuthService<R> {
    /// Create or reuse a user login without issuing transport authority.
    pub(crate) async fn create_session(
        &self,
        input: CreateSessionInput,
    ) -> Result<IdempotentOutcome<SessionRecord>, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("createdAt", input.created_at)?;
        let expires_at = u64::try_from(input.created_at)
            .ok()
            .and_then(|created| created.checked_add(self.config.session_ttl_ms))
            .filter(|expires| *expires <= super::super::MAX_PROTOCOL_INTEGER)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("session expiry overflow".to_owned())
            })? as i64;
        let session = SessionRecord::from_new(NewSession {
            session_id: Ulid::new().to_string(),
            principal_id: input.principal_id,
            participant_id: input.participant_id,
            participant_kind: input.participant_kind,
            session_public_key: input.session_public_key,
            created_at: input.created_at,
            expires_at: Some(expires_at),
        })?;
        let actions = input.actions;
        super::validation::validate_idempotency_and_actions(&input.idempotency, &actions)?;
        self.repository
            .create_session(SessionCreation {
                session,
                idempotency: input.idempotency,
                actions,
            })
            .await
    }

    /// Revoke a user login and durably enqueue its event and kick intents.
    pub(crate) async fn revoke_session(
        &self,
        session_id: String,
        expected_version: u64,
        revoked_at: i64,
        mut idempotency: IdempotencyResultRecord,
        actions: Vec<PostCommitActionRecord>,
    ) -> Result<IdempotentOutcome<SessionRecord>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&idempotency, &actions)?;
        super::super::domain::require_protocol_timestamp("revokedAt", revoked_at)?;
        super::validation::validate_session_revocation_actions(&actions)?;
        idempotency.result = serde_json::json!({ "sessionId": session_id, "state": "revoked" });
        self.repository
            .revoke_session(SessionRevocation {
                session_id,
                expected_version,
                revoked_at,
                idempotency,
                actions,
            })
            .await
    }
}
