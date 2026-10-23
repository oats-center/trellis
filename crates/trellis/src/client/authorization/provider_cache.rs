//! One connected runtime's authority, issuer trust and bounded peer material.

use std::{
    collections::BTreeMap,
    ops::Deref,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use async_nats::jetstream;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::StreamExt as _;
use tokio::sync::{broadcast, watch, Mutex, OnceCell, Semaphore};
use trellis_protocol::{
    verify_issuer_metadata, verify_session_authority, verify_session_response,
    AuthenticatedCatalog, AuthenticatedProviderCertificate, AuthorityIssuerKey,
    AuthorityIssuerState, CatalogActionIdentity, IssuerMetadataQuery, IssuerMetadataRequest,
    IssuerMetadataResult, OriginalEventPublication, SessionAuthorityPurpose,
    SessionAuthorityVerificationInput, SessionAuthorityVerificationPolicy, SessionEvent,
    SessionEventProof, SessionRequest, SessionRequestProof, SessionResponse, SessionResponseKind,
    SessionResponseProof, SessionResponseVerificationInput, SignedIssuerMetadata,
    SignedSessionAuthority, SignedSessionRevocation, VerifiedIssuerMetadata,
    VerifiedSessionAuthority, MAX_SESSION_AUTHORITY_BYTES,
};

use super::core::{
    AuthorizationVerificationCore, EventVerificationInput, RequestVerificationInput,
    VerifiedAuthorizationEvent, VerifiedAuthorizationRequest,
};

use super::registry::{
    AuthorizationRegistryReader, MaterialKind, RegistryMaterial, UnverifiedMaterial,
    REVOCATION_BUCKET,
};
use crate::client::{proof::now_iat_seconds, TrellisClientError};

const MAX_CACHED_MATERIAL: usize = 256;
const MAX_ISSUERS: usize = 256;
const MAX_OBSERVED_REVOCATIONS: usize = 4096;
const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use serde_json::Map;
    use sha2::{Digest as _, Sha256};
    use trellis_protocol::{
        sign_session_authority, sign_session_request, ApiSurfaceKind, AuthorityApi,
        PermissionAction, PrincipalKind, SessionBinding, U64s, UnsignedSessionAuthority,
    };

    #[tokio::test]
    async fn cold_provider_material_replays_real_backlog_after_expiry_and_issuer_retirement() {
        use trellis_protocol::{
            sign_session_event, AuthorityProvider, CatalogAction, OriginalEventPublication,
            SessionEvent, SignedCatalogSnapshot, SignedProviderCertificate,
        };

        let source = tempfile::tempdir().unwrap();
        trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
            source.path(),
        ))
        .unwrap();
        let logs = tempfile::tempdir().unwrap();
        let mut nats = trellis_local_nats::LocalNats::builder()
            .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
            .cache_dir(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/trellis-test-cache"),
            )
            .source(source.path())
            .temporary_state()
            .ephemeral_ports()
            .output(trellis_local_nats::NatsOutput::Log {
                path: logs.path().join("nats.log"),
                mirror: false,
            })
            .start()
            .unwrap();
        let transport = async_nats::ConnectOptions::new()
            .credentials_file(source.path().join("creds/trellis-auth.creds"))
            .await
            .unwrap()
            .connect(nats.nats_url())
            .await
            .unwrap();
        let js = jetstream::new(transport.clone());
        let material = js
            .create_key_value(jetstream::kv::Config {
                bucket: super::super::registry::MATERIAL_BUCKET.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let subject = "event.documents.Changed";
        let stream = js
            .create_stream(jetstream::stream::Config {
                name: "retained_provider_events".into(),
                subjects: vec![subject.into()],
                ..Default::default()
            })
            .await
            .unwrap();
        // Cryptographic adapter test: issuer-owned native protocol material, not
        // a substitute Auth responder or fabricated broker publication timestamp.
        let issuer_key = SigningKey::from_bytes(&[2; 32]);
        let runtime_key = SigningKey::from_bytes(&[3; 32]);
        let issuer = AuthorityIssuerKey {
            key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(issuer_key.verifying_key().as_bytes())),
            public_key: URL_SAFE_NO_PAD.encode(issuer_key.verifying_key().as_bytes()),
            state: AuthorityIssuerState::Active,
        };
        let now = now_iat_seconds() as i64;
        let action = CatalogActionIdentity {
            kind: ApiSurfaceKind::Event,
            name: "Documents.Changed".into(),
            direction: PermissionAction::Publish,
        };
        let catalog = SignedCatalogSnapshot {
            format: trellis_protocol::CATALOG_SNAPSHOT_FORMAT_V1.into(),
            issuer_key_id: issuer.key_id.clone(),
            trellis_instance_id: "01JY0000000000000000000001".into(),
            audience_nats_account: "application-account".into(),
            api_id: "documents@v1".into(),
            generation: U64s::new(1),
            accepted_revision: U64s::new(1),
            definition_digest: URL_SAFE_NO_PAD.encode([4; 32]),
            actions: vec![CatalogAction {
                identity: action.clone(),
                introduced_revision: U64s::new(1),
            }],
            capabilities: vec![],
            issued_at: now,
            extensions: Map::new(),
            critical: vec![],
            signature: String::new(),
        }
        .sign(&issuer_key)
        .unwrap();
        let certificate = SignedProviderCertificate {
            format: trellis_protocol::PROVIDER_CERTIFICATE_FORMAT_V1.into(),
            issuer_key_id: issuer.key_id.clone(),
            trellis_instance_id: catalog.trellis_instance_id.clone(),
            audience_nats_account: catalog.audience_nats_account.clone(),
            principal_id: "01JY0000000000000000000002".into(),
            deployment_id: "01JY0000000000000000000003".into(),
            instance_id: "01JY0000000000000000000004".into(),
            session_public_key: URL_SAFE_NO_PAD.encode(runtime_key.verifying_key().as_bytes()),
            api_id: catalog.api_id.clone(),
            generation: catalog.generation,
            implementation_digest: URL_SAFE_NO_PAD.encode([5; 32]),
            implemented_actions: vec![action.clone()],
            issued_at: now,
            not_before: now,
            expires_at: now + 3,
            extensions: Map::new(),
            critical: vec![],
            signature: String::new(),
        }
        .sign(&issuer_key)
        .unwrap();
        let authority = sign_session_authority(
            UnsignedSessionAuthority {
                format: trellis_protocol::SESSION_AUTHORITY_FORMAT_V1.into(),
                issuer_key_id: issuer.key_id.clone(),
                trellis_instance_id: catalog.trellis_instance_id.clone(),
                audience_nats_account: catalog.audience_nats_account.clone(),
                principal_id: certificate.principal_id.clone(),
                principal_kind: PrincipalKind::Service,
                binding: SessionBinding::Service {
                    deployment_id: certificate.deployment_id.clone(),
                    instance_id: certificate.instance_id.clone(),
                    participant_id: "documents.service".into(),
                },
                authorization_session_id: "01JY0000000000000000000005".into(),
                runtime_id: URL_SAFE_NO_PAD.encode([6; 16]),
                session_public_key: certificate.session_public_key.clone(),
                inbox_prefix: "_INBOX.runtime".into(),
                login_session_id: None,
                oauth_grant_id: None,
                capabilities: vec![],
                apis: vec![AuthorityApi {
                    api_id: catalog.api_id.clone(),
                    generation: catalog.generation,
                    accepted_revision: catalog.accepted_revision,
                    catalog_snapshot_digest: catalog.digest().unwrap(),
                }],
                provider_bindings: vec![AuthorityProvider {
                    api_id: certificate.api_id.clone(),
                    generation: certificate.generation,
                    deployment_id: certificate.deployment_id.clone(),
                    implementation_digest: certificate.implementation_digest.clone(),
                    provider_certificate_digest: certificate.digest().unwrap(),
                }],
                resource_bindings_digest: None,
                platform_privileges: vec![],
                issued_at: now,
                not_before: now,
                expires_at: now + 240,
                extensions: Map::new(),
                critical: vec![],
            },
            &issuer_key,
        )
        .unwrap();
        for (prefix, digest, bytes) in [
            (
                "api",
                catalog.digest().unwrap(),
                serde_json::to_vec(&catalog).unwrap(),
            ),
            (
                "provider",
                certificate.digest().unwrap(),
                serde_json::to_vec(&certificate).unwrap(),
            ),
            (
                "context",
                authority.digest().unwrap(),
                serde_json::to_vec(&authority).unwrap(),
            ),
        ] {
            material
                .put(format!("{prefix}.{digest}"), bytes.into())
                .await
                .unwrap();
        }
        let event = SessionEvent {
            authority_digest: authority.digest().unwrap(),
            api_id: catalog.api_id.clone(),
            api_generation: catalog.generation,
            accepted_revision: catalog.accepted_revision,
            action: action.name.clone(),
            subject: subject.into(),
            event_id: "01JY0000000000000000000006".into(),
            event_time: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
        };
        let payload = b"retained provider payload";
        let proof = sign_session_event(&event, payload, &runtime_key).unwrap();
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(
            "Trellis-Session-Event",
            serde_json::to_string(&event).unwrap(),
        );
        headers.insert("proof", proof.as_str());
        let first = js
            .publish_with_headers(subject, headers.clone(), payload.as_slice().into())
            .await
            .unwrap()
            .await
            .unwrap();
        let raw = stream.get_raw_message(first.sequence).await.unwrap();
        assert!(raw.time.unix_timestamp() < certificate.expires_at);
        tokio::time::timeout(Duration::from_secs(10), async {
            while (now_iat_seconds() as i64) < certificate.expires_at {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        // Start cold after expiry: no previously authenticated certificate can
        // mask a current-time rejection on the shared material-fetch path.
        let owner = AuthorizationOwner::new(
            RuntimeAuthorizationTrust {
                instance_id: catalog.trellis_instance_id.clone(),
                account: catalog.audience_nats_account.clone(),
                issuer: issuer.clone(),
                rotation_sequence: 0,
                root_certificates: Vec::new(),
            },
            authority.unsigned.runtime_id.clone(),
            certificate.session_public_key.clone(),
        )
        .unwrap();
        let successor = SigningKey::from_bytes(&[8; 32]);
        owner
            .accept_issuer_rotation(
                trellis_protocol::SignedIssuerRotation {
                    format: trellis_protocol::ISSUER_ROTATION_FORMAT_V1.into(),
                    trellis_instance_id: catalog.trellis_instance_id.clone(),
                    audience_nats_account: catalog.audience_nats_account.clone(),
                    sequence: U64s::new(1),
                    previous_key_id: issuer.key_id.clone(),
                    next_key_id: URL_SAFE_NO_PAD
                        .encode(Sha256::digest(successor.verifying_key().as_bytes())),
                    next_public_key: URL_SAFE_NO_PAD.encode(successor.verifying_key().as_bytes()),
                    activated_at: now,
                    previous_retired_at: now,
                    extensions: Map::new(),
                    critical: vec![],
                    signature: String::new(),
                }
                .sign(&issuer_key)
                .unwrap(),
            )
            .unwrap();
        assert!(owner.assert_issuer_current(&issuer.key_id).is_err());
        let provider = owner
            .material(
                &transport,
                MaterialKind::Provider,
                &certificate.digest().unwrap(),
            )
            .await
            .expect("expired authentic provider material must load cold");
        let RegistryMaterial::Provider(certificate) = provider.as_ref() else {
            panic!("provider material")
        };
        let catalog_material = owner
            .material(
                &transport,
                MaterialKind::Catalog,
                &catalog.digest().unwrap(),
            )
            .await
            .unwrap();
        let RegistryMaterial::Catalog(catalog) = catalog_material.as_ref() else {
            panic!("catalog material")
        };
        let authority_material = owner
            .material(
                &transport,
                MaterialKind::Authority,
                &authority.digest().unwrap(),
            )
            .await
            .unwrap();
        let RegistryMaterial::Authority(authority) = authority_material.as_ref() else {
            panic!("authority material")
        };
        let issuer = owner.issuer(&transport, &issuer.key_id).await.unwrap();
        let policy = owner.policy();
        assert!(certificate
            .verify(
                &issuer,
                &catalog.trellis_instance_id,
                &catalog.audience_nats_account,
                policy.now_unix_seconds
            )
            .is_err());
        let verified = owner
            .verify_authority(authority, &issuer, SessionAuthorityPurpose::HistoricalEvent)
            .unwrap();
        let raw = stream.get_raw_message(first.sequence).await.unwrap();
        let retained: SessionEvent =
            serde_json::from_str(raw.headers.get("Trellis-Session-Event").unwrap().as_str())
                .unwrap();
        let retained_proof =
            trellis_protocol::SessionEventProof::parse(raw.headers.get("proof").unwrap().as_str())
                .unwrap();
        let original = OriginalEventPublication {
            published_at: raw
                .time
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
            stream_sequence: U64s::new(raw.sequence),
        };
        let check = |authority, event, proof, publication, payload: &[u8]| {
            AuthorizationVerificationCore::new().verify_event(EventVerificationInput {
                authority,
                event,
                proof,
                payload,
                policy: &policy,
                known_revoked: false,
                catalog,
                catalog_issuer: &issuer,
                action: &action,
                original_publication: Some(publication),
                provider: Some((certificate, &issuer)),
            })
        };
        check(
            verified.clone(),
            &retained,
            &retained_proof,
            &original,
            &raw.payload,
        )
        .unwrap();
        assert!(check(
            verified.clone(),
            &retained,
            &retained_proof,
            &original,
            b"forged payload"
        )
        .is_err());
        let wrong_generation = SessionEvent {
            api_generation: U64s::new(2),
            ..retained.clone()
        };
        let wrong_proof =
            sign_session_event(&wrong_generation, &raw.payload, &runtime_key).unwrap();
        assert!(check(
            verified.clone(),
            &wrong_generation,
            &wrong_proof,
            &original,
            &raw.payload
        )
        .is_err());
        let late = js
            .publish_with_headers(subject, headers, payload.as_slice().into())
            .await
            .unwrap()
            .await
            .unwrap();
        let late = stream.get_raw_message(late.sequence).await.unwrap();
        let late_publication = OriginalEventPublication {
            published_at: late
                .time
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
            stream_sequence: U64s::new(late.sequence),
        };
        assert!(check(
            verified,
            &retained,
            &retained_proof,
            &late_publication,
            &late.payload
        )
        .is_err());
        owner
            .accept_revocation(
                SignedSessionRevocation {
                    format: trellis_protocol::SESSION_REVOCATION_FORMAT_V1.into(),
                    issuer_key_id: issuer.key_id.clone(),
                    trellis_instance_id: catalog.trellis_instance_id.clone(),
                    audience_nats_account: catalog.audience_nats_account.clone(),
                    authorization_session_id: authority.unsigned.authorization_session_id.clone(),
                    effective_cutoff: now,
                    reason: "closed".into(),
                    issued_at: policy.now_unix_seconds,
                    latest_context_expiry: authority.unsigned.expires_at,
                    extensions: Map::new(),
                    critical: vec![],
                    signature: String::new(),
                }
                .sign(&issuer_key)
                .unwrap(),
            )
            .unwrap();
        let revoked = owner
            .verify_authority(authority, &issuer, SessionAuthorityPurpose::HistoricalEvent)
            .unwrap();
        assert!(check(revoked, &retained, &retained_proof, &original, &raw.payload).is_err());
        nats.stop().unwrap();
    }

    #[tokio::test]
    async fn possession_and_signed_retirement_follow_real_mirror_coverage() {
        let source = tempfile::tempdir().unwrap();
        trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
            source.path(),
        ))
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut nats = trellis_local_nats::LocalNats::builder()
            .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
            .cache_dir(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/trellis-test-cache"),
            )
            .source(source.path())
            .temporary_state()
            .ephemeral_ports()
            .output(trellis_local_nats::NatsOutput::Log {
                path: state.path().join("nats.log"),
                mirror: false,
            })
            .start()
            .unwrap();
        let transport = async_nats::ConnectOptions::new()
            .credentials_file(source.path().join("creds/trellis-auth.creds"))
            .await
            .unwrap()
            .connect(nats.nats_url())
            .await
            .unwrap();
        let bucket = jetstream::new(transport.clone())
            .create_key_value(jetstream::kv::Config {
                bucket: REVOCATION_BUCKET.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let issuer_key = SigningKey::from_bytes(&[2; 32]);
        let runtime_key = SigningKey::from_bytes(&[3; 32]);
        let issuer = AuthorityIssuerKey {
            key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(issuer_key.verifying_key().as_bytes())),
            public_key: URL_SAFE_NO_PAD.encode(issuer_key.verifying_key().as_bytes()),
            state: AuthorityIssuerState::Active,
        };
        let now = now_iat_seconds() as i64;
        let body = UnsignedSessionAuthority {
            format: trellis_protocol::SESSION_AUTHORITY_FORMAT_V1.into(),
            issuer_key_id: issuer.key_id.clone(),
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
            session_public_key: URL_SAFE_NO_PAD.encode(runtime_key.verifying_key().as_bytes()),
            inbox_prefix: "_INBOX.runtime".into(),
            login_session_id: Some("01JY0000000000000000000004".into()),
            oauth_grant_id: Some("01JY0000000000000000000005".into()),
            capabilities: vec![],
            apis: vec![AuthorityApi {
                api_id: "documents@v1".into(),
                generation: U64s::new(1),
                accepted_revision: U64s::new(3),
                catalog_snapshot_digest: URL_SAFE_NO_PAD.encode([5; 32]),
            }],
            provider_bindings: vec![],
            resource_bindings_digest: None,
            platform_privileges: vec![],
            issued_at: now,
            not_before: now,
            expires_at: now + 240,
            extensions: Map::new(),
            critical: vec![],
        };
        let owner = AuthorizationOwner::new(
            RuntimeAuthorizationTrust {
                instance_id: body.trellis_instance_id.clone(),
                account: body.audience_nats_account.clone(),
                issuer: issuer.clone(),
                rotation_sequence: 0,
                root_certificates: Vec::new(),
            },
            body.runtime_id.clone(),
            body.session_public_key.clone(),
        )
        .unwrap();
        let signed = sign_session_authority(body.clone(), &issuer_key).unwrap();
        let authority = owner
            .verify_authority(&signed, &issuer, SessionAuthorityPurpose::Live)
            .unwrap();
        owner.promote_own(authority.clone(), None).unwrap();
        assert!(owner.own_authority().is_err());
        assert!(owner.assert_current(&authority).is_err());
        let (stop, _) = watch::channel(false);
        let start_mirror = || {
            let owner = owner.clone();
            let transport = transport.clone();
            let shutdown = stop.subscribe();
            let (ready, snapshot) = tokio::sync::oneshot::channel();
            (
                tokio::spawn(
                    async move { owner.run_revocations(&transport, shutdown, ready).await },
                ),
                snapshot,
            )
        };
        let (first, snapshot) = start_mirror();
        tokio::time::timeout(RESOLUTION_TIMEOUT, snapshot)
            .await
            .unwrap()
            .unwrap();
        owner.assert_current(&authority).unwrap();
        let (second, snapshot) = start_mirror();
        tokio::time::timeout(RESOLUTION_TIMEOUT, snapshot)
            .await
            .unwrap()
            .unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        owner.assert_current(&authority).unwrap();
        second.abort();
        assert!(second.await.unwrap_err().is_cancelled());
        assert!(owner.assert_current(&authority).is_err());
        assert!(owner.assert_accepted_response_allowed(&authority).is_err());
        assert!(owner.own_authority().is_err());
        let (third, snapshot) = start_mirror();
        tokio::time::timeout(RESOLUTION_TIMEOUT, snapshot)
            .await
            .unwrap()
            .unwrap();
        owner.assert_current(&authority).unwrap();
        let request = SessionRequest {
            authority_digest: authority.digest().into(),
            api_id: "documents@v1".into(),
            api_generation: U64s::new(1),
            accepted_revision: U64s::new(3),
            action: "Documents.Get".into(),
            subject: "rpc.v1.documents.Documents.Get".into(),
            reply_subject: Some("_INBOX.runtime.reply".into()),
            request_id: "01JY0000000000000000000010".into(),
            issued_at: now,
        };
        let bytes = br#"{"id":1,"future":true}"#;
        let proof = sign_session_request(&request, bytes, &runtime_key).unwrap();
        let core = AuthorizationVerificationCore::new();
        let policy = owner.policy();
        let mut opening = core
            .verify_possession(authority.clone(), &request, bytes, &proof, &policy, false)
            .unwrap();
        assert!(opening.authorized_action().is_none());
        assert_eq!(
            opening.digest(),
            &trellis_protocol::session_request_signing_digest(&request, bytes).unwrap()
        );
        assert!(core
            .verify_possession(
                authority.clone(),
                &request,
                br#"{"id":1}"#,
                &proof,
                &policy,
                false
            )
            .is_err());
        let mut action = CatalogActionIdentity {
            kind: ApiSurfaceKind::Rpc,
            name: "Documents.Delete".into(),
            direction: PermissionAction::Call,
        };
        assert!(opening.authorize_handler(action.clone()).is_err());
        action.name = request.action.clone();
        opening.authorize_handler(action.clone()).unwrap();
        assert_eq!(opening.authorized_action(), Some(&action));

        let replacement = sign_session_authority(
            UnsignedSessionAuthority {
                expires_at: now + 250,
                ..body
            },
            &issuer_key,
        )
        .unwrap();
        let replacement = owner
            .verify_authority(&replacement, &issuer, SessionAuthorityPurpose::Live)
            .unwrap();
        owner
            .promote_own(replacement.clone(), Some(authority.digest()))
            .unwrap();
        assert!(owner.assert_current(&authority).is_err());
        owner.assert_current(&replacement).unwrap();
        owner.assert_accepted_response_allowed(&authority).unwrap();
        let retirement = SignedSessionRevocation {
            format: trellis_protocol::SESSION_REVOCATION_FORMAT_V1.into(),
            issuer_key_id: issuer.key_id.clone(),
            trellis_instance_id: signed.unsigned.trellis_instance_id.clone(),
            audience_nats_account: signed.unsigned.audience_nats_account.clone(),
            authorization_session_id: signed.unsigned.authorization_session_id.clone(),
            effective_cutoff: now,
            reason: "closed".into(),
            issued_at: now,
            latest_context_expiry: now + 250,
            extensions: Map::new(),
            critical: vec![],
            signature: String::new(),
        }
        .sign(&issuer_key)
        .unwrap();
        let mut forged = retirement.clone();
        forged.reason = "forged".into();
        assert!(owner.accept_revocation(forged).is_err());
        owner.assert_current(&replacement).unwrap();
        let mut changes = owner.subscribe_changes();
        bucket
            .put(
                &retirement.authorization_session_id,
                serde_json::to_vec(&retirement).unwrap().into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(RESOLUTION_TIMEOUT, changes.recv())
            .await
            .unwrap()
            .unwrap();
        owner.accept_revocation(retirement).unwrap();
        assert!(owner.assert_current(&replacement).is_err());
        assert!(owner.assert_accepted_response_allowed(&authority).is_err());
        assert!(owner
            .promote_own(replacement.clone(), Some(replacement.digest()))
            .is_err());
        third.abort();
        assert!(third.await.unwrap_err().is_cancelled());
        let (fourth, snapshot) = start_mirror();
        tokio::time::timeout(RESOLUTION_TIMEOUT, snapshot)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            owner.assert_current(&replacement),
            Err(TrellisClientError::Protocol(_))
        ));
        assert!(owner.assert_accepted_response_allowed(&authority).is_err());
        fourth.abort();
        assert!(fourth.await.unwrap_err().is_cancelled());

        let successor = SigningKey::from_bytes(&[8; 32]);
        let rotation = trellis_protocol::SignedIssuerRotation {
            format: trellis_protocol::ISSUER_ROTATION_FORMAT_V1.into(),
            trellis_instance_id: signed.unsigned.trellis_instance_id.clone(),
            audience_nats_account: signed.unsigned.audience_nats_account.clone(),
            sequence: U64s::new(1),
            previous_key_id: issuer.key_id.clone(),
            next_key_id: URL_SAFE_NO_PAD
                .encode(Sha256::digest(successor.verifying_key().as_bytes())),
            next_public_key: URL_SAFE_NO_PAD.encode(successor.verifying_key().as_bytes()),
            activated_at: now,
            previous_retired_at: now + 120,
            extensions: Map::new(),
            critical: vec![],
            signature: String::new(),
        }
        .sign(&issuer_key)
        .unwrap();
        let mut forged = rotation.clone();
        forged.previous_retired_at = now;
        assert!(owner.accept_issuer_rotation(forged).is_err());
        owner.accept_issuer_rotation(rotation.clone()).unwrap();
        owner.assert_issuer_current(&issuer.key_id).unwrap();
        owner.assert_issuer_current(&rotation.next_key_id).unwrap();
        assert!(owner.accept_issuer_rotation(rotation.clone()).is_err());
        let immediate = AuthorizationOwner::new(
            owner.trust.clone(),
            owner.runtime_id.clone(),
            owner.session_public_key.clone(),
        )
        .unwrap();
        immediate
            .accept_issuer_rotation(
                trellis_protocol::SignedIssuerRotation {
                    previous_retired_at: now,
                    ..rotation
                }
                .sign(&issuer_key)
                .unwrap(),
            )
            .unwrap();
        assert!(immediate.assert_issuer_current(&issuer.key_id).is_err());
        nats.stop().unwrap();
    }
}

