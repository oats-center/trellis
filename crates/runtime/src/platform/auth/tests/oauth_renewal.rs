use super::*;
use crate::platform::auth::authorization_sessions::IssuedAuthority;
use crate::platform::auth::rpc_credentials::{renew_admission, renew_authority};
use trellis_protocol::{sign_session_request, AuthenticatedCaller, SessionRequest};
use trellis_rs::client::{AuthorizationVerificationCore, RequestVerificationInput};
use trellis_runtime_apis::__types::trellis::{
    AuthAdmissionRenewRequest, AuthAuthorityRenewRequest,
};

fn caller(f: &Fixture, issued: &IssuedAuthority) -> AuthenticatedCaller {
    let issuer = f.store.issuer().unwrap();
    let policy = SessionAuthorityVerificationPolicy {
        now_unix_seconds: NOW,
        allowed_clock_skew_seconds: 5,
        maximum_authority_lifetime_seconds: 60,
        maximum_authority_bytes: 65536,
        maximum_entries: 256,
    };
    let authority = Arc::new(
        verify_session_authority(SessionAuthorityVerificationInput {
            issuer: &issuer,
            trellis_instance_id: &f.store.settings.instance_id,
            audience_nats_account: &f.store.settings.account,
            authority: &issued.authority,
            policy: &policy,
            purpose: SessionAuthorityPurpose::Live,
            revocation_cutoff: None,
        })
        .unwrap(),
    );
    let api = issued
        .authority
        .unsigned
        .apis
        .iter()
        .find(|api| api.api_id == f.api)
        .unwrap();
    let request = SessionRequest {
        authority_digest: issued.digest.clone(),
        api_id: api.api_id.clone(),
        api_generation: api.generation,
        accepted_revision: api.accepted_revision,
        action: "Get".into(),
        subject: "documents.Get".into(),
        reply_subject: Some(format!("{}.reply", issued.authority.unsigned.inbox_prefix)),
        request_id: id(),
        issued_at: NOW,
    };
    let payload = br#"{"id":"document"}"#;
    let proof = sign_session_request(&request, payload, &SigningKey::from_bytes(&[9; 32])).unwrap();
    let catalog = trellis_protocol::AuthenticatedCatalog::authenticate(
        f.store.current_catalog(&f.api).unwrap(),
        &issuer,
        &f.store.settings.instance_id,
        &f.store.settings.account,
    )
    .unwrap();
    AuthorizationVerificationCore::new()
        .verify_request(RequestVerificationInput {
            authority,
            request: &request,
            payload,
            proof: &proof,
            policy: &policy,
            known_revoked: false,
            catalog: &catalog,
            catalog_issuer: &issuer,
            action: &CatalogActionIdentity {
                kind: ApiSurfaceKind::Rpc,
                name: "Get".into(),
                direction: PermissionAction::Call,
            },
        })
        .unwrap()
        .caller()
        .clone()
}

