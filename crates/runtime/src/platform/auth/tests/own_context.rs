use super::*;

#[test]
fn oversized_provider_authority_is_rejected_during_admission() {
    let f = fixture();
    let mut source = String::from("model Document { id: string; }\n");
    for index in 0..200 {
        source.push_str(&format!(
            r#"
api a{index:06}@v1 {{
 title "Documents";
 description "Document management.";
 rpc Get {{ input Document; output Document; }}
 capabilities {{
  capability documents.read {{ title "Read"; description "Read documents."; consequence "Sees contents."; consent_revision 1; allows {{ rpc Get; }} }}
 }}
}}
"#
        ));
    }
    source.push_str("service LargeProvider {\n");
    for index in 0..200 {
        source.push_str(&format!("implements a{index:06};\n"));
    }
    source.push('}');
    let proposed = evidence(&source);
    let graph = trellis_idl::compile_evidence(proposed.clone()).unwrap();
    let mut expected_api_generations = BTreeMap::new();
    for api in graph.root_package().apis().keys() {
        let review = f
            .store
            .review_api(&f.admin, proposed.clone(), &api.to_string(), NOW)
            .unwrap();
        let acceptance = ApiAcceptance {
            review_id: review.review_id,
            reviewed_digest: review.definition_digest,
            force: false,
            acknowledge_breakage: false,
            confirm_consent_meanings: false,
            materially_changed: review.consent_changes,
            policy_edits: vec![],
        };
        let accepted = f
            .store
            .accept_api(&mutation(&f.admin, &acceptance), &acceptance, NOW)
            .unwrap();
        expected_api_generations.insert(accepted.api_id, accepted.generation.get());
    }
    let identity = SigningKey::from_bytes(&[11; 32]);
    let provisioning = DeploymentProvisioning {
        evidence: proposed,
        participant_id: graph
            .root_package()
            .participants()
            .keys()
            .next()
            .unwrap()
            .to_string(),
        expected_api_generations,
        identity_public_key: URL_SAFE_NO_PAD.encode(identity.verifying_key().as_bytes()),
        resource_commitments: serde_json::json!({}),
        expires_at: None,
    };
    let provisioned = f
        .store
        .provision_deployment(&mutation(&f.admin, &provisioning), &provisioning, NOW)
        .unwrap();
    let ephemeral = nkeys::KeyPair::new_user();
    let credential = trellis_protocol::sign_provisioned_admission(
        trellis_protocol::ProvisionedAdmission {
            format: trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1.into(),
            trellis_instance_id: f.store.settings.instance_id.clone(),
            audience_nats_account: f.store.settings.account.clone(),
            credential_id: provisioned.identity_key_id,
            runtime_id: URL_SAFE_NO_PAD.encode([17; 16]),
            ephemeral_nkey: ephemeral.public_key(),
            issued_at: NOW,
            expires_at: NOW + 60,
        },
        &identity,
    )
    .unwrap();
    let admitted = f.store.admit_connection(
        &serde_json::to_vec(&credential).unwrap(),
        &ephemeral.public_key(),
        &ephemeral.public_key(),
        "broker",
        1,
        NOW,
    );
    assert!(
        matches!(
            &admitted,
            Err(AuthError::Protocol(
                trellis_protocol::ProtocolError::Authorization {
                    code: trellis_protocol::AuthorizationErrorCode::ContextTooLarge,
                    ..
                }
            ))
        ),
        "oversized authority must fail at admission, not first-context retrieval: {admitted:?}"
    );
}

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
            &f.store.issuer().unwrap(),
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
            &f.store.issuer().unwrap(),
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
        let issued =
            crate::platform::auth::tests::issue_authority(&f.store, &f.admission, None, NOW)
                .unwrap();
        let session = &issued.authority.unsigned.authorization_session_id;
        let key = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
            .unwrap()
            .public_key();
        let admission = f
            .store
            .transaction(|connection| {
                f.store.sign_oauth_admission_in(
                    connection,
                    &f.admission,
                    &key,
                    &super::super::policy::evaluate(connection, &f.admission.credential, NOW)?,
                    NOW,
                )
            })
            .unwrap();
        let (attachment, _) = f
            .store
            .admit_connection(
                &serde_json::to_vec(&admission).unwrap(),
                &key,
                &key,
                "broker",
                8,
                NOW,
            )
            .unwrap();
        assert_eq!(&attachment.authorization_session_id, session);
        let attachment = attachment.attachment_id;
        let now = if revoked {
            let scope = EnforcementScope::Grant(f.grant.clone());
            crate::platform::auth::tests::revoke_root(
                &f.store,
                &mutation(&f.admin, &scope),
                &scope,
                NOW + 1,
            )
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
                &f.store.issuer().unwrap(),
                &f.store.settings.instance_id,
                &f.store.settings.account,
            )
            .unwrap();
        assert_eq!(cutoff.effective_cutoff, now);
        if revoked {
            assert_eq!(
                crate::platform::auth::tests::session_status(&f.store, session).state,
                trellis_protocol::AuthorizationSessionState::Revoked,
            );
        }
        assert_eq!(
            crate::platform::auth::tests::resolve_authority(&f.store, &issued.digest).unwrap(),
            issued.authority
        );

        // Denial must commit across restart, retain immutable verification
        // evidence, and publish the cutoff before kicking the admitted socket.
        drop(f.store);
        let store = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
        assert!(store.own_context(&bytes, &subject, &reply, now).is_err());
        assert_eq!(store.resolve_revocation(session).unwrap().unwrap(), cutoff);
        assert_eq!(
            crate::platform::auth::tests::resolve_authority(&store, &issued.digest).unwrap(),
            issued.authority
        );
        let effects = store.claim_effects(256, now, 10, false).unwrap();
        assert!(!effects.iter().any(|effect| effect.kind == "kick"));
        let revoke = effects
            .iter()
            .find(|effect| {
                effect.kind == "session_revoke"
                    && effect.payload["authorizationSessionId"] == *session
            })
            .unwrap();
        store.complete_effect(revoke, now).unwrap();
        let effects = store.claim_effects(256, now, 10, false).unwrap();
        let kick = effects
            .iter()
            .find(|effect| effect.kind == "kick" && effect.payload["attachmentId"] == attachment)
            .unwrap();
        store.complete_effect(kick, now).unwrap();
        while store.enforce_next_page(now).unwrap().is_some() {}
        assert!(store
            .claim_effects(256, now, 10, false)
            .unwrap()
            .iter()
            .all(|effect| effect.kind != "session_revoke" && effect.kind != "kick"));
    }
}

