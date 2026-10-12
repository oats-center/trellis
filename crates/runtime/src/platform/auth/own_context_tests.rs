use super::*;

#[test]
fn overlapping_callout_attachments_resolve_only_the_signed_exact_socket() {
    use trellis_protocol::{
        admission_get_subject, sign_admission_get, verify_admission_context, AdmissionGet,
    };
    let mut f = fixture();
    f.admission.inbox_prefix =
        trellis_protocol::admission_inbox_prefix(&f.admission.session_public_key).unwrap();
    let key = SigningKey::from_bytes(&[9; 32]);
    let nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
        .unwrap()
        .public_key();
    let admission = f
        .store
        .transaction(|connection| {
            f.store.sign_oauth_admission_in(
                connection,
                &f.admission,
                &nkey,
                &super::super::policy::evaluate(connection, &f.admission.credential, NOW)?,
                NOW,
            )
        })
        .unwrap();
    let bytes = serde_json::to_vec(&admission).unwrap();
    let (older, _) = f
        .store
        .admit_connection(&bytes, &nkey, &nkey, "broker", 1, NOW)
        .unwrap();
    let (newer, _) = f
        .store
        .admit_connection(&bytes, &nkey, &nkey, "broker", 2, NOW + 1)
        .unwrap();
    assert_eq!(
        older.authorization_session_id,
        newer.authorization_session_id
    );
    let subject = admission_get_subject(&f.admission.session_public_key).unwrap();
    let reply = format!("{}.reply", f.admission.inbox_prefix);
    let request = AdmissionGet {
        trellis_instance_id: f.store.settings.instance_id.clone(),
        runtime_id: f.admission.runtime_id.clone(),
        session_public_key: f.admission.session_public_key.clone(),
        attachment_id: older.attachment_id.clone(),
        request_nonce: URL_SAFE_NO_PAD.encode([23; 16]),
        issued_at: NOW + 2,
    };
    for attachment in [&older, &newer] {
        let request = AdmissionGet {
            attachment_id: attachment.attachment_id.clone(),
            ..request.clone()
        };
        let signed = sign_admission_get(request.clone(), &subject, &reply, &key).unwrap();
        let response = f
            .store
            .own_context(
                &serde_json::to_vec(&signed).unwrap(),
                &subject,
                &reply,
                NOW + 2,
            )
            .unwrap();
        let verified = verify_admission_context(
            &serde_json::to_vec(&response).unwrap(),
            &f.store.issuer(),
            &request,
            &subject,
            &reply,
            &f.store.settings.account,
            NOW + 2,
        )
        .unwrap();
        assert_eq!(
            verified.response.registry["attachmentId"],
            attachment.attachment_id
        );
        let other = AdmissionGet {
            attachment_id: if attachment.attachment_id == older.attachment_id {
                newer.attachment_id.clone()
            } else {
                older.attachment_id.clone()
            },
            ..request.clone()
        };
        assert!(verify_admission_context(
            &serde_json::to_vec(&response).unwrap(),
            &f.store.issuer(),
            &other,
            &subject,
            &reply,
            &f.store.settings.account,
            NOW + 2
        )
        .is_err());
        let mut tampered = signed;
        tampered.request.attachment_id = other.attachment_id;
        assert!(f
            .store
            .own_context(
                &serde_json::to_vec(&tampered).unwrap(),
                &subject,
                &reply,
                NOW + 2
            )
            .is_err());
    }
    for bad in [
        AdmissionGet {
            attachment_id: id(),
            ..request.clone()
        },
        AdmissionGet {
            runtime_id: URL_SAFE_NO_PAD.encode([55; 16]),
            ..request.clone()
        },
    ] {
        let signed = sign_admission_get(bad, &subject, &reply, &key).unwrap();
        assert!(f
            .store
            .own_context(
                &serde_json::to_vec(&signed).unwrap(),
                &subject,
                &reply,
                NOW + 2
            )
            .is_err());
    }
    let foreign = SigningKey::from_bytes(&[88; 32]);
    let foreign_public = URL_SAFE_NO_PAD.encode(foreign.verifying_key().as_bytes());
    let foreign_subject = admission_get_subject(&foreign_public).unwrap();
    let foreign_reply = format!(
        "{}.reply",
        trellis_protocol::admission_inbox_prefix(&foreign_public).unwrap()
    );
    let signed = sign_admission_get(
        AdmissionGet {
            session_public_key: foreign_public,
            ..request.clone()
        },
        &foreign_subject,
        &foreign_reply,
        &foreign,
    )
    .unwrap();
    assert!(f
        .store
        .own_context(
            &serde_json::to_vec(&signed).unwrap(),
            &foreign_subject,
            &foreign_reply,
            NOW + 2
        )
        .is_err());
    f.store
        .disconnect_attachment("broker", 1, &nkey, NOW + 3)
        .unwrap();
    let signed = sign_admission_get(request, &subject, &reply, &key).unwrap();
    assert!(f
        .store
        .own_context(
            &serde_json::to_vec(&signed).unwrap(),
            &subject,
            &reply,
            NOW + 3
        )
        .is_err());
}

