use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use ulid::Ulid;

use super::super::*;
use super::bearer_secret_digest;

/// Client-keyed service identity provisioning input.
#[derive(Clone, Debug)]
pub struct ProvisionServiceIdentityInput {
    /// Existing deployment ID.
    pub deployment_id: String,
    /// Caller-selected stable instance ID, or a generated ID.
    pub instance_id: Option<String>,
    /// Canonical client-generated Ed25519 identity public key.
    pub identity_public_key: String,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Durable proof claim; its result is replaced with committed identities.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Administrator device provisioning input.
#[derive(Clone, Debug)]
pub struct ProvisionDeviceInput {
    /// Existing deployment ID.
    pub deployment_id: String,
    /// Caller-selected stable instance ID, or a generated ID.
    pub instance_id: Option<String>,
    /// Optional client-generated identity installed during provisioning.
    pub identity_public_key: Option<String>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
    /// Durable proof claim; its result excludes the one-time raw secret.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// One-time result returned only when device provisioning is first applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvisionedDevice {
    /// Stable device principal ID.
    pub principal_id: String,
    /// Stable runtime instance ID.
    pub instance_id: String,
    /// Raw one-time provisioning secret, never stored by Trellis.
    pub provisioning_secret: Option<String>,
    /// Secret expiry in Unix milliseconds.
    pub expires_at: i64,
}

/// Device identity enrollment input using a one-time provisioning secret.
#[derive(Clone, Debug)]
pub struct EnrollDeviceIdentityInput {
    /// Raw one-time provisioning secret.
    pub provisioning_secret: String,
    /// Expected pending secret version.
    pub expected_version: u64,
    /// Stable provisioned device principal ID.
    pub principal_id: String,
    /// Stable deployment ID.
    pub deployment_id: String,
    /// Stable runtime instance ID.
    pub instance_id: String,
    /// Canonical device-generated Ed25519 identity public key.
    pub identity_public_key: String,
    /// Enrollment time in Unix milliseconds.
    pub consumed_at: i64,
    /// Durable proof claim; its result is replaced with identity metadata.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Device activation-review request input.
#[derive(Clone, Debug)]
pub struct CreateActivationReviewInput {
    /// Stable activation review ID.
    pub review_id: String,
    /// Stable device principal ID.
    pub principal_id: String,
    /// Stable deployment ID.
    pub deployment_id: String,
    /// Stable runtime instance ID.
    pub instance_id: String,
    /// Canonical activation-request digest.
    pub request_digest: String,
    /// Immutable request metadata.
    pub payload: serde_json::Value,
    /// Request time in Unix milliseconds.
    pub requested_at: i64,
    /// Authoritative review expiry in Unix milliseconds.
    pub expires_at: i64,
    /// Durable proof claim; its result is replaced with the review ID.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Administrator activation-review decision input.
#[derive(Clone, Debug)]
pub struct DecideActivationReviewInput {
    /// Stable review ID.
    pub review_id: String,
    /// Expected pending review version.
    pub expected_version: u64,
    /// Approved or rejected terminal state.
    pub state: DeviceActivationReviewState,
    /// Stable deciding principal or operator.
    pub decided_by: String,
    /// Required-nullable safe reason.
    pub reason: Option<String>,
    /// Optional approved delegation.
    pub delegation: Option<DeviceDelegationRecord>,
    /// Separate user session installed for the companion.
    pub companion_session: Option<SessionRecord>,
    /// Whether this decision satisfies every requirement for device readiness.
    pub activate_device: bool,
    /// Decision time in Unix milliseconds.
    pub decided_at: i64,
    /// Durable proof claim; its result is replaced with the review outcome.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

/// Authenticated user activation-review claim input.
#[derive(Clone, Debug)]
pub struct ClaimActivationReviewInput {
    /// Stable review ID.
    pub review_id: String,
    /// Expected review version.
    pub expected_version: u64,
    /// Activating user principal.
    pub activated_by_user_principal_id: String,
    /// Authoritative server time in Unix milliseconds.
    pub now: i64,
    /// Active delegation created when claiming an already approved review.
    pub delegation: Option<DeviceDelegationRecord>,
    /// Separate user session installed for the companion.
    pub companion_session: Option<SessionRecord>,
    /// Durable operation result.
    pub idempotency: IdempotencyResultRecord,
    /// Deterministic post-commit actions.
    pub actions: Vec<PostCommitActionRecord>,
}

impl<R> AuthService<R>
where
    R: ProvisioningRepository + Clone,
{
    /// Provision immutable service identity metadata around a client-generated key.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed keys or timestamps and a
    /// conflict when deployment or stable identity relationships do not match.
    pub(crate) async fn provision_service_identity(
        &self,
        mut input: ProvisionServiceIdentityInput,
    ) -> Result<IdempotentOutcome<ProvisionedIdentityRecord>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("createdAt", input.created_at)?;
        let identity_key_id = super::super::domain::validate_ed25519_public_key(
            "identityPublicKey",
            &input.identity_public_key,
        )?;
        let principal_id = format!("svc_{}", Ulid::new());
        let instance_id = input
            .instance_id
            .take()
            .unwrap_or_else(|| format!("ins_{}", Ulid::new()));
        let principal = PrincipalRecord {
            principal_id: principal_id.clone(),
            kind: PrincipalKind::Service,
            state: PrincipalState::Active,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let instance = RuntimeInstanceRecord {
            instance_id: instance_id.clone(),
            deployment_id: input.deployment_id.clone(),
            principal_id: principal_id.clone(),
            state: RuntimeInstanceState::Active,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
        };
        let identity = ProvisionedIdentityRecord {
            identity_key_id: identity_key_id.clone(),
            identity_public_key: input.identity_public_key,
            principal_id: principal_id.clone(),
            deployment_id: input.deployment_id.clone(),
            instance_id: instance_id.clone(),
            kind: ProvisionedIdentityKind::Service,
            state: ProvisionedIdentityState::Active,
            created_at: input.created_at,
            revoked_at: None,
        };
        super::validation::validate_provisioned_identity(&identity)?;
        super::validation::validate_provisioning_aggregate(
            &principal,
            &instance,
            ProvisionedIdentityKind::Service,
        )?;
        input.idempotency.result = json!({
            "principalId": principal_id,
            "instanceId": instance_id,
            "identityKeyId": identity_key_id,
        });
        self.repository
            .provision_service_identity(ServiceIdentityProvisioning {
                principal,
                instance,
                identity,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await
    }

    /// Provision a disabled device and return its raw secret exactly once.
    ///
    /// The durable replay result deliberately excludes the raw secret. A caller
    /// that loses the first successful response must create a new provisioning
    /// record rather than recover secret material from Trellis.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for unsafe timestamps and a repository
    /// conflict when the deployment or generated identities cannot be committed.
    pub(crate) async fn provision_device(
        &self,
        mut input: ProvisionDeviceInput,
    ) -> Result<IdempotentOutcome<ProvisionedDevice>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("createdAt", input.created_at)?;
        let expires_at = u64::try_from(input.created_at)
            .ok()
            .and_then(|created| created.checked_add(self.config.device_provisioning_secret_ttl_ms))
            .filter(|expires| *expires <= super::super::MAX_PROTOCOL_INTEGER)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "device provisioning expiry overflow".to_owned(),
                )
            })? as i64;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| {
            AuthorizationStateError::Storage(format!(
                "device provisioning secret generation failed: {error}"
            ))
        })?;
        let provisioning_secret = URL_SAFE_NO_PAD.encode(secret);
        let secret_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret));
        let principal_id = format!("dev_{}", Ulid::new());
        let instance_id = input
            .instance_id
            .take()
            .unwrap_or_else(|| format!("ins_{}", Ulid::new()));
        let principal = PrincipalRecord {
            principal_id: principal_id.clone(),
            kind: PrincipalKind::Device,
            state: PrincipalState::Active,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
            disabled_at: None,
            revoked_at: None,
        };
        let instance = RuntimeInstanceRecord {
            instance_id: instance_id.clone(),
            deployment_id: input.deployment_id.clone(),
            principal_id: principal_id.clone(),
            state: RuntimeInstanceState::Active,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
        };
        let device = DeviceRecord {
            principal_id: principal_id.clone(),
            deployment_id: input.deployment_id.clone(),
            state: DeviceState::Pending,
            created_at: input.created_at,
            updated_at: input.created_at,
            version: 1,
        };
        let returns_secret = input.identity_public_key.is_none();
        let identity = input
            .identity_public_key
            .map(|identity_public_key| {
                let identity_key_id = super::super::domain::validate_ed25519_public_key(
                    "identityPublicKey",
                    &identity_public_key,
                )?;
                Ok(ProvisionedIdentityRecord {
                    identity_key_id,
                    identity_public_key,
                    principal_id: principal_id.clone(),
                    deployment_id: input.deployment_id.clone(),
                    instance_id: instance_id.clone(),
                    kind: ProvisionedIdentityKind::Device,
                    state: ProvisionedIdentityState::Active,
                    created_at: input.created_at,
                    revoked_at: None,
                })
            })
            .transpose()?;
        if let Some(identity) = &identity {
            super::validation::validate_provisioned_identity(identity)?;
        }
        let durable_secret = DeviceProvisioningSecretRecord {
            secret_id: format!("dps_{}", Ulid::new()),
            instance_id: instance_id.clone(),
            secret_hash,
            state: if identity.is_some() {
                ProvisioningSecretState::Consumed
            } else {
                ProvisioningSecretState::Pending
            },
            created_at: input.created_at,
            expires_at,
            consumed_at: identity.as_ref().map(|_| input.created_at),
            version: 1,
        };
        super::validation::validate_provisioning_aggregate(
            &principal,
            &instance,
            ProvisionedIdentityKind::Device,
        )?;
        super::validation::validate_provisioning_secret(&durable_secret)?;
        super::super::authority::validate_device(&device)?;
        input.idempotency.result = json!({
            "principalId": principal_id,
            "instanceId": instance_id,
            "expiresAt": expires_at,
        });
        match self
            .repository
            .provision_device(DeviceProvisioning {
                principal,
                instance,
                device,
                identity,
                secret: durable_secret,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?
        {
            IdempotentOutcome::Applied(_) => Ok(IdempotentOutcome::Applied(ProvisionedDevice {
                principal_id,
                instance_id,
                provisioning_secret: returns_secret.then_some(provisioning_secret),
                expires_at,
            })),
            IdempotentOutcome::Replayed(value) => Ok(IdempotentOutcome::Replayed(value)),
        }
    }

    /// Consume one device secret and bind its client-generated identity key.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed secret or key encodings,
    /// and a conflict for expired, consumed, or mismatched provisioning state.
    pub(crate) async fn enroll_device_identity(
        &self,
        mut input: EnrollDeviceIdentityInput,
    ) -> Result<IdempotentOutcome<ProvisionedIdentityRecord>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("consumedAt", input.consumed_at)?;
        let secret_hash = bearer_secret_digest(&input.provisioning_secret)?;
        let identity_key_id = super::super::domain::validate_ed25519_public_key(
            "identityPublicKey",
            &input.identity_public_key,
        )?;
        let identity = ProvisionedIdentityRecord {
            identity_key_id: identity_key_id.clone(),
            identity_public_key: input.identity_public_key,
            principal_id: input.principal_id,
            deployment_id: input.deployment_id,
            instance_id: input.instance_id,
            kind: ProvisionedIdentityKind::Device,
            state: ProvisionedIdentityState::Active,
            created_at: input.consumed_at,
            revoked_at: None,
        };
        super::validation::validate_provisioned_identity(&identity)?;
        input.idempotency.result = json!({ "identityKeyId": identity_key_id });
        match self
            .repository
            .consume_device_provisioning_secret(DeviceProvisioningSecretConsumption {
                secret_hash,
                expected_version: input.expected_version,
                identity: identity.clone(),
                consumed_at: input.consumed_at,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?
        {
            IdempotentOutcome::Applied(_) => Ok(IdempotentOutcome::Applied(identity)),
            IdempotentOutcome::Replayed(value) => Ok(IdempotentOutcome::Replayed(value)),
        }
    }

    /// Create one immutable pending device activation review.
    ///
    /// # Errors
    ///
    /// Returns an invalid-record error for malformed request evidence and a
    /// conflict unless device, deployment, and runtime instance match exactly.
    pub(crate) async fn create_activation_review(
        &self,
        mut input: CreateActivationReviewInput,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::super::domain::require_protocol_timestamp("requestedAt", input.requested_at)?;
        let review = DeviceActivationReviewRecord {
            review_id: input.review_id.clone(),
            principal_id: input.principal_id,
            deployment_id: input.deployment_id,
            instance_id: input.instance_id,
            request_digest: input.request_digest,
            payload: input.payload,
            state: DeviceActivationReviewState::Pending,
            requested_at: input.requested_at,
            expires_at: input.expires_at,
            activated_by_user_principal_id: None,
            decided_at: None,
            decided_by: None,
            reason: None,
            version: 1,
        };
        super::validation::validate_activation_review(&review)?;
        input.idempotency.result = json!({ "reviewId": input.review_id });
        let outcome = self
            .repository
            .create_activation_review(ActivationReviewCreation {
                review,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?;
        if let IdempotentOutcome::Applied(review) = &outcome {
            self.activation_reviews.notify(&review.review_id).await;
        }
        Ok(outcome)
    }

    /// Expire every pending activation review due at the supplied server time.
    pub(crate) async fn expire_due_activation_reviews(
        &self,
        now: i64,
    ) -> Result<Vec<DeviceActivationReviewRecord>, AuthorizationStateError> {
        super::super::domain::require_protocol_timestamp("now", now)?;
        let reviews = self.repository.expire_due_activation_reviews(now).await?;
        for review in &reviews {
            self.activation_reviews.notify(&review.review_id).await;
        }
        Ok(reviews)
    }

    /// Claim one activation review for an authenticated user.
    pub(crate) async fn claim_activation_review(
        &self,
        mut input: ClaimActivationReviewInput,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        if input.review_id.is_empty() || input.activated_by_user_principal_id.is_empty() {
            return Err(AuthorizationStateError::InvalidRecord(
                "activation review claim identifiers must be non-empty".to_owned(),
            ));
        }
        input.idempotency.result = json!({ "reviewId": input.review_id });
        let outcome = self
            .repository
            .claim_activation_review(ActivationReviewClaim {
                review_id: input.review_id,
                expected_version: input.expected_version,
                activated_by_user_principal_id: input.activated_by_user_principal_id,
                now: input.now,
                delegation: input.delegation,
                companion_session: input.companion_session,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?;
        if let IdempotentOutcome::Applied(review) = &outcome {
            self.activation_reviews.notify(&review.review_id).await;
        }
        Ok(outcome)
    }

    /// Decide a device activation review and apply approved device state.
    ///
    /// # Errors
    ///
    /// Returns a conflict for stale reviews and an invalid-record error for
    /// unsupported states or inconsistent delegation evidence.
    pub(crate) async fn decide_activation_review(
        &self,
        mut input: DecideActivationReviewInput,
    ) -> Result<IdempotentOutcome<DeviceActivationReviewRecord>, AuthorizationStateError> {
        super::validation::validate_idempotency_and_actions(&input.idempotency, &input.actions)?;
        super::validation::validate_activation_decision(
            input.state,
            input.decided_at,
            &input.decided_by,
        )?;
        let decision = ActivationReviewDecision {
            review_id: input.review_id.clone(),
            expected_version: input.expected_version,
            state: input.state,
            decided_at: input.decided_at,
            decided_by: input.decided_by.clone(),
            reason: input.reason.clone(),
            delegation: input.delegation.clone(),
            companion_session: input.companion_session.clone(),
            activate_device: input.activate_device,
            idempotency: input.idempotency.clone(),
            actions: input.actions.clone(),
        };
        super::validation::validate_activation_decision_changes(&decision)?;
        input.idempotency.result = json!({
            "reviewId": input.review_id,
            "state": input.state,
        });
        let outcome = self
            .repository
            .decide_activation_review(ActivationReviewDecision {
                review_id: input.review_id,
                expected_version: input.expected_version,
                state: input.state,
                decided_at: input.decided_at,
                decided_by: input.decided_by,
                reason: input.reason,
                delegation: input.delegation,
                companion_session: input.companion_session,
                activate_device: input.activate_device,
                idempotency: input.idempotency,
                actions: input.actions,
            })
            .await?;
        if let IdempotentOutcome::Applied(review) = &outcome {
            self.activation_reviews.notify(&review.review_id).await;
        }
        Ok(outcome)
    }
}
