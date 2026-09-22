use super::*;
use crate::platform::auth::{
    DeviceActivationReviewState, DeviceReviewMode, DeviceState, EnrollDeviceIdentityInput,
    ProvisioningSecretState, RuntimeInstanceState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceEnrollmentRequest {
    identity_key_id: String,
    identity_public_key: String,
    session_key: String,
    connection_id: String,
    request_id: String,
    #[serde(rename = "iat")]
    _iat: i64,
    participant_id: String,
    #[serde(rename = "packageEvidence")]
    _package_evidence: Value,
    participant_path: String,
    package_digest: String,
    challenge_digest: String,
    confirmation_code: String,
    provisioning_secret: Option<String>,
    name: Option<String>,
    companion: Option<DeviceCompanionClaim>,
    proof: Value,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceCompanionClaim {
    participant_id: String,
    kind: trellis_protocol::ParticipantKind,
    installation_public_key: String,
    request_proof: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::platform::auth::http) struct DeviceEnrollmentResponse {
    server_now: i64,
    state: &'static str,
    activation: Option<DeviceEnrollmentActivation>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceEnrollmentActivation {
    state: &'static str,
    activation_url: String,
    review_id: String,
    expires_at: i64,
    retry_after_ms: u64,
    principal_id: String,
    deployment_id: String,
    instance_id: String,
}

pub(in crate::platform::auth::http) async fn device_enroll<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Json(raw): Json<Value>,
) -> Result<Json<DeviceEnrollmentResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + DeploymentRepository
        + GrantRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let request: DeviceEnrollmentRequest = serde_json::from_value(raw.clone())
        .map_err(|_| HttpError::bad_request("invalid_device_enrollment"))?;
    if request
        .name
        .as_ref()
        .is_some_and(|name| name.chars().count() > 128)
        || request.confirmation_code.len() != 8
        || !request
            .confirmation_code
            .bytes()
            .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
    {
        return Err(HttpError::bad_request("invalid_device_enrollment"));
    }
    let derived_key_id = crate::platform::auth::validate_ed25519_public_key(
        "identityPublicKey",
        &request.identity_public_key,
    )?;
    if derived_key_id != request.identity_key_id {
        return Err(HttpError::unauthorized("identity_key_mismatch"));
    }
    crate::platform::auth::validate_ed25519_public_key("sessionKey", &request.session_key)?;
    ulid::Ulid::from_string(&request.connection_id)
        .map_err(|_| HttpError::bad_request("invalid_connection_id"))?;
    let mut unsigned_request = raw.clone();
    unsigned_request
        .as_object_mut()
        .ok_or_else(|| HttpError::bad_request("invalid_device_enrollment"))?
        .remove("proof");
    let proof_input = SessionProofInput::device_enrollment(NativeBootstrapSessionProofInput {
        origin: state.public_origin.clone(),
        unsigned_request,
    })
    .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
    let request_digest = proof_request_digest(&raw)
        .map_err(|_| HttpError::bad_request("invalid_device_enrollment"))?;
    verify_session_proof(
        &proof_input,
        &parse_session_proof(&request.proof)
            .map_err(|_| HttpError::unauthorized("invalid_proof"))?,
        &request.identity_public_key,
        now_ms()?,
        state.proof_policy,
    )
    .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
    let companion_request_proof_digest = if let Some(companion) = &request.companion {
        let mut enrollment = raw.clone();
        let enrollment = enrollment
            .as_object_mut()
            .ok_or_else(|| HttpError::bad_request("invalid_device_enrollment"))?;
        enrollment.remove("proof");
        enrollment.remove("companion");
        let claim = json!({
            "participantId": companion.participant_id,
            "kind": companion.kind,
            "installationPublicKey": companion.installation_public_key,
        });
        let digest = trellis_protocol::digest_json(&json!({
            "format": "trellis.device.user-companion.v1",
            "origin": state.public_origin,
            "enrollment": enrollment,
            "claim": claim,
        }))
        .map_err(|_| HttpError::bad_request("invalid_device_enrollment"))?;
        crate::platform::auth::verify_detached_ed25519_proof(
            &companion.installation_public_key,
            &digest,
            &companion.request_proof,
        )
        .map_err(|_| HttpError::unauthorized("invalid_companion_proof"))?;
        Some(digest)
    } else {
        None
    };

    let now = now_ms()?;
    let existing_identity = state
        .service
        .repository()
        .get_provisioned_identity(&request.identity_key_id)
        .await?;
    let identity = if let Some(identity) = existing_identity {
        if identity.identity_public_key != request.identity_public_key
            || identity.kind != ProvisionedIdentityKind::Device
            || identity.state != ProvisionedIdentityState::Active
        {
            return Err(HttpError::unauthorized("identity_inactive"));
        }
        identity
    } else {
        let secret = request
            .provisioning_secret
            .as_deref()
            .ok_or_else(|| HttpError::unauthorized("provisioning_secret_required"))?;
        let secret_hash = crate::platform::auth::application::bearer_secret_digest(secret)?;
        let secret_record = state
            .service
            .repository()
            .get_device_provisioning_secret_by_hash(&secret_hash)
            .await?
            .ok_or_else(|| HttpError::unauthorized("provisioning_secret_invalid"))?;
        if secret_record.state != ProvisioningSecretState::Pending
            || secret_record.expires_at <= now
        {
            return Err(HttpError::unauthorized("provisioning_secret_invalid"));
        }
        let instance = state
            .service
            .repository()
            .get_runtime_instance(&secret_record.instance_id)
            .await?
            .ok_or_else(|| HttpError::unauthorized("instance_not_found"))?;
        let deployment = state
            .service
            .repository()
            .get_deployment_evidence(&instance.deployment_id)
            .await?
            .ok_or_else(|| HttpError::unauthorized("deployment_not_found"))?;
        if instance.state != RuntimeInstanceState::Active
            || !deployment.active
            || deployment.participant_id != request.participant_id
        {
            return Err(HttpError::unauthorized("provisioning_assignment_mismatch"));
        }
        state
            .service
            .enroll_device_identity(EnrollDeviceIdentityInput {
                provisioning_secret: secret.to_owned(),
                expected_version: secret_record.version,
                principal_id: instance.principal_id,
                deployment_id: instance.deployment_id,
                instance_id: instance.instance_id,
                identity_public_key: request.identity_public_key.clone(),
                consumed_at: now,
                idempotency: idempotency(
                    &request.identity_key_id,
                    "device.identity.enroll",
                    &request.identity_key_id,
                    &request.request_id,
                    &request_digest,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        state
            .service
            .repository()
            .get_provisioned_identity(&request.identity_key_id)
            .await?
            .ok_or_else(|| HttpError::internal("enrolled_identity_missing"))?
    };
    let instance = state
        .service
        .repository()
        .get_runtime_instance(&identity.instance_id)
        .await?
        .ok_or_else(|| HttpError::unauthorized("instance_not_found"))?;
    let deployment = state
        .service
        .repository()
        .get_deployment_evidence(&identity.deployment_id)
        .await?
        .ok_or_else(|| HttpError::unauthorized("deployment_not_found"))?;
    let (_, installed) = state
        .service
        .repository()
        .get_installed_participant_record(request.participant_id.clone(), None)
        .await?
        .ok_or_else(|| HttpError::unauthorized("participant_not_installed"))?;
    let expected_companion = installed.resolve()?;
    if installed.participant_path != request.participant_path
        || installed.package_digest != request.package_digest
    {
        return Err(HttpError::unauthorized("participant_evidence_mismatch"));
    }
    match (
        &request.companion,
        &expected_companion.companion_participant_id,
    ) {
        (None, None) => {}
        (Some(claim), Some(expected_id))
            if claim.participant_id == *expected_id
                && Some(claim.kind) == expected_companion.companion_participant_kind
                && matches!(
                    claim.kind,
                    trellis_protocol::ParticipantKind::App
                        | trellis_protocol::ParticipantKind::Agent
                ) => {}
        _ => return Err(HttpError::unauthorized("companion_claim_mismatch")),
    }
    let device = state
        .service
        .repository()
        .get_device(&identity.principal_id, &identity.deployment_id)
        .await?
        .ok_or_else(|| HttpError::unauthorized("device_not_found"))?;
    if instance.principal_id != identity.principal_id
        || instance.state != RuntimeInstanceState::Active
        || !deployment.active
        || deployment.participant_id != request.participant_id
    {
        return Err(HttpError::unauthorized("identity_assignment_mismatch"));
    }
    if device.state == DeviceState::Active {
        return Ok(Json(DeviceEnrollmentResponse {
            server_now: now,
            state: "approved",
            activation: None,
        }));
    }
    if device.state != DeviceState::Pending {
        return Ok(Json(DeviceEnrollmentResponse {
            server_now: now,
            state: "rejected",
            activation: None,
        }));
    }

    state.service.expire_due_activation_reviews(now).await?;
    let existing = state
        .service
        .repository()
        .list_activation_reviews()
        .await?
        .into_iter()
        .filter(|review| {
            review.principal_id == identity.principal_id
                && review.deployment_id == identity.deployment_id
                && review.instance_id == identity.instance_id
        })
        .max_by_key(|review| review.requested_at);
    if existing
        .as_ref()
        .is_some_and(|review| review.state == DeviceActivationReviewState::Rejected)
    {
        return Ok(Json(DeviceEnrollmentResponse {
            server_now: now,
            state: "rejected",
            activation: None,
        }));
    }
    let expires_at = now
        .checked_add(crate::platform::auth::DEVICE_ACTIVATION_REVIEW_TTL_MS)
        .ok_or_else(|| HttpError::internal("device_activation_expiry_overflow"))?;
    let (review_id, review_expires_at) = if let Some(review) = existing.filter(|review| {
        review.state == DeviceActivationReviewState::Pending && review.expires_at > now
    }) {
        (review.review_id, review.expires_at)
    } else {
        let review_id = format!("dar_{}", ulid::Ulid::new());
        let profile = state
            .service
            .repository()
            .get_deployment_profile(&identity.deployment_id)
            .await?
            .ok_or_else(|| HttpError::unauthorized("deployment_not_found"))?;
        let actions = (profile.review_mode == Some(DeviceReviewMode::Required))
            .then(|| {
                let mut payload = json!({
                    "eventType": "Auth.DeviceUserAuthorities.ReviewRequested",
                    "eventId": format!("evt_{}", digest_parts(&[&review_id, "review-requested"])),
                    "occurredAt": now,
                    "reviewId": review_id,
                    "deploymentId": identity.deployment_id,
                    "instanceId": identity.instance_id,
                    "requestedAt": now,
                    "expiresAt": expires_at,
                });
                payload["eventSubject"] = json!(crate::platform::auth::auth_event_subject::<
                    trellis_runtime_apis::apis::trellis_auth_v1::events::DeviceUserAuthoritiesReviewRequested,
                >(&payload).expect("valid device review event subject"));
                PostCommitActionRecord {
                predecessor_action_id: None,
                action_id: crate::platform::auth::activation_review_event_action_id(
                    &review_id,
                    "review-requested",
                )
                .expect("valid activation review event action"),
                kind: PostCommitActionKind::Event,
                payload,
                created_at: now,
                attempts: 0,
                next_attempt_at: now,
                claimed_until: None,
                last_error: None,
            }})
            .into_iter()
            .collect();
        let outcome = state
            .service
            .create_activation_review(CreateActivationReviewInput {
                review_id: review_id.clone(),
                principal_id: identity.principal_id.clone(),
                deployment_id: identity.deployment_id.clone(),
                instance_id: identity.instance_id.clone(),
                request_digest: request.challenge_digest.clone(),
                payload: json!({
                    "source": "device_enrollment",
                    "publicIdentityKey": request.identity_public_key,
                    "participantId": request.participant_id,
                    "confirmationCode": request.confirmation_code,
                    "expiresAt": expires_at,
                    "companion": request.companion.as_ref().map(|companion| json!({
                        "participantId": companion.participant_id,
                        "kind": companion.kind,
                        "installationPublicKey": companion.installation_public_key,
                        "requestProof": companion.request_proof,
                        "requestProofDigest": companion_request_proof_digest,
                    })),
                    "companionRequired": expected_companion.companion_required,
                }),
                requested_at: now,
                expires_at,
                idempotency: idempotency(
                    &request.identity_key_id,
                    "device.activation.request",
                    &request.identity_key_id,
                    &request.request_id,
                    &request_digest,
                    now,
                )?,
                actions,
            })
            .await?;
        let review_id = match outcome {
            IdempotentOutcome::Applied(review) => review.review_id,
            IdempotentOutcome::Replayed(value) => value
                .get("reviewId")
                .and_then(Value::as_str)
                .ok_or_else(|| HttpError::internal("invalid_activation_replay"))?
                .to_owned(),
        };
        (review_id, expires_at)
    };
    let portal = super::super::browser::select_device_portal(
        state.service.repository(),
        &request.participant_id,
        &identity.deployment_id,
    )
    .await?;
    let entry = portal.entry_url.as_deref().map_or_else(
        || format!("{}/login/device", state.public_origin.trim_end_matches('/')),
        ToOwned::to_owned,
    );
    let mut activation_url =
        Url::parse(&entry).map_err(|_| HttpError::internal("portal_entry_invalid"))?;
    activation_url
        .query_pairs_mut()
        .append_pair("flowId", &review_id);
    Ok(Json(DeviceEnrollmentResponse {
        server_now: now,
        state: "pending",
        activation: Some(DeviceEnrollmentActivation {
            state: "pending",
            activation_url: activation_url.into(),
            review_id,
            expires_at: review_expires_at,
            retry_after_ms: 1_000,
            principal_id: identity.principal_id,
            deployment_id: identity.deployment_id,
            instance_id: identity.instance_id,
        }),
    }))
}
