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
fn authority_budget_bounds_issuance_and_permissive_readers() {
    let (_, pinned, authority, mut policy) = issued();
    let issuer = SigningKey::from_bytes(&[2; 32]);
    let mut body = authority.unsigned;
    body.extensions.insert(
        "payload".into(),
        json!("x".repeat(trellis_protocol::MAX_SESSION_AUTHORITY_BYTES / 2)),
    );
    let signed = sign_session_authority(body.clone(), &issuer).unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    policy.maximum_authority_bytes = trellis_protocol::MAX_SESSION_AUTHORITY_BYTES * 4;
    let parsed = parse_session_authority(&bytes, policy.maximum_authority_bytes).unwrap();
    verify_session_authority(SessionAuthorityVerificationInput {
        authority: &parsed,
        issuer: &pinned,
        trellis_instance_id: &parsed.unsigned.trellis_instance_id,
        audience_nats_account: &parsed.unsigned.audience_nats_account,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
        policy: &policy,
    })
    .unwrap();
    body.extensions.insert(
        "payload".into(),
        json!("x".repeat(trellis_protocol::MAX_SESSION_AUTHORITY_BYTES)),
    );
    assert!(sign_session_authority(body, &issuer).is_err());
    let mut padded = bytes;
    padded.resize(trellis_protocol::MAX_SESSION_AUTHORITY_BYTES + 1, b' ');
    assert!(parse_session_authority(&padded, policy.maximum_authority_bytes).is_err());
}

