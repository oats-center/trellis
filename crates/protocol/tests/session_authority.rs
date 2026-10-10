//! Target authority/request serialization, integrity, scope, and retirement behavior.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use serde_json::{json, Map};
use sha2::{Digest as _, Sha256};
use std::{fs, path::PathBuf, process::Command};
use trellis_protocol::{
    parse_session_authority, sign_session_authority, sign_session_event, sign_session_request,
    verify_session_authority, verify_session_event, verify_session_request, AuthorityApi,
    AuthorityIssuerKey, AuthorityIssuerState, CapabilityAuthority, OriginalEventPublication,
    PlatformPrivilege, PrincipalKind, SessionAuthorityPurpose, SessionAuthorityVerificationInput,
    SessionAuthorityVerificationPolicy, SessionBinding, SessionEvent,
    SessionEventVerificationInput, SessionRequest, SessionRequestVerificationInput,
    SignedSessionAuthority, U64s, UnsignedSessionAuthority, SESSION_AUTHORITY_FORMAT_V1,
};

fn issued() -> (
    SigningKey,
    AuthorityIssuerKey,
    SignedSessionAuthority,
    SessionAuthorityVerificationPolicy,
) {
    let issuer = SigningKey::from_bytes(&[2; 32]);
    let runtime = SigningKey::from_bytes(&[3; 32]);
    let key_id = URL_SAFE_NO_PAD.encode(Sha256::digest(issuer.verifying_key().as_bytes()));
    let pinned = AuthorityIssuerKey {
        key_id: key_id.clone(),
        public_key: URL_SAFE_NO_PAD.encode(issuer.verifying_key().as_bytes()),
        state: AuthorityIssuerState::Active,
    };
    let authority = sign_session_authority(
        UnsignedSessionAuthority {
            format: SESSION_AUTHORITY_FORMAT_V1.into(),
            issuer_key_id: key_id,
            trellis_instance_id: "01JY0000000000000000000001".into(),
            audience_nats_account: "application-account".into(),
            principal_id: "01JY0000000000000000000002".into(),
            principal_kind: PrincipalKind::User,
            binding: SessionBinding::Browser {
                client_id: "https://app.example/client.json".into(),
                origin: "https://app.example".into(),
            },
            authorization_session_id: "01JY0000000000000000000003".into(),
            runtime_id: URL_SAFE_NO_PAD.encode([4; 16]),
            session_public_key: URL_SAFE_NO_PAD.encode(runtime.verifying_key().as_bytes()),
            inbox_prefix: "_INBOX.runtime".into(),
            login_session_id: Some("01JY0000000000000000000004".into()),
            oauth_grant_id: Some("01JY0000000000000000000005".into()),
            capabilities: vec![CapabilityAuthority {
                capability_id: "documents::read".into(),
                identity_generation: U64s::new(1),
                consent_revision: U64s::new(2),
            }],
            apis: vec![AuthorityApi {
                api_id: "documents@v1".into(),
                generation: U64s::new(1),
                accepted_revision: U64s::new(3),
                catalog_snapshot_digest: URL_SAFE_NO_PAD.encode([5; 32]),
            }],
            provider_bindings: vec![],
            resource_bindings_digest: None,
            platform_privileges: vec![PlatformPrivilege::RolesManage],
            issued_at: 1_100,
            not_before: 1_100,
            expires_at: 1_300,
            extensions: Map::from_iter([("futureField".into(), json!({"nested": [true, 1]}))]),
            critical: vec![],
        },
        &issuer,
    )
    .unwrap();
    let policy = SessionAuthorityVerificationPolicy {
        now_unix_seconds: 1_110,
        allowed_clock_skew_seconds: 30,
        maximum_authority_lifetime_seconds: 300,
        maximum_authority_bytes: 16_384,
        maximum_entries: 32,
    };
    (runtime, pinned, authority, policy)
}