#[test]
fn signed_live_renewal_keeps_exact_family_key_and_session_and_callout_accepts_it() {
    let mut f = fixture();
    f.admission.inbox_prefix =
        trellis_protocol::admission_inbox_prefix(&f.admission.session_public_key).unwrap();
    f.admission.refresh_family_id = Some(oauth_family(&f.store, &f.grant, NOW, NOW + 20));
    let nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
        .unwrap()
        .public_key();
    let signed = f
        .store
        .transaction(|connection| {
            f.store.sign_oauth_admission_in(
                connection,
                &f.admission,
                &nkey,
                &crate::platform::auth::policy::evaluate(connection, &f.admission.credential, NOW)?,
                NOW,
            )
        })
        .unwrap();
    let (attachment, _) = f
        .store
        .admit_connection(
            &serde_json::to_vec(&signed).unwrap(),
            &nkey,
            &nkey,
            "broker",
            1,
            NOW,
        )
        .unwrap();
    let issued = crate::platform::auth::tests::issue_authority(
        &f.store,
        &f.admission,
        Some(&attachment.authorization_session_id),
        NOW,
    )
    .unwrap();
    let caller = caller(&f, &issued);
    let input = AuthAdmissionRenewRequest {
        context_digest: issued.digest.clone().into(),
        extra: Default::default(),
    };
    let renewed = renew_admission(&f.store, &caller, &input, NOW + 1)
        .unwrap()
        .unwrap();
    let signed_new: trellis_protocol::SignedOAuthAdmission =
        serde_json::from_str(&renewed.admission_credential).unwrap();
    assert_eq!(
        signed_new.credential.refresh_family_id,
        signed.credential.refresh_family_id
    );
    assert_eq!(
        signed_new.credential.runtime_id,
        signed.credential.runtime_id
    );
    assert_eq!(signed_new.credential.ephemeral_nkey, nkey);
    assert_eq!(signed_new.credential.expires_at, NOW + 20);
    assert!(signed_new.credential.issued_at > signed.credential.issued_at);
    let (reconnected, _) = f
        .store
        .admit_connection(
            renewed.admission_credential.as_bytes(),
            &nkey,
            &nkey,
            "broker",
            2,
            NOW + 1,
        )
        .unwrap();
    assert_eq!(
        reconnected.authorization_session_id,
        attachment.authorization_session_id
    );
    let authority = renew_authority(
        &f.store,
        &caller,
        &AuthAuthorityRenewRequest {
            context_digest: issued.digest.clone().into(),
            extra: Default::default(),
        },
        NOW + 2,
    )
    .unwrap();
    let authority =
        trellis_protocol::parse_session_authority(&authority.authority.0, 65536).unwrap();
    assert_eq!(authority.unsigned.expires_at, NOW + 20);
    // Even another live context using the same public key does not own this digest.
    let mut peer = f.admission.clone();
    peer.runtime_id = URL_SAFE_NO_PAD.encode([71; 16]);
    let peer = crate::platform::auth::tests::issue_authority(&f.store, &peer, None, NOW).unwrap();
    let peer_input = AuthAdmissionRenewRequest {
        context_digest: peer.digest.into(),
        extra: Default::default(),
    };
    assert!(matches!(
        renew_admission(&f.store, &caller, &peer_input, NOW + 2),
        Err(AuthError::Denied)
    ));
    // A second valid family for the same grant cannot rebind the active session.
    let mut replacement = f.admission.clone();
    replacement.refresh_family_id = Some(oauth_family(&f.store, &f.grant, NOW, NOW + 3600));
    let replacement = f
        .store
        .transaction(|connection| {
            f.store.sign_oauth_admission_in(
                connection,
                &replacement,
                &nkey,
                &crate::platform::auth::policy::evaluate(connection, &replacement.credential, NOW)?,
                NOW,
            )
        })
        .unwrap();
    assert!(matches!(
        f.store.admit_connection(
            &serde_json::to_vec(&replacement).unwrap(),
            &nkey,
            &nkey,
            "broker",
            3,
            NOW + 2
        ),
        Err(AuthError::Denied)
    ));
}

