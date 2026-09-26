use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use super::super::application::repository::{
    ActivationReviewClaim, ActivationReviewCreation, ActivationReviewDecision, DeviceProvisioning,
    DeviceProvisioningSecretConsumption, IdempotentOutcome, ProvisionedInstanceMutation,
    ProvisioningRepository, ServiceIdentityProvisioning,
};
use super::super::context::{
    revoke_sql_contexts, AuthorizationContextRevocationReason, AuthorizationContextSelector,
};
use super::super::{
    activation_review_event, activation_review_event_action_id, AuthorizationStateError,
    DeviceActivationReviewRecord, DeviceActivationReviewState, DeviceDelegationState,
    DeviceProvisioningSecretRecord, DeviceRecord, DeviceState, IdempotencyResultRecord,
    PrincipalKind, PrincipalRecord, PrincipalState, ProvisionedIdentityKind,
    ProvisionedIdentityRecord, ProvisionedIdentityState, ProvisioningSecretState,
    RuntimeInstanceRecord, RuntimeInstanceState,
};
use super::common::{
    decode_enum, decode_json, encode_enum, encode_json, from_sql_version, map_write_error,
    sql_error, to_sql_version,
};
use super::evidence::{
    load_deployment, load_device, load_device_delegation, load_runtime_instance,
    validate_sql_device_relationships,
};
use super::outbox::{insert_sql_idempotency_and_actions, sqlite_idempotency_replay};
use super::principals::load_principal;
use super::validation::next_version;
use super::SqliteAuthorizationStore;

impl SqliteAuthorizationStore {
    pub(crate) async fn install_runtime_identity(
        &self,
        instance: RuntimeInstanceRecord,
        identity: ProvisionedIdentityRecord,
    ) -> Result<(), AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            load_principal(&transaction, &instance.principal_id)?
                .ok_or(AuthorizationStateError::PrincipalMissing)?;
            let expected_kind = match identity.kind {
                ProvisionedIdentityKind::Service => trellis_protocol::ParticipantKind::Service,
                ProvisionedIdentityKind::Device => trellis_protocol::ParticipantKind::Device,
            };
            if load_deployment(&transaction, &instance.deployment_id)?
                .is_none_or(|deployment| deployment.participant_kind != expected_kind)
            {
                return Err(AuthorizationStateError::InvalidRecord(
                    "provisioned instance deployment kind does not match".to_owned(),
                ));
            }
            if load_runtime_instance(&transaction, &instance.instance_id)?.is_some() {
                return Err(AuthorizationStateError::StorageConflict);
            }
            insert_sql_runtime_instance(&transaction, &instance)?;
            validate_sql_identity_relationships(&transaction, &identity)?;
            insert_sql_provisioned_identity(&transaction, &identity)?;
            transaction.commit().map_err(sql_error)
        })
        .await
    }
}
use crate::platform::auth::model::validate_provisioned_identity;