#[test]
fn historical_event_requires_original_publication_before_expiry_and_revocation() {
    let (runtime, issuer, authority, mut policy) = issued();
    let event = SessionEvent {
        authority_digest: authority.digest().unwrap(),
        api_id: "documents@v1".into(),
        api_generation: U64s::new(1),
        accepted_revision: U64s::new(3),
        action: "Documents.Changed".into(),
        subject: "event.documents.Changed".into(),
        event_id: "01JY0000000000000000000006".into(),
        event_time: "1970-01-01T00:18:30Z".into(),
    };
    let payload = br#"{"id":"document-1","unknown":{"retained":true}}"#;
    let proof = sign_session_event(&event, payload, &runtime).unwrap();
    policy.now_unix_seconds = 1_000_000;
    for cutoff in [None, Some(1_200)] {
        let verified = verify_session_authority(SessionAuthorityVerificationInput {
            issuer: &issuer,
            trellis_instance_id: &authority.unsigned.trellis_instance_id,
            audience_nats_account: &authority.unsigned.audience_nats_account,
            authority: &authority,
            policy: &policy,
            purpose: SessionAuthorityPurpose::HistoricalEvent,
            revocation_cutoff: cutoff,
        })
        .unwrap();
        let publication = OriginalEventPublication {
            published_at: "1970-01-01T00:19:59.999999999Z".into(),
            stream_sequence: U64s::new(17),
        };
        let check = |original_publication| {
            verify_session_event(SessionEventVerificationInput {
                authority: &verified,
                event: &event,
                raw_payload: payload,
                proof: &proof,
                policy: &policy,
                original_publication,
                known_revoked: cutoff.is_some(),
            })
        };
        let retained = check(Some(&publication)).unwrap();
        assert_eq!(retained.raw_payload(), payload);
        assert_eq!(
            retained.original_publication().unwrap().stream_sequence,
            U64s::new(17)
        );
        assert!(
            check(None).is_err(),
            "signed event time alone is not historical evidence"
        );
        let republished = OriginalEventPublication {
            published_at: if cutoff.is_some() {
                "1970-01-01T00:20:00Z".into()
            } else {
                "1970-01-01T00:21:40Z".into()
            },
            stream_sequence: U64s::new(18),
        };
        assert!(
            check(Some(&republished)).is_err(),
            "a backdated signed event cannot authenticate a new publication after the cutoff"
        );
    }
}

#[test]
fn event_proof_binds_descriptor_subject_payload_and_api_stamp() {
    let (runtime, issuer, authority, policy) = issued();
    let verified = verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &issuer,
        trellis_instance_id: &authority.unsigned.trellis_instance_id,
        audience_nats_account: &authority.unsigned.audience_nats_account,
        authority: &authority,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
    })
    .unwrap();
    let event = SessionEvent {
        authority_digest: authority.digest().unwrap(),
        api_id: "documents@v1".into(),
        api_generation: U64s::new(1),
        accepted_revision: U64s::new(3),
        action: "Documents.Changed".into(),
        subject: "event.documents.Changed".into(),
        event_id: "01JY0000000000000000000006".into(),
        event_time: "1970-01-01T00:18:30Z".into(),
    };
    let payload = br#"{"id":"document-1","unknown":{"retained":true}}"#;
    let proof = sign_session_event(&event, payload, &runtime).unwrap();
    let check = |event: &SessionEvent, raw_payload: &[u8]| {
        verify_session_event(SessionEventVerificationInput {
            authority: &verified,
            event,
            raw_payload,
            proof: &proof,
            policy: &policy,
            original_publication: None,
            known_revoked: false,
        })
        .map(|verified| verified.event().clone())
    };
    assert_eq!(check(&event, payload).unwrap(), event);
    assert!(check(
        &event,
        br#"{"id":"document-1","unknown":{"retained":false}}"#
    )
    .is_err());
    let mut changed = event.clone();
    changed.action = "Documents".into();
    assert!(check(&changed, payload).is_err());
    changed = event.clone();
    changed.subject.push_str(".other");
    assert!(check(&changed, payload).is_err());
    changed = event.clone();
    changed.accepted_revision = U64s::new(4);
    assert!(check(&changed, payload).is_err());
}