/// Provisioning/OAuth-established trust. A metadata responder cannot choose it.
#[derive(Clone, Debug)]
pub struct RuntimeAuthorizationTrust {
    /// Stable installation identity.
    pub instance_id: String,
    /// Public broker account owned by this installation.
    pub account: String,
    /// Explicitly pinned issuer key.
    pub issuer: AuthorityIssuerKey,
    /// Authenticated position at which this issuer was pinned.
    pub rotation_sequence: u64,
    /// PEM CA files that are the only trust anchors for this installation's
    /// broker and OAuth TLS endpoints. Empty uses the platform trust store.
    ///
    /// Set it from provisioning or explicit operator configuration when the
    /// installation uses a private CA, such as the Trellis-generated
    /// installation CA. A broker or metadata response can never supply it.
    pub root_certificates: Vec<std::path::PathBuf>,
}

/// Retained-event evidence. Current consumer read authority is checked separately.
#[derive(Debug)]
pub struct HistoricalEventVerificationInput<'a> {
    /// Event identity reconstructed from the actual publication and generated descriptor.
    pub event: &'a SessionEvent,
    /// Exact original publication bytes.
    pub payload: &'a [u8],
    /// Original publisher signature.
    pub proof: &'a SessionEventProof,
    /// Registered publish action, not an inferred subject name.
    pub action: &'a CatalogActionIdentity,
    /// Trusted preserved original JetStream metadata.
    pub original_publication: &'a OriginalEventPublication,
    /// Fresh issuer-authenticated status for this exact logical session.
    pub status: &'a VerifiedIssuerMetadata,
}