#[test]
fn provider_publication_requires_exact_signed_implementation_and_never_grants_caller_rights() {
    use trellis_protocol::{
        ApiSurfaceKind, AuthorityProvider, CatalogAction, CatalogActionIdentity, PermissionAction,
        SignedCatalogSnapshot, SignedProviderCertificate, CATALOG_SNAPSHOT_FORMAT_V1,
        PROVIDER_CERTIFICATE_FORMAT_V1,
    };
    let (_, pinned, initial, policy) = issued();
    let issuer = SigningKey::from_bytes(&[2; 32]);
    let publish = CatalogActionIdentity {
        kind: ApiSurfaceKind::Event,
        name: "Changed".into(),
        direction: PermissionAction::Publish,
    };
    let consume = CatalogActionIdentity {
        direction: PermissionAction::Subscribe,
        ..publish.clone()
    };
    let call = CatalogActionIdentity {
        kind: ApiSurfaceKind::Rpc,
        name: "Get".into(),
        direction: PermissionAction::Call,
    };
    let catalog = SignedCatalogSnapshot {
        format: CATALOG_SNAPSHOT_FORMAT_V1.into(),
        issuer_key_id: pinned.key_id.clone(),
        trellis_instance_id: initial.unsigned.trellis_instance_id.clone(),
        audience_nats_account: initial.unsigned.audience_nats_account.clone(),
        api_id: "documents@v1".into(),
        generation: U64s::new(1),
        accepted_revision: U64s::new(3),
        definition_digest: URL_SAFE_NO_PAD.encode([9; 32]),
        actions: [publish.clone(), consume.clone(), call.clone()]
            .into_iter()
            .map(|identity| CatalogAction {
                identity,
                introduced_revision: U64s::new(3),
            })
            .collect(),
        capabilities: vec![],
        issued_at: 1_100,
        extensions: Map::new(),
        critical: vec![],
        signature: String::new(),
    }
    .sign(&issuer)
    .unwrap();
    catalog
        .verify(
            &pinned,
            &initial.unsigned.trellis_instance_id,
            &initial.unsigned.audience_nats_account,
        )
        .unwrap();
    let certificate = SignedProviderCertificate {
        format: PROVIDER_CERTIFICATE_FORMAT_V1.into(),
        issuer_key_id: pinned.key_id.clone(),
        trellis_instance_id: initial.unsigned.trellis_instance_id.clone(),
        audience_nats_account: initial.unsigned.audience_nats_account.clone(),
        principal_id: initial.unsigned.principal_id.clone(),
        deployment_id: "01JY0000000000000000000006".into(),
        instance_id: "01JY0000000000000000000007".into(),
        session_public_key: initial.unsigned.session_public_key.clone(),
        api_id: catalog.api_id.clone(),
        generation: catalog.generation,
        implementation_digest: URL_SAFE_NO_PAD.encode([8; 32]),
        implemented_actions: vec![publish.clone(), consume.clone(), call.clone()],
        issued_at: 1_100,
        not_before: 1_100,
        expires_at: 1_300,
        extensions: Map::new(),
        critical: vec![],
        signature: String::new(),
    }
    .sign(&issuer)
    .unwrap();
    let mut body = initial.unsigned;
    body.principal_kind = PrincipalKind::Service;
    body.binding = SessionBinding::Service {
        deployment_id: certificate.deployment_id.clone(),
        instance_id: certificate.instance_id.clone(),
        participant_id: "documents.service".into(),
    };
    body.login_session_id = None;
    body.oauth_grant_id = None;
    body.capabilities.clear();
    body.platform_privileges.clear();
    body.apis[0].catalog_snapshot_digest = catalog.digest().unwrap();
    body.provider_bindings = vec![AuthorityProvider {
        api_id: certificate.api_id.clone(),
        generation: certificate.generation,
        deployment_id: certificate.deployment_id.clone(),
        implementation_digest: certificate.implementation_digest.clone(),
        provider_certificate_digest: certificate.digest().unwrap(),
    }];
    let authority = sign_session_authority(body.clone(), &issuer).unwrap();
    verify_session_authority(SessionAuthorityVerificationInput {
        authority: &authority,
        issuer: &pinned,
        trellis_instance_id: &body.trellis_instance_id,
        audience_nats_account: &body.audience_nats_account,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
    })
    .unwrap();
    assert!(certificate
        .authorizes_event_publication(&pinned, &authority, &catalog, &publish, 1_110)
        .unwrap());
    assert!(!catalog.authorizes(&authority, &publish));
    for action in [&consume, &call] {
        assert!(!certificate
            .authorizes_event_publication(&pinned, &authority, &catalog, action, 1_110)
            .unwrap());
    }
    let mut forged = certificate.clone();
    forged.instance_id = "01JY0000000000000000000008".into();
    assert!(forged
        .authorizes_event_publication(&pinned, &authority, &catalog, &publish, 1_110)
        .is_err());
    let replaced = forged.sign(&issuer).unwrap();
    assert!(!replaced
        .authorizes_event_publication(&pinned, &authority, &catalog, &publish, 1_110)
        .unwrap());
    for field in [
        "instance",
        "principal",
        "deployment",
        "key",
        "implementation",
        "digest",
        "generation",
        "revision",
    ] {
        let mut altered = body.clone();
        match field {
            "instance" => {
                if let SessionBinding::Service { instance_id, .. } = &mut altered.binding {
                    *instance_id = "01JY0000000000000000000008".into();
                }
            }
            "principal" => altered.principal_id = "01JY0000000000000000000008".into(),
            "deployment" => {
                if let SessionBinding::Service { deployment_id, .. } = &mut altered.binding {
                    *deployment_id = "01JY0000000000000000000008".into();
                }
                altered.provider_bindings[0].deployment_id = "01JY0000000000000000000008".into();
            }
            "key" => {
                altered.session_public_key = URL_SAFE_NO_PAD
                    .encode(SigningKey::from_bytes(&[12; 32]).verifying_key().as_bytes())
            }
            "implementation" => {
                altered.provider_bindings[0].implementation_digest = URL_SAFE_NO_PAD.encode([7; 32])
            }
            "digest" => {
                altered.provider_bindings[0].provider_certificate_digest =
                    URL_SAFE_NO_PAD.encode([7; 32])
            }
            "generation" => {
                altered.apis[0].generation = U64s::new(2);
                altered.provider_bindings[0].generation = U64s::new(2);
            }
            "revision" => altered.apis[0].accepted_revision = U64s::new(2),
            _ => unreachable!(),
        }
        let altered = sign_session_authority(altered, &issuer).unwrap();
        assert!(
            !certificate
                .authorizes_event_publication(&pinned, &altered, &catalog, &publish, 1_110)
                .unwrap(),
            "{field}"
        );
    }
    let mut subset = certificate.clone();
    subset
        .implemented_actions
        .retain(|action| action != &publish);
    let subset = subset.sign(&issuer).unwrap();
    let mut narrowed = body.clone();
    narrowed.provider_bindings[0].provider_certificate_digest = subset.digest().unwrap();
    let narrowed = sign_session_authority(narrowed, &issuer).unwrap();
    assert!(!subset
        .authorizes_event_publication(&pinned, &narrowed, &catalog, &publish, 1_110)
        .unwrap());
    // A retained event is checked at original publication time, not at replay's
    // current clock; evidence outside the certificate's lifetime still fails.
    certificate
        .verify_authenticity(
            &pinned,
            &body.trellis_instance_id,
            &body.audience_nats_account,
        )
        .unwrap();
    assert!(certificate
        .verify(
            &pinned,
            &body.trellis_instance_id,
            &body.audience_nats_account,
            1_300
        )
        .is_err());
    let retired = AuthorityIssuerKey {
        state: AuthorityIssuerState::Retired,
        ..pinned.clone()
    };
    certificate
        .verify_authenticity(
            &retired,
            &body.trellis_instance_id,
            &body.audience_nats_account,
        )
        .unwrap();
    let revoked = AuthorityIssuerKey {
        state: AuthorityIssuerState::Revoked,
        ..pinned.clone()
    };
    assert!(certificate
        .verify_authenticity(
            &revoked,
            &body.trellis_instance_id,
            &body.audience_nats_account
        )
        .is_err());
    assert!(certificate
        .verify_authenticity(&pinned, &body.trellis_instance_id, "other-account")
        .is_err());
    let mut forged = certificate.clone();
    forged.expires_at += 1;
    assert!(forged
        .verify_authenticity(
            &pinned,
            &body.trellis_instance_id,
            &body.audience_nats_account
        )
        .is_err());
    let invalid = SignedProviderCertificate {
        expires_at: certificate.issued_at,
        ..certificate.clone()
    }
    .sign(&issuer)
    .unwrap();
    assert!(invalid
        .verify_authenticity(
            &pinned,
            &body.trellis_instance_id,
            &body.audience_nats_account
        )
        .is_err());
    assert!(certificate
        .authorizes_event_publication(&pinned, &authority, &catalog, &publish, 1_299)
        .unwrap());
    assert!(certificate
        .authorizes_event_publication(&pinned, &authority, &catalog, &publish, 1_300)
        .is_err());
    assert!(certificate
        .authorizes_event_publication(&pinned, &authority, &catalog, &publish, 1_099)
        .is_err());
}

