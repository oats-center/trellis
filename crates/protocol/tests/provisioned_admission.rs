//! Direct CONNECT identity possession and target/runtime isolation.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    sign_provisioned_admission, verify_provisioned_admission, ProvisionedAdmission,
    PROVISIONED_ADMISSION_FORMAT_V1,
};

#[test]
fn provisioned_possession_cannot_move_to_another_installation_account_or_runtime_key() {
    let identity = SigningKey::from_bytes(&[11; 32]);
    let public = URL_SAFE_NO_PAD.encode(identity.verifying_key().as_bytes());
    let session = nkeys::KeyPair::new_user();
    let instance = "01JY0000000000000000000001";
    let credential = ProvisionedAdmission {
        format: PROVISIONED_ADMISSION_FORMAT_V1.into(),
        trellis_instance_id: instance.into(),
        audience_nats_account: "application-account".into(),
        credential_id: URL_SAFE_NO_PAD.encode(Sha256::digest(identity.verifying_key().as_bytes())),
        runtime_id: URL_SAFE_NO_PAD.encode([17; 16]),
        ephemeral_nkey: session.public_key(),
        issued_at: 1_000,
        expires_at: 1_060,
    };
    let signed = sign_provisioned_admission(credential.clone(), &identity).unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    let verify = |bytes: &[u8], public: &str, instance: &str, account: &str, key: &str, now| {
        verify_provisioned_admission(bytes, public, instance, account, key, now)
    };
    assert!(verify(
        &bytes,
        &public,
        instance,
        "application-account",
        &session.public_key(),
        1_000
    )
    .is_ok());
    for (target, account, key) in [
        (
            "01JY0000000000000000000002",
            "application-account",
            session.public_key(),
        ),
        (instance, "other-account", session.public_key()),
        (
            instance,
            "application-account",
            nkeys::KeyPair::new_user().public_key(),
        ),
    ] {
        assert!(verify(&bytes, &public, target, account, &key, 1_000).is_err());
    }
    let attacker = SigningKey::from_bytes(&[12; 32]);
    let forged = sign_provisioned_admission(credential, &attacker).unwrap();
    assert!(verify(
        &serde_json::to_vec(&forged).unwrap(),
        &public,
        instance,
        "application-account",
        &session.public_key(),
        1_000
    )
    .is_err());
    assert!(verify(
        &bytes,
        &public,
        instance,
        "application-account",
        &session.public_key(),
        1_060
    )
    .is_err());
    let mut replay = signed;
    replay.credential.runtime_id = URL_SAFE_NO_PAD.encode([18; 16]);
    assert!(verify(
        &serde_json::to_vec(&replay).unwrap(),
        &public,
        instance,
        "application-account",
        &session.public_key(),
        1_000
    )
    .is_err());
}
