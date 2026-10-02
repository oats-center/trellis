use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Value};
use trellis_idl::project::{GenerateConfig, PackageManifest, PackageMetadata};
use trellis_idl::{
    canonical_package, compile_project, CanonicalMode, PackageEvidence, PackageSourceEvidence,
    SourceUnit,
};
use trellis_local_nats::{ManagedNatsServer, NatsBinarySource, NatsOutput, NatsServerBinary};
use trellis_protocol::{GrantSet, ParticipantKind, ParticipantResourceKind, PermissionTarget};

use crate::platform::auth::{
    application::repository::SessionCreation,
    authority::{IssuanceConnection, IssuanceCredential},
    context::{AuthorizationContextIssueRequest, AuthorizationContextService},
    evidence::PackageEvidenceInput,
    resources::{reconcile_resource, resource_id, ReconcileResourcePayload},
    ApprovalMode, AuthorizationStateError, DelegationCeiling, DeploymentRepository,
    GrantBindingReplacement, GrantBindingState, GrantOwnerKind, IdempotencyResultRecord,
    NewSession, ParticipantBindingRecord, SessionRecord, SessionRepository,
    SqliteAuthorizationStore,
};

const NOW: i64 = 1_800_000_000_000;

fn proof(purpose: &str) -> IdempotencyResultRecord {
    IdempotencyResultRecord {
        scope_key: trellis_protocol::digest_json(&json!([purpose])).unwrap(),
        purpose: purpose.to_owned(),
        signer_id: "resource-review".to_owned(),
        request_id: purpose.to_owned(),
        request_digest: trellis_protocol::digest_json(&json!([purpose, "request"])).unwrap(),
        result: Value::Null,
        created_at: NOW,
        expires_at: NOW + 60_000,
    }
}