#[async_trait]
impl ProvisioningRepository for SqliteAuthorizationStore {
    async fn get_device_provisioning_secret_by_hash(
        &self,
        secret_hash: &str,
    ) -> Result<Option<DeviceProvisioningSecretRecord>, AuthorizationStateError> {
        let secret_hash = secret_hash.to_owned();
        self.run(move |connection| load_provisioning_secret_by_hash(connection, &secret_hash))
            .await
    }
    async fn list_provisioned_identities(
        &self,
    ) -> Result<Vec<ProvisionedIdentityRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT identity_key_id FROM auth_provisioned_identities ORDER BY identity_key_id",
                )
                .map_err(sql_error)?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            ids.into_iter()
                .map(|identity_key_id| {
                    load_provisioned_identity(connection, &identity_key_id)?
                        .ok_or(AuthorizationStateError::StorageConflict)
                })
                .collect()
        })
        .await
    }

    async fn get_provisioned_identity(
        &self,
        identity_key_id: &str,
    ) -> Result<Option<ProvisionedIdentityRecord>, AuthorizationStateError> {
        let identity_key_id = identity_key_id.to_owned();
        self.run_read(move |connection| load_provisioned_identity(connection, &identity_key_id))
            .await
    }

    async fn consume_device_provisioning_secret(
        &self,
        command: DeviceProvisioningSecretConsumption,
    ) -> Result<IdempotentOutcome<DeviceProvisioningSecretRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_provisioning_secret_by_hash(&transaction, &command.secret_hash)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.version != command.expected_version
                || current.state != ProvisioningSecretState::Pending
                || command.consumed_at < current.created_at
                || command.consumed_at >= current.expires_at
                || command.identity.instance_id != current.instance_id
                || command.identity.kind != ProvisionedIdentityKind::Device
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            validate_sql_identity_relationships(&transaction, &command.identity)?;
            insert_sql_provisioned_identity(&transaction, &command.identity)?;
            let next = next_version(command.expected_version)?;
            let changed = transaction
                .execute(
                    "UPDATE auth_device_provisioning_secrets SET state = 'consumed', consumed_at = ?1, version = ?2
                 WHERE secret_hash = ?3 AND version = ?4 AND state = 'pending' AND expires_at > ?1",
                    params![
                        command.consumed_at,
                        to_sql_version(next)?,
                        command.secret_hash,
                        to_sql_version(command.expected_version)?
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let result = load_provisioning_secret_by_hash(&transaction, &command.secret_hash)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(result))
        })
        .await
    }

    async fn create_activation_review(
        &self,
        command: ActivationReviewCreation,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let device = load_device(
                &transaction,
                &command.review.principal_id,
                &command.review.deployment_id,
            )?;
            let instance = load_runtime_instance(&transaction, &command.review.instance_id)?;
            if device.is_none_or(|value| value.state == DeviceState::Revoked)
                || instance.is_none_or(|value| {
                    value.principal_id != command.review.principal_id
                        || value.deployment_id != command.review.deployment_id
                })
            {
                return Err(AuthorizationStateError::InvalidRecord(
                    "activation review relationships do not match exactly".to_owned(),
                ));
            }
            transaction
                .execute(
                    "INSERT INTO auth_device_activation_reviews (review_id, principal_id, deployment_id, instance_id, request_digest, payload_json, state, requested_at, expires_at, activated_by_user_principal_id, decided_at, decided_by, reason, version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                    params![
                        command.review.review_id,
                        command.review.principal_id,
                        command.review.deployment_id,
                        command.review.instance_id,
                        command.review.request_digest,
                        encode_json(&command.review.payload)?,
                        encode_enum(command.review.state)?,
                        command.review.requested_at,
                        command.review.expires_at,
                        command.review.activated_by_user_principal_id,
                        command.review.decided_at,
                        command.review.decided_by,
                        command.review.reason,
                        to_sql_version(command.review.version)?
                    ],
                )
                .map_err(map_write_error)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.review))
        })
        .await
    }

    async fn get_activation_review(
        &self,
        review_id: &str,
    ) -> Result<Option<DeviceActivationReviewRecord>, AuthorizationStateError> {
        let review_id = review_id.to_owned();
        self.run_read(move |connection| load_activation_review(connection, &review_id))
            .await
    }

    async fn expire_due_activation_reviews(
        &self,
        now: i64,
    ) -> Result<Vec<DeviceActivationReviewRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let ids = {
                let mut statement = transaction
                    .prepare(
                        "SELECT review.review_id, review.state, review.activated_by_user_principal_id
                         FROM auth_device_activation_reviews AS review
                         WHERE review.state IN ('pending', 'approved')
                           AND review.expires_at <= ?1
                           AND EXISTS (
                               SELECT 1 FROM auth_devices AS device
                               WHERE device.principal_id = review.principal_id
                                 AND device.deployment_id = review.deployment_id
                                 AND device.state = 'pending'
                           )
                         ORDER BY review.review_id",
                    )
                    .map_err(sql_error)?;
                let ids = statement
                    .query_map([now], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            decode_enum::<DeviceActivationReviewState>(row.get::<_, String>(1)?)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    })
                    .map_err(sql_error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(sql_error)?;
                ids
            };
            transaction
                .execute(
                    "UPDATE auth_device_activation_reviews AS review
                     SET state = 'expired', version = version + 1
                     WHERE review.state IN ('pending', 'approved')
                       AND review.expires_at <= ?1
                       AND EXISTS (
                           SELECT 1 FROM auth_devices AS device
                           WHERE device.principal_id = review.principal_id
                             AND device.deployment_id = review.deployment_id
                             AND device.state = 'pending'
                       )",
                    [now],
                )
                .map_err(map_write_error)?;
            let reviews = ids
                .into_iter()
                .map(|(review_id, previous_state, claimant)| {
                    let review = load_activation_review(&transaction, &review_id)?
                        .ok_or(AuthorizationStateError::StorageConflict)?;
                    let mut action = activation_review_event::<
                        trellis_runtime_apis::apis::trellis_auth_v1::events::DeviceUserAuthoritiesResolved,
                    >(
                        &review,
                        "resolved",
                        "Auth.DeviceUserAuthorities.Resolved",
                        now,
                        serde_json::json!({ "state": "expired" }),
                    )?;
                    action.predecessor_action_id = Some(activation_review_event_action_id(
                        &review_id,
                        if previous_state == DeviceActivationReviewState::Approved {
                            "approved"
                        } else if claimant.is_some() {
                            "requested"
                        } else {
                            "review-requested"
                        },
                    )?);
                    insert_sql_idempotency_and_actions(
                        &transaction,
                        &IdempotencyResultRecord {
                            scope_key: trellis_protocol::digest_json(&serde_json::json!({
                                "purpose": "device.activation.expire",
                                "reviewId": review_id,
                            }))
                            .map_err(|error| {
                                AuthorizationStateError::InvalidRecord(error.to_string())
                            })?,
                            purpose: "device.activation.expire".to_owned(),
                            signer_id: "trellis.auth".to_owned(),
                            request_id: review_id,
                            request_digest: review.request_digest.clone(),
                            result: serde_json::Value::Null,
                            created_at: now,
                            expires_at: now.checked_add(86_400_000).ok_or_else(|| {
                                AuthorizationStateError::InvalidRecord(
                                    "activation expiry idempotency overflow".to_owned(),
                                )
                            })?,
                        },
                        &[action],
                    )?;
                    Ok(review)
                })
                .collect::<Result<Vec<_>, _>>()?;
            transaction.commit().map_err(sql_error)?;
            Ok(reviews)
        })
        .await
    }

    async fn claim_activation_review(
        &self,
        command: ActivationReviewClaim,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_activation_review(&transaction, &command.review_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.version != command.expected_version
                || !matches!(
                    current.state,
                    DeviceActivationReviewState::Pending
                        | DeviceActivationReviewState::Approved
                )
                || current.activated_by_user_principal_id.as_deref().is_some_and(|principal| {
                    principal != command.activated_by_user_principal_id
                })
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            if let Some(delegation) = &command.delegation {
                if current.state != DeviceActivationReviewState::Approved
                    || delegation.principal_id != current.principal_id
                    || delegation.deployment_id != current.deployment_id
                    || delegation.state != DeviceDelegationState::Active
                {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "activation claim delegation does not match approved review".to_owned(),
                    ));
                }
                if let Some(session) = &command.companion_session {
                    super::sessions::insert_sql_session(&transaction, session)?;
                }
                transaction
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
                            delegation.expires_at
                        ],
                    )
                    .map_err(map_write_error)?;
                let changed = transaction
                    .execute(
                        "UPDATE auth_devices SET state = ?1, updated_at = ?2, version = version + 1
                         WHERE principal_id = ?3 AND deployment_id = ?4",
                        params![
                            encode_enum(DeviceState::Active)?,
                            command.now,
                            current.principal_id,
                            current.deployment_id,
                        ],
                    )
                    .map_err(map_write_error)?;
                if changed != 1 {
                    return Err(AuthorizationStateError::StorageConflict);
                }
            }
            let next = next_version(command.expected_version)?;
            let changed = transaction
                .execute(
                    "UPDATE auth_device_activation_reviews SET activated_by_user_principal_id = ?1, version = ?2 WHERE review_id = ?3 AND version = ?4 AND expires_at > ?5 AND (activated_by_user_principal_id IS NULL OR activated_by_user_principal_id = ?1)",
                    params![
                        command.activated_by_user_principal_id,
                        to_sql_version(next)?,
                        command.review_id,
                        to_sql_version(command.expected_version)?,
                        command.now,
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let result = load_activation_review(&transaction, &command.review_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(result))
        })
        .await
    }

    async fn list_activation_reviews(
        &self,
    ) -> Result<Vec<DeviceActivationReviewRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare("SELECT review_id FROM auth_device_activation_reviews ORDER BY review_id")
                .map_err(sql_error)?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            ids.into_iter()
                .map(|review_id| {
                    load_activation_review(connection, &review_id)?
                        .ok_or(AuthorizationStateError::StorageConflict)
                })
                .collect()
        })
        .await
    }

    async fn decide_activation_review(
        &self,
        command: ActivationReviewDecision,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_activation_review(&transaction, &command.review_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.version != command.expected_version
                || current.state != DeviceActivationReviewState::Pending
                || command.decided_at < current.requested_at
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            if command.activate_device {
                let changed = transaction
                    .execute(
                        "UPDATE auth_devices SET state = ?1, updated_at = ?2, version = version + 1
                     WHERE principal_id = ?3 AND deployment_id = ?4",
                        params![
                            encode_enum(DeviceState::Active)?,
                            command.decided_at,
                            current.principal_id,
                            current.deployment_id
                        ],
                    )
                    .map_err(map_write_error)?;
                if changed != 1 {
                    return Err(AuthorizationStateError::StorageConflict);
                }
            }
            if let Some(delegation) = &command.delegation {
                if delegation.principal_id != current.principal_id
                    || delegation.deployment_id != current.deployment_id
                {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "activation decision delegation does not match review".to_owned(),
                    ));
                }
                if let Some(session) = &command.companion_session {
                    super::sessions::insert_sql_session(&transaction, session)?;
                }
                transaction
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
                            delegation.expires_at
                        ],
                    )
                    .map_err(map_write_error)?;
            }
            let next = next_version(command.expected_version)?;
            let changed = transaction
                .execute(
                    "UPDATE auth_device_activation_reviews SET state = ?1, decided_at = ?2, decided_by = ?3, reason = ?4, version = ?5
                   WHERE review_id = ?6 AND version = ?7 AND state = 'pending' AND expires_at > ?2",
                    params![
                        encode_enum(command.state)?,
                        command.decided_at,
                        command.decided_by,
                        command.reason,
                        to_sql_version(next)?,
                        command.review_id,
                        to_sql_version(command.expected_version)?
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let result = load_activation_review(&transaction, &command.review_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(result))
        })
        .await
    }

    async fn provision_service_identity(
        &self,
        mut command: ServiceIdentityProvisioning,
    ) -> Result<IdempotentOutcome<ProvisionedIdentityRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            // The identity key is the stable identity. The existing-versus-new
            // decision is made here, inside the transaction, so a repeated
            // provision reuses the same principal and instance instead of
            // racing a separate lookup against a fresh insert.
            let identity = match load_provisioned_identity(&transaction, &command.identity_key_id)?
            {
                Some(existing) => {
                    if existing.kind != ProvisionedIdentityKind::Service
                        || existing.identity_public_key != command.identity_public_key
                        || existing.deployment_id != command.deployment_id
                    {
                        return Err(AuthorizationStateError::InvalidRecord(
                            "service identity does not match provisioning".to_owned(),
                        ));
                    }
                    if command
                        .requested_instance_id
                        .as_ref()
                        .is_some_and(|requested| requested != &existing.instance_id)
                    {
                        return Err(AuthorizationStateError::InvalidRecord(
                            "requested instance does not match the existing identity".to_owned(),
                        ));
                    }
                    let principal = load_principal(&transaction, &existing.principal_id)?
                        .ok_or(AuthorizationStateError::StorageConflict)?;
                    let instance = load_runtime_instance(&transaction, &existing.instance_id)?
                        .ok_or(AuthorizationStateError::StorageConflict)?;
                    if existing.state == ProvisionedIdentityState::Revoked
                        || instance.state == RuntimeInstanceState::Revoked
                        || principal.state == PrincipalState::Revoked
                    {
                        return Err(AuthorizationStateError::InvalidRecord(
                            "revoked service identity cannot be reprovisioned".to_owned(),
                        ));
                    }
                    let mut authorization_changed = false;
                    if existing.state != ProvisionedIdentityState::Active {
                        transaction
                            .execute(
                                "UPDATE auth_provisioned_identities SET state = ?1, revoked_at = NULL
                                 WHERE identity_key_id = ?2",
                                params![
                                    encode_enum(ProvisionedIdentityState::Active)?,
                                    existing.identity_key_id
                                ],
                            )
                            .map_err(map_write_error)?;
                        authorization_changed = true;
                    }
                    if instance.state != RuntimeInstanceState::Active {
                        transaction
                            .execute(
                                "UPDATE auth_instances SET state = ?1, updated_at = ?2, version = ?3
                                 WHERE instance_id = ?4 AND version = ?5",
                                params![
                                    encode_enum(RuntimeInstanceState::Active)?,
                                    command.created_at,
                                    to_sql_version(next_version(instance.version)?)?,
                                    instance.instance_id,
                                    to_sql_version(instance.version)?
                                ],
                            )
                            .map_err(map_write_error)?;
                        authorization_changed = true;
                    }
                    if principal.state != PrincipalState::Active {
                        transaction
                            .execute(
                                "UPDATE auth_principals SET state = ?1, updated_at = ?2, version = ?3,
                                        disabled_at = NULL, revoked_at = NULL
                                 WHERE principal_id = ?4 AND version = ?5",
                                params![
                                    encode_enum(PrincipalState::Active)?,
                                    command.created_at,
                                    to_sql_version(next_version(principal.version)?)?,
                                    principal.principal_id,
                                    to_sql_version(principal.version)?
                                ],
                            )
                            .map_err(map_write_error)?;
                        authorization_changed = true;
                    }
                    if authorization_changed {
                        revoke_sql_contexts(
                            &transaction,
                            &AuthorizationContextSelector::Instance(instance.instance_id.clone()),
                            AuthorizationContextRevocationReason::InstanceChanged,
                            command.created_at.div_euclid(1_000),
                        )?;
                    }
                    ProvisionedIdentityRecord {
                        state: ProvisionedIdentityState::Active,
                        revoked_at: None,
                        ..existing
                    }
                }
                None => {
                    let principal_id = command.proposed_principal_id;
                    let instance_id = command
                        .requested_instance_id
                        .unwrap_or(command.proposed_instance_id);
                    let principal = PrincipalRecord {
                        principal_id: principal_id.clone(),
                        kind: PrincipalKind::Service,
                        state: PrincipalState::Active,
                        created_at: command.created_at,
                        updated_at: command.created_at,
                        version: 1,
                        disabled_at: None,
                        revoked_at: None,
                    };
                    let instance = RuntimeInstanceRecord {
                        instance_id: instance_id.clone(),
                        deployment_id: command.deployment_id.clone(),
                        principal_id: principal_id.clone(),
                        state: RuntimeInstanceState::Active,
                        created_at: command.created_at,
                        updated_at: command.created_at,
                        version: 1,
                    };
                    let identity = ProvisionedIdentityRecord {
                        identity_key_id: command.identity_key_id.clone(),
                        identity_public_key: command.identity_public_key.clone(),
                        principal_id,
                        deployment_id: command.deployment_id.clone(),
                        instance_id,
                        kind: ProvisionedIdentityKind::Service,
                        state: ProvisionedIdentityState::Active,
                        created_at: command.created_at,
                        revoked_at: None,
                    };
                    validate_provisioned_identity(&identity)?;
                    validate_sql_new_runtime_relationships(
                        &transaction,
                        &principal,
                        &instance,
                        ProvisionedIdentityKind::Service,
                    )?;
                    insert_sql_principal(&transaction, &principal)?;
                    insert_sql_runtime_instance(&transaction, &instance)?;
                    insert_sql_provisioned_identity(&transaction, &identity)?;
                    identity
                }
            };
            command.idempotency.result = serde_json::json!({
                "principalId": identity.principal_id,
                "instanceId": identity.instance_id,
                "identityKeyId": identity.identity_key_id,
            });
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(identity))
        })
        .await
    }

    async fn provision_device(
        &self,
        command: DeviceProvisioning,
    ) -> Result<IdempotentOutcome<DeviceProvisioningSecretRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            if let Some(identity) = &command.identity {
                if identity.principal_id != command.principal.principal_id
                    || identity.deployment_id != command.instance.deployment_id
                    || identity.instance_id != command.instance.instance_id
                    || identity.kind != ProvisionedIdentityKind::Device
                    || command.secret.state != ProvisioningSecretState::Consumed
                {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "immediate device identity does not match provisioning".to_owned(),
                    ));
                }
            }
            validate_sql_new_runtime_relationships(
                &transaction,
                &command.principal,
                &command.instance,
                ProvisionedIdentityKind::Device,
            )?;
            if command.device.principal_id != command.principal.principal_id
                || command.device.deployment_id != command.instance.deployment_id
                || command.secret.instance_id != command.instance.instance_id
                || command.device.state != DeviceState::Pending
            {
                return Err(AuthorizationStateError::InvalidRecord(
                    "device provisioning aggregate does not match exactly".to_owned(),
                ));
            }
            insert_sql_principal(&transaction, &command.principal)?;
            validate_sql_device_relationships(&transaction, &command.device)?;
            insert_sql_runtime_instance(&transaction, &command.instance)?;
            transaction
                .execute(
                    "INSERT INTO auth_devices (principal_id, deployment_id, state, created_at, updated_at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        command.device.principal_id,
                        command.device.deployment_id,
                        encode_enum(command.device.state)?,
                        command.device.created_at,
                        command.device.updated_at,
                        to_sql_version(command.device.version)?
                    ],
                )
                .map_err(map_write_error)?;
            insert_sql_provisioning_secret(&transaction, &command.secret)?;
            if let Some(identity) = &command.identity {
                insert_sql_provisioned_identity(&transaction, identity)?;
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.secret))
        })
        .await
    }

    async fn mutate_provisioned_instance(
        &self,
        command: ProvisionedInstanceMutation,
    ) -> Result<IdempotentOutcome<RuntimeInstanceRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(result) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(result));
            }
            let current = load_runtime_instance(&transaction, &command.instance.instance_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            let current_device = command
                .device
                .as_ref()
                .map(|device| {
                    load_device(&transaction, &device.principal_id, &device.deployment_id)
                })
                .transpose()?
                .flatten();
            let principal = load_principal(&transaction, &command.instance.principal_id)?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            let principal_state = match command.instance.state {
                RuntimeInstanceState::Active => PrincipalState::Active,
                RuntimeInstanceState::Disabled | RuntimeInstanceState::Stale => {
                    PrincipalState::Disabled
                }
                RuntimeInstanceState::Revoked => PrincipalState::Revoked,
            };
            let current_identity = command
                .identity
                .as_ref()
                .map(|identity| load_provisioned_identity(&transaction, &identity.identity_key_id))
                .transpose()?
                .flatten();
            let authorization_changed = current.state != command.instance.state
                || principal.state != principal_state
                || match (&current_device, &command.device) {
                    (Some(current), Some(next)) => current.state != next.state,
                    (None, None) => false,
                    _ => true,
                }
                || match (&current_identity, &command.identity) {
                    (Some(current), Some(next)) => {
                        current.state != next.state || current.revoked_at != next.revoked_at
                    }
                    (None, None) => false,
                    _ => true,
                };
            let visible_version = current_device
                .as_ref()
                .map_or(current.version, |device| device.version);
            if visible_version != command.expected_version
                || current.created_at != command.instance.created_at
                || current.deployment_id != command.instance.deployment_id
                || current.principal_id != command.instance.principal_id
                || command.instance.version != next_version(current.version)?
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE auth_instances SET state = ?1, updated_at = ?2, version = ?3
                 WHERE instance_id = ?4 AND version = ?5",
                    params![
                        encode_enum(command.instance.state)?,
                        command.instance.updated_at,
                        to_sql_version(command.instance.version)?,
                        command.instance.instance_id,
                        to_sql_version(current.version)?
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            transaction
                .execute(
                    "UPDATE auth_principals SET state = ?1, updated_at = ?2, version = ?3,
                     disabled_at = ?4, revoked_at = ?5
                 WHERE principal_id = ?6 AND version = ?7",
                    params![
                        encode_enum(principal_state)?,
                        command.instance.updated_at,
                        to_sql_version(next_version(principal.version)?)?,
                        (principal_state == PrincipalState::Disabled)
                            .then_some(command.instance.updated_at),
                        (principal_state == PrincipalState::Revoked)
                            .then_some(command.instance.updated_at),
                        command.instance.principal_id,
                        to_sql_version(principal.version)?
                    ],
                )
                .map_err(map_write_error)?;
            if let Some(device) = &command.device {
                if device.version != next_version(command.expected_version)? {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                let changed = transaction
                    .execute(
                        "UPDATE auth_devices SET state = ?1, updated_at = ?2, version = ?3
                     WHERE principal_id = ?4 AND deployment_id = ?5 AND version = ?6",
                        params![
                            encode_enum(device.state)?,
                            device.updated_at,
                            to_sql_version(device.version)?,
                            device.principal_id,
                            device.deployment_id,
                            to_sql_version(command.expected_version)?
                        ],
                    )
                    .map_err(map_write_error)?;
                if changed != 1 {
                    return Err(AuthorizationStateError::StorageConflict);
                }
            }
            if let Some(identity) = &command.identity {
                let current_identity = current_identity
                    .as_ref()
                    .ok_or(AuthorizationStateError::StorageConflict)?;
                if current_identity.identity_public_key != identity.identity_public_key
                    || current_identity.principal_id != identity.principal_id
                    || current_identity.deployment_id != identity.deployment_id
                    || current_identity.instance_id != identity.instance_id
                {
                    return Err(AuthorizationStateError::StorageConflict);
                }
                transaction
                    .execute(
                        "UPDATE auth_provisioned_identities SET state = ?1, revoked_at = ?2
                     WHERE identity_key_id = ?3",
                        params![
                            encode_enum(identity.state)?,
                            identity.revoked_at,
                            identity.identity_key_id
                        ],
                    )
                    .map_err(map_write_error)?;
            }
            if authorization_changed {
                revoke_sql_contexts(
                    &transaction,
                    &AuthorizationContextSelector::Instance(command.instance.instance_id.clone()),
                    AuthorizationContextRevocationReason::InstanceChanged,
                    command.instance.updated_at.div_euclid(1_000),
                )?;
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.instance))
        })
        .await
    }

    async fn mutate_device_delegation(
        &self,
        command: super::super::application::repository::DeviceDelegationMutation,
    ) -> Result<IdempotentOutcome<DeviceRecord>, AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            if let Some(value) = sqlite_idempotency_replay(&transaction, &command.idempotency)? {
                return Ok(IdempotentOutcome::Replayed(value));
            }
            let current = load_device(
                &transaction,
                &command.device.principal_id,
                &command.device.deployment_id,
            )?
            .ok_or(AuthorizationStateError::StorageConflict)?;
            let current_delegation = load_device_delegation(
                &transaction,
                &command.device.principal_id,
                &command.device.deployment_id,
            )?
            .ok_or(AuthorizationStateError::StorageConflict)?;
            if current.version != command.expected_version
                || command.device.version != next_version(current.version)?
                || command.device.created_at != current.created_at
                || command.delegation.principal_id != current_delegation.principal_id
                || command.delegation.deployment_id != current_delegation.deployment_id
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            let device_changed = current.state != command.device.state;
            let changed = transaction
                .execute(
                    "UPDATE auth_devices SET state = ?1, updated_at = ?2, version = ?3
                     WHERE principal_id = ?4 AND deployment_id = ?5 AND version = ?6",
                    params![
                        encode_enum(command.device.state)?,
                        command.device.updated_at,
                        to_sql_version(command.device.version)?,
                        command.device.principal_id,
                        command.device.deployment_id,
                        to_sql_version(command.expected_version)?
                    ],
                )
                .map_err(map_write_error)?;
            if changed != 1 {
                return Err(AuthorizationStateError::StorageConflict);
            }
            transaction
                .execute(
                     "UPDATE auth_device_delegations SET companion_participant_id = ?1,
                        user_login_session_id = ?2, installation_public_key = ?3,
                        device_grant_revision = ?4, child_grant_revision = ?5, required = ?6,
                        state = ?7, expires_at = ?8 WHERE principal_id = ?9 AND deployment_id = ?10",
                    params![
                        command.delegation.companion_participant_id,
                        command.delegation.user_login_session_id,
                        command.delegation.installation_public_key,
                        command.delegation.device_grant_revision,
                        command.delegation.child_grant_revision,
                        command.delegation.required,
                        encode_enum(command.delegation.state)?,
                        command.delegation.expires_at,
                        command.delegation.principal_id,
                        command.delegation.deployment_id
                    ],
                )
                .map_err(map_write_error)?;
            if current_delegation.state == DeviceDelegationState::Active
                && command.delegation.state == DeviceDelegationState::Revoked
            {
                if let Some(session_id) = &current_delegation.user_login_session_id {
                    transaction
                        .execute(
                            "UPDATE auth_sessions SET state = 'revoked', revoked_at = ?1, version = version + 1
                             WHERE session_id = ?2 AND state = 'active'",
                            params![command.device.updated_at, session_id],
                        )
                        .map_err(map_write_error)?;
                    revoke_sql_contexts(
                        &transaction,
                        &AuthorizationContextSelector::Login(session_id.clone()),
                        AuthorizationContextRevocationReason::SessionRevoked,
                        command.device.updated_at.div_euclid(1_000),
                    )?;
                }
            }
            if device_changed {
                revoke_sql_contexts(
                    &transaction,
                    &AuthorizationContextSelector::Principal(command.device.principal_id.clone()),
                    AuthorizationContextRevocationReason::DeviceChanged,
                    command.device.updated_at.div_euclid(1_000),
                )?;
            }
            insert_sql_idempotency_and_actions(
                &transaction,
                &command.idempotency,
                &command.actions,
            )?;
            transaction.commit().map_err(sql_error)?;
            Ok(IdempotentOutcome::Applied(command.device))
        })
        .await
    }
}

