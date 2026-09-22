//! SQLite-boundary validation for relational/current-state preconditions.
//!
//! Application workflows validate command shape before dispatch. SQLite keeps
//! checks that depend on the current transaction and its optimistic versions.

use crate::platform::auth::application::repository::LocalLoginAttempt;
use crate::platform::auth::domain::{
    require_positive, AuthorizationStateError, PrincipalKind, PrincipalRecord, PrincipalState,
    MAX_PROTOCOL_INTEGER,
};
use crate::platform::auth::model::{
    LocalCredentialRecord, PostCommitActionRecord, UserProfileRecord,
};

pub(crate) fn local_login_attempt_result(
    current: &LocalCredentialRecord,
    attempt: &LocalLoginAttempt,
) -> Result<LocalCredentialRecord, AuthorizationStateError> {
    if current.principal_id != attempt.principal_id || current.version != attempt.expected_version {
        return Err(AuthorizationStateError::StorageConflict);
    }
    if attempt.succeeded && current.failed_attempts == 0 && current.locked_until.is_none() {
        return Ok(current.clone());
    }
    let mut next = current.clone();
    next.version = next_version(current.version)?;
    next.updated_at = attempt.attempted_at;
    if attempt.succeeded {
        next.failed_attempts = 0;
        next.locked_until = None;
    } else {
        next.failed_attempts = current.failed_attempts.checked_add(1).ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("failedAttempts overflow".to_owned())
        })?;
        if next.failed_attempts >= attempt.maximum_failures {
            let locked_until = u64::try_from(attempt.attempted_at)
                .ok()
                .and_then(|at| at.checked_add(attempt.lock_duration_ms))
                .filter(|until| *until <= MAX_PROTOCOL_INTEGER)
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord("lockedUntil overflow".to_owned())
                })?;
            next.locked_until = Some(locked_until as i64);
        }
    }
    Ok(next)
}

pub(crate) fn user_account_replacement(
    current_principal: &PrincipalRecord,
    current_profile: &UserProfileRecord,
    mut principal: PrincipalRecord,
    profile: UserProfileRecord,
    expected_version: u64,
) -> Result<(PrincipalRecord, UserProfileRecord), AuthorizationStateError> {
    require_positive("expectedVersion", expected_version)?;
    if current_principal.version != expected_version
        || current_profile.version != expected_version
        || current_principal.version != current_profile.version
        || current_principal.state == PrincipalState::Revoked
    {
        return Err(AuthorizationStateError::StorageConflict);
    }
    if current_principal.kind != PrincipalKind::User
        || principal.principal_id != current_principal.principal_id
        || current_profile.principal_id != current_principal.principal_id
        || principal.created_at != current_principal.created_at
        || profile.created_at != current_profile.created_at
        || principal.updated_at < current_principal.updated_at
        || profile.updated_at < current_profile.updated_at
    {
        return Err(AuthorizationStateError::StorageConflict);
    }
    principal.disabled_at = match principal.state {
        PrincipalState::Active => None,
        PrincipalState::Disabled if current_principal.state == PrincipalState::Disabled => {
            current_principal.disabled_at
        }
        PrincipalState::Disabled => Some(principal.updated_at),
        PrincipalState::Revoked => None,
    };
    principal.revoked_at =
        (principal.state == PrincipalState::Revoked).then_some(principal.updated_at);
    crate::platform::auth::model::validate_user_account_replacement(
        &principal,
        &profile,
        expected_version,
    )?;
    Ok((principal, profile))
}

pub(crate) fn post_commit_action_identity_equal(
    left: &PostCommitActionRecord,
    right: &PostCommitActionRecord,
) -> bool {
    left.action_id == right.action_id
        && left.predecessor_action_id == right.predecessor_action_id
        && left.kind == right.kind
        && left.payload == right.payload
        && left.created_at == right.created_at
}

pub(in crate::platform::auth) fn next_version(
    version: u64,
) -> Result<u64, AuthorizationStateError> {
    let next = version
        .checked_add(1)
        .ok_or_else(|| AuthorizationStateError::InvalidRecord("version overflow".to_owned()))?;
    require_positive("version", next)?;
    Ok(next)
}