#[tokio::test]
async fn exact_resource_approval_survives_pending_partial_and_complete_issuance() {
    // Real broker and signer exercise both initial issuance and commit-time resolution.
    let dir = tempfile::tempdir().unwrap();
    let cache = std::env::var_os("TRELLIS_CACHE_DIR").map(PathBuf::from);
    let binary = NatsServerBinary::resolve(
        &std::env::var_os("TRELLIS_TEST_NATS_BIN")
            .map(|path| NatsBinarySource::Path(path.into()))
            .unwrap_or(NatsBinarySource::DownloadPinned),
        cache.as_deref(),
    )
    .unwrap();
    let listeners: Vec<_> = (0..3)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<_> = listeners
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    let config = dir.path().join("nats.conf");
    std::fs::write(&config, format!(
        "host: 127.0.0.1\nport: {}\nhttp_port: {}\nwebsocket {{ host: 127.0.0.1, port: {}, no_tls: true }}\njetstream {{ store_dir: '{}' }}\n",
        ports[0], ports[1], ports[2], dir.path().join("jetstream").display(),
    )).unwrap();
    drop(listeners);
    let mut broker = ManagedNatsServer::start(
        &binary,
        &config,
        ports[0],
        ports[1],
        ports[2],
        &dir.path().join("nats.pid"),
        &NatsOutput::Log {
            path: dir.path().join("nats.log"),
            mirror: false,
        },
    )
    .unwrap();
    let nats = async_nats::connect(format!("nats://127.0.0.1:{}", ports[0]))
        .await
        .unwrap();
    let seed = dir.path().join("issuer.seed");
    std::fs::write(&seed, URL_SAFE_NO_PAD.encode([42; 32])).unwrap();

    for required in [false, true] {
        let store = SqliteAuthorizationStore::open_in_memory().unwrap();
        let actor =
            crate::platform::auth::tests::conformance::fixtures::install_login_mutation_actor(
                &store, NOW,
            )
            .await
            .unwrap();
        let source = format!(
            r#"
model Empty {{}}
api access@v1 {{
  title "Access"; description "Independent work.";
  rpc Public {{ input Empty; output Empty; }}
  capabilities {{ public {{ allows {{ rpc Public; }} }} }}
}}
service Provider {{ implements access; }}
app Client {{
  use access {{ rpc Public; }}
  kv {optional} first {{ title "First"; description "First cache."; schema Empty; history 1; ttl 5m; }}
  kv {optional} second {{ title "Second"; description "Second cache."; schema Empty; history 1; ttl 5m; }}
}}
"#,
            optional = if required { "" } else { "optional" }
        );
        let manifest = PackageManifest {
            package: PackageMetadata {
                name: "resource-review".into(),
                version: "1.0.0".parse().unwrap(),
            },
            sources: BTreeMap::from([("contract".into(), "contract.trellis".into())]),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        };
        let graph = compile_project(
            &manifest,
            vec![SourceUnit {
                alias: "contract".into(),
                path: "contract.trellis".into(),
                source,
            }],
            BTreeMap::new(),
        )
        .unwrap();
        let evidence = PackageEvidence {
            root_package: "resource-review".into(),
            root_digest: graph.root_digest().into(),
            packages: vec![PackageSourceEvidence {
                name: "resource-review".into(),
                version: "1.0.0".parse().unwrap(),
                digest: graph.root_digest().into(),
                source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation)
                    .unwrap(),
            }],
        };
        let mut client = None;
        for path in ["Provider", "Client"] {
            let (participant, evidence_json) = ParticipantBindingRecord::from_package_evidence(
                &PackageEvidenceInput {
                    package_digest: evidence.root_digest.clone(),
                    package_evidence: evidence.clone(),
                    participant_path: path.into(),
                },
                NOW,
            )
            .unwrap();
            store
                .install_participant(
                    actor.clone(),
                    participant.clone(),
                    "resource-review".into(),
                    evidence_json,
                    false,
                    (0, proof(&format!("install.{path}"))),
                )
                .await
                .unwrap();
            if path == "Provider" {
                store
                    .create_deployment_profile(
                        crate::platform::auth::application::repository::DeploymentProfileCreation {
                            principal: crate::platform::auth::PrincipalRecord {
                                principal_id: "resource-provider".into(),
                                kind: crate::platform::auth::PrincipalKind::Service,
                                state: crate::platform::auth::PrincipalState::Active,
                                created_at: NOW,
                                updated_at: NOW,
                                version: 1,
                                disabled_at: None,
                                revoked_at: None,
                            },
                            profile: crate::platform::auth::DeploymentProfileRecord {
                                deployment_id: "resource-provider".into(),
                                kind: crate::platform::auth::PrincipalKind::Service,
                                display_name: "Resource provider".into(),
                                participant_id: Some(participant.participant_id.clone()),
                                portal_id: None,
                                review_mode: None,
                                requires_device_delegation: false,
                                expires_at: None,
                                state: crate::platform::auth::DeploymentProfileState::Active,
                                created_at: NOW,
                                updated_at: NOW,
                                version: 1,
                            },
                            idempotency: proof("provider.create"),
                            actions: Vec::new(),
                        },
                    )
                    .await
                    .unwrap();
            } else {
                client = Some(participant);
            }
        }
        let participant = client.unwrap();
        crate::platform::auth::resolve_api_bindings(&store, &participant, None)
            .await
            .unwrap();
        let participant_id = participant.participant_id.clone();
        let projection = participant.resolve().unwrap();
        let grants = GrantSet::new(
            projection
                .required_grants
                .permissions()
                .iter()
                .chain(
                    projection
                        .optional_grant_bundles
                        .values()
                        .flat_map(|g| g.permissions()),
                )
                .cloned()
                .collect(),
        );
        let approved_resources =
            crate::platform::auth::policy::participant_resource_commitments(&participant).unwrap();
        store
            .set_grant_binding(
                GrantBindingReplacement {
                    owner_kind: GrantOwnerKind::User,
                    owner_id: actor.principal_id.clone(),
                    participant_id: participant_id.clone(),
                    installed_revision: 1,
                    grants: grants.clone(),
                    approval_mode: ApprovalMode::Exact,
                    approved_capabilities: Vec::new(),
                    approved_resources: approved_resources.clone(),
                    delegation_ceiling: DelegationCeiling {
                        capabilities: Vec::new(),
                        exact_restrictions: Some(grants.clone()),
                        platform_privileges: Vec::new(),
                    },
                    approval_decision_digest: "A".repeat(43),
                    companion_approved: false,
                    platform_privileges: Vec::new(),
                    expected_revision: 0,
                    expected_current_installed_revision: Some(1),
                    state: GrantBindingState::Active,
                    expires_at: None,
                    provenance: None,
                },
                proof("grant.approve"),
            )
            .await
            .unwrap();
        let original = store
            .get_grant_binding(
                GrantOwnerKind::User,
                actor.principal_id.clone(),
                participant_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        let session_id = ulid::Ulid::new().to_string();
        let public_key = actor.session_public_key.clone();
        store
            .create_session(SessionCreation {
                session: SessionRecord::from_new(NewSession {
                    session_id: session_id.clone(),
                    principal_id: actor.principal_id.clone(),
                    participant_id: participant_id.clone(),
                    participant_kind: ParticipantKind::App,
                    installed_revision: 1,
                    session_public_key: public_key.clone(),
                    created_at: NOW,
                    expires_at: Some(NOW + 3_600_000),
                })
                .unwrap(),
                idempotency: proof("session.create"),
                actions: Vec::new(),
            })
            .await
            .unwrap();
        let contexts = AuthorizationContextService::start(
            Arc::new(store.clone()),
            nats.clone(),
            crate::config::AuthorizationConfig {
                issuer_signing_seed_file: seed.clone(),
                context_lifetime_seconds: 3_600,
                refresh_lead_seconds: 60,
                refresh_jitter_seconds: 0,
                minimum_context_lifetime_seconds: 120,
                maximum_bootstrap_jwt_lifetime_seconds: 300,
                allowed_clock_skew_seconds: 0,
                maximum_context_bytes: 1_048_576,
                maximum_permissions: 16_384,
                context_bucket: format!("REVIEW_CONTEXTS_{}", required),
                registry_replicas: 1,
            },
            "http://127.0.0.1".into(),
            NOW / 1_000,
            "APP".into(),
        )
        .await
        .unwrap();
        let connection = IssuanceConnection {
            credential: IssuanceCredential::Login(session_id),
            connection_id: ulid::Ulid::new().to_string(),
            session_public_key: public_key,
        };
        let issue = |step: usize| AuthorizationContextIssueRequest {
            connection: connection.clone(),
            request_id: format!("issue.{step}"),
            request_digest: trellis_protocol::digest_json(&json!([step])).unwrap(),
        };
        for completed in 0..=2 {
            let result = contexts
                .issue_with_state(issue(completed), NOW / 1_000 + completed as i64)
                .await;
            if required && completed < 2 {
                assert!(
                    matches!(
                        result,
                        Err(AuthorizationStateError::MaterializationStale
                            | AuthorizationStateError::RequiredResourceUnavailable(_))
                    ),
                    "{result:?}"
                );
            } else {
                let (_, state) = result.unwrap();
                let expected = GrantSet::new(
                    grants
                        .permissions()
                        .iter()
                        .filter(|atom| match atom.target() {
                            PermissionTarget::ParticipantResource { name, .. } => {
                                completed == 2 || (completed == 1 && name == "first")
                            }
                            _ => true,
                        })
                        .cloned()
                        .collect(),
                );
                assert_eq!(state.grant_set, expected);
                assert!(state.grant_set.permissions().iter().any(|atom| !matches!(
                    atom.target(),
                    PermissionTarget::ParticipantResource { .. }
                )));
                // A new request on the same connection renews through the real issuer.
                let (_, renewed) = contexts
                    .issue_with_state(issue(completed + 10), NOW / 1_000 + completed as i64 + 1)
                    .await
                    .unwrap();
                assert_eq!(renewed.grant_set, expected);
            }
            let retained = store
                .get_grant_binding(
                    GrantOwnerKind::User,
                    actor.principal_id.clone(),
                    participant_id.clone(),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.grants, original.grants);
            assert_eq!(retained.revision, original.revision);
            assert_eq!(retained.approved_resources, original.approved_resources);
            if completed < 2 {
                let name = if completed == 0 { "first" } else { "second" };
                let payload = ReconcileResourcePayload {
                    resource_id: resource_id(
                        GrantOwnerKind::User,
                        &actor.principal_id,
                        &participant_id,
                        ParticipantResourceKind::Kv,
                        name,
                    ),
                    binding_revision: original.revision,
                    catalog_revision: 1,
                };
                reconcile_resource(&nats, &store, payload, NOW + completed as i64)
                    .await
                    .unwrap();
            }
        }
    }
    broker.stop().unwrap();
}