#[test]
fn certified_responses_bind_opening_bytes_action_destination_and_result_kind() {
    use trellis_protocol::{
        session_request_signing_digest, sign_session_response, verify_session_response,
        ApiSurfaceKind, AuthorityProvider, CatalogActionIdentity, PermissionAction,
        SessionResponse, SessionResponseKind, SessionResponseVerificationInput,
        SignedProviderCertificate, PROVIDER_CERTIFICATE_FORMAT_V1,
    };
    let (_, pinned, caller, policy) = issued();
    let issuer = SigningKey::from_bytes(&[2; 32]);
    let provider_key = SigningKey::from_bytes(&[8; 32]);
    let action = CatalogActionIdentity {
        kind: ApiSurfaceKind::Rpc,
        name: "Documents.Get".into(),
        direction: PermissionAction::Call,
    };
    let certificate = SignedProviderCertificate {
        format: PROVIDER_CERTIFICATE_FORMAT_V1.into(),
        issuer_key_id: pinned.key_id.clone(),
        trellis_instance_id: caller.unsigned.trellis_instance_id.clone(),
        audience_nats_account: caller.unsigned.audience_nats_account.clone(),
        principal_id: "01JY0000000000000000000006".into(),
        deployment_id: "01JY0000000000000000000007".into(),
        instance_id: "01JY0000000000000000000008".into(),
        session_public_key: URL_SAFE_NO_PAD.encode(provider_key.verifying_key().as_bytes()),
        api_id: "documents@v1".into(),
        generation: U64s::new(1),
        implementation_digest: URL_SAFE_NO_PAD.encode([9; 32]),
        implemented_actions: vec![action.clone()],
        issued_at: 1_100,
        not_before: 1_100,
        expires_at: 1_300,
        extensions: Map::new(),
        critical: vec![],
        signature: String::new(),
    }
    .sign(&issuer)
    .unwrap();
    let mut provider_body = caller.unsigned.clone();
    provider_body.principal_id = certificate.principal_id.clone();
    provider_body.principal_kind = PrincipalKind::Service;
    provider_body.binding = SessionBinding::Service {
        deployment_id: certificate.deployment_id.clone(),
        instance_id: certificate.instance_id.clone(),
        participant_id: "documents.service".into(),
    };
    provider_body.authorization_session_id = "01JY0000000000000000000009".into();
    provider_body.session_public_key = certificate.session_public_key.clone();
    provider_body.login_session_id = None;
    provider_body.oauth_grant_id = None;
    provider_body.capabilities.clear();
    provider_body.platform_privileges.clear();
    provider_body.provider_bindings = vec![AuthorityProvider {
        api_id: certificate.api_id.clone(),
        generation: certificate.generation,
        deployment_id: certificate.deployment_id.clone(),
        implementation_digest: certificate.implementation_digest.clone(),
        provider_certificate_digest: certificate.digest().unwrap(),
    }];
    let signed_provider = sign_session_authority(provider_body, &issuer).unwrap();
    let provider = verify_session_authority(SessionAuthorityVerificationInput {
        authority: &signed_provider,
        issuer: &pinned,
        trellis_instance_id: &caller.unsigned.trellis_instance_id,
        audience_nats_account: &caller.unsigned.audience_nats_account,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: None,
    })
    .unwrap();
    let request = SessionRequest {
        authority_digest: caller.digest().unwrap(),
        api_id: "documents@v1".into(),
        api_generation: U64s::new(1),
        accepted_revision: U64s::new(3),
        action: action.name.clone(),
        subject: "documents.get".into(),
        reply_subject: Some("_INBOX.runtime.reply".into()),
        request_id: "01JY0000000000000000000010".into(),
        issued_at: policy.now_unix_seconds,
    };
    let pending_digest = session_request_signing_digest(&request, b"original opening").unwrap();
    let response = SessionResponse {
        trellis_instance_id: caller.unsigned.trellis_instance_id.clone(),
        audience_nats_account: caller.unsigned.audience_nats_account.clone(),
        provider_authority_digest: provider.digest().into(),
        provider_certificate_digest: certificate.digest().unwrap(),
        api_id: request.api_id.clone(),
        api_generation: request.api_generation,
        action: request.action.clone(),
        request_authority_digest: request.authority_digest.clone(),
        request_id: request.request_id.clone(),
        request_proof_digest: URL_SAFE_NO_PAD.encode(pending_digest),
        subject: request.reply_subject.clone().unwrap(),
        kind: SessionResponseKind::Success,
        issued_at: policy.now_unix_seconds,
    };
    let payload = b"actual provider result";
    let proof = sign_session_response(&response, payload, &provider_key).unwrap();
    // Callers authenticate a certificate once, when they fetch it.
    let authenticated_certificate =
        trellis_protocol::AuthenticatedProviderCertificate::authenticate(
            certificate.clone(),
            &pinned,
            &provider.authority().trellis_instance_id,
            &provider.authority().audience_nats_account,
        )
        .unwrap();
    // Even an authentic provider cannot claim another deployment as the owner
    // of a returned durable handle: signing the payload is not sufficient.
    let forged_owner = "01JY0000000000000000000011";
    let forged_reference = serde_json::to_vec(&serde_json::json!({
        "kind": "accepted", "ref": {
            "id": "01JY0000000000000000000012", "service": "documents",
            "operation": "Generate", "deploymentId": forged_owner,
        },
    }))
    .unwrap();
    let forged_reference_proof =
        sign_session_response(&response, &forged_reference, &provider_key).unwrap();
    assert!(verify_session_response(SessionResponseVerificationInput {
        provider: &provider,
        certificate: &authenticated_certificate,
        certificate_issuer: &pinned,
        pending_request: &request,
        pending_request_digest: &pending_digest,
        action: &action,
        expected_deployment_id: Some(forged_owner),
        received_subject: &response.subject,
        response: &response,
        payload_sha256: sha256(&forged_reference),
        proof: &forged_reference_proof,
        policy: &policy,
        known_revoked: false,
    })
    .is_err());
    let verify = |response, bytes, opening_digest, subject, expected_action, revoked| {
        verify_session_response(SessionResponseVerificationInput {
            provider: &provider,
            certificate: &authenticated_certificate,
            certificate_issuer: &pinned,
            pending_request: &request,
            pending_request_digest: opening_digest,
            action: expected_action,
            expected_deployment_id: Some(&certificate.deployment_id),
            received_subject: subject,
            response,
            payload_sha256: sha256(bytes),
            proof: &proof,
            policy: &policy,
            known_revoked: revoked,
        })
        .map(|_| bytes.to_vec())
    };
    assert_eq!(
        verify(
            &response,
            payload,
            &pending_digest,
            &response.subject,
            &action,
            false
        )
        .unwrap(),
        payload
    );
    assert!(verify(
        &response,
        b"forged result",
        &pending_digest,
        &response.subject,
        &action,
        false
    )
    .is_err());
    let other_opening = session_request_signing_digest(&request, b"substituted opening").unwrap();
    assert!(verify(
        &response,
        payload,
        &other_opening,
        &response.subject,
        &action,
        false
    )
    .is_err());
    assert!(verify(
        &response,
        payload,
        &pending_digest,
        "_INBOX.runtime.other",
        &action,
        false
    )
    .is_err());
    let mut substituted_kind = response.clone();
    substituted_kind.kind = SessionResponseKind::Error;
    assert!(verify(
        &substituted_kind,
        payload,
        &pending_digest,
        &response.subject,
        &action,
        false
    )
    .is_err());
    let mut other_action = action.clone();
    other_action.name = "Documents.Delete".into();
    assert!(verify(
        &response,
        payload,
        &pending_digest,
        &response.subject,
        &other_action,
        false
    )
    .is_err());
    assert!(verify(
        &response,
        payload,
        &pending_digest,
        &response.subject,
        &action,
        true
    )
    .is_err());
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root
        .join(".local/protocol-interop")
        .join(ulid::Ulid::new().to_string());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("response.json");
    fs::write(&path, serde_json::to_vec(&json!({
                "authority": signed_provider,
                "providerSeed": URL_SAFE_NO_PAD.encode(provider_key.to_bytes()),
        "authorityVerification": {
            "issuer": pinned,
            "trellisInstanceId": caller.unsigned.trellis_instance_id,
            "audienceNatsAccount": caller.unsigned.audience_nats_account,
            "policy": policy,
            "purpose": "live",
            "revocationCutoff": null
        },
        "verification": {
            "response": response,
            "proof": proof,
            "certificate": certificate,
            "certificateIssuer": pinned,
            "pendingRequest": request,
            "pendingRequestDigest": URL_SAFE_NO_PAD.encode(pending_digest),
            "action": action,
            "expectedDeploymentId": certificate.deployment_id,
            "receivedSubject": response.subject,
            "policy": policy,
            "knownRevoked": false
        },
        "payload": String::from_utf8(payload.to_vec()).unwrap(),
        "digest": URL_SAFE_NO_PAD.encode(trellis_protocol::session_response_signing_digest(&response, payload).unwrap())
    })).unwrap()).unwrap();
    let output = Command::new("node")
        .current_dir(&root)
        .arg("crates/protocol/tests/session_response_wasm.ts")
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
    let mut signed_error = response;
    signed_error.kind = SessionResponseKind::Error;
    let error_bytes = b"declared application error";
    let error_proof = sign_session_response(&signed_error, error_bytes, &provider_key).unwrap();
    assert_eq!(
        verify_session_response(SessionResponseVerificationInput {
            provider: &provider,
            certificate: &authenticated_certificate,
            certificate_issuer: &pinned,
            pending_request: &request,
            pending_request_digest: &pending_digest,
            action: &action,
            expected_deployment_id: None,
            received_subject: &signed_error.subject,
            response: &signed_error,
            payload_sha256: sha256(error_bytes),
            proof: &error_proof,
            policy: &policy,
            known_revoked: false,
        })
        .unwrap()
        .response()
        .kind,
        SessionResponseKind::Error
    );
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
                payload_sha256: sha256(payload),
                proof: &proof,
                policy: &policy,
                original_publication,
                known_revoked: cutoff.is_some(),
            })
        };
        let retained = check(Some(&publication)).unwrap();
        if cutoff.is_some() {
            for (event_time, accepted) in [
                ("1970-01-01T00:19:59.999999999Z", true),
                ("1970-01-01T00:20:00Z", false),
                ("1970-01-01T00:20:00.000000001Z", false),
            ] {
                let event = SessionEvent {
                    event_time: event_time.into(),
                    ..event.clone()
                };
                let proof = sign_session_event(&event, payload, &runtime).unwrap();
                assert_eq!(
                    verify_session_event(SessionEventVerificationInput {
                        authority: &verified,
                        event: &event,
                        payload_sha256: sha256(payload),
                        proof: &proof,
                        policy: &policy,
                        original_publication: Some(&publication),
                        known_revoked: true,
                    })
                    .is_ok(),
                    accepted,
                    "signed event time must precede the exclusive cutoff: {event_time}"
                );
            }
        }
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
            payload_sha256: sha256(raw_payload),
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
            payload_sha256: sha256(bytes),
            proof: &proof,
            policy: &policy,
            known_revoked: false,
        })
        .map(|verified| verified.digest().to_owned())
    };
    assert!(verify(&request, payload).is_ok());
    // A signed Transfer request may reply only on its own grant's signal route.
    let routes = trellis_protocol::transfer::derive_transfer_subjects(
        "provider-runtime",
        &authority.authority().runtime_id,
        "AAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let mut transfer_request = request.clone();
    transfer_request.subject = routes.control_subject.clone();
    for (reply, allowed) in [
        (routes.signal_subject.clone(), true),
        (
            routes
                .signal_subject
                .replace("AAAAAAAAAAAAAAAAAAAAAA", "BBBBBBBBBBBBBBBBBBBBBB"),
            false,
        ),
        (routes.control_subject.clone(), false),
    ] {
        transfer_request.reply_subject = Some(reply);
        let proof = sign_session_request(&transfer_request, payload, &runtime).unwrap();
        assert_eq!(
            verify_session_request(SessionRequestVerificationInput {
                authority: &authority,
                request: &transfer_request,
                payload_sha256: sha256(payload),
                proof: &proof,
                policy: &policy,
                known_revoked: false,
            })
            .is_ok(),
            allowed
        );
    }
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
fn current_issuer_metadata_authenticates_only_the_requested_historical_key() {
    use trellis_protocol::{
        sign_issuer_metadata, verify_issuer_metadata, IssuerMetadata, IssuerMetadataQuery,
        IssuerMetadataRequest, IssuerMetadataResult, ISSUER_METADATA_DOMAIN_V1,
        ISSUER_METADATA_SUBJECT,
    };
    let (_, old, authority, policy) = issued();
    let signer = SigningKey::from_bytes(&[12; 32]);
    let current = AuthorityIssuerKey {
        key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(signer.verifying_key().as_bytes())),
        public_key: URL_SAFE_NO_PAD.encode(signer.verifying_key().as_bytes()),
        state: AuthorityIssuerState::Active,
    };
    let historical = AuthorityIssuerKey {
        state: AuthorityIssuerState::Retired,
        ..old.clone()
    };
    let request = IssuerMetadataRequest {
        trellis_instance_id: authority.unsigned.trellis_instance_id.clone(),
        audience_nats_account: authority.unsigned.audience_nats_account.clone(),
        request_nonce: URL_SAFE_NO_PAD.encode([8; 16]),
        query: IssuerMetadataQuery::IssuerKey {
            key_id: historical.key_id.clone(),
        },
    };
    let reply = "_INBOX.runtime.historicalIssuer";
    let response = IssuerMetadata {
        format: ISSUER_METADATA_DOMAIN_V1.into(),
        issuer_key_id: current.key_id.clone(),
        request: request.clone(),
        request_subject: ISSUER_METADATA_SUBJECT.into(),
        reply_subject: reply.into(),
        result: IssuerMetadataResult::Value {
            payload: serde_json::to_string(&historical).unwrap(),
        },
        issued_at: policy.now_unix_seconds,
        expires_at: policy.now_unix_seconds + 30,
    };
    let signed = sign_issuer_metadata(response, &signer).unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    let verify = |bytes: &[u8], trusted: &AuthorityIssuerKey, retained: &IssuerMetadataRequest| {
        verify_issuer_metadata(
            bytes,
            trusted,
            retained,
            ISSUER_METADATA_SUBJECT,
            reply,
            policy.now_unix_seconds,
        )
    };
    let verified = verify(&bytes, &current, &request).unwrap();
    let IssuerMetadataResult::Value { payload } = verified.result() else {
        panic!("current issuer authenticates the requested historical key")
    };
    let resolved: AuthorityIssuerKey = serde_json::from_str(payload).unwrap();
    // Apply the attested public key at a real cryptographic boundary: an old
    // authority's historical signature remains valid, but not a live admission.
    let input = SessionAuthorityVerificationInput {
        issuer: &resolved,
        trellis_instance_id: &request.trellis_instance_id,
        audience_nats_account: &request.audience_nats_account,
        authority: &authority,
        policy: &policy,
        purpose: SessionAuthorityPurpose::HistoricalEvent,
        revocation_cutoff: None,
    };
    verify_session_authority(SessionAuthorityVerificationInput { ..input }).unwrap();
    assert!(verify_session_authority(SessionAuthorityVerificationInput {
        purpose: SessionAuthorityPurpose::Live,
        ..input
    })
    .is_err());

    let mut forged = signed.clone();
    let replacement = if forged.signature.starts_with('A') {
        "B"
    } else {
        "A"
    };
    forged.signature.replace_range(..1, replacement);
    assert!(verify(&serde_json::to_vec(&forged).unwrap(), &current, &request).is_err());
    let mut tampered = signed.clone();
    tampered.response.result = IssuerMetadataResult::Missing;
    assert!(verify(&serde_json::to_vec(&tampered).unwrap(), &current, &request).is_err());

    // Even a correctly signed envelope cannot substitute another key, attach
    // unrelated public bytes to the requested digest, or introduce an active pin.
    for candidate in [
        AuthorityIssuerKey {
            state: AuthorityIssuerState::Retired,
            ..current.clone()
        },
        AuthorityIssuerKey {
            public_key: current.public_key.clone(),
            ..historical.clone()
        },
        AuthorityIssuerKey {
            public_key: format!("{}=", historical.public_key),
            ..historical.clone()
        },
        AuthorityIssuerKey {
            state: AuthorityIssuerState::Active,
            ..historical.clone()
        },
    ] {
        let mut response = signed.response.clone();
        response.result = IssuerMetadataResult::Value {
            payload: serde_json::to_string(&candidate).unwrap(),
        };
        let invalid = sign_issuer_metadata(response, &signer).unwrap();
        assert!(verify(&serde_json::to_vec(&invalid).unwrap(), &current, &request).is_err());
    }
    for retained in [
        IssuerMetadataRequest {
            request_nonce: URL_SAFE_NO_PAD.encode([9; 16]),
            ..request.clone()
        },
        IssuerMetadataRequest {
            audience_nats_account: "another-account".into(),
            ..request.clone()
        },
        IssuerMetadataRequest {
            trellis_instance_id: "01JY0000000000000000000009".into(),
            ..request.clone()
        },
        IssuerMetadataRequest {
            query: IssuerMetadataQuery::IssuerKey {
                key_id: current.key_id.clone(),
            },
            ..request.clone()
        },
    ] {
        assert!(verify(&bytes, &current, &retained).is_err());
    }
    assert!(verify_issuer_metadata(
        &bytes,
        &current,
        &request,
        ISSUER_METADATA_SUBJECT,
        "_INBOX.another.exchange",
        policy.now_unix_seconds
    )
    .is_err());
    assert!(verify_issuer_metadata(
        &bytes,
        &current,
        &request,
        "rpc.other.Metadata",
        reply,
        policy.now_unix_seconds
    )
    .is_err());
    for inactive in [AuthorityIssuerState::Retired, AuthorityIssuerState::Revoked] {
        assert!(verify(
            &bytes,
            &AuthorityIssuerKey {
                state: inactive,
                ..current.clone()
            },
            &request
        )
        .is_err());
    }
    assert!(verify(&bytes, &old, &request).is_err());
    // A valid signature from a different, unpinned active issuer does not make
    // its historical payload acceptable under the retained current pin.
    let foreign_signer = SigningKey::from_bytes(&[13; 32]);
    let mut response = signed.response.clone();
    response.issuer_key_id =
        URL_SAFE_NO_PAD.encode(Sha256::digest(foreign_signer.verifying_key().as_bytes()));
    let foreign = sign_issuer_metadata(response, &foreign_signer).unwrap();
    assert!(verify(&serde_json::to_vec(&foreign).unwrap(), &current, &request).is_err());

    let mut response = signed.response.clone();
    response.result = IssuerMetadataResult::Value {
        payload: serde_json::to_string(&AuthorityIssuerKey {
            state: AuthorityIssuerState::Revoked,
            ..historical
        })
        .unwrap(),
    };
    let revoked = sign_issuer_metadata(response, &signer).unwrap();
    let revoked_bytes = serde_json::to_vec(&revoked).unwrap();
    let verified = verify(&revoked_bytes, &current, &request).unwrap();
    let IssuerMetadataResult::Value { payload } = verified.result() else {
        panic!("compromise state is authenticated, not treated as unavailable")
    };
    let compromised: AuthorityIssuerKey = serde_json::from_str(payload).unwrap();
    assert!(verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &compromised,
        ..input
    })
    .is_err());
    assert!(verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &compromised,
        purpose: SessionAuthorityPurpose::Live,
        ..input
    })
    .is_err());
}