#[test]
fn exact_family_expiry_or_revocation_retires_without_using_another_live_family() {
    use oauth_as::store::Storage;
    for revoked in [false, true] {
        let mut f = fixture();
        f.admission.inbox_prefix =
            trellis_protocol::admission_inbox_prefix(&f.admission.session_public_key).unwrap();
        let family = oauth_family(&f.store, &f.grant, NOW, NOW + 20);
        f.admission.refresh_family_id = Some(family.clone());
        let nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
            .unwrap()
            .public_key();
        let signed = f
            .store
            .transaction(|connection| {
                f.store.sign_oauth_admission_in(
                    connection,
                    &f.admission,
                    &nkey,
                    &crate::platform::auth::policy::evaluate(
                        connection,
                        &f.admission.credential,
                        NOW,
                    )?,
                    NOW,
                )
            })
            .unwrap();
        let (attachment, _) = f
            .store
            .admit_connection(
                &serde_json::to_vec(&signed).unwrap(),
                &nkey,
                &nkey,
                "broker",
                1,
                NOW,
            )
            .unwrap();
        let issued = crate::platform::auth::tests::issue_authority(
            &f.store,
            &f.admission,
            Some(&attachment.authorization_session_id),
            NOW,
        )
        .unwrap();
        let caller = caller(&f, &issued);
        let other = oauth_family(&f.store, &f.grant, NOW, NOW + 3600);
        let now = if revoked {
            let storage = crate::platform::auth::oauth::OAuthStorage::new(
                Arc::new(open(&f.directory, SigningKey::from_bytes(&[42; 32]))),
                &[17; 32],
            )
            .unwrap();
            ready(storage.revoke_token_family(
                &family,
                oauth_as::store::RevocationWindow {
                    recorded_at: std::time::UNIX_EPOCH
                        + std::time::Duration::from_secs((NOW + 1) as u64),
                    until: std::time::UNIX_EPOCH
                        + std::time::Duration::from_secs((NOW + 3600) as u64),
                },
            ))
            .unwrap();
            NOW + 1
        } else {
            NOW + 20
        };
        assert!(f
            .store
            .transaction(|connection| f
                .store
                .oauth_family_deadline_in(connection, &f.grant, &other, now))
            .is_ok());
        assert!(matches!(
            renew_admission(
                &f.store,
                &caller,
                &AuthAdmissionRenewRequest {
                    context_digest: issued.digest.into(),
                    extra: Default::default()
                },
                now
            ),
            Err(AuthError::Retired)
        ));
        let cutoff = f
            .store
            .resolve_revocation(&attachment.authorization_session_id)
            .unwrap()
            .unwrap();
        assert_eq!(cutoff.effective_cutoff, now);
        assert!(crate::platform::auth::tests::issue_authority(
            &f.store,
            &f.admission,
            Some(&attachment.authorization_session_id),
            now
        )
        .is_err());
        assert!(f
            .store
            .admit_connection(
                &serde_json::to_vec(&signed).unwrap(),
                &nkey,
                &nkey,
                "broker",
                2,
                now
            )
            .is_err());
        for effect in f.store.claim_effects(256, now, 30, false).unwrap() {
            f.store.complete_effect(&effect, now).unwrap();
        }
        let effects = f.store.claim_effects(256, now, 30, false).unwrap();
        assert!(effects.iter().any(|effect| effect.kind == "kick"
            && effect.payload["attachmentId"] == attachment.attachment_id));
    }
}