pub(in crate::platform::auth) fn validate_sql_identity_relationships(
    connection: &Connection,
    identity: &ProvisionedIdentityRecord,
) -> Result<(), AuthorizationStateError> {
    let principal = load_principal(connection, &identity.principal_id)?.ok_or_else(|| {
        AuthorizationStateError::InvalidRecord(
            "provisioned identity principal is missing".to_owned(),
        )
    })?;
    let deployment = load_deployment(connection, &identity.deployment_id)?.ok_or_else(|| {
        AuthorizationStateError::InvalidRecord(
            "provisioned identity deployment is missing".to_owned(),
        )
    })?;
    let instance = load_runtime_instance(connection, &identity.instance_id)?.ok_or_else(|| {
        AuthorizationStateError::InvalidRecord(
            "provisioned identity instance is missing".to_owned(),
        )
    })?;
    let kinds_match = matches!(
        (identity.kind, principal.kind, deployment.participant_kind),
        (
            ProvisionedIdentityKind::Service,
            PrincipalKind::Service,
            trellis_protocol::ParticipantKind::Service
        ) | (
            ProvisionedIdentityKind::Device,
            PrincipalKind::Device,
            trellis_protocol::ParticipantKind::Device
        )
    );
    let device_matches = identity.kind != ProvisionedIdentityKind::Device
        || load_device(connection, &identity.principal_id, &identity.deployment_id)?.is_some();
    if !kinds_match
        || !device_matches
        || instance.principal_id != identity.principal_id
        || instance.deployment_id != identity.deployment_id
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "provisioned identity relationships do not match exactly".to_owned(),
        ));
    }
    Ok(())
}

