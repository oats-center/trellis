use super::*;
use crate::platform::auth::authorization_sessions::IssuedAuthority;
use trellis_runtime_apis::__types::trellis::{
    AuthServiceClaimCreateRequest, AuthServiceClaimRemoveRequest, AuthServiceClaimState,
};

/// Sign in as `key`'s identity through the ordinary Auth Callout owner, the
/// way a service connects.
fn admit(f: &Fixture, key: &SigningKey) -> Result<IssuedAuthority, AuthError> {
    let public = URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes());
    let ephemeral = nkeys::KeyPair::new_user();
    let signed = trellis_protocol::sign_provisioned_admission(
        trellis_protocol::ProvisionedAdmission {
            format: trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1.into(),
            trellis_instance_id: f.store.settings.instance_id.clone(),
            audience_nats_account: f.store.settings.account.clone(),
            credential_id: super::super::deployments::identity_key_id(&public)?,
            runtime_id: URL_SAFE_NO_PAD.encode([31; 16]),
            ephemeral_nkey: ephemeral.public_key(),
            issued_at: NOW,
            expires_at: NOW + 60,
        },
        key,
    )
    .map_err(|_| AuthError::Denied)?;
    admit_credential(
        &f.store,
        &serde_json::to_vec(&signed)?,
        &ephemeral.public_key(),
        NOW,
    )
}

#[test]
fn a_waiting_claim_may_only_submit_its_service_and_a_removed_one_is_turned_away() {
    let f = fixture();
    let key = SigningKey::from_bytes(&[23; 32]);
    let input: AuthServiceClaimCreateRequest = serde_json::from_value(serde_json::json!({
        "title": "third-party",
        "instanceTitle": "third-party",
        "identityPublicKey": URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
        "idempotencyKey": id(),
    }))
    .unwrap();
    let claim = super::super::service_updates::create_claim(
        &f.store,
        &mutation(&f.admin, &input),
        &input,
        NOW,
    )
    .unwrap()
    .claim;
    assert_eq!(claim.state, AuthServiceClaimState::Waiting);

    let admitted = admit(&f, &key).unwrap().authority.unsigned;
    // Nothing is installed: the binding names no participant, so the service
    // submits whatever build connects, and that is all it may do.
    assert_eq!(
        admitted.binding,
        trellis_protocol::SessionBinding::Service {
            deployment_id: claim.claim_id.to_string(),
            instance_id: claim.claim_id.to_string(),
            participant_id: String::new(),
        }
    );
    assert_eq!(
        admitted
            .capabilities
            .iter()
            .map(|capability| capability.capability_id.as_str())
            .collect::<Vec<_>>(),
        ["trellis.auth.v1:serviceUpdates.submit"]
    );

    let remove: AuthServiceClaimRemoveRequest = serde_json::from_value(serde_json::json!({
        "claimId": claim.claim_id,
        "expectedRevision": claim.revision,
        "idempotencyKey": id(),
    }))
    .unwrap();
    super::super::service_updates::remove_claim(
        &f.store,
        &mutation(&f.admin, &remove),
        &remove,
        NOW,
    )
    .unwrap();
    assert!(admit(&f, &key).is_err());
}