/// Reply evidence bound to the exact request still awaiting an authenticated result.
#[derive(Debug)]
pub struct ResponseVerificationInput<'a> {
    /// Original signed request awaiting this response.
    pub pending_request: &'a SessionRequest,
    /// Digest retained when that request was signed.
    pub pending_request_digest: &'a [u8; 32],
    /// Generated action selected by the original request.
    pub action: &'a CatalogActionIdentity,
    /// Required provider deployment, when ownership pins the response.
    pub expected_deployment_id: Option<&'a str>,
    /// Actual broker subject on which the response arrived.
    pub received_subject: &'a str,
    /// Response metadata decoded from the received headers.
    pub response: &'a SessionResponse,
    /// Exact received response bytes, including errors.
    pub payload: &'a [u8],
    /// Provider signature over the response and payload.
    pub proof: &'a SessionResponseProof,
}

/// A retained peer authority and its pinned immutable cache entry.
#[derive(Clone, Debug)]
pub struct AuthorizationLease {
    authority: Arc<VerifiedSessionAuthority>,
    _material: Arc<RegistryMaterial>,
}

impl Deref for AuthorizationLease {
    type Target = VerifiedSessionAuthority;

    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}

impl AuthorizationLease {
    /// Retain signature-verified authority for an accepted finite request.
    pub fn verified(&self) -> Arc<VerifiedSessionAuthority> {
        self.authority.clone()
    }
}

/// A certified provider result, authenticated against an actual pending request.
#[derive(Debug)]
pub struct VerifiedAuthorizationResponse<'a> {
    provider: Arc<VerifiedSessionAuthority>,
    response: &'a SessionResponse,
    payload: &'a [u8],
}

impl<'a> VerifiedAuthorizationResponse<'a> {
    /// The certified provider that signed this result.
    pub fn provider(&self) -> &VerifiedSessionAuthority {
        &self.provider
    }

    /// Authenticated result identity and success/error classification.
    pub fn response(&self) -> &'a SessionResponse {
        self.response
    }

    /// Exact verified bytes, ready for application decoding.
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// The provider-authenticated result classification, never a broker status header.
    pub fn kind(&self) -> SessionResponseKind {
        self.response.kind
    }
}

type MaterialResult = Result<Arc<RegistryMaterial>, String>;

#[derive(Debug)]
struct CacheEntry {
    cell: Arc<OnceCell<MaterialResult>>,
    last_used: Instant,
}

#[derive(Debug)]
struct OwnerState {
    current_issuer: AuthorityIssuerKey,
    rotation_sequence: u64,
    issuers: BTreeMap<String, AuthorityIssuerKey>,
    issuer_retirements: BTreeMap<String, i64>,
    own: Option<Arc<VerifiedSessionAuthority>>,
    material: BTreeMap<(MaterialKind, String), CacheEntry>,
    revocations: BTreeMap<String, SignedSessionRevocation>,
    revocation_overflow_until: i64,
    revocation_epoch: Option<Arc<async_nats::Client>>,
    revocation_predecessor_epoch: Option<Arc<async_nats::Client>>,
    revocation_pending_epoch: Option<Arc<async_nats::Client>>,
    revocation_covered: bool,
}

impl OwnerState {
    fn require_live_coverage(&self) -> Result<(), TrellisClientError> {
        if !self.revocation_covered {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation mirror is not current".into(),
            ));
        }
        Ok(())
    }
    fn issuer(&self, key_id: &str, now: i64) -> Option<AuthorityIssuerKey> {
        let mut issuer = self.issuers.get(key_id)?.clone();
        if issuer.state == AuthorityIssuerState::Active
            && self
                .issuer_retirements
                .get(key_id)
                .is_some_and(|deadline| *deadline <= now)
        {
            issuer.state = AuthorityIssuerState::Retired;
        }
        Some(issuer)
    }
}

struct RevocationCoverage {
    owner: AuthorizationOwner,
    epoch: Arc<async_nats::Client>,
}

impl RevocationCoverage {
    fn ready(&self) -> Result<(), TrellisClientError> {
        let mut state = self.owner.state.write().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        if !state
            .revocation_pending_epoch
            .as_ref()
            .is_some_and(|epoch| Arc::ptr_eq(epoch, &self.epoch))
            || state.revocation_overflow_until > self.owner.policy().now_unix_seconds
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation snapshot superseded or incomplete".into(),
            ));
        }
        state.revocation_predecessor_epoch = if state.revocation_covered {
            state.revocation_epoch.take()
        } else {
            None
        };
        state.revocation_epoch = Some(Arc::clone(&self.epoch));
        state.revocation_pending_epoch = None;
        state.revocation_covered = true;
        drop(state);
        let _ = self.owner.changes.send(());
        Ok(())
    }
}

impl Drop for RevocationCoverage {
    fn drop(&mut self) {
        if let Ok(mut state) = self.owner.state.write() {
            if state
                .revocation_pending_epoch
                .as_ref()
                .is_some_and(|epoch| Arc::ptr_eq(epoch, &self.epoch))
            {
                state.revocation_pending_epoch = None;
            }
            if state
                .revocation_predecessor_epoch
                .as_ref()
                .is_some_and(|epoch| Arc::ptr_eq(epoch, &self.epoch))
            {
                state.revocation_predecessor_epoch = None;
            }
            if state
                .revocation_epoch
                .as_ref()
                .is_some_and(|epoch| Arc::ptr_eq(epoch, &self.epoch))
            {
                // A completed candidate can still lose its final transport
                // check. Its never-stopped predecessor remains valid coverage.
                state.revocation_epoch =
                    state
                        .revocation_predecessor_epoch
                        .take()
                        .filter(|transport| {
                            transport.connection_state() == async_nats::connection::State::Connected
                        });
                state.revocation_covered = state.revocation_epoch.is_some();
                drop(state);
                let _ = self.owner.changes.send(());
            }
        }
    }
}

/// Shared authority owner for a connected application, service or device runtime.
///
/// All message adapters share this owner. It retains one non-evictable own
/// authority and positive logical-session revocations, not per-context watches
/// or a policy database. Transport leases remain owned by transport generations.
#[derive(Clone, Debug)]
pub struct AuthorizationOwner {
    trust: RuntimeAuthorizationTrust,
    runtime_id: String,
    session_public_key: String,
    state: Arc<RwLock<OwnerState>>,
    rotations: Arc<Mutex<()>>,
    metadata_reads: Arc<Semaphore>,
    changes: broadcast::Sender<()>,
}

