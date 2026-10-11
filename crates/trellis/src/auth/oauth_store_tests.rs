use super::*;
use p256::ecdsa::{signature::Verifier, VerifyingKey};

#[test]
fn native_custody_preserves_key_possession_and_recovers_instead_of_replaying_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("oauth");
    let scope = NativeCredentialScope {
        issuer: "https://oauth.example".into(),
        instance_id: "one".into(),
        account: "public".into(),
        client_id: "native-cli".into(),
        credential_handle: ulid::Ulid::new(),
    };
    let mut first = NativeCredentialLease::lock(&root, scope.clone()).unwrap();
    let jwk = first.public_jwk();
    let mut point = vec![4];
    point.extend(URL_SAFE_NO_PAD.decode(jwk["x"].as_str().unwrap()).unwrap());
    point.extend(URL_SAFE_NO_PAD.decode(jwk["y"].as_str().unwrap()).unwrap());
    let verifier = VerifyingKey::from_sec1_bytes(&point).unwrap();
    first
        .install(NativeOAuthGrant {
            principal_id: "user-one".into(),
            oauth_grant_id: "grant".into(),
            login_session_id: "login".into(),
            refresh_token: "rotating-one".into(),
        })
        .unwrap();
    let competing = fs::File::open(first.path.with_extension("lock")).unwrap();
    assert!(competing.try_lock().is_err());
    assert_eq!(first.begin_rotation().unwrap(), "rotating-one");
    drop(first);
    competing.try_lock().unwrap();
    competing.unlock().unwrap();
    let mut reopened = NativeCredentialLease::lock(&root, scope).unwrap();
    let payload = b"new runtime proof signed by the persisted native DPoP identity";
    let signature = Signature::from_slice(&reopened.sign_dpop(payload)).unwrap();
    verifier.verify(payload, &signature).unwrap();
    assert!(reopened.begin_rotation().is_err());
    assert!(reopened
        .install(NativeOAuthGrant {
            principal_id: "other-user".into(),
            oauth_grant_id: "other-grant".into(),
            login_session_id: "other-login".into(),
            refresh_token: "other-secret".into()
        })
        .is_err());
    reopened
        .install(NativeOAuthGrant {
            principal_id: "user-one".into(),
            oauth_grant_id: "new-grant".into(),
            login_session_id: "new-login".into(),
            refresh_token: "rotating-two".into(),
        })
        .unwrap();
    assert_eq!(reopened.begin_rotation().unwrap(), "rotating-two");
}

#[test]
fn native_custody_refuses_unsafe_existing_directory_without_chmod_or_overwrite() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("shared");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("user-data"), b"retained").unwrap();
    let scope = NativeCredentialScope {
        issuer: "https://oauth.example".into(),
        instance_id: "one".into(),
        account: "public".into(),
        client_id: "native-cli".into(),
        credential_handle: ulid::Ulid::new(),
    };
    assert!(NativeCredentialLease::lock(&root, scope).is_err());
    assert_eq!(fs::read(root.join("user-data")).unwrap(), b"retained");
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o755
    );
}