pub(in crate::platform::auth) fn validate_sql_new_runtime_relationships(
    connection: &Connection,
    principal: &PrincipalRecord,
    instance: &RuntimeInstanceRecord,
    kind: ProvisionedIdentityKind,
) -> Result<(), AuthorizationStateError> {
    let participant_kind = match kind {
        ProvisionedIdentityKind::Service => trellis_protocol::ParticipantKind::Service,
        ProvisionedIdentityKind::Device => trellis_protocol::ParticipantKind::Device,
    };
    if load_deployment(connection, &instance.deployment_id)?
        .is_none_or(|deployment| deployment.participant_kind != participant_kind)
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "provisioned instance deployment kind does not match".to_owned(),
        ));
    }
    if load_principal(connection, &principal.principal_id)?.is_some()
        || load_runtime_instance(connection, &instance.instance_id)?.is_some()
    {
        return Err(AuthorizationStateError::StorageConflict);
    }
    Ok(())
}

pub(in crate::platform::auth) fn insert_sql_principal(
    connection: &Connection,
    principal: &PrincipalRecord,
) -> Result<(), AuthorizationStateError> {
    connection
    .execute(
        "INSERT INTO auth_principals (principal_id, kind, state, created_at, updated_at, version, disabled_at, revoked_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            principal.principal_id,
            encode_enum(principal.kind)?,
            encode_enum(principal.state)?,
            principal.created_at,
            principal.updated_at,
            to_sql_version(principal.version)?,
            principal.disabled_at,
            principal.revoked_at
        ],
    )
    .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn insert_sql_runtime_instance(
    connection: &Connection,
    instance: &RuntimeInstanceRecord,
) -> Result<(), AuthorizationStateError> {
    connection
    .execute(
        "INSERT INTO auth_instances (instance_id, deployment_id, principal_id, state, created_at, updated_at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            instance.instance_id,
            instance.deployment_id,
            instance.principal_id,
            encode_enum(instance.state)?,
            instance.created_at,
            instance.updated_at,
            to_sql_version(instance.version)?
        ],
    )
    .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn insert_sql_provisioned_identity(
    connection: &Connection,
    identity: &ProvisionedIdentityRecord,
) -> Result<(), AuthorizationStateError> {
    let changed = connection
        .execute(
            "INSERT INTO auth_provisioned_identities
             (identity_key_id, identity_public_key, principal_id, deployment_id, instance_id,
              participant_id, kind, state, created_at, revoked_at)
             SELECT ?1, ?2, ?3, ?4, ?5, deployment.participant_id, ?6, ?7, ?8, ?9
             FROM auth_deployments AS deployment
             WHERE deployment.deployment_id = ?4",
            params![
                identity.identity_key_id,
                identity.identity_public_key,
                identity.principal_id,
                identity.deployment_id,
                identity.instance_id,
                encode_enum(identity.kind)?,
                encode_enum(identity.state)?,
                identity.created_at,
                identity.revoked_at
            ],
        )
        .map_err(map_write_error)?;
    if changed != 1 {
        return Err(AuthorizationStateError::InvalidRecord(
            "provisioned identity requires an assigned deployment participant".to_owned(),
        ));
    }
    Ok(())
}