#[test]
fn signed_authority_and_exact_request_survive_serialization_but_reject_transplantation() {
    let (runtime, pinned, signed, policy) = issued();
    let wire = serde_json::to_vec(&signed).unwrap();
    let parsed = parse_session_authority(&wire, policy.maximum_authority_bytes).unwrap();
    let authority = verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &pinned,
        trellis_instance_id: &signed.unsigned.trellis_instance_id,
        audience_nats_account: "application-account",
        authority: &parsed,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
    })
    .unwrap();
    let request = SessionRequest {
        authority_digest: authority.digest().into(),
        api_id: "documents@v1".into(),
        api_generation: U64s::new(1),
        accepted_revision: U64s::new(3),
        action: "Documents.Get".into(),
        subject: "rpc.documents.Get".into(),
        reply_subject: Some("_INBOX.runtime.reply".into()),
        request_id: "01JY0000000000000000000006".into(),
        issued_at: 1_110,
    };
    let payload = br#"{"id":"document-1","unknown":{"retained":true}}"#;
    let proof = sign_session_request(&request, payload, &runtime).unwrap();
    let verify = |request: &SessionRequest, bytes: &[u8]| {
        verify_session_request(SessionRequestVerificationInput {
            authority: &authority,
            request,
            raw_payload: bytes,
            proof: &proof,
            policy: &policy,
            known_revoked: false,
        })
        .map(|verified| verified.digest().to_owned())
    };
    assert!(verify(&request, payload).is_ok());
    assert!(verify(
        &request,
        br#"{"id":"document-1","unknown":{"retained":false}}"#
    )
    .is_err());
    for mutation in 0..5 {
        let mut changed = request.clone();
        match mutation {
            0 => changed.subject.push_str(".other"),
            1 => changed.reply_subject = Some("_INBOX.runtime.other".into()),
            2 => changed.action = "Documents.Delete".into(),
            3 => changed.accepted_revision = U64s::new(4),
            _ => changed.request_id = "01JY0000000000000000000007".into(),
        }
        assert!(verify(&changed, payload).is_err());
    }
    let mut changed = parsed.clone();
    changed.unsigned.binding = SessionBinding::Browser {
        client_id: "https://other.example/client.json".into(),
        origin: "https://other.example".into(),
    };
    assert!(verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &pinned,
        trellis_instance_id: &signed.unsigned.trellis_instance_id,
        audience_nats_account: "application-account",
        authority: &changed,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
    })
    .is_err());
}

#[test]
fn authority_scope_expiry_and_logical_revocation_fail_closed() {
    let (_, pinned, signed, policy) = issued();
    let verify =
        |instance: &str, account: &str, policy: &SessionAuthorityVerificationPolicy, revoked| {
            verify_session_authority(SessionAuthorityVerificationInput {
                issuer: &pinned,
                trellis_instance_id: instance,
                audience_nats_account: account,
                authority: &signed,
                policy,
                purpose: SessionAuthorityPurpose::Live,
                revocation_cutoff: revoked,
            })
        };
    assert!(verify(
        &signed.unsigned.trellis_instance_id,
        "another-account",
        &policy,
        None
    )
    .is_err());
    assert!(verify(
        "01JY0000000000000000000009",
        "application-account",
        &policy,
        None
    )
    .is_err());
    assert!(verify(
        &signed.unsigned.trellis_instance_id,
        "application-account",
        &policy,
        Some(1_110)
    )
    .is_err());
    let verified = verify(
        &signed.unsigned.trellis_instance_id,
        "application-account",
        &policy,
        None,
    )
    .unwrap();
    assert!(verified.assert_current(&policy, true).is_err());
    let expired = SessionAuthorityVerificationPolicy {
        now_unix_seconds: 1_300,
        ..policy
    };
    assert!(verified.assert_current(&expired, false).is_err());
}

