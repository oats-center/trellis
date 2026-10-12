//! Native OAuth signatures must retain their meaning across the packaged WASM boundary.

use std::{fs, path::PathBuf, process::Command};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    sign_oauth_admission, verify_oauth_admission, AuthorityIssuerKey, AuthorityIssuerState,
    OAuthAdmission, OAuthAdmissionBinding, OAUTH_ADMISSION_FORMAT_V1,
};

#[test]
fn native_oauth_admission_authenticates_scope_time_and_identity_in_packaged_wasm() {
    let signing_key = SigningKey::from_bytes(&[29; 32]);
    let issuer = AuthorityIssuerKey {
        key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(signing_key.verifying_key().as_bytes())),
        public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes()),
        state: AuthorityIssuerState::Active,
    };
    let instance = "01JY0000000000000000000001";
    let account = nkeys::KeyPair::new_account().public_key();
    let ephemeral_nkey = nkeys::KeyPair::new_user().public_key();
    let jkt = URL_SAFE_NO_PAD.encode([31; 32]);
    let signed = sign_oauth_admission(
        OAuthAdmission {
            format: OAUTH_ADMISSION_FORMAT_V1.into(),
            issuer_key_id: issuer.key_id.clone(),
            trellis_instance_id: instance.into(),
            audience_nats_account: account.clone(),
            oauth_grant_id: "01JY0000000000000000000002".into(),
            refresh_family_id: "refresh-family".into(),
            login_session_id: "01JY0000000000000000000003".into(),
            principal_id: "01JY0000000000000000000004".into(),
            client_id: "https://app.example/native.json".into(),
            binding: OAuthAdmissionBinding::Native {
                native_dpop_jkt: jkt.clone(),
            },
            persistent_dpop_jkt: jkt,
            runtime_id: URL_SAFE_NO_PAD.encode([37; 16]),
            ephemeral_nkey: ephemeral_nkey.clone(),
            issued_at: 1_000,
            not_before: 1_000,
            expires_at: 1_060,
        },
        &signing_key,
    )
    .unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    assert_eq!(
        verify_oauth_admission(
            &bytes,
            &issuer,
            instance,
            &account,
            &ephemeral_nkey,
            1_000,
            60
        )
        .unwrap(),
        signed
    );
    let mut forged = signed.clone();
    forged.credential.principal_id = "01JY0000000000000000000005".into();
    assert!(verify_oauth_admission(
        &serde_json::to_vec(&forged).unwrap(),
        &issuer,
        instance,
        &account,
        &ephemeral_nkey,
        1_000,
        60,
    )
    .is_err());

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root
        .join(".local/protocol-interop")
        .join(ulid::Ulid::new().to_string());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("oauth-admission.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "signed": signed,
            "verification": {
                "issuer": issuer,
                "trellisInstanceId": instance,
                "audienceNatsAccount": account,
                "ephemeralNkey": ephemeral_nkey,
                "nowUnixSeconds": 1_000,
                "maximumLifetimeSeconds": 60,
            },
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new("node")
        .current_dir(&root)
        .arg("crates/protocol/tests/oauth_admission_wasm.ts")
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_dir_all(&directory).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