#[test]
fn issuer_metadata_authenticates_public_material_and_absence_for_one_exchange() {
    use trellis_protocol::{
        sign_issuer_metadata, verify_issuer_metadata, IssuerMetadata, IssuerMetadataQuery,
        IssuerMetadataRequest, IssuerMetadataResult, ISSUER_METADATA_DOMAIN_V1,
    };
    let (_, issuer, authority, policy) = issued();
    let signer = SigningKey::from_bytes(&[2; 32]);
    let request = IssuerMetadataRequest {
        trellis_instance_id: authority.unsigned.trellis_instance_id.clone(),
        audience_nats_account: authority.unsigned.audience_nats_account.clone(),
        request_nonce: URL_SAFE_NO_PAD.encode([8; 16]),
        query: IssuerMetadataQuery::AuthorityResolve {
            context_digest: authority.digest().unwrap(),
        },
    };
    let subject = "rpc.trellis.auth.Authority.Resolve";
    let reply = "_INBOX.runtime.metadata";
    let payload = serde_json::to_string(&authority).unwrap();
    let signed = sign_issuer_metadata(
        IssuerMetadata {
            format: ISSUER_METADATA_DOMAIN_V1.into(),
            issuer_key_id: issuer.key_id.clone(),
            request: request.clone(),
            request_subject: subject.into(),
            reply_subject: reply.into(),
            result: IssuerMetadataResult::Value {
                payload: payload.clone(),
            },
            issued_at: policy.now_unix_seconds,
            expires_at: policy.now_unix_seconds + 30,
        },
        &signer,
    )
    .unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    let verify = |bytes: &[u8], retained: &IssuerMetadataRequest, destination: &str| {
        verify_issuer_metadata(
            bytes,
            &issuer,
            retained,
            subject,
            destination,
            policy.now_unix_seconds,
        )
    };
    let verified = verify(&bytes, &request, reply).unwrap();
    assert!(
        matches!(verified.result(), IssuerMetadataResult::Value { payload: got } if got == &payload)
    );
    let mut changed = signed.clone();
    changed.response.result = IssuerMetadataResult::Missing;
    assert!(verify(&serde_json::to_vec(&changed).unwrap(), &request, reply).is_err());
    let mut other = request.clone();
    other.request_nonce = URL_SAFE_NO_PAD.encode([9; 16]);
    assert!(verify(&bytes, &other, reply).is_err());
    other = request.clone();
    other.query = IssuerMetadataQuery::AuthorityStatus {
        authorization_session_id: authority.unsigned.authorization_session_id.clone(),
    };
    assert!(verify(&bytes, &other, reply).is_err());
    other = request.clone();
    other.audience_nats_account = "another-account".into();
    assert!(verify(&bytes, &other, reply).is_err());
    assert!(verify(&bytes, &request, "_INBOX.another.exchange").is_err());
    assert!(verify_issuer_metadata(
        &bytes,
        &issuer,
        &request,
        subject,
        reply,
        signed.response.expires_at
    )
    .is_err());
    let revoked = AuthorityIssuerKey {
        state: AuthorityIssuerState::Revoked,
        ..issuer.clone()
    };
    assert!(verify_issuer_metadata(
        &bytes,
        &revoked,
        &request,
        subject,
        reply,
        policy.now_unix_seconds
    )
    .is_err());
    for result in [
        IssuerMetadataResult::Missing,
        IssuerMetadataResult::Unavailable,
    ] {
        let mut response = signed.response.clone();
        response.result = result.clone();
        let absence = sign_issuer_metadata(response, &signer).unwrap();
        assert_eq!(
            verify(&serde_json::to_vec(&absence).unwrap(), &request, reply)
                .unwrap()
                .result(),
            &result
        );
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root
        .join(".local/protocol-interop")
        .join(ulid::Ulid::new().to_string());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("metadata.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "signed": signed,
            "verification": { "issuer": issuer, "request": request, "requestSubject": subject,
                "replySubject": reply, "nowUnixSeconds": policy.now_unix_seconds }
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new("node")
        .current_dir(&root)
        .arg("crates/protocol/tests/issuer_metadata_wasm.ts")
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
        payload_sha256: sha256(payload.as_bytes()),
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
        payload_sha256: sha256(payload.as_bytes()),
        proof: &event_proof,
        policy: &historical_policy,
        original_publication: Some(&publication),
        known_revoked: true,
    })
    .unwrap();
    let signed_time_cases = [
        ("1970-01-01T00:19:59.999999999Z", true),
        ("1970-01-01T00:20:00Z", false),
        ("1970-01-01T00:20:00.000000001Z", false),
        ("1970-01-01T00:21:00Z", false),
    ]
    .into_iter()
    .map(|(event_time, accepted)| {
        let mut candidate = event.clone();
        candidate.event_time = event_time.into();
        let proof = sign_session_event(&candidate, payload.as_bytes(), &runtime).unwrap();
        json!({"event": candidate, "proof": proof, "accepted": accepted})
    })
    .collect::<Vec<_>>();
    let fixture = json!({
        "runtimeSeed": URL_SAFE_NO_PAD.encode(runtime.to_bytes()),
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
        "signedTimeCases": signed_time_cases,
    });
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root
        .join(".local/protocol-interop")
        .join(ulid::Ulid::new().to_string());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("request.json");
    fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let output = Command::new("node")
        .current_dir(&root)
        .arg("crates/protocol/tests/session_authority_wasm.ts")
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

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}