pub(in crate::platform::auth) fn insert_sql_provisioning_secret(
    connection: &Connection,
    secret: &DeviceProvisioningSecretRecord,
) -> Result<(), AuthorizationStateError> {
    connection
    .execute(
        "INSERT INTO auth_device_provisioning_secrets (secret_id, instance_id, secret_hash, state, created_at, expires_at, consumed_at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            secret.secret_id,
            secret.instance_id,
            secret.secret_hash,
            encode_enum(secret.state)?,
            secret.created_at,
            secret.expires_at,
            secret.consumed_at,
            to_sql_version(secret.version)?
        ],
    )
    .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn load_provisioned_identity(
    connection: &Connection,
    identity_key_id: &str,
) -> Result<Option<ProvisionedIdentityRecord>, AuthorizationStateError> {
    let identity = connection
        .query_row(
        "SELECT identity_key_id, identity_public_key, principal_id, deployment_id, instance_id, kind, state, created_at, revoked_at FROM auth_provisioned_identities WHERE identity_key_id = ?1",
        [identity_key_id],
        |row| {
            Ok(ProvisionedIdentityRecord {
                identity_key_id: row.get(0)?,
                identity_public_key: row.get(1)?,
                principal_id: row.get(2)?,
                deployment_id: row.get(3)?,
                instance_id: row.get(4)?,
                kind: decode_enum(row.get(5)?)?,
                state: decode_enum(row.get(6)?)?,
                created_at: row.get(7)?,
                revoked_at: row.get(8)?,
            })
        },
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(identity) = &identity {
        validate_provisioned_identity(identity)?;
    }
    Ok(identity)
}

pub(in crate::platform::auth) fn decode_provisioning_secret(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<DeviceProvisioningSecretRecord> {
    Ok(DeviceProvisioningSecretRecord {
        secret_id: row.get(0)?,
        instance_id: row.get(1)?,
        secret_hash: row.get(2)?,
        state: decode_enum(row.get(3)?)?,
        created_at: row.get(4)?,
        expires_at: row.get(5)?,
        consumed_at: row.get(6)?,
        version: from_sql_version(row.get(7)?)?,
    })
}

pub(in crate::platform::auth) fn load_provisioning_secret_by_hash(
    connection: &Connection,
    secret_hash: &str,
) -> Result<Option<DeviceProvisioningSecretRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT secret_id, instance_id, secret_hash, state, created_at, expires_at, consumed_at, version FROM auth_device_provisioning_secrets WHERE secret_hash = ?1",
        [secret_hash],
        decode_provisioning_secret,
    )
    .optional()
    .map_err(sql_error)
}