#[test]
fn native_signed_request_verifies_in_packaged_wasm() {
    let (runtime, issuer, authority, policy) = issued();
    let request = SessionRequest {
        authority_digest: authority.digest().unwrap(),
        api_id: "documents@v1".into(),
        api_generation: U64s::new(1),
        accepted_revision: U64s::new(3),
        action: "Documents.Get".into(),
        subject: "rpc.documents.Get".into(),
        reply_subject: Some("_INBOX.runtime.reply".into()),
        request_id: "01JY0000000000000000000006".into(),
        issued_at: 1_110,
    };
    let payload = r#"{"id":"document-1","unknown":{"retained":true}}"#;
    let proof = sign_session_request(&request, payload.as_bytes(), &runtime).unwrap();
    let digest =
        trellis_protocol::session_request_signing_digest(&request, payload.as_bytes()).unwrap();
    let verified_authority = verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &issuer,
        trellis_instance_id: &authority.unsigned.trellis_instance_id,
        audience_nats_account: &authority.unsigned.audience_nats_account,
        authority: &authority,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
    })
    .unwrap();
    let caller = verify_session_request(SessionRequestVerificationInput {
        authority: &verified_authority,
        request: &request,
        raw_payload: payload.as_bytes(),
        proof: &proof,
        policy: &policy,
        known_revoked: false,
    })
    .unwrap()
    .caller();
    let event = SessionEvent {
        authority_digest: authority.digest().unwrap(),
        api_id: "documents@v1".into(),
        api_generation: U64s::new(1),
        accepted_revision: U64s::new(3),
        action: "Documents.Changed".into(),
        subject: "event.documents.Changed".into(),
        event_id: "01JY0000000000000000000007".into(),
        event_time: "1970-01-01T00:18:30Z".into(),
    };
    let event_proof = sign_session_event(&event, payload.as_bytes(), &runtime).unwrap();
    let mut historical_policy = policy.clone();
    historical_policy.now_unix_seconds = 1_000_000;
    let historical_authority = verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &issuer,
        trellis_instance_id: &authority.unsigned.trellis_instance_id,
        audience_nats_account: &authority.unsigned.audience_nats_account,
        authority: &authority,
        policy: &historical_policy,
        purpose: SessionAuthorityPurpose::HistoricalEvent,
        revocation_cutoff: Some(1_200),
    })
    .unwrap();
    let publication = OriginalEventPublication {
        published_at: "1970-01-01T00:19:59.999999999Z".into(),
        stream_sequence: U64s::new(17),
    };
    let verified_event = verify_session_event(SessionEventVerificationInput {
        authority: &historical_authority,
        event: &event,
        raw_payload: payload.as_bytes(),
        proof: &event_proof,
        policy: &historical_policy,
        original_publication: Some(&publication),
        known_revoked: true,
    })
    .unwrap();
    let fixture = json!({
        "verification": {
            "issuer": issuer,
            "trellisInstanceId": authority.unsigned.trellis_instance_id,
            "audienceNatsAccount": authority.unsigned.audience_nats_account,
            "policy": policy,
            "purpose": "live",
            "revocationCutoff": null,
        },
        "authority": authority,
        "request": request,
        "proof": proof,
        "payload": payload,
        "requestProofDigest": URL_SAFE_NO_PAD.encode(digest),
        "caller": caller,
        "eventVerification": {
            "issuer": issuer,
            "trellisInstanceId": authority.unsigned.trellis_instance_id,
            "audienceNatsAccount": authority.unsigned.audience_nats_account,
            "policy": historical_policy,
            "purpose": "historicalEvent",
            "revocationCutoff": 1_200,
        },
        "event": event,
        "eventProof": event_proof,
        "eventProofDigest": URL_SAFE_NO_PAD.encode(verified_event.digest()),
        "publisherAuthority": historical_authority.authority(),
        "originalPublication": publication,
    });
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root
        .join(".local/protocol-interop")
        .join(ulid::Ulid::new().to_string());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("request.json");
    fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let output = Command::new("deno")
        .current_dir(&root)
        .args([
            "run",
            "--allow-read",
            "--config",
            "ts/deno.json",
            "crates/protocol/tests/session_authority_wasm.ts",
        ])
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