impl AuthorizationOwner {
    /// Current authenticated issuer pin for durable OAuth credential renewal.
    /// This does not grant application authority or require a live attachment.
    pub(crate) fn current_trust(&self) -> Result<RuntimeAuthorizationTrust, TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner lock poisoned".into())
        })?;
        Ok(RuntimeAuthorizationTrust {
            issuer: state.current_issuer.clone(),
            rotation_sequence: state.rotation_sequence,
            ..self.trust.clone()
        })
    }
    /// Create an owner from the externally established issuer pin and fresh runtime key.
    pub fn new(
        trust: RuntimeAuthorizationTrust,
        runtime_id: String,
        session_public_key: String,
    ) -> Result<Self, TrellisClientError> {
        trust.issuer.verifying_key()?;
        if trust.issuer.state != AuthorityIssuerState::Active
            || ulid::Ulid::from_string(&trust.instance_id).is_err()
            || !URL_SAFE_NO_PAD
                .decode(&runtime_id)
                .is_ok_and(|bytes| bytes.len() == 16 && URL_SAFE_NO_PAD.encode(bytes) == runtime_id)
            || !URL_SAFE_NO_PAD
                .decode(&session_public_key)
                .is_ok_and(|bytes| {
                    bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == session_public_key
                })
            || trust.account.is_empty()
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "invalid runtime trust pin".into(),
            ));
        }
        let state = OwnerState {
            current_issuer: trust.issuer.clone(),
            rotation_sequence: trust.rotation_sequence,
            issuers: BTreeMap::from([(trust.issuer.key_id.clone(), trust.issuer.clone())]),
            issuer_retirements: BTreeMap::new(),
            own: None,
            material: BTreeMap::new(),
            revocations: BTreeMap::new(),
            revocation_overflow_until: 0,
            revocation_epoch: None,
            revocation_predecessor_epoch: None,
            revocation_pending_epoch: None,
            revocation_covered: false,
        };
        Ok(Self {
            trust,
            runtime_id,
            session_public_key,
            state: Arc::new(RwLock::new(state)),
            rotations: Arc::new(Mutex::new(())),
            metadata_reads: Arc::new(Semaphore::new(16)),
            changes: broadcast::channel(16).0,
        })
    }

    /// Current wall-clock verification policy, recomputed after asynchronous work.
    pub fn policy(&self) -> SessionAuthorityVerificationPolicy {
        SessionAuthorityVerificationPolicy {
            now_unix_seconds: now_iat_seconds() as i64,
            allowed_clock_skew_seconds: 30,
            maximum_authority_lifetime_seconds: 300,
            maximum_authority_bytes: MAX_SESSION_AUTHORITY_BYTES,
            maximum_entries: 4096,
        }
    }

    /// Subscribe to authority promotion and observed logical-session retirement.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }

    /// Retain own authority only while the current revocation snapshot is covered.
    pub fn own_authority(&self) -> Result<Arc<VerifiedSessionAuthority>, TrellisClientError> {
        let authority = self.staged_own_authority()?;
        self.assert_current(&authority)?;
        Ok(authority)
    }

    /// Return only retained transport identity, never application authority.
    /// Expiry and issuer retirement require renewal; known session cutoffs still
    /// forbid reusing the attachment and require ordinary fresh admission.
    pub(crate) fn maintenance_identity(&self) -> Result<(String, String), TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        let authority = state.own.as_ref().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable("own authority not installed".into())
        })?;
        if state.revocation_overflow_until > self.policy().now_unix_seconds {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation capacity reached".into(),
            ));
        }
        let session_id = &authority.authority().authorization_session_id;
        if state.revocations.contains_key(session_id) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own logical session retired".into(),
            ));
        }
        Ok((authority.digest().to_owned(), session_id.clone()))
    }

    /// Private transport preparation only: current installed evidence remains subject to
    /// signed expiry, issuer retirement and sticky revocation, but is not application-usable.
    pub(crate) fn staged_own_authority(
        &self,
    ) -> Result<Arc<VerifiedSessionAuthority>, TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        let authority = state.own.clone().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable("own authority not installed".into())
        })?;
        let policy = self.policy();
        if state.revocation_overflow_until > policy.now_unix_seconds {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation capacity reached".into(),
            ));
        }
        authority.assert_current(
            &policy,
            state
                .revocations
                .contains_key(&authority.authority().authorization_session_id),
        )?;
        let issuer = state
            .issuer(
                &authority.authority().issuer_key_id,
                policy.now_unix_seconds,
            )
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable("own issuer unavailable".into())
            })?;
        if issuer.state != AuthorityIssuerState::Active {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own issuer retired".into(),
            ));
        }
        Ok(authority)
    }

    /// Check a private own transport candidate before coverage exists. This does not
    /// grant peer/application usability or require the candidate to be promoted yet.
    pub(crate) fn assert_prepared_current(
        &self,
        authority: &VerifiedSessionAuthority,
    ) -> Result<(), TrellisClientError> {
        if authority.authority().runtime_id != self.runtime_id
            || authority.authority().session_public_key != self.session_public_key
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "prepared authority runtime mismatch".into(),
            ));
        }
        let issuer = {
            let state = self.state.read().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
            })?;
            state
                .issuer(
                    &authority.authority().issuer_key_id,
                    self.policy().now_unix_seconds,
                )
                .ok_or_else(|| {
                    TrellisClientError::AuthorizationUnavailable(
                        "prepared issuer unavailable".into(),
                    )
                })?
        };
        self.verify_authority(authority.signed(), &issuer, SessionAuthorityPurpose::Live)?;
        Ok(())
    }

    /// Reject a retained certificate when its authenticated issuer has retired.
    pub fn assert_issuer_current(&self, key_id: &str) -> Result<(), TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        if !state
            .issuer(key_id, self.policy().now_unix_seconds)
            .is_some_and(|issuer| issuer.state == AuthorityIssuerState::Active)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "issuer no longer current".into(),
            ));
        }
        Ok(())
    }

    /// Recheck retained authority against this runtime's current trust, expiry and retirement.
    pub fn assert_current(
        &self,
        authority: &VerifiedSessionAuthority,
    ) -> Result<(), TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        state.require_live_coverage()?;
        let policy = self.policy();
        if authority.authority().trellis_instance_id != self.trust.instance_id
            || authority.authority().audience_nats_account != self.trust.account
            || (authority.authority().runtime_id == self.runtime_id
                && authority.authority().session_public_key == self.session_public_key
                && state.own.as_ref().map(|own| own.digest()) != Some(authority.digest()))
            || state.revocation_overflow_until > policy.now_unix_seconds
            || !state
                .issuer(
                    &authority.authority().issuer_key_id,
                    policy.now_unix_seconds,
                )
                .is_some_and(|issuer| issuer.state == AuthorityIssuerState::Active)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authority no longer trusted".into(),
            ));
        }
        authority.assert_current(
            &policy,
            state
                .revocations
                .contains_key(&authority.authority().authorization_session_id),
        )?;
        Ok(())
    }

    /// Permit delivery of a verified finite response to its already-accepted opening.
    /// The opening need not remain live, but terminal ownership, lost coverage,
    /// known revocation and compromised issuer trust still fence delivery.
    pub(crate) fn assert_accepted_response_allowed(
        &self,
        opening: &VerifiedSessionAuthority,
    ) -> Result<(), TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        state.require_live_coverage()?;
        let body = opening.authority();
        let policy = self.policy();
        if body.trellis_instance_id != self.trust.instance_id
            || body.audience_nats_account != self.trust.account
            || body.runtime_id != self.runtime_id
            || body.session_public_key != self.session_public_key
            || !state.own.as_ref().is_some_and(|own| {
                own.authority().authorization_session_id == body.authorization_session_id
            })
            || state
                .revocations
                .contains_key(&body.authorization_session_id)
            || state.revocation_overflow_until > policy.now_unix_seconds
            || !state
                .issuer(&body.issuer_key_id, policy.now_unix_seconds)
                .is_some_and(|issuer| issuer.state != AuthorityIssuerState::Revoked)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "accepted response ownership no longer trusted".into(),
            ));
        }
        Ok(())
    }

    /// Resolve authenticated catalog membership and recheck live authority after cold work.
    pub async fn authorize_action(
        &self,
        transport: &async_nats::Client,
        authority: &VerifiedSessionAuthority,
        api_id: &str,
        action: &CatalogActionIdentity,
    ) -> Result<Arc<AuthenticatedCatalog>, TrellisClientError> {
        let reference = authority
            .authority()
            .apis
            .iter()
            .find(|api| api.api_id == api_id)
            .ok_or(super::core::AuthorizationVerificationError::PermissionDenied)?;
        let material = self
            .material(
                transport,
                MaterialKind::Catalog,
                &reference.catalog_snapshot_digest,
            )
            .await?;
        let RegistryMaterial::Catalog(catalog) = material.as_ref() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "wrong catalog material".into(),
            ));
        };
        let issuer = self.issuer(transport, &catalog.issuer_key_id).await?;
        self.assert_current(authority)?;
        let policy = self.policy();
        let delegated = {
            let state = self.state.read().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
            })?;
            state.require_live_coverage()?;
            let issuer = state
                .issuer(&issuer.key_id, policy.now_unix_seconds)
                .ok_or_else(|| {
                    TrellisClientError::AuthorizationUnavailable(
                        "catalog issuer unavailable".into(),
                    )
                })?;
            if authority.authority().runtime_id == self.runtime_id
                && authority.authority().session_public_key == self.session_public_key
                && state.own.as_ref().map(|own| own.digest()) != Some(authority.digest())
            {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "own authority superseded".into(),
                ));
            }
            authority.assert_current(
                &policy,
                state
                    .revocations
                    .contains_key(&authority.authority().authorization_session_id),
            )?;
            if state.revocation_overflow_until > policy.now_unix_seconds
                || !state
                    .issuer(
                        &authority.authority().issuer_key_id,
                        policy.now_unix_seconds,
                    )
                    .is_some_and(|key| key.state == AuthorityIssuerState::Active)
            {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "revocation capacity reached".into(),
                ));
            }
            catalog.check(&issuer, &self.trust.instance_id, &self.trust.account)?;
            if catalog.api_id != api_id {
                return Err(super::core::AuthorizationVerificationError::PermissionDenied.into());
            }
            catalog.authorizes(authority.signed(), action)
        };
        if !delegated {
            if action.kind != trellis_protocol::ApiSurfaceKind::Event
                || action.direction != trellis_protocol::PermissionAction::Publish
                || !authority
                    .authority()
                    .provider_bindings
                    .iter()
                    .any(|binding| binding.api_id == api_id)
            {
                return Err(super::core::AuthorizationVerificationError::PermissionDenied.into());
            }
            let certificate = self
                .provider_certificate(transport, authority, api_id)
                .await?;
            let issuer = self.issuer(transport, &certificate.issuer_key_id).await?;
            if !certificate.authorizes_event_publication(
                &issuer,
                authority.signed(),
                catalog,
                action,
                policy.now_unix_seconds,
            )? {
                return Err(super::core::AuthorizationVerificationError::PermissionDenied.into());
            }
            self.assert_current(authority)?;
        }
        Ok(Arc::clone(catalog))
    }

    /// Resolve certified implementation rights; caller capability membership grants no provider
    /// rights.
    pub async fn authorize_provider(
        &self,
        transport: &async_nats::Client,
        authority: &VerifiedSessionAuthority,
        api_id: &str,
        action: &CatalogActionIdentity,
    ) -> Result<Arc<AuthenticatedProviderCertificate>, TrellisClientError> {
        let certificate = self
            .provider_certificate(transport, authority, api_id)
            .await?;
        if !certificate.implemented_actions.contains(action) {
            return Err(super::core::AuthorizationVerificationError::PermissionDenied.into());
        }
        Ok(certificate)
    }

    /// Resolve the current, authenticated implementation metadata for a provider.
    pub(crate) async fn provider_certificate(
        &self,
        transport: &async_nats::Client,
        authority: &VerifiedSessionAuthority,
        api_id: &str,
    ) -> Result<Arc<AuthenticatedProviderCertificate>, TrellisClientError> {
        let binding = authority
            .authority()
            .provider_bindings
            .iter()
            .find(|binding| binding.api_id == api_id)
            .ok_or(super::core::AuthorizationVerificationError::PermissionDenied)?;
        let material = self
            .material(
                transport,
                MaterialKind::Provider,
                &binding.provider_certificate_digest,
            )
            .await?;
        let RegistryMaterial::Provider(certificate) = material.as_ref() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "wrong provider material".into(),
            ));
        };
        let issuer = self.issuer(transport, &certificate.issuer_key_id).await?;
        self.assert_current(authority)?;
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        state.require_live_coverage()?;
        let policy = self.policy();
        let issuer = state
            .issuer(&issuer.key_id, policy.now_unix_seconds)
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable("provider issuer unavailable".into())
            })?;
        if authority.authority().runtime_id == self.runtime_id
            && authority.authority().session_public_key == self.session_public_key
            && state.own.as_ref().map(|own| own.digest()) != Some(authority.digest())
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own authority superseded".into(),
            ));
        }
        authority.assert_current(
            &policy,
            state
                .revocations
                .contains_key(&authority.authority().authorization_session_id),
        )?;
        if state.revocation_overflow_until > policy.now_unix_seconds
            || !state
                .issuer(
                    &authority.authority().issuer_key_id,
                    policy.now_unix_seconds,
                )
                .is_some_and(|key| key.state == AuthorityIssuerState::Active)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation capacity reached".into(),
            ));
        }
        if issuer.state != AuthorityIssuerState::Active {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "certificate issuer retired".into(),
            ));
        }
        certificate.check_at(
            &issuer,
            &self.trust.instance_id,
            &self.trust.account,
            policy.now_unix_seconds,
        )?;
        let body = authority.authority();
        let coordinates_match = match &body.binding {
            trellis_protocol::SessionBinding::Service {
                deployment_id,
                instance_id,
                ..
            }
            | trellis_protocol::SessionBinding::Device {
                deployment_id,
                instance_id,
                ..
            } => {
                deployment_id == &certificate.deployment_id
                    && instance_id == &certificate.instance_id
            }
            _ => false,
        };
        let now = policy.now_unix_seconds;
        if !coordinates_match
            || certificate.principal_id != body.principal_id
            || certificate.session_public_key != body.session_public_key
            || certificate.api_id != api_id
            || certificate.generation != binding.generation
            || certificate.deployment_id != binding.deployment_id
            || certificate.implementation_digest != binding.implementation_digest
            || certificate.not_before > now
            || certificate.expires_at <= now
        {
            return Err(super::core::AuthorizationVerificationError::PermissionDenied.into());
        }
        Ok(Arc::clone(certificate))
    }

    /// Verify a replacement privately. Promotion happens only after its transport is admitted.
    pub async fn prepare_own(
        &self,
        transport: &async_nats::Client,
        signed: &SignedSessionAuthority,
    ) -> Result<Arc<VerifiedSessionAuthority>, TrellisClientError> {
        if signed.unsigned.runtime_id != self.runtime_id
            || signed.unsigned.session_public_key != self.session_public_key
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own authority runtime mismatch".into(),
            ));
        }
        let issuer = self
            .issuer(transport, &signed.unsigned.issuer_key_id)
            .await?;
        self.verify_authority(signed, &issuer, SessionAuthorityPurpose::Live)
    }

    /// Publish a prepared own authority at the transport's checked promotion boundary.
    pub fn promote_own(
        &self,
        candidate: Arc<VerifiedSessionAuthority>,
        expected_digest: Option<&str>,
    ) -> Result<(), TrellisClientError> {
        let mut state = self.state.write().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        if state.own.as_ref().map(|authority| authority.digest()) != expected_digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own candidate superseded".into(),
            ));
        }
        if state.own.as_ref().is_some_and(|current| {
            current.authority().principal_id != candidate.authority().principal_id
                || current.authority().binding != candidate.authority().binding
        }) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own candidate changes runtime identity".into(),
            ));
        }
        if candidate.authority().runtime_id != self.runtime_id
            || candidate.authority().session_public_key != self.session_public_key
            || candidate.authority().trellis_instance_id != self.trust.instance_id
            || candidate.authority().audience_nats_account != self.trust.account
            || state.revocation_overflow_until > self.policy().now_unix_seconds
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own candidate unavailable".into(),
            ));
        }
        let issuer = state
            .issuer(
                &candidate.authority().issuer_key_id,
                self.policy().now_unix_seconds,
            )
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable("candidate issuer unavailable".into())
            })?;
        if issuer.state != AuthorityIssuerState::Active {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "candidate issuer retired".into(),
            ));
        }
        candidate.assert_current(
            &self.policy(),
            state
                .revocations
                .contains_key(&candidate.authority().authorization_session_id),
        )?;
        state.own = Some(candidate);
        drop(state);
        let _ = self.changes.send(());
        Ok(())
    }

    /// Resolve a current peer, rejecting expiry and sticky retirement after cold reads.
    pub async fn resolve_authority(
        &self,
        transport: &async_nats::Client,
        digest: &str,
    ) -> Result<AuthorizationLease, TrellisClientError> {
        let material = self
            .material(transport, MaterialKind::Authority, digest)
            .await?;
        let RegistryMaterial::Authority(signed) = material.as_ref() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "wrong authority material".into(),
            ));
        };
        let issuer = self
            .issuer(transport, &signed.unsigned.issuer_key_id)
            .await?;
        let authority = self.verify_authority(signed, &issuer, SessionAuthorityPurpose::Live)?;
        self.assert_current(&authority)?;
        Ok(AuthorizationLease {
            authority,
            _material: material,
        })
    }

    fn issuer_renewal_pending(&self) -> Result<bool, TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        let Some(own) = state.own.as_ref() else {
            return Ok(false);
        };
        if own.authority().issuer_key_id == state.current_issuer.key_id
            || state
                .revocations
                .contains_key(&own.authority().authorization_session_id)
        {
            return Ok(false);
        }
        state.require_live_coverage()?;
        if state.revocation_overflow_until > self.policy().now_unix_seconds {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation capacity reached".into(),
            ));
        }
        Ok(true)
    }

    /// Authenticate an opening against exact bytes and the generated registered action.
    pub async fn verify_request(
        &self,
        transport: &async_nats::Client,
        request: &SessionRequest,
        payload: &[u8],
        proof: &SessionRequestProof,
        action: &CatalogActionIdentity,
    ) -> Result<VerifiedAuthorizationRequest, TrellisClientError> {
        tokio::time::timeout(RESOLUTION_TIMEOUT, async {
            loop {
                let mut changes = self.subscribe_changes();
                let own = match self.own_authority() {
                    Ok(own) => own,
                    Err(error) => {
                        if !self.issuer_renewal_pending()? {
                            return Err(error);
                        }
                        if matches!(
                            changes.recv().await,
                            Err(tokio::sync::broadcast::error::RecvError::Closed)
                        ) {
                            return Err(TrellisClientError::AuthorizationUnavailable(
                                "authority owner closed".into(),
                            ));
                        }
                        continue;
                    }
                };
                let result = async {
                    let authority = self
                        .resolve_authority(transport, &request.authority_digest)
                        .await?;
                    let certificate = self
                        .authorize_provider(transport, &own, &request.api_id, action)
                        .await?;
                    if certificate.generation != request.api_generation {
                        return Err(
                            super::core::AuthorizationVerificationError::PermissionDenied.into(),
                        );
                    }
                    // Caller grants are bound to the caller's catalog, independently of the
                    // provider's pinned implementation checked above.
                    let reference = authority
                        .authority()
                        .apis
                        .iter()
                        .find(|api| api.api_id == request.api_id)
                        .ok_or(super::core::AuthorizationVerificationError::PermissionDenied)?;
                    let material = self
                        .material(
                            transport,
                            MaterialKind::Catalog,
                            &reference.catalog_snapshot_digest,
                        )
                        .await?;
                    let RegistryMaterial::Catalog(catalog) = material.as_ref() else {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "wrong catalog material".into(),
                        ));
                    };
                    let issuer = self.issuer(transport, &catalog.issuer_key_id).await?;
                    let policy = self.policy();
                    let state = self.state.read().map_err(|_| {
                        TrellisClientError::AuthorizationUnavailable(
                            "authority owner unavailable".into(),
                        )
                    })?;
                    state.require_live_coverage()?;
                    if state.revocation_overflow_until > policy.now_unix_seconds {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "revocation capacity reached".into(),
                        ));
                    }
                    let known_revoked = state
                        .revocations
                        .contains_key(&authority.authority().authorization_session_id);
                    own.assert_current(
                        &policy,
                        state
                            .revocations
                            .contains_key(&own.authority().authorization_session_id),
                    )?;
                    if !state
                        .issuer(&own.authority().issuer_key_id, policy.now_unix_seconds)
                        .is_some_and(|key| key.state == AuthorityIssuerState::Active)
                    {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "provider issuer retired".into(),
                        ));
                    }
                    if state.own.as_ref().map(|current| current.digest()) != Some(own.digest()) {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "provider authority superseded during verification".into(),
                        ));
                    }
                    if !state
                        .issuer(
                            &authority.authority().issuer_key_id,
                            policy.now_unix_seconds,
                        )
                        .is_some_and(|key| key.state == AuthorityIssuerState::Active)
                    {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "caller issuer retired".into(),
                        ));
                    }
                    let issuer = state
                        .issuer(&issuer.key_id, policy.now_unix_seconds)
                        .ok_or_else(|| {
                            TrellisClientError::AuthorizationUnavailable(
                                "catalog issuer unavailable".into(),
                            )
                        })?;
                    let certificate_issuer = state
                        .issuer(&certificate.issuer_key_id, policy.now_unix_seconds)
                        .ok_or_else(|| {
                            TrellisClientError::AuthorizationUnavailable(
                                "certificate issuer unavailable".into(),
                            )
                        })?;
                    if certificate_issuer.state != AuthorityIssuerState::Active {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "provider material issuer retired".into(),
                        ));
                    }
                    certificate.check_at(
                        &certificate_issuer,
                        &self.trust.instance_id,
                        &self.trust.account,
                        policy.now_unix_seconds,
                    )?;
                    Ok(AuthorizationVerificationCore::new().verify_request(
                        RequestVerificationInput {
                            authority: authority.verified(),
                            request,
                            payload,
                            proof,
                            policy: &policy,
                            known_revoked,
                            catalog,
                            catalog_issuer: &issuer,
                            action,
                        },
                    )?)
                }
                .await;
                // Cold caller resolution can authenticate a successor and retire
                // this provider's signer before private renewal finishes. Retain
                // this unopened request within the same resolution deadline;
                // no handler has run and nothing is republished.
                if result.is_err() {
                    while self.issuer_renewal_pending()? {
                        if matches!(
                            changes.recv().await,
                            Err(tokio::sync::broadcast::error::RecvError::Closed)
                        ) {
                            return Err(TrellisClientError::AuthorizationUnavailable(
                                "authority owner closed".into(),
                            ));
                        }
                    }
                }
                // Recheck exact bytes only after verified own authority changed.
                // Coverage loss, cutoffs and unchanged authority remain fenced.
                if result.is_err()
                    && self
                        .own_authority()
                        .is_ok_and(|current| current.digest() != own.digest())
                {
                    continue;
                }
                return result;
            }
        })
        .await
        .map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("request verification timed out".into())
        })?
    }

    /// Authenticate exact request possession without authorizing any catalog action.
    /// Only an explicitly payload-authorized handler may subsequently admit this opening.
    pub async fn verify_possession(
        &self,
        transport: &async_nats::Client,
        request: &SessionRequest,
        payload: &[u8],
        proof: &SessionRequestProof,
    ) -> Result<VerifiedAuthorizationRequest, TrellisClientError> {
        let authority = self
            .resolve_authority(transport, &request.authority_digest)
            .await?;
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        state.require_live_coverage()?;
        let policy = self.policy();
        if state.revocation_overflow_until > policy.now_unix_seconds
            || !state
                .issuer(
                    &authority.authority().issuer_key_id,
                    policy.now_unix_seconds,
                )
                .is_some_and(|issuer| issuer.state == AuthorityIssuerState::Active)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "caller authority unavailable".into(),
            ));
        }
        Ok(AuthorizationVerificationCore::new().verify_possession(
            authority.verified(),
            request,
            payload,
            proof,
            &policy,
            state
                .revocations
                .contains_key(&authority.authority().authorization_session_id),
        )?)
    }

    /// Authenticate a public metadata result against the caller's retained exchange.
    /// Neither a responder-selected issuer nor an unsigned absence installs trust.
    pub async fn verify_metadata(
        &self,
        transport: &async_nats::Client,
        bytes: &[u8],
        request: &IssuerMetadataRequest,
        actual_subject: &str,
        actual_reply: &str,
    ) -> Result<VerifiedIssuerMetadata, TrellisClientError> {
        let _permit = self.metadata_reads.try_acquire().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable(
                "metadata verification capacity reached".into(),
            )
        })?;
        self.verify_metadata_inner(transport, bytes, request, actual_subject, actual_reply)
            .await
    }

    async fn verify_metadata_inner(
        &self,
        transport: &async_nats::Client,
        bytes: &[u8],
        request: &IssuerMetadataRequest,
        actual_subject: &str,
        actual_reply: &str,
    ) -> Result<VerifiedIssuerMetadata, TrellisClientError> {
        if bytes.is_empty() || bytes.len() > trellis_protocol::MAX_ADMISSION_RESPONSE_BYTES {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "metadata exceeds byte budget".into(),
            ));
        }
        let signed: SignedIssuerMetadata = serde_json::from_slice(bytes)?;
        if request.trellis_instance_id != self.trust.instance_id
            || request.audience_nats_account != self.trust.account
            || signed.response.request != *request
            || signed.response.request_subject != actual_subject
            || signed.response.reply_subject != actual_reply
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "metadata scope mismatch".into(),
            ));
        }
        let issuer = self
            .forward_issuer(transport, &signed.response.issuer_key_id)
            .await?;
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        let issuer = state
            .issuer(&issuer.key_id, self.policy().now_unix_seconds)
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable("metadata issuer unavailable".into())
            })?;
        let verified = verify_issuer_metadata(
            bytes,
            &issuer,
            request,
            actual_subject,
            actual_reply,
            self.policy().now_unix_seconds,
        )?;
        drop(state);
        if let (IssuerMetadataQuery::IssuerKey { .. }, IssuerMetadataResult::Value { payload }) =
            (&request.query, verified.result())
        {
            let historical: AuthorityIssuerKey = serde_json::from_str(payload)?;
            let mut state = self.state.write().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
            })?;
            if let Some(previous) = state.issuers.get(&historical.key_id) {
                if previous.public_key != historical.public_key {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "historical issuer conflict".into(),
                    ));
                }
                if previous.state == AuthorityIssuerState::Revoked {
                    return Ok(verified);
                }
                if previous.state == AuthorityIssuerState::Active {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "historical issuer is a live pin".into(),
                    ));
                }
            } else if state.issuers.len() >= MAX_ISSUERS {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "issuer chain capacity reached".into(),
                ));
            }
            state.issuers.insert(historical.key_id.clone(), historical);
        }
        Ok(verified)
    }

    /// Resolve fresh issuer-signed logical-session status for a retained publisher.
    /// The dedicated admitted-connection metadata bootstrap returns signed
    /// evidence; administrative Authority.Status remains a protected typed RPC.
    pub async fn historical_status(
        &self,
        transport: &async_nats::Client,
        authority_digest: &str,
    ) -> Result<VerifiedIssuerMetadata, TrellisClientError> {
        let _permit = self.metadata_reads.try_acquire().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("metadata read capacity reached".into())
        })?;
        tokio::time::timeout(RESOLUTION_TIMEOUT, async {
            let material = self
                .material(transport, MaterialKind::Authority, authority_digest)
                .await?;
            let RegistryMaterial::Authority(authority) = material.as_ref() else {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "wrong publisher material".into(),
                ));
            };
            let request = IssuerMetadataRequest {
                trellis_instance_id: self.trust.instance_id.clone(),
                audience_nats_account: self.trust.account.clone(),
                request_nonce: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>()),
                query: IssuerMetadataQuery::AuthorityStatus {
                    authorization_session_id: authority.unsigned.authorization_session_id.clone(),
                },
            };
            let subject = trellis_protocol::ISSUER_METADATA_SUBJECT.to_string();
            let reply = transport.new_inbox();
            let mut replies = transport.subscribe(reply.clone()).await.map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "metadata reply registration unavailable".into(),
                )
            })?;
            transport.flush().await.map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "metadata reply registration unavailable".into(),
                )
            })?;
            transport
                .publish_with_reply(
                    subject.clone(),
                    reply.clone(),
                    serde_json::to_vec(&request)?.into(),
                )
                .await
                .map_err(|_| {
                    TrellisClientError::AuthorizationUnavailable(
                        "metadata request unavailable".into(),
                    )
                })?;
            while let Some(message) = replies.next().await {
                if let Ok(metadata) = self
                    .verify_metadata_inner(transport, &message.payload, &request, &subject, &reply)
                    .await
                {
                    return Ok(metadata);
                }
                // Spoofed replies cannot consume the retained exchange.
            }
            Err(TrellisClientError::AuthorizationUnavailable(
                "metadata reply unavailable".into(),
            ))
        })
        .await
        .map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("metadata read timed out".into())
        })?
    }

    /// Verify retained publication authenticity without producing a current RPC caller.
    /// A fresh signed status supplies historical cutoffs even after the live mirror
    /// has retired its expired entries. Missing or unavailable status is not absence.
    pub async fn verify_historical_event(
        &self,
        transport: &async_nats::Client,
        input: HistoricalEventVerificationInput<'_>,
    ) -> Result<VerifiedAuthorizationEvent, TrellisClientError> {
        use crate::__types::trellis::{AuthAuthorityStatusResponse, AuthAuthorizationSessionState};

        let material = self
            .material(
                transport,
                MaterialKind::Authority,
                &input.event.authority_digest,
            )
            .await?;
        let RegistryMaterial::Authority(signed) = material.as_ref() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "wrong authority material".into(),
            ));
        };
        let status = input.status.response();
        if status.request.trellis_instance_id != self.trust.instance_id
            || status.request.audience_nats_account != self.trust.account
            || !matches!(
                &status.request.query,
                IssuerMetadataQuery::AuthorityStatus { authorization_session_id }
                    if authorization_session_id == &signed.unsigned.authorization_session_id
            )
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "historical status scope mismatch".into(),
            ));
        }
        let IssuerMetadataResult::Value { payload } = input.status.result() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "historical status unavailable".into(),
            ));
        };
        let resolved: AuthAuthorityStatusResponse = serde_json::from_str(payload)?;
        if resolved.authorization_session_id.to_string() != signed.unsigned.authorization_session_id
            || matches!(resolved.state, AuthAuthorizationSessionState::Unknown(_))
            || (resolved.state != AuthAuthorizationSessionState::Active
                && resolved.revocation.is_none())
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "historical cutoff unavailable".into(),
            ));
        }
        let revocation: Option<SignedSessionRevocation> = resolved
            .revocation
            .map(|blob| serde_json::from_slice(&blob.0 .0))
            .transpose()?;
        let issuer = self
            .issuer(transport, &signed.unsigned.issuer_key_id)
            .await?;
        if let Some(record) = &revocation {
            self.issuer(transport, &record.issuer_key_id).await?;
            self.accept_revocation(record.clone())?;
        }
        let reference = signed
            .unsigned
            .apis
            .iter()
            .find(|api| api.api_id == input.event.api_id)
            .ok_or(super::core::AuthorizationVerificationError::PermissionDenied)?;
        let catalog_material = self
            .material(
                transport,
                MaterialKind::Catalog,
                &reference.catalog_snapshot_digest,
            )
            .await?;
        let RegistryMaterial::Catalog(catalog) = catalog_material.as_ref() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "wrong catalog material".into(),
            ));
        };
        self.issuer(transport, &catalog.issuer_key_id).await?;
        let provider_material = if !catalog.authorizes(signed, input.action) {
            if let Some(binding) = signed
                .unsigned
                .provider_bindings
                .iter()
                .find(|binding| binding.api_id == input.event.api_id)
            {
                Some(
                    self.material(
                        transport,
                        MaterialKind::Provider,
                        &binding.provider_certificate_digest,
                    )
                    .await?,
                )
            } else {
                None
            }
        } else {
            None
        };
        let provider = match provider_material.as_deref() {
            Some(RegistryMaterial::Provider(certificate)) => Some(certificate),
            Some(_) => {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "wrong provider material".into(),
                ))
            }
            None => None,
        };
        if let Some(certificate) = provider {
            self.issuer(transport, &certificate.issuer_key_id).await?;
        }
        let policy = self.policy();
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        if policy.now_unix_seconds >= status.expires_at
            || status.issued_at > policy.now_unix_seconds.saturating_add(5)
            || !state
                .issuer(&status.issuer_key_id, policy.now_unix_seconds)
                .is_some_and(|key| key.state == AuthorityIssuerState::Active)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "historical status expired or issuer retired".into(),
            ));
        }
        let mut cutoff = state
            .revocations
            .get(&signed.unsigned.authorization_session_id)
            .map(|record| record.effective_cutoff);
        if let Some(record) = &revocation {
            let key = state
                .issuer(&record.issuer_key_id, policy.now_unix_seconds)
                .ok_or_else(|| {
                    TrellisClientError::AuthorizationUnavailable("cutoff issuer unavailable".into())
                })?;
            record.verify(&key, &self.trust.instance_id, &self.trust.account)?;
            if record.authorization_session_id != signed.unsigned.authorization_session_id {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "historical cutoff session mismatch".into(),
                ));
            }
            cutoff = Some(cutoff.map_or(record.effective_cutoff, |known| {
                known.min(record.effective_cutoff)
            }));
        }
        let authority = Arc::new(verify_session_authority(
            SessionAuthorityVerificationInput {
                issuer: &state
                    .issuer(&issuer.key_id, policy.now_unix_seconds)
                    .ok_or_else(|| {
                        TrellisClientError::AuthorizationUnavailable(
                            "publisher issuer unavailable".into(),
                        )
                    })?,
                trellis_instance_id: &self.trust.instance_id,
                audience_nats_account: &self.trust.account,
                authority: signed,
                policy: &policy,
                purpose: SessionAuthorityPurpose::HistoricalEvent,
                revocation_cutoff: cutoff,
            },
        )?);
        let provider_issuer = provider
            .map(|certificate| {
                state
                    .issuer(&certificate.issuer_key_id, policy.now_unix_seconds)
                    .ok_or_else(|| {
                        TrellisClientError::AuthorizationUnavailable(
                            "provider issuer unavailable".into(),
                        )
                    })
            })
            .transpose()?;
        Ok(
            AuthorizationVerificationCore::new().verify_event(EventVerificationInput {
                authority,
                event: input.event,
                payload: input.payload,
                proof: input.proof,
                policy: &policy,
                known_revoked: cutoff.is_some(),
                catalog,
                catalog_issuer: &state
                    .issuer(&catalog.issuer_key_id, policy.now_unix_seconds)
                    .ok_or_else(|| {
                        TrellisClientError::AuthorizationUnavailable(
                            "catalog issuer unavailable".into(),
                        )
                    })?,
                action: input.action,
                original_publication: Some(input.original_publication),
                provider: provider
                    .map(|certificate| &**certificate)
                    .zip(provider_issuer.as_ref()),
            })?,
        )
    }

    /// Authenticate success and error bytes before a pending request can be consumed.
    /// An invalid reply leaves the caller's pending request available for a later valid reply.
    pub async fn verify_response<'a>(
        &self,
        transport: &async_nats::Client,
        input: ResponseVerificationInput<'a>,
    ) -> Result<VerifiedAuthorizationResponse<'a>, TrellisClientError> {
        let ResponseVerificationInput {
            pending_request,
            pending_request_digest,
            action,
            expected_deployment_id,
            received_subject,
            response,
            payload,
            proof,
        } = input;
        let provider = self
            .resolve_authority(transport, &response.provider_authority_digest)
            .await?;
        let material = self
            .material(
                transport,
                MaterialKind::Provider,
                &response.provider_certificate_digest,
            )
            .await?;
        let RegistryMaterial::Provider(certificate) = material.as_ref() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "wrong provider material".into(),
            ));
        };
        let issuer = self.issuer(transport, &certificate.issuer_key_id).await?;
        let policy = self.policy();
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        state.require_live_coverage()?;
        if state.revocation_overflow_until > policy.now_unix_seconds {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation capacity reached".into(),
            ));
        }
        let known_revoked = state
            .revocations
            .contains_key(&provider.authority().authorization_session_id);
        if !state
            .issuer(&provider.authority().issuer_key_id, policy.now_unix_seconds)
            .is_some_and(|key| key.state == AuthorityIssuerState::Active)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider issuer retired".into(),
            ));
        }
        let issuer = state
            .issuer(&issuer.key_id, policy.now_unix_seconds)
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable("provider issuer unavailable".into())
            })?;
        if issuer.state != AuthorityIssuerState::Active {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "response certificate issuer retired".into(),
            ));
        }
        let payload_sha256: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(payload).into();
        verify_session_response(SessionResponseVerificationInput {
            provider: &provider,
            certificate,
            certificate_issuer: &issuer,
            pending_request,
            pending_request_digest,
            action,
            expected_deployment_id,
            received_subject,
            response,
            payload_sha256,
            proof,
            policy: &policy,
            known_revoked,
        })?;
        Ok(VerifiedAuthorizationResponse {
            provider: provider.verified(),
            response,
            payload,
        })
    }

    fn verify_authority(
        &self,
        signed: &SignedSessionAuthority,
        issuer: &AuthorityIssuerKey,
        purpose: SessionAuthorityPurpose,
    ) -> Result<Arc<VerifiedSessionAuthority>, TrellisClientError> {
        let state = self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        let policy = self.policy();
        if state.revocation_overflow_until > policy.now_unix_seconds {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation capacity reached".into(),
            ));
        }
        Ok(Arc::new(verify_session_authority(
            SessionAuthorityVerificationInput {
                issuer: &state
                    .issuer(&issuer.key_id, policy.now_unix_seconds)
                    .ok_or_else(|| {
                        TrellisClientError::AuthorizationUnavailable(
                            "issuer no longer trusted".into(),
                        )
                    })?,
                trellis_instance_id: &self.trust.instance_id,
                audience_nats_account: &self.trust.account,
                authority: signed,
                policy: &policy,
                purpose,
                revocation_cutoff: state
                    .revocations
                    .get(&signed.unsigned.authorization_session_id)
                    .map(|record| record.effective_cutoff),
            },
        )?))
    }

    pub(crate) async fn material(
        &self,
        transport: &async_nats::Client,
        kind: MaterialKind,
        digest: &str,
    ) -> Result<Arc<RegistryMaterial>, TrellisClientError> {
        if digest.len() != 43
            || !URL_SAFE_NO_PAD
                .decode(digest)
                .is_ok_and(|bytes| bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == digest)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "invalid material digest".into(),
            ));
        }
        let key = (kind, digest.to_owned());
        let cell = {
            let mut state = self.state.write().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
            })?;
            if !state.material.contains_key(&key) && state.material.len() >= MAX_CACHED_MATERIAL {
                let victim = state
                    .material
                    .iter()
                    .filter(|(_, entry)| {
                        Arc::strong_count(&entry.cell) == 1
                            && match entry.cell.get() {
                                Some(Ok(material)) => Arc::strong_count(material) == 1,
                                Some(Err(_)) => true,
                                None => false,
                            }
                    })
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(key, _)| key.clone());
                let Some(victim) = victim else {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "verification material capacity reached".into(),
                    ));
                };
                state.material.remove(&victim);
            }
            let entry = state
                .material
                .entry(key.clone())
                .or_insert_with(|| CacheEntry {
                    cell: Arc::new(OnceCell::new()),
                    last_used: Instant::now(),
                });
            entry.last_used = Instant::now();
            entry.cell.clone()
        };
        let result = cell
            .get_or_init(|| async {
                match tokio::time::timeout(RESOLUTION_TIMEOUT, async {
                    let material =
                        AuthorizationRegistryReader::material(transport, kind, digest).await?;
                    let issuer = self.issuer(transport, material.issuer_key_id()).await?;
                    // Authenticate once; each use rechecks only what can change.
                    let material = match material {
                        UnverifiedMaterial::Authority(authority) => {
                            self.verify_authority(
                                &authority,
                                &issuer,
                                SessionAuthorityPurpose::HistoricalEvent,
                            )?;
                            RegistryMaterial::Authority(Box::new(authority))
                        }
                        UnverifiedMaterial::Catalog(catalog) => {
                            RegistryMaterial::Catalog(Arc::new(AuthenticatedCatalog::authenticate(
                                catalog,
                                &issuer,
                                &self.trust.instance_id,
                                &self.trust.account,
                            )?))
                        }
                        UnverifiedMaterial::Provider(certificate) => RegistryMaterial::Provider(
                            Arc::new(AuthenticatedProviderCertificate::authenticate(
                                certificate,
                                &issuer,
                                &self.trust.instance_id,
                                &self.trust.account,
                            )?),
                        ),
                    };
                    Ok::<_, TrellisClientError>(Arc::new(material))
                })
                .await
                {
                    Ok(Ok(material)) => Ok(material),
                    _ => Err("verification material unavailable".into()),
                }
            })
            .await
            .clone();
        // A failed read is shared by existing waiters but must not permanently
        // poison a digest when the broker or material publisher recovers.
        if result.is_err() {
            let mut state = self.state.write().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
            })?;
            if state
                .material
                .get(&key)
                .is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell))
            {
                state.material.remove(&key);
            }
        }
        result.map_err(TrellisClientError::AuthorizationUnavailable)
    }

    /// Authenticate exactly the next issuer-chain link and schedule its predecessor's
    /// live retirement at the signed deadline, not at successor activation.
    pub fn accept_issuer_rotation(
        &self,
        rotation: trellis_protocol::SignedIssuerRotation,
    ) -> Result<(), TrellisClientError> {
        let mut state = self.state.write().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        if state.issuers.len() >= MAX_ISSUERS {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "issuer chain capacity reached".into(),
            ));
        }
        let successor = rotation.verify_successor(
            &state.current_issuer,
            &self.trust.instance_id,
            &self.trust.account,
            state.rotation_sequence,
            self.policy().now_unix_seconds,
        )?;
        if state.issuers.contains_key(&successor.key_id) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "issuer chain reuses a retired signer".into(),
            ));
        }
        let previous_key_id = state.current_issuer.key_id.clone();
        state
            .issuer_retirements
            .insert(previous_key_id, rotation.previous_retired_at);
        state
            .issuers
            .insert(successor.key_id.clone(), successor.clone());
        state.current_issuer = successor;
        state.rotation_sequence = rotation.sequence.get();
        drop(state);
        let _ = self.changes.send(());
        Ok(())
    }

    /// Apply HTTP-authenticated OAuth rotation links through the shared pinned owner.
    pub(crate) async fn install_oauth_rotations(
        &self,
        rotations: Vec<trellis_protocol::SignedIssuerRotation>,
    ) -> Result<(), TrellisClientError> {
        let _rotation = self.rotations.lock().await;
        for rotation in rotations {
            if rotation.sequence.get() > self.current_trust()?.rotation_sequence {
                self.accept_issuer_rotation(rotation)?;
            }
        }
        Ok(())
    }

    /// Resolve an unknown signer only through consecutive predecessor signatures.
    pub async fn issuer(
        &self,
        transport: &async_nats::Client,
        key_id: &str,
    ) -> Result<AuthorityIssuerKey, TrellisClientError> {
        if key_id.len() != 43
            || !URL_SAFE_NO_PAD
                .decode(key_id)
                .is_ok_and(|bytes| bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == key_id)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "invalid issuer digest".into(),
            ));
        }
        match self.forward_issuer(transport, key_id).await {
            Ok(issuer) => return Ok(issuer),
            Err(error) => {
                let state = self.state.read().map_err(|_| {
                    TrellisClientError::AuthorizationUnavailable(
                        "authority owner unavailable".into(),
                    )
                })?;
                if state.issuers.contains_key(key_id) {
                    return Err(error);
                }
            }
        }
        // Only the active forward-authenticated pin may attest a historical key.
        // This read can bootstrap mirror verification, but grants no application use.
        let _permit = self.metadata_reads.try_acquire().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("metadata read capacity reached".into())
        })?;
        tokio::time::timeout(RESOLUTION_TIMEOUT, async {
            let request = IssuerMetadataRequest {
                trellis_instance_id: self.trust.instance_id.clone(),
                audience_nats_account: self.trust.account.clone(),
                request_nonce: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>()),
                query: IssuerMetadataQuery::IssuerKey {
                    key_id: key_id.into(),
                },
            };
            let subject = trellis_protocol::ISSUER_METADATA_SUBJECT;
            let reply = format!("{}.metadata", transport.new_inbox());
            let mut replies = transport.subscribe(reply.clone()).await.map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("metadata reply unavailable".into())
            })?;
            transport.flush().await.map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "metadata transport unavailable".into(),
                )
            })?;
            transport
                .publish_with_reply(subject, reply.clone(), serde_json::to_vec(&request)?.into())
                .await
                .map_err(|_| {
                    TrellisClientError::AuthorizationUnavailable(
                        "metadata request unavailable".into(),
                    )
                })?;
            let mut candidates = 0usize;
            let mut bytes = 0usize;
            while let Some(message) = replies.next().await {
                candidates += 1;
                bytes = bytes.saturating_add(message.payload.len());
                if candidates > 16 || bytes > trellis_protocol::MAX_ADMISSION_RESPONSE_BYTES {
                    break;
                }
                if let Ok(verified) = self
                    .verify_metadata_inner(transport, &message.payload, &request, subject, &reply)
                    .await
                {
                    if !matches!(verified.result(), IssuerMetadataResult::Value { .. }) {
                        break;
                    }
                    let state = self.state.read().map_err(|_| {
                        TrellisClientError::AuthorizationUnavailable(
                            "authority owner unavailable".into(),
                        )
                    })?;
                    return state
                        .issuer(key_id, self.policy().now_unix_seconds)
                        .ok_or_else(|| {
                            TrellisClientError::AuthorizationUnavailable(
                                "historical issuer unavailable".into(),
                            )
                        });
                }
            }
            Err(TrellisClientError::AuthorizationUnavailable(
                "historical issuer unavailable".into(),
            ))
        })
        .await
        .map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("historical issuer read timed out".into())
        })?
    }

    async fn forward_issuer(
        &self,
        transport: &async_nats::Client,
        key_id: &str,
    ) -> Result<AuthorityIssuerKey, TrellisClientError> {
        if key_id.len() != 43
            || !URL_SAFE_NO_PAD
                .decode(key_id)
                .is_ok_and(|bytes| bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == key_id)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "invalid issuer digest".into(),
            ));
        }
        tokio::time::timeout(RESOLUTION_TIMEOUT, async {
            {
                let state = self.state.read().map_err(|_| {
                    TrellisClientError::AuthorizationUnavailable(
                        "authority owner unavailable".into(),
                    )
                })?;
                if let Some(issuer) = state.issuer(key_id, self.policy().now_unix_seconds) {
                    return Ok(issuer);
                }
            }
            let _rotation = self.rotations.lock().await;
            for _ in 0..MAX_ISSUERS {
                let sequence = {
                    let state = self.state.read().map_err(|_| {
                        TrellisClientError::AuthorizationUnavailable(
                            "authority owner unavailable".into(),
                        )
                    })?;
                    if let Some(issuer) = state.issuer(key_id, self.policy().now_unix_seconds) {
                        return Ok(issuer);
                    }
                    if state.issuers.len() >= MAX_ISSUERS {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "issuer chain capacity reached".into(),
                        ));
                    }
                    state.rotation_sequence
                };
                let next_sequence = sequence.checked_add(1).ok_or_else(|| {
                    TrellisClientError::AuthorizationUnavailable("issuer chain exhausted".into())
                })?;
                let rotation = AuthorizationRegistryReader::rotation(transport, next_sequence)
                    .await?
                    .ok_or_else(|| {
                        TrellisClientError::AuthorizationUnavailable("unknown issuer".into())
                    })?;
                self.accept_issuer_rotation(rotation)?;
            }
            Err(TrellisClientError::AuthorizationUnavailable(
                "unknown issuer".into(),
            ))
        })
        .await
        .map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("issuer resolution timed out".into())
        })?
    }

    /// Authenticate a mirror update before making its retirement sticky.
    pub async fn observe_revocation(
        &self,
        transport: &async_nats::Client,
        record: SignedSessionRevocation,
    ) -> Result<(), TrellisClientError> {
        self.issuer(transport, &record.issuer_key_id).await?;
        self.accept_revocation(record)
    }

    /// Authenticate retirement evidence using an already trusted issuer and retain it
    /// across transport replacement. Missing signer trust requires bounded resolution first.
    pub fn accept_revocation(
        &self,
        record: SignedSessionRevocation,
    ) -> Result<(), TrellisClientError> {
        let mut state = self.state.write().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
        })?;
        let now = self.policy().now_unix_seconds;
        let issuer = state.issuer(&record.issuer_key_id, now).ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable("revocation issuer unavailable".into())
        })?;
        record.verify(&issuer, &self.trust.instance_id, &self.trust.account)?;
        state
            .revocations
            .retain(|_, known| known.latest_context_expiry > now);
        if record.latest_context_expiry <= now {
            return Ok(());
        }
        if let Some(known) = state.revocations.get_mut(&record.authorization_session_id) {
            if known.effective_cutoff != record.effective_cutoff {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "conflicting session cutoff".into(),
                ));
            }
            if record.latest_context_expiry > known.latest_context_expiry {
                *known = record;
            }
        } else if state.revocations.len() < MAX_OBSERVED_REVOCATIONS {
            state
                .revocations
                .insert(record.authorization_session_id.clone(), record);
        } else {
            state.revocation_overflow_until = state
                .revocation_overflow_until
                .max(record.latest_context_expiry);
            drop(state);
            let _ = self.changes.send(());
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation capacity reached".into(),
            ));
        }
        drop(state);
        let _ = self.changes.send(());
        Ok(())
    }

    /// Invalidate pending snapshots without erasing signed retirements.
    /// A complete mirror on a still-connected carrier may cover retained work;
    /// a displaced snapshot cannot restore coverage after a transport change.
    pub(crate) fn fence_revocation_coverage(&self) {
        if let Ok(mut state) = self.state.write() {
            let changed = state.revocation_covered || state.revocation_epoch.is_some();
            state.revocation_pending_epoch = None;
            if !state.revocation_epoch.as_ref().is_some_and(|transport| {
                transport.connection_state() == async_nats::connection::State::Connected
            }) {
                state.revocation_covered = false;
                state.revocation_epoch = None;
                state.revocation_predecessor_epoch = None;
            }
            drop(state);
            if changed {
                let _ = self.changes.send(());
            }
        }
    }

    /// Run one logical-session mirror for this attachment, not one watch per peer.
    /// A replacement stages its snapshot while a healthy predecessor continues
    /// covering retained work. Snapshot completion atomically hands over its
    /// scoped epoch; failure never erases signed retirements or revives a stopped
    /// predecessor.
    pub async fn run_revocations(
        &self,
        transport: &async_nats::Client,
        mut shutdown: watch::Receiver<bool>,
        ready: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(), TrellisClientError> {
        let epoch = Arc::new(transport.clone());
        {
            let mut state = self.state.write().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("authority owner unavailable".into())
            })?;
            // Preparing a successor is not loss of a healthy shared mirror.
            // Only a complete authenticated snapshot may displace its epoch.
            state.revocation_pending_epoch = Some(Arc::clone(&epoch));
        }
        let coverage = RevocationCoverage {
            owner: self.clone(),
            epoch,
        };
        let _ = self.changes.send(());
        if *shutdown.borrow() {
            return Ok(());
        }
        let stream = jetstream::new(transport.clone())
            .get_stream(format!("KV_{REVOCATION_BUCKET}"))
            .await
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("revocation mirror unavailable".into())
            })?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                description: Some("Trellis logical-session retirement mirror".into()),
                filter_subject: format!("$KV.{REVOCATION_BUCKET}.>"),
                deliver_policy: jetstream::consumer::DeliverPolicy::LastPerSubject,
                replay_policy: jetstream::consumer::ReplayPolicy::Instant,
                ack_policy: jetstream::consumer::AckPolicy::None,
                ..Default::default()
            })
            .await
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("revocation mirror unavailable".into())
            })?;
        let consumer_name = consumer.cached_info().name.clone();
        let mut updates = consumer.messages().await.map_err(|_| {
            TrellisClientError::AuthorizationUnavailable(
                "revocation mirror subscription unavailable".into(),
            )
        })?;
        transport.flush().await.map_err(|_| {
            TrellisClientError::AuthorizationUnavailable(
                "revocation mirror registration unavailable".into(),
            )
        })?;
        // Pull delivery cannot run before the reply subscription is installed.
        // Re-read after registration: updates may already be buffered locally,
        // so both pending and delivered sequence must precede an empty readiness.
        let initial = stream.consumer_info(&consumer_name).await.map_err(|_| {
            TrellisClientError::AuthorizationUnavailable(
                "revocation snapshot status unavailable".into(),
            )
        })?;
        let initial_delivered = initial.delivered.consumer_sequence;
        let mut ready = Some(ready);
        if initial_delivered == 0 && initial.num_pending == 0 {
            coverage.ready()?;
            if let Some(ready) = ready.take() {
                let _ = ready.send(());
            }
        }
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                update = updates.next() => {
                    let Some(Ok(entry)) = update else {
                        return Err(TrellisClientError::AuthorizationUnavailable(
                            "revocation mirror disconnected".into(),
                        ));
                    };
                    let info = entry.info().map_err(|_| {
                        TrellisClientError::AuthorizationUnavailable(
                            "revocation mirror metadata invalid".into(),
                        )
                    })?;
                    let snapshot_complete =
                        info.pending == 0 && info.consumer_sequence >= initial_delivered;
                    let key = entry
                        .subject
                        .strip_prefix(&format!("$KV.{REVOCATION_BUCKET}."));
                    let deleted = entry
                        .headers
                        .as_ref()
                        .and_then(|headers| headers.get("KV-Operation"))
                        .is_some_and(|value| matches!(value.as_str(), "DEL" | "PURGE"));
                    if !deleted && entry.payload.len() <= MAX_SESSION_AUTHORITY_BYTES {
                        if let Ok(record) =
                            serde_json::from_slice::<SignedSessionRevocation>(&entry.payload)
                        {
                            if key == Some(record.authorization_session_id.as_str()) {
                                // Bad signed inputs never erase prior retirement evidence.
                                // Capacity failure has already made acceptance unavailable.
                                if let Err(
                                    error @ TrellisClientError::AuthorizationUnavailable(_),
                                ) = self.observe_revocation(transport, record).await
                                {
                                    return Err(error);
                                }
                            }
                        }
                    }
                    if snapshot_complete && ready.is_some() {
                        coverage.ready()?;
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(());
                        }
                    }
                }
            }
        }
    }
}