#[tokio::test]
async fn oauth_sqlite_restart_retains_refresh_rotation_and_reuse_revokes_authority() {
    use oauth_as::authorization::AuthorizationRequest;
    use oauth_as::server::{AuthorizationServer, ServerConfig, TokenRequest, UserApproval};
    use oauth_as::store::Storage;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    let f = fixture();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let grant = approve(&f.store, &f.principal, &f.read, &f.write, now);
    let key_directory = tempfile::tempdir().unwrap();
    trellis_local_bootstrap::generate_local_oauth_storage_key(key_directory.path()).unwrap();
    let sealing_key_file = key_directory.path().join("oauth-storage.key");
    let mut config = ServerConfig::new("https://auth.example", "https://auth.example/oauth/device");
    config.refresh_token_ttl = Some(std::time::Duration::from_secs(3600));
    let engine = AuthorizationServer::new(
        config.clone(),
        super::super::oauth::OAuthStorage::open(f.store, &sealing_key_file).unwrap(),
    );
    let verifier = URL_SAFE_NO_PAD.encode([31; 32]);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let request = AuthorizationRequest::from_pairs([
        ("client_id", "native-client"),
        ("response_type", "code"),
        ("redirect_uri", "http://127.0.0.1:9123/callback"),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge.as_str()),
    ]);
    let request = engine
        .validate_authorization_request(&request)
        .await
        .unwrap();
    let authorization = engine
        .issue_authorization_code(UserApproval::granted(&request, &grant))
        .await
        .unwrap();
    let issued = engine
        .token(TokenRequest::AuthorizationCode {
            client_id: oauth_as::ClientId::new("native-client"),
            client_secret: None,
            code: authorization.code,
            redirect_uri: Some("http://127.0.0.1:9123/callback".into()),
            code_verifier: Some(verifier),
        })
        .await
        .unwrap();
    let refresh = issued.refresh_token.unwrap();
    drop(engine);

    // Startup must not begin writing a second set of ciphertext after key loss
    // or replacement. Check against the OAuth records created by redemption.
    let authority = Arc::new(open(&f.directory, SigningKey::from_bytes(&[42; 32])));
    let key_bytes = std::fs::read(&sealing_key_file).unwrap();
    assert!(
        trellis_local_bootstrap::generate_local_oauth_storage_key(key_directory.path()).is_err()
    );
    std::fs::write(&sealing_key_file, URL_SAFE_NO_PAD.encode([84; 32])).unwrap();
    assert!(super::super::oauth::OAuthStorage::open(authority.clone(), &sealing_key_file).is_err());
    std::fs::write(&sealing_key_file, "invalid key").unwrap();
    assert!(super::super::oauth::OAuthStorage::open(authority.clone(), &sealing_key_file).is_err());
    std::fs::remove_file(&sealing_key_file).unwrap();
    assert!(super::super::oauth::OAuthStorage::open(authority.clone(), &sealing_key_file).is_err());
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&sealing_key_file)
            .unwrap();
        file.write_all(&key_bytes).unwrap();
        drop(file);
        std::fs::set_permissions(&sealing_key_file, std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(
            super::super::oauth::OAuthStorage::open(authority.clone(), &sealing_key_file).is_err()
        );
        std::fs::set_permissions(&sealing_key_file, std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let link = key_directory.path().join("linked.key");
        std::os::unix::fs::symlink(&sealing_key_file, &link).unwrap();
        assert!(super::super::oauth::OAuthStorage::open(authority.clone(), &link).is_err());
        let alias = key_directory.path().join("hardlinked.key");
        std::fs::hard_link(&sealing_key_file, &alias).unwrap();
        assert!(
            super::super::oauth::OAuthStorage::open(authority.clone(), &sealing_key_file).is_err()
        );
        std::fs::remove_file(alias).unwrap();
        let mut oversized = key_bytes.clone();
        oversized.extend(vec![b' '; 256]);
        std::fs::write(&sealing_key_file, oversized).unwrap();
        assert!(
            super::super::oauth::OAuthStorage::open(authority.clone(), &sealing_key_file).is_err()
        );
        std::fs::write(&sealing_key_file, &key_bytes).unwrap();
    }
    #[cfg(not(unix))]
    std::fs::write(&sealing_key_file, &key_bytes).unwrap();
    drop(authority);

    // Reopen the real, migrated database through the production constructor.
    // There is no memory mirror or restored test-only OAuth state.
    let engine = AuthorizationServer::new(
        config,
        super::super::oauth::OAuthStorage::open(
            Arc::new(open(&f.directory, SigningKey::from_bytes(&[42; 32]))),
            &sealing_key_file,
        )
        .unwrap(),
    );
    let rotated = engine
        .token(TokenRequest::RefreshToken {
            client_id: oauth_as::ClientId::new("native-client"),
            client_secret: None,
            refresh_token: refresh.clone(),
            scope: None,
        })
        .await
        .unwrap();
    let mut admission = f.admission;
    admission.refresh_family_id = Some(
        engine
            .store()
            .get_refresh_token(rotated.refresh_token.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap()
            .family_id
            .clone(),
    );
    admission.credential = Credential::OAuthGrant(grant);
    issue_authority(&engine.store().authority, &admission, None, now).unwrap();
    assert!(engine
        .token(TokenRequest::RefreshToken {
            client_id: oauth_as::ClientId::new("native-client"),
            client_secret: None,
            refresh_token: refresh,
            scope: None,
        })
        .await
        .is_err());
    assert!(engine
        .store()
        .get_token(&rotated.access_token)
        .await
        .unwrap()
        .is_none());
    assert!(engine
        .store()
        .get_refresh_token(&rotated.refresh_token.unwrap())
        .await
        .unwrap()
        .is_none());
    assert!(issue_authority(&engine.store().authority, &admission, None, now).is_err());
}
