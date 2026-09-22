use crate::platform::auth::domain::require_positive;
use crate::platform::auth::domain::{
    require_digest, require_nonempty, require_protocol_timestamp, validate_ed25519_public_key,
};
use crate::platform::auth::{
    AuthorizationStateError, PrincipalKind, PrincipalRecord, PrincipalState,
    ProvisionedIdentityRecord, ProvisionedIdentityState, UserProfileRecord,
};

pub(crate) fn validate_user_account_replacement(
    principal: &PrincipalRecord,
    profile: &UserProfileRecord,
    expected_version: u64,
) -> Result<(), AuthorizationStateError> {
    require_positive("expectedVersion", expected_version)?;
    let replacement_version = expected_version
        .checked_add(1)
        .ok_or_else(|| AuthorizationStateError::InvalidRecord("version overflow".to_owned()))?;
    if principal.kind != PrincipalKind::User
        || profile.principal_id != principal.principal_id
        || principal.version != replacement_version
        || profile.version != principal.version
        || principal.updated_at != profile.updated_at
        || !matches!(
            (principal.state, principal.disabled_at, principal.revoked_at),
            (PrincipalState::Active, None, None)
                | (PrincipalState::Disabled, Some(_), None)
                | (PrincipalState::Revoked, None, Some(_))
        )
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "user account replacement identity, version, state, or timestamps are invalid"
                .to_owned(),
        ));
    }
    require_protocol_timestamp("principal.updatedAt", principal.updated_at)?;
    require_protocol_timestamp("profile.updatedAt", profile.updated_at)?;
    if let Some(display_name) = &profile.display_name {
        require_nonempty("displayName", display_name)?;
    }
    Ok(())
}

pub(crate) fn validate_provisioned_identity(
    identity: &ProvisionedIdentityRecord,
) -> Result<(), AuthorizationStateError> {
    require_digest("identityKeyId", &identity.identity_key_id)?;
    let derived_key_id =
        validate_ed25519_public_key("identityPublicKey", &identity.identity_public_key)?;
    require_nonempty("principalId", &identity.principal_id)?;
    require_nonempty("deploymentId", &identity.deployment_id)?;
    require_nonempty("instanceId", &identity.instance_id)?;
    require_protocol_timestamp("createdAt", identity.created_at)?;
    if identity.identity_key_id != derived_key_id {
        return Err(AuthorizationStateError::InvalidRecord(
            "provisioned identity key ID does not match its public key".to_owned(),
        ));
    }
    if let Some(revoked_at) = identity.revoked_at {
        require_protocol_timestamp("revokedAt", revoked_at)?;
    }
    match (identity.state, identity.revoked_at.is_some()) {
        (ProvisionedIdentityState::Active, false) | (ProvisionedIdentityState::Revoked, true) => {
            Ok(())
        }
        _ => Err(AuthorizationStateError::InvalidRecord(
            "provisioned identity lifecycle is invalid".to_owned(),
        )),
    }
}