#[test]
fn context_get_retires_expired_or_revoked_credentials_before_the_enforcement_worker() {
    for (revoked, family_expired) in [(false, false), (true, false), (false, true)] {
        let mut f = fixture();
        if family_expired {
            f.admission.refresh_family_id = Some(oauth_family(&f.store, &f.grant, NOW, NOW + 20));
        }
        f.admission.inbox_prefix =
            trellis_protocol::admission_inbox_prefix(&f.admission.session_public_key).unwrap();
        let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
        let session = &issued.authority.unsigned.authorization_session_id;
        let policy = f
            .store
            .transaction(|connection| {
                f.store
                    .compile_transport(connection, &issued.authority.unsigned)
            })
            .unwrap();
        let attachment = id();
        let key = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
            .unwrap()
            .public_key();
        f.store
            .admit_attachment(
                &Attachment {
                    attachment_id: attachment.clone(),
                    server_id: "broker".into(),
                    broker_client_id: 8,
                    authorization_session_id: session.clone(),
                    context_digest: issued.digest.clone(),
                    ephemeral_nkey: key.clone(),
                    broker_user_nkey: key,
                    transport_policy_digest: policy.digest().unwrap(),
                },
                NOW,
            )
            .unwrap();
        let now = if revoked {
            let scope = EnforcementScope::Grant(f.grant.clone());
            f.store
                .revoke_root(&mutation(&f.admin, &scope), &scope, NOW + 1)
                .unwrap();
            NOW + 1
        } else if family_expired {
            NOW + 20
        } else {
            NOW + 3600
        };
        let subject =
            trellis_protocol::admission_get_subject(&f.admission.session_public_key).unwrap();
        let reply = format!("{}.renewal", f.admission.inbox_prefix);
        let request = trellis_protocol::sign_admission_get(
            trellis_protocol::AdmissionGet {
                trellis_instance_id: f.store.settings.instance_id.clone(),
                runtime_id: f.admission.runtime_id.clone(),
                session_public_key: f.admission.session_public_key.clone(),
                attachment_id: attachment.clone(),
                request_nonce: URL_SAFE_NO_PAD.encode([23; 16]),
                issued_at: now,
            },
            &subject,
            &reply,
            &SigningKey::from_bytes(&[9; 32]),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(matches!(
            f.store.own_context(&bytes, &subject, &reply, now),
            Err(AuthError::Retired)
        ));
        let cutoff = f.store.resolve_revocation(session).unwrap().unwrap();
        cutoff
            .verify(
                &f.store.issuer(),
                &f.store.settings.instance_id,
                &f.store.settings.account,
            )
            .unwrap();
        assert_eq!(cutoff.effective_cutoff, now);
        if revoked {
            assert_eq!(
                f.store
                    .authorization_session_status(session.parse().unwrap())
                    .unwrap()
                    .state,
                trellis_runtime_apis::types::AuthAuthorizationSessionState::Revoked,
            );
        }
        assert_eq!(
            f.store.resolve_authority(&issued.digest).unwrap(),
            issued.authority
        );

        // Denial must commit across restart, retain immutable verification
        // evidence, and publish the cutoff before kicking the admitted socket.
        drop(f.store);
        let store = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
        assert!(store.own_context(&bytes, &subject, &reply, now).is_err());
        assert_eq!(store.resolve_revocation(session).unwrap().unwrap(), cutoff);
        assert_eq!(
            store.resolve_authority(&issued.digest).unwrap(),
            issued.authority
        );
        let effects = store.claim_effects(256, now, 10).unwrap();
        assert!(!effects.iter().any(|effect| effect.kind == "kick"));
        let revoke = effects
            .iter()
            .find(|effect| {
                effect.kind == "session_revoke"
                    && effect.payload["authorizationSessionId"] == *session
            })
            .unwrap();
        store.complete_effect(revoke, now).unwrap();
        let effects = store.claim_effects(256, now, 10).unwrap();
        let kick = effects
            .iter()
            .find(|effect| effect.kind == "kick" && effect.payload["attachmentId"] == attachment)
            .unwrap();
        store.complete_effect(kick, now).unwrap();
        while store.enforce_next_page(now).unwrap().is_some() {}
        assert!(store
            .claim_effects(256, now, 10)
            .unwrap()
            .iter()
            .all(|effect| effect.kind != "session_revoke" && effect.kind != "kick"));
    }
}
