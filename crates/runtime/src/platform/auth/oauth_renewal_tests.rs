use super::*;
use crate::platform::auth::authorization_sessions::IssuedAuthority;
use crate::platform::auth::rpc_credentials::{renew_admission, renew_authority};
use trellis_protocol::{sign_session_request, AuthenticatedCaller, SessionRequest};
use trellis_rs::client::{AuthorizationVerificationCore, RequestVerificationInput};
use trellis_runtime_apis::__types::trellis::{
    AuthAdmissionRenewRequest, AuthAuthorityRenewRequest,
};

fn caller(f: &Fixture, issued: &IssuedAuthority) -> AuthenticatedCaller {
    let issuer = f.store.issuer();
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
    let catalog = f.store.current_catalog(&f.api).unwrap();
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
    let issued = f
        .store
        .issue_authority(
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
    let peer = f.store.issue_authority(&peer, None, NOW).unwrap();
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
    use futures_util::FutureExt;
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
        let issued = f
            .store
            .issue_authority(
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
            storage
                .revoke_token_family(
                    &family,
                    oauth_as::store::RevocationWindow {
                        recorded_at: std::time::UNIX_EPOCH
                            + std::time::Duration::from_secs((NOW + 1) as u64),
                        until: std::time::UNIX_EPOCH
                            + std::time::Duration::from_secs((NOW + 3600) as u64),
                    },
                )
                .now_or_never()
                .unwrap()
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
        assert!(f
            .store
            .issue_authority(
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