#[test]
fn own_context_requires_an_admitted_runtime_and_binds_the_exact_reply_before_policy_recheck() {
    use trellis_protocol::{
        admission_get_subject, sign_admission_get, verify_admission_context, AdmissionGet,
    };
    let mut f = fixture();
    f.admission.inbox_prefix =
        trellis_protocol::admission_inbox_prefix(&f.admission.session_public_key).unwrap();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let subject = admission_get_subject(&f.admission.session_public_key).unwrap();
    let reply = format!("{}.initial", f.admission.inbox_prefix);
    let attachment_id = id();
    let request = AdmissionGet {
        trellis_instance_id: f.store.settings.instance_id.clone(),
        runtime_id: f.admission.runtime_id.clone(),
        session_public_key: f.admission.session_public_key.clone(),
        attachment_id: attachment_id.clone(),
        request_nonce: URL_SAFE_NO_PAD.encode([23; 16]),
        issued_at: NOW,
    };
    let signed = sign_admission_get(
        request.clone(),
        &subject,
        &reply,
        &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    assert!(f.store.own_context(&bytes, &subject, &reply, NOW).is_err());
    let attachment_id = {
        let nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
            .unwrap()
            .public_key();
        f.store.admit_connection(
            &fixture_admission(&f, &nkey, NOW),
            &nkey,
            &nkey,
            "broker",
            1,
            NOW,
        )
    }
    .unwrap()
    .0
    .attachment_id;
    let request = AdmissionGet {
        attachment_id,
        ..request
    };
    let signed = sign_admission_get(
        request.clone(),
        &subject,
        &reply,
        &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    assert!(f
        .store
        .own_context(&bytes, &subject, "_INBOX.attacker.reply", NOW)
        .is_err());
    let response = f.store.own_context(&bytes, &subject, &reply, NOW).unwrap();
    let encoded = serde_json::to_vec(&response).unwrap();
    let verified = verify_admission_context(
        &encoded,
        &f.store.issuer().unwrap(),
        &request,
        &subject,
        &reply,
        &f.store.settings.account,
        NOW,
    )
    .unwrap();
    let context = trellis_protocol::parse_session_authority(
        verified.response.session_authority.as_bytes(),
        64 * 1024,
    )
    .unwrap();
    assert_eq!(context.digest().unwrap(), issued.digest);
    assert!(verify_admission_context(
        &encoded,
        &f.store.issuer().unwrap(),
        &request,
        &subject,
        "_INBOX.attacker.reply",
        &f.store.settings.account,
        NOW
    )
    .is_err());
    let mut other_nonce = request.clone();
    other_nonce.request_nonce = URL_SAFE_NO_PAD.encode([24; 16]);
    assert!(verify_admission_context(
        &encoded,
        &f.store.issuer().unwrap(),
        &other_nonce,
        &subject,
        &reply,
        &f.store.settings.account,
        NOW
    )
    .is_err());
    assert!(verify_admission_context(
        &encoded,
        &f.store.issuer().unwrap(),
        &request,
        &subject,
        &reply,
        "another-account",
        NOW
    )
    .is_err());
    let mut forged = response;
    forged.response.session_authority.push(' ');
    assert!(verify_admission_context(
        &serde_json::to_vec(&forged).unwrap(),
        &f.store.issuer().unwrap(),
        &request,
        &subject,
        &reply,
        &f.store.settings.account,
        NOW
    )
    .is_err());
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityRevoke {
            principal_id: f.principal.clone(),
            capability_id: f.write.clone(),
            expected_revision: 1,
        },
        NOW + 1,
    );
    assert!(matches!(
        f.store.own_context(&bytes, &subject, &reply, NOW + 1),
        Err(AuthError::Retired)
    ));
    assert!(f
        .store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn own_context_errors_never_publish_to_another_keys_reply_namespace() {
    use futures_util::StreamExt as _;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::AsyncBufReadExt as _;
    use trellis_protocol::{
        admission_get_subject, admission_inbox_prefix, sign_admission_get, AdmissionGet,
    };

    let f = fixture();
    let instance_id = f.store.settings.instance_id.clone();
    let jetstream_directory = f.directory.path().join("nats");
    let binary =
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
    let mut server = tokio::process::Command::new(binary)
        .args(["-a", "127.0.0.1", "-p", "-1", "-js", "-sd"])
        .arg(&jetstream_directory)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("set TRELLIS_NATS_SERVER_BIN to the producer-built NATS executable");
    let mut lines = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
    let address = tokio::time::timeout(Duration::from_secs(5), async {
        let mut address = None;
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some((_, value)) = line.split_once("Listening for client connections on ") {
                address = Some(value.to_owned());
            }
            if line.contains("Server is ready") {
                return address.expect("NATS listen diagnostic");
            }
        }
        panic!("NATS exited before readiness");
    })
    .await
    .expect("NATS readiness deadline");
    let client = async_nats::connect(format!("nats://{address}"))
        .await
        .unwrap();
    let material = async_nats::jetstream::new(client.clone())
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: super::super::broker::MATERIAL_BUCKET.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let service =
        super::super::own_context::OwnContextService::start(client.clone(), material, f.store)
            .await
            .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(service.run(async move {
        let _ = stopped.await;
    }));
    let signer = SigningKey::from_bytes(&[88; 32]);
    let public = URL_SAFE_NO_PAD.encode(signer.verifying_key().as_bytes());
    let subject = admission_get_subject(&public).unwrap();
    let reply = format!("{}.reply", admission_inbox_prefix(&public).unwrap());
    let foreign = format!(
        "{}.reply",
        admission_inbox_prefix(
            &URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[89; 32]).verifying_key().as_bytes())
        )
        .unwrap()
    );
    let mut foreign_messages = client.subscribe(foreign.clone()).await.unwrap();
    let mut own_messages = client.subscribe(reply.clone()).await.unwrap();
    client.flush().await.unwrap();
    let request = sign_admission_get(
        AdmissionGet {
            trellis_instance_id: instance_id,
            runtime_id: URL_SAFE_NO_PAD.encode([88; 16]),
            session_public_key: public,
            attachment_id: id(),
            request_nonce: URL_SAFE_NO_PAD.encode([89; 16]),
            issued_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        },
        &subject,
        &reply,
        &signer,
    )
    .unwrap();
    let bytes = serde_json::to_vec(&request).unwrap();
    client
        .publish_with_reply(subject.clone(), foreign.clone(), bytes.clone().into())
        .await
        .unwrap();
    client
        .publish_with_reply(subject, reply, bytes.into())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), own_messages.next())
            .await
            .is_err()
    );
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    // Drain is a real responder completion barrier. The marker uses the exact
    // foreign subject and same publisher, never cross-subscription ordering or sleep.
    client.publish(foreign, "completed".into()).await.unwrap();
    client.flush().await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), foreign_messages.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.payload.as_ref(), b"completed");
    server.kill().await.unwrap();
    server.wait().await.unwrap();
}