pub(in crate::platform::auth) fn load_activation_review(
    connection: &Connection,
    review_id: &str,
) -> Result<Option<DeviceActivationReviewRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT review_id, principal_id, deployment_id, instance_id, request_digest, payload_json, state, requested_at, expires_at, activated_by_user_principal_id, decided_at, decided_by, reason, version FROM auth_device_activation_reviews WHERE review_id = ?1",
        [review_id],
        |row| {
            Ok(DeviceActivationReviewRecord {
                review_id: row.get(0)?,
                principal_id: row.get(1)?,
                deployment_id: row.get(2)?,
                instance_id: row.get(3)?,
                request_digest: row.get(4)?,
                payload: decode_json(row.get(5)?)?,
                state: decode_enum(row.get(6)?)?,
                requested_at: row.get(7)?,
                expires_at: row.get(8)?,
                activated_by_user_principal_id: row.get(9)?,
                decided_at: row.get(10)?,
                decided_by: row.get(11)?,
                reason: row.get(12)?,
                version: from_sql_version(row.get(13)?)?,
            })
        },
    )
    .optional()
    .map_err(sql_error)
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::SigningKey;
    use serde_json::json;
    use sha2::{Digest as _, Sha256};
    use trellis_protocol::{
        canonicalize_json, sign_authorization_context, AuthorizationPrincipalKind, GrantOwnerKind,
        GrantSet, ParticipantKind, UnsignedAuthorizationContext, AUTHORIZATION_CONTEXT_FORMAT_V1,
    };

    use super::*;
    use crate::platform::auth::{
        authority::AuthorityEvidenceRepository,
        builtins,
        context::{AuthorizationContextRepository, AuthorizationContextState},
        DeploymentRecord, IdempotencyResultRecord, ProvisionedIdentityState,
    };

    const NOW: i64 = 1_800_000_000_000;

    fn idempotency(request_id: &str) -> IdempotencyResultRecord {
        IdempotencyResultRecord {
            scope_key: URL_SAFE_NO_PAD.encode(Sha256::digest(request_id.as_bytes())),
            purpose: "provisioning-regression".to_owned(),
            signer_id: "test-signer".to_owned(),
            request_id: request_id.to_owned(),
            request_digest: URL_SAFE_NO_PAD.encode(Sha256::digest(request_id.as_bytes())),
            result: json!({ "requestId": request_id }),
            created_at: NOW,
            expires_at: NOW + 60_000,
        }
    }

    #[tokio::test]
    async fn revoking_one_service_instance_preserves_its_history_and_its_sibling() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("open sqlite auth store");
        let participant = builtins::auth_runtime_participant_binding(NOW)
            .expect("build service participant evidence");
        let participant_id = participant.participant_id.clone();
        store
            .put_participant_binding(participant)
            .await
            .expect("install service participant evidence");
        let deployment_id = ulid::Ulid::new().to_string();
        let first_principal_id = ulid::Ulid::new().to_string();
        let first_instance_id = ulid::Ulid::new().to_string();
        let second_principal_id = ulid::Ulid::new().to_string();
        let second_instance_id = ulid::Ulid::new().to_string();
        store
            .run({
                let deployment_id = deployment_id.clone();
                let participant_id = participant_id.clone();
                move |connection| {
                    super::super::evidence::put_sql_deployment_evidence(
                        connection,
                        DeploymentRecord {
                            deployment_id,
                            participant_id: participant_id.clone(),
                            participant_kind: ParticipantKind::Service,
                            active: true,
                            expires_at: None,
                        },
                    )
                }
            })
            .await
            .expect("install deployment evidence");

        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let identity_public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
        let identity_key_id =
            URL_SAFE_NO_PAD.encode(Sha256::digest(signing_key.verifying_key().as_bytes()));
        let first_principal = PrincipalRecord {
            principal_id: first_principal_id.clone(),
            kind: PrincipalKind::Service,
            state: PrincipalState::Active,
            created_at: NOW,
            updated_at: NOW,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let first_instance = RuntimeInstanceRecord {
            instance_id: first_instance_id.clone(),
            deployment_id: deployment_id.clone(),
            principal_id: first_principal_id.clone(),
            state: RuntimeInstanceState::Active,
            created_at: NOW,
            updated_at: NOW,
            version: 1,
        };
        let first_identity = ProvisionedIdentityRecord {
            identity_key_id: identity_key_id.clone(),
            identity_public_key: identity_public_key.clone(),
            principal_id: first_principal_id.clone(),
            deployment_id: deployment_id.clone(),
            instance_id: first_instance_id.clone(),
            kind: ProvisionedIdentityKind::Service,
            state: ProvisionedIdentityState::Active,
            created_at: NOW,
            revoked_at: None,
        };
        let first = ServiceIdentityProvisioning {
            deployment_id: deployment_id.clone(),
            identity_key_id: identity_key_id.clone(),
            identity_public_key: identity_public_key.clone(),
            requested_instance_id: Some(first_instance_id.clone()),
            proposed_principal_id: first_principal_id.clone(),
            proposed_instance_id: first_instance_id.clone(),
            created_at: NOW,
            idempotency: idempotency("provision-first"),
            actions: Vec::new(),
        };
        let second_key = SigningKey::from_bytes(&[8; 32]);
        let second_public_key = URL_SAFE_NO_PAD.encode(second_key.verifying_key().as_bytes());
        let second_identity_key_id =
            URL_SAFE_NO_PAD.encode(Sha256::digest(second_key.verifying_key().as_bytes()));
        let second_principal = PrincipalRecord {
            principal_id: second_principal_id.clone(),
            ..first_principal.clone()
        };
        let second_instance = RuntimeInstanceRecord {
            instance_id: second_instance_id.clone(),
            principal_id: second_principal_id.clone(),
            ..first_instance.clone()
        };
        let second_identity = ProvisionedIdentityRecord {
            identity_key_id: second_identity_key_id.clone(),
            identity_public_key: second_public_key.clone(),
            principal_id: second_principal_id.clone(),
            instance_id: second_instance_id.clone(),
            ..first_identity.clone()
        };
        let second = ServiceIdentityProvisioning {
            deployment_id: deployment_id.clone(),
            identity_key_id: second_identity_key_id.clone(),
            identity_public_key: second_public_key.clone(),
            requested_instance_id: Some(second_instance_id.clone()),
            proposed_principal_id: second_principal_id.clone(),
            proposed_instance_id: second_instance_id.clone(),
            created_at: NOW,
            idempotency: idempotency("provision-second"),
            actions: Vec::new(),
        };
        store
            .provision_service_identity(first.clone())
            .await
            .expect("provision first service");
        store
            .provision_service_identity(second.clone())
            .await
            .expect("provision second service");

        for (byte, principal, identity, instance) in [
            (1_u8, &first_principal, &first_identity, &first_instance),
            (2, &second_principal, &second_identity, &second_instance),
        ] {
            let context_key = SigningKey::from_bytes(&[byte; 32]);
            let public_key = URL_SAFE_NO_PAD.encode(context_key.verifying_key().as_bytes());
            let connection_id = ulid::Ulid::new().to_string();
            let unsigned = UnsignedAuthorizationContext {
                format: AUTHORIZATION_CONTEXT_FORMAT_V1.to_owned(),
                issuer_key_id: URL_SAFE_NO_PAD
                    .encode(Sha256::digest(context_key.verifying_key().as_bytes())),
                connection_id: connection_id.clone(),
                session_key: public_key.clone(),
                principal_id: principal.principal_id.clone(),
                principal_kind: AuthorizationPrincipalKind::Service,
                participant_id: participant_id.clone(),
                owner_kind: GrantOwnerKind::Deployment,
                owner_id: deployment_id.clone(),
                grant_revision: 1,
                identity_key_id: Some(identity.identity_key_id.clone()),
                login_session_id: None,
                deployment_id: Some(deployment_id.clone()),
                instance_id: Some(instance.instance_id.clone()),
                inbox_prefix: format!("_INBOX.{connection_id}"),
                issued_at: NOW / 1_000,
                not_before: NOW / 1_000,
                expires_at: NOW / 1_000 + 3_600,
                grants: GrantSet::new(Vec::new()),
                platform_privileges: Vec::new(),
                extensions: serde_json::Map::new(),
                critical: Vec::new(),
            };
            let signed = sign_authorization_context(unsigned.clone(), &context_key)
                .expect("sign authorization context");
            let digest = signed.digest().expect("digest authorization context");
            let signed_json = canonicalize_json(
                &serde_json::to_value(signed).expect("serialize authorization context"),
            )
            .expect("canonicalize authorization context");
            store
                .run(move |connection| {
                    connection.execute(
                        "INSERT INTO auth_authorization_contexts (
                            context_digest, connection_id, session_public_key, inbox_prefix, principal_id,
                            principal_kind, participant_id, owner_kind, owner_id, grant_revision,
                            installed_revision, identity_key_id, login_session_id, issuer_key_id,
                            signed_context_json, issuance_snapshot_token, issued_at, not_before, refresh_at,
                            expires_at, state, published_at, revoked_at, revocation_reason, version
                         ) VALUES (?1, ?2, ?3, ?4, ?5, 'service', ?6, 'deployment', ?7, 1, 1,
                            ?8, NULL, ?9, ?10, ?11, ?12, ?12, ?12, ?13, 'active', ?12, NULL, NULL, 1)",
                        params![digest, connection_id, public_key, unsigned.inbox_prefix,
                            unsigned.principal_id, unsigned.participant_id, unsigned.owner_id,
                            unsigned.identity_key_id, unsigned.issuer_key_id, signed_json,
                            URL_SAFE_NO_PAD.encode([byte; 32]), unsigned.issued_at, unsigned.expires_at],
                    ).map_err(sql_error)?;
                    Ok(())
                })
                .await
                .expect("retain authorization context");
        }

        let mut revoked_instance = first_instance.clone();
        revoked_instance.state = RuntimeInstanceState::Revoked;
        revoked_instance.updated_at = NOW + 1_000;
        revoked_instance.version = 2;
        let mut revoked_identity = first_identity.clone();
        revoked_identity.state = ProvisionedIdentityState::Revoked;
        revoked_identity.revoked_at = Some(NOW + 1_000);
        assert!(matches!(
            store
                .mutate_provisioned_instance(ProvisionedInstanceMutation {
                    instance: revoked_instance.clone(),
                    device: None,
                    identity: Some(revoked_identity.clone()),
                    expected_version: 1,
                    idempotency: idempotency("revoke-first"),
                    actions: Vec::new(),
                })
                .await
                .expect("revoke first service"),
            IdempotentOutcome::Applied(value) if value == revoked_instance
        ));

        assert_eq!(
            store
                .get_runtime_instance(&first_instance_id)
                .await
                .expect("read first instance"),
            Some(revoked_instance)
        );
        assert_eq!(
            store
                .get_provisioned_identity(&first_identity.identity_key_id)
                .await
                .expect("read first identity"),
            Some(revoked_identity)
        );
        assert_eq!(
            store
                .get_runtime_instance(&second_instance_id)
                .await
                .expect("read second instance"),
            Some(second_instance.clone())
        );
        assert_eq!(
            store
                .get_provisioned_identity(&second_identity.identity_key_id)
                .await
                .expect("read second identity"),
            Some(second_identity.clone())
        );

        let contexts = store
            .list_contexts(None, 10)
            .await
            .expect("list retained contexts");
        let first_context = contexts
            .iter()
            .find(|context| context.principal_id == first_principal_id)
            .expect("first context remains in history");
        assert_eq!(first_context.state, AuthorizationContextState::Revoked);
        assert_eq!(first_context.revoked_at, Some((NOW + 1_000) / 1_000));
        assert_eq!(
            first_context.revocation_reason,
            Some(AuthorizationContextRevocationReason::InstanceChanged)
        );
        assert_eq!(
            contexts
                .iter()
                .find(|context| context.principal_id == second_principal_id)
                .expect("second context remains in history")
                .state,
            AuthorizationContextState::Active
        );
    }

    #[tokio::test]
    async fn provisioning_the_same_identity_reuses_its_aggregate() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("open sqlite auth store");
        let participant = builtins::auth_runtime_participant_binding(NOW)
            .expect("build service participant evidence");
        let participant_id = participant.participant_id.clone();
        store
            .put_participant_binding(participant)
            .await
            .expect("install service participant evidence");
        let deployment_id = ulid::Ulid::new().to_string();
        store
            .run({
                let deployment_id = deployment_id.clone();
                let participant_id = participant_id.clone();
                move |connection| {
                    super::super::evidence::put_sql_deployment_evidence(
                        connection,
                        DeploymentRecord {
                            deployment_id,
                            participant_id,
                            participant_kind: ParticipantKind::Service,
                            active: true,
                            expires_at: None,
                        },
                    )
                }
            })
            .await
            .expect("install deployment evidence");

        let signing_key = SigningKey::from_bytes(&[21; 32]);
        let identity_public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
        let identity_key_id =
            URL_SAFE_NO_PAD.encode(Sha256::digest(signing_key.verifying_key().as_bytes()));
        // Each request proposes fresh IDs; the identity key is the stable
        // identity, so the transaction must reuse the first aggregate.
        let command = |request_id: &str| ServiceIdentityProvisioning {
            deployment_id: deployment_id.clone(),
            identity_key_id: identity_key_id.clone(),
            identity_public_key: identity_public_key.clone(),
            requested_instance_id: None,
            proposed_principal_id: ulid::Ulid::new().to_string(),
            proposed_instance_id: ulid::Ulid::new().to_string(),
            created_at: NOW,
            idempotency: idempotency(request_id),
            actions: Vec::new(),
        };
        let first = store
            .provision_service_identity(command("provision-first-attempt"))
            .await
            .expect("first provision");
        let second = store
            .provision_service_identity(command("provision-second-attempt"))
            .await
            .expect("second provision");
        let first_identity = match first {
            IdempotentOutcome::Applied(identity) => identity,
            IdempotentOutcome::Replayed(_) => panic!("first provision replayed"),
        };
        let second_identity = match second {
            IdempotentOutcome::Applied(identity) => identity,
            IdempotentOutcome::Replayed(_) => panic!("second provision replayed"),
        };
        assert_eq!(first_identity.principal_id, second_identity.principal_id);
        assert_eq!(first_identity.instance_id, second_identity.instance_id);
        assert_eq!(
            first_identity.identity_key_id,
            second_identity.identity_key_id
        );
        assert_eq!(
            store
                .get_provisioned_identity(&identity_key_id)
                .await
                .expect("read identity"),
            Some(first_identity)
        );
    }

    #[tokio::test]
    async fn disabling_one_device_instance_preserves_its_history_and_its_sibling() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("open sqlite auth store");
        let deployment_id = ulid::Ulid::new().to_string();
        let participant =
            builtins::auth_runtime_participant_binding(NOW).expect("build participant evidence");
        let participant_id = participant.participant_id.clone();
        store
            .put_participant_binding(participant)
            .await
            .expect("install participant evidence");
        store
            .run({
                let deployment_id = deployment_id.clone();
                let participant_id = participant_id.clone();
                move |connection| {
                    super::super::evidence::put_sql_deployment_evidence(
                        connection,
                        DeploymentRecord {
                            deployment_id,
                            participant_id,
                            participant_kind: ParticipantKind::Device,
                            active: true,
                            expires_at: None,
                        },
                    )
                }
            })
            .await
            .expect("install device deployment evidence");

        let mut devices = Vec::new();
        for byte in [11_u8, 12] {
            let principal_id = ulid::Ulid::new().to_string();
            let instance_id = ulid::Ulid::new().to_string();
            let identity_key = SigningKey::from_bytes(&[byte; 32]);
            let identity_public_key =
                URL_SAFE_NO_PAD.encode(identity_key.verifying_key().as_bytes());
            let identity_key_id =
                URL_SAFE_NO_PAD.encode(Sha256::digest(identity_key.verifying_key().as_bytes()));
            let principal = PrincipalRecord {
                principal_id: principal_id.clone(),
                kind: PrincipalKind::Device,
                state: PrincipalState::Active,
                created_at: NOW,
                updated_at: NOW,
                version: 1,
                disabled_at: None,
                revoked_at: None,
            };
            let instance = RuntimeInstanceRecord {
                instance_id,
                deployment_id: deployment_id.clone(),
                principal_id: principal_id.clone(),
                state: RuntimeInstanceState::Active,
                created_at: NOW,
                updated_at: NOW,
                version: 1,
            };
            let device = DeviceRecord {
                principal_id: principal_id.clone(),
                deployment_id: deployment_id.clone(),
                state: DeviceState::Pending,
                created_at: NOW,
                updated_at: NOW,
                version: 1,
            };
            let identity = ProvisionedIdentityRecord {
                identity_key_id,
                identity_public_key,
                principal_id,
                deployment_id: deployment_id.clone(),
                instance_id: instance.instance_id.clone(),
                kind: ProvisionedIdentityKind::Device,
                state: ProvisionedIdentityState::Active,
                created_at: NOW,
                revoked_at: None,
            };
            let request_id = ulid::Ulid::new().to_string();
            store
                .provision_device(DeviceProvisioning {
                    principal,
                    instance: instance.clone(),
                    device: device.clone(),
                    identity: Some(identity.clone()),
                    secret: DeviceProvisioningSecretRecord {
                        secret_id: ulid::Ulid::new().to_string(),
                        instance_id: instance.instance_id.clone(),
                        secret_hash: URL_SAFE_NO_PAD.encode(Sha256::digest([byte])),
                        state: ProvisioningSecretState::Consumed,
                        created_at: NOW,
                        expires_at: NOW + 60_000,
                        consumed_at: Some(NOW),
                        version: 1,
                    },
                    idempotency: idempotency(&request_id),
                    actions: Vec::new(),
                })
                .await
                .expect("provision device");

            let active_instance = RuntimeInstanceRecord {
                updated_at: NOW + 1_000,
                version: 2,
                ..instance
            };
            let active_device = DeviceRecord {
                state: DeviceState::Active,
                updated_at: NOW + 1_000,
                version: 2,
                ..device
            };
            let request_id = ulid::Ulid::new().to_string();
            store
                .mutate_provisioned_instance(ProvisionedInstanceMutation {
                    instance: active_instance.clone(),
                    device: Some(active_device.clone()),
                    identity: Some(identity.clone()),
                    expected_version: 1,
                    idempotency: idempotency(&request_id),
                    actions: Vec::new(),
                })
                .await
                .expect("activate device");
            devices.push((active_instance, active_device, identity));
        }

        for (byte, (instance, device, identity)) in [21_u8, 22].into_iter().zip(&devices) {
            let context_key = SigningKey::from_bytes(&[byte; 32]);
            let public_key = URL_SAFE_NO_PAD.encode(context_key.verifying_key().as_bytes());
            let connection_id = ulid::Ulid::new().to_string();
            let unsigned = UnsignedAuthorizationContext {
                format: AUTHORIZATION_CONTEXT_FORMAT_V1.to_owned(),
                issuer_key_id: URL_SAFE_NO_PAD
                    .encode(Sha256::digest(context_key.verifying_key().as_bytes())),
                connection_id: connection_id.clone(),
                session_key: public_key.clone(),
                principal_id: device.principal_id.clone(),
                principal_kind: AuthorizationPrincipalKind::Device,
                participant_id: participant_id.clone(),
                owner_kind: GrantOwnerKind::Deployment,
                owner_id: deployment_id.clone(),
                grant_revision: 1,
                identity_key_id: Some(identity.identity_key_id.clone()),
                login_session_id: None,
                deployment_id: Some(deployment_id.clone()),
                instance_id: Some(instance.instance_id.clone()),
                inbox_prefix: format!("_INBOX.{connection_id}"),
                issued_at: NOW / 1_000,
                not_before: NOW / 1_000,
                expires_at: NOW / 1_000 + 3_600,
                grants: GrantSet::new(Vec::new()),
                platform_privileges: Vec::new(),
                extensions: serde_json::Map::new(),
                critical: Vec::new(),
            };
            let signed = sign_authorization_context(unsigned.clone(), &context_key)
                .expect("sign device authorization context");
            let digest = signed
                .digest()
                .expect("digest device authorization context");
            let signed_json = canonicalize_json(
                &serde_json::to_value(signed).expect("serialize device authorization context"),
            )
            .expect("canonicalize device authorization context");
            store
                .run(move |connection| {
                    connection.execute(
                        "INSERT INTO auth_authorization_contexts (
                            context_digest, connection_id, session_public_key, inbox_prefix, principal_id,
                            principal_kind, participant_id, owner_kind, owner_id, grant_revision,
                            installed_revision, identity_key_id, login_session_id, issuer_key_id,
                            signed_context_json, issuance_snapshot_token, issued_at, not_before, refresh_at,
                            expires_at, state, published_at, revoked_at, revocation_reason, version
                         ) VALUES (?1, ?2, ?3, ?4, ?5, 'device', ?6, 'deployment', ?7, 1, 1,
                            ?8, NULL, ?9, ?10, ?11, ?12, ?12, ?12, ?13, 'active', ?12, NULL, NULL, 1)",
                        params![digest, connection_id, public_key, unsigned.inbox_prefix,
                            unsigned.principal_id, unsigned.participant_id, unsigned.owner_id,
                            unsigned.identity_key_id, unsigned.issuer_key_id, signed_json,
                            URL_SAFE_NO_PAD.encode([byte; 32]), unsigned.issued_at, unsigned.expires_at],
                    ).map_err(sql_error)?;
                    Ok(())
                })
                .await
                .expect("retain device authorization context");
        }

        let (first_instance, first_device, first_identity) = &devices[0];
        let disabled_instance = RuntimeInstanceRecord {
            state: RuntimeInstanceState::Disabled,
            updated_at: NOW + 2_000,
            version: 3,
            ..first_instance.clone()
        };
        let disabled_device = DeviceRecord {
            state: DeviceState::Disabled,
            updated_at: NOW + 2_000,
            version: 3,
            ..first_device.clone()
        };
        let request_id = ulid::Ulid::new().to_string();
        assert!(matches!(
            store
                .mutate_provisioned_instance(ProvisionedInstanceMutation {
                    instance: disabled_instance.clone(),
                    device: Some(disabled_device.clone()),
                    identity: Some(first_identity.clone()),
                    expected_version: 2,
                    idempotency: idempotency(&request_id),
                    actions: Vec::new(),
                })
                .await
                .expect("disable first device"),
            IdempotentOutcome::Applied(value) if value == disabled_instance
        ));

        assert_eq!(
            store
                .get_device(&first_device.principal_id, &deployment_id)
                .await
                .expect("read disabled device"),
            Some(disabled_device)
        );
        let (second_instance, second_device, second_identity) = &devices[1];
        assert_eq!(
            store
                .get_runtime_instance(&second_instance.instance_id)
                .await
                .expect("read sibling instance"),
            Some(second_instance.clone())
        );
        assert_eq!(
            store
                .get_device(&second_device.principal_id, &deployment_id)
                .await
                .expect("read sibling device"),
            Some(second_device.clone())
        );
        assert_eq!(
            store
                .get_provisioned_identity(&second_identity.identity_key_id)
                .await
                .expect("read sibling identity"),
            Some(second_identity.clone())
        );

        let contexts = store
            .list_contexts(None, 10)
            .await
            .expect("list retained device contexts");
        let disabled_context = contexts
            .iter()
            .find(|context| context.principal_id == first_device.principal_id)
            .expect("disabled device context remains in history");
        assert_eq!(disabled_context.state, AuthorizationContextState::Revoked);
        assert_eq!(disabled_context.revoked_at, Some((NOW + 2_000) / 1_000));
        assert_eq!(
            disabled_context.revocation_reason,
            Some(AuthorizationContextRevocationReason::InstanceChanged)
        );
        assert_eq!(
            contexts
                .iter()
                .find(|context| context.principal_id == second_device.principal_id)
                .expect("sibling device context remains in history")
                .state,
            AuthorizationContextState::Active
        );
    }
}
