use super::*;
use crate::platform::auth::tests::{fixture, mutation, NOW};

#[test]
fn device_aggregate_compatible_apply_preserves_pinned_authority_and_remove_cannot_revive_identity()
{
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use sha2::{Digest as _, Sha256};
    let f = fixture();
    let source = crate::platform::auth::tests::SOURCE.replace("service Reader", "device Reader");
    crate::platform::auth::tests::accept(&f.store, &f.admin, &source, false, NOW);
    let evidence: AuthPackageEvidence = f
        .store
        .transaction(|connection| {
            let value: String = connection.query_row(
                "SELECT definition_json \
                FROM auth_accepted_apis \
                WHERE api_id=(SELECT api_id \
                FROM auth_capability_identities \
                WHERE capability_id=?1)",
                [&f.read],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str(&value)?)
        })
        .unwrap();
    let graph = trellis_idl::compile_evidence(
        serde_json::from_value(serde_json::to_value(&evidence).unwrap()).unwrap(),
    )
    .unwrap();
    let create = AuthDeploymentCreateRequest {
        package_evidence: evidence.clone(),
        participant_id: graph
            .root_package()
            .participants()
            .keys()
            .next()
            .unwrap()
            .to_string(),
        kind: AuthDeploymentKind::Device,
        title: "Reader device".to_string().into(),
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let created = create_deployment(&f.store, &mutation(&f.admin, &create), &create, NOW).unwrap();
    let public = ed25519_dalek::SigningKey::from_bytes(&[19; 32]).verifying_key();
    let provision = AuthInstanceProvisionRequest {
        deployment_id: created.deployment.deployment_id,
        identity_public_key: Some(URL_SAFE_NO_PAD.encode(public.as_bytes()).into()),
        title: "Reader".to_string().into(),
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let provisioned = provision_instance(
        &f.store,
        &mutation(&f.admin, &provision),
        &provision,
        "device",
        NOW,
    )
    .unwrap();
    assert!(provisioned.provisioning_secret.is_none());
    let mut admission = f.admission.clone();
    admission.refresh_family_id = None;
    admission.credential = super::super::Credential::ProvisionedIdentity(
        URL_SAFE_NO_PAD.encode(Sha256::digest(public.as_bytes())),
    );
    let issued =
        crate::platform::auth::tests::issue_authority(&f.store, &admission, None, NOW).unwrap();
    assert_eq!(
        issued.authority.unsigned.principal_kind,
        trellis_protocol::PrincipalKind::Device
    );
    let unchanged = AuthDeploymentApplyRequest {
        deployment_id: created.deployment.deployment_id,
        expected_revision: created.deployment.revision,
        package_evidence: evidence,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let unchanged_identity = mutation(&f.admin, &unchanged);
    let applied = apply_deployment(&f.store, &unchanged_identity, &unchanged, NOW + 1).unwrap();
    crate::platform::auth::tests::issue_authority(&f.store, &admission, None, NOW + 1).unwrap();
    assert!(matches!(
        apply_deployment(
            &f.store,
            &mutation(&f.admin, &unchanged),
            &unchanged,
            NOW + 1
        ),
        Err(AuthError::Conflict)
    ));
    let changed = source.replace(
        "required capability read; capability write;",
        "required capability read; required capability write;",
    );
    crate::platform::auth::tests::accept(&f.store, &f.admin, &changed, false, NOW + 2);
    let evidence = f
        .store
        .transaction(|connection| {
            let value: String = connection.query_row(
                "SELECT definition_json \
                FROM auth_accepted_apis \
                WHERE api_id=(SELECT api_id \
                FROM auth_capability_identities \
                WHERE capability_id=?1)",
                [&f.read],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str(&value)?)
        })
        .unwrap();
    let apply = AuthDeploymentApplyRequest {
        package_evidence: evidence,
        expected_revision: applied.deployment.revision,
        ..unchanged.clone()
    };
    let applied = apply_deployment(&f.store, &mutation(&f.admin, &apply), &apply, NOW + 2).unwrap();
    let pinned =
        crate::platform::auth::tests::issue_authority(&f.store, &admission, None, NOW + 2).unwrap();
    assert_eq!(
        pinned.authority.unsigned.capabilities,
        issued.authority.unsigned.capabilities
    );
    assert_eq!(
        apply_deployment(&f.store, &unchanged_identity, &unchanged, NOW + 3)
            .unwrap()
            .deployment
            .revision
            .0
             .0,
        2
    );
    let instance = f
        .store
        .transaction(|connection| {
            read_instance(connection, &provisioned.instance.instance_id.to_string())
        })
        .unwrap();
    let enable = AuthInstanceMutationRequest {
        instance_id: instance.instance_id,
        expected_revision: instance.revision,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        change_instance(
            &f.store,
            &mutation(&f.admin, &enable),
            &enable,
            "device",
            "active",
            NOW + 3
        ),
        Err(AuthError::Conflict)
    ));
    let remove = AuthDeploymentMutationRequest {
        deployment_id: created.deployment.deployment_id,
        expected_revision: applied.deployment.revision,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let removed = change_deployment(
        &f.store,
        &mutation(&f.admin, &remove),
        &remove,
        "revoked",
        NOW + 4,
    )
    .unwrap();
    assert!(matches!(
        crate::platform::auth::tests::issue_authority(&f.store, &admission, None, NOW + 4),
        Err(AuthError::Denied)
    ));
    let enable = AuthDeploymentMutationRequest {
        expected_revision: removed.deployment.revision,
        ..remove
    };
    assert!(matches!(
        change_deployment(
            &f.store,
            &mutation(&f.admin, &enable),
            &enable,
            "active",
            NOW + 4
        ),
        Err(AuthError::Conflict)
    ));
}

#[test]
fn typed_creation_and_provisioning_bind_admission_and_lifecycle_replay() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use sha2::{Digest as _, Sha256};
    let f = fixture();
    let evidence: AuthPackageEvidence = f
        .store
        .transaction(|connection| {
            let value: String = connection.query_row(
                "SELECT definition_json \
                FROM auth_accepted_apis \
                WHERE api_id=(SELECT api_id \
                FROM auth_capability_identities \
                WHERE capability_id=?1)",
                [&f.read],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str(&value)?)
        })
        .unwrap();
    let graph = trellis_idl::compile_evidence(
        serde_json::from_value(serde_json::to_value(&evidence).unwrap()).unwrap(),
    )
    .unwrap();
    let participant = graph
        .root_package()
        .participants()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let create = AuthDeploymentCreateRequest {
        package_evidence: evidence.clone(),
        participant_id: participant,
        kind: AuthDeploymentKind::Service,
        title: "Reader deployment".to_string().into(),
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        create_deployment(&f.store, &mutation(&f.principal, &create), &create, NOW),
        Err(AuthError::Denied)
    ));
    let creation_identity = mutation(&f.admin, &create);
    let created = create_deployment(&f.store, &creation_identity, &create, NOW).unwrap();
    let missing_key = AuthInstanceProvisionRequest {
        deployment_id: created.deployment.deployment_id,
        identity_public_key: None,
        title: "Reader instance".to_string().into(),
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let failed_identity = mutation(&f.admin, &missing_key);
    let principal_count = f
        .store
        .transaction(|connection| {
            Ok(
                connection.query_row("SELECT count(*) FROM auth_principals", [], |row| {
                    row.get::<_, u64>(0)
                })?,
            )
        })
        .unwrap();
    assert!(matches!(
        provision_instance(&f.store, &failed_identity, &missing_key, "service", NOW),
        Err(AuthError::Invalid(code)) if code == "identity_public_key_required"
    ));
    f.store
        .transaction(|connection| {
            let instances = read_instances(
                connection,
                &AuthInstancesListRequest {
                    deployment_id: Some(created.deployment.deployment_id),
                    page: None,
                    extra: Default::default(),
                },
                "service",
            )?;
            assert!(instances.items.is_empty());
            assert_eq!(
                connection.query_row("SELECT count(*) FROM auth_principals", [], |row| row
                    .get::<_, u64>(0))?,
                principal_count
            );
            Ok(())
        })
        .unwrap();
    let client_key = ed25519_dalek::SigningKey::from_bytes(&[27; 32]);
    let public = client_key.verifying_key();
    let provision = AuthInstanceProvisionRequest {
        identity_public_key: Some(URL_SAFE_NO_PAD.encode(public.as_bytes()).into()),
        ..missing_key
    };
    // A rejected prerequisite does not consume the caller's idempotency key.
    let provision_identity = Mutation {
        request_digest: super::super::sqlite::digest(&provision).unwrap(),
        ..failed_identity
    };
    let provisioned =
        provision_instance(&f.store, &provision_identity, &provision, "service", NOW).unwrap();
    f.store
        .transaction(|connection| {
            let deployment =
                read_deployment(connection, &created.deployment.deployment_id.to_string())?;
            assert_eq!(deployment.title, "Reader deployment");
            let deployments = read_deployments(
                connection,
                &AuthPageRequest {
                    search: Some("Reader deployment".to_string().into()),
                    page: None,
                    extra: Default::default(),
                },
            )?;
            assert_eq!(deployments.items[0], deployment);
            let instances = read_instances(
                connection,
                &AuthInstancesListRequest {
                    deployment_id: Some(deployment.deployment_id),
                    page: None,
                    extra: Default::default(),
                },
                "service",
            )?;
            assert_eq!(instances.items[0].principal.title, "Reader instance");
            assert_eq!(instances.items[0], provisioned.instance);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        provision_instance(
            &f.store,
            &provision_identity,
            &provision,
            "service",
            NOW + 1
        )
        .unwrap(),
        provisioned
    );
    assert_eq!(
        URL_SAFE_NO_PAD.encode(public.as_bytes()),
        provisioned
            .instance
            .identity_public_key
            .as_ref()
            .unwrap()
            .as_ref()
    );
    let mut admission = f.admission.clone();
    admission.refresh_family_id = None;
    admission.credential = super::super::Credential::ProvisionedIdentity(
        URL_SAFE_NO_PAD.encode(Sha256::digest(public.as_bytes())),
    );
    let issued =
        crate::platform::auth::tests::issue_authority(&f.store, &admission, None, NOW).unwrap();
    let ephemeral = nkeys::KeyPair::new_user();
    let signed = trellis_protocol::sign_provisioned_admission(
        trellis_protocol::ProvisionedAdmission {
            format: trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1.into(),
            trellis_instance_id: f.store.settings.instance_id.clone(),
            audience_nats_account: f.store.settings.account.clone(),
            credential_id: URL_SAFE_NO_PAD.encode(Sha256::digest(public.as_bytes())),
            runtime_id: URL_SAFE_NO_PAD.encode([28; 16]),
            ephemeral_nkey: ephemeral.public_key(),
            issued_at: NOW,
            expires_at: NOW + 60,
        },
        &client_key,
    )
    .unwrap();
    let connected = crate::platform::auth::tests::issue_provisioned_authority(
        &f.store,
        &serde_json::to_vec(&signed).unwrap(),
        &ephemeral.public_key(),
        NOW,
    )
    .unwrap();
    assert_eq!(
        connected.authority.unsigned.principal_id,
        provisioned.instance.principal.principal_id.to_string()
    );
    let impostor = trellis_protocol::sign_provisioned_admission(
        signed.credential.clone(),
        &ed25519_dalek::SigningKey::from_bytes(&[29; 32]),
    )
    .unwrap();
    assert!(crate::platform::auth::tests::issue_provisioned_authority(
        &f.store,
        &serde_json::to_vec(&impostor).unwrap(),
        &ephemeral.public_key(),
        NOW
    )
    .is_err());
    let replacement_key = AuthInstanceProvisionRequest {
        identity_public_key: Some(
            URL_SAFE_NO_PAD
                .encode(
                    ed25519_dalek::SigningKey::from_bytes(&[29; 32])
                        .verifying_key()
                        .as_bytes(),
                )
                .into(),
        ),
        ..provision.clone()
    };
    let replacement_identity = Mutation {
        request_digest: super::super::sqlite::digest(&replacement_key).unwrap(),
        ..provision_identity.clone()
    };
    assert!(matches!(
        provision_instance(
            &f.store,
            &replacement_identity,
            &replacement_key,
            "service",
            NOW
        ),
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        crate::platform::auth::tests::issue_provisioned_authority(
            &f.store,
            &serde_json::to_vec(&signed).unwrap(),
            &ephemeral.public_key(),
            NOW
        )
        .unwrap()
        .authority
        .unsigned
        .principal_id,
        provisioned.instance.principal.principal_id.to_string()
    );
    assert_eq!(
        issued.authority.unsigned.principal_id,
        provisioned.instance.principal.principal_id.to_string()
    );
    let instance_command = AuthInstanceMutationRequest {
        instance_id: provisioned.instance.instance_id,
        expected_revision: provisioned.instance.revision,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let disable_identity = mutation(&f.admin, &instance_command);
    let disabled = change_instance(
        &f.store,
        &disable_identity,
        &instance_command,
        "service",
        "disabled",
        NOW + 1,
    )
    .unwrap();
    assert!(!disabled.instance.principal.enabled);
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    assert!(f
        .store
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
    assert!(matches!(
        crate::platform::auth::tests::issue_authority(&f.store, &admission, None, NOW + 1),
        Err(AuthError::Denied)
    ));
    assert!(matches!(
        change_instance(
            &f.store,
            &mutation(&f.admin, &instance_command),
            &instance_command,
            "service",
            "active",
            NOW + 1
        ),
        Err(AuthError::Conflict)
    ));
    let enable_command = AuthInstanceMutationRequest {
        expected_revision: disabled.instance.revision,
        ..instance_command.clone()
    };
    let enabled = change_instance(
        &f.store,
        &mutation(&f.admin, &enable_command),
        &enable_command,
        "service",
        "active",
        NOW + 2,
    )
    .unwrap();
    let mut fresh = admission.clone();
    fresh.runtime_id = URL_SAFE_NO_PAD.encode([24; 16]);
    crate::platform::auth::tests::issue_authority(&f.store, &fresh, None, NOW + 2).unwrap();
    assert_eq!(
        change_instance(
            &f.store,
            &disable_identity,
            &instance_command,
            "service",
            "disabled",
            NOW + 3
        )
        .unwrap(),
        disabled
    );
    let removal = AuthInstanceMutationRequest {
        expected_revision: enabled.instance.revision,
        ..instance_command
    };
    let removed = change_instance(
        &f.store,
        &mutation(&f.admin, &removal),
        &removal,
        "service",
        "revoked",
        NOW + 3,
    )
    .unwrap();
    fresh.runtime_id = URL_SAFE_NO_PAD.encode([25; 16]);
    assert!(matches!(
        crate::platform::auth::tests::issue_authority(&f.store, &fresh, None, NOW + 3),
        Err(AuthError::Denied)
    ));
    let resurrection = AuthInstanceMutationRequest {
        expected_revision: removed.instance.revision,
        ..removal
    };
    assert!(matches!(
        change_instance(
            &f.store,
            &mutation(&f.admin, &resurrection),
            &resurrection,
            "service",
            "active",
            NOW + 4
        ),
        Err(AuthError::Conflict)
    ));
    let deployment_command = AuthDeploymentMutationRequest {
        deployment_id: created.deployment.deployment_id,
        expected_revision: created.deployment.revision,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let disabled = change_deployment(
        &f.store,
        &mutation(&f.admin, &deployment_command),
        &deployment_command,
        "disabled",
        NOW + 4,
    )
    .unwrap();
    assert!(matches!(
        provision_instance(
            &f.store,
            &mutation(&f.admin, &provision),
            &provision,
            "service",
            NOW + 4
        ),
        Err(AuthError::Denied)
    ));
    let remove = AuthDeploymentMutationRequest {
        expected_revision: disabled.deployment.revision,
        ..deployment_command
    };
    change_deployment(
        &f.store,
        &mutation(&f.admin, &remove),
        &remove,
        "revoked",
        NOW + 5,
    )
    .unwrap();
    assert_eq!(
        create_deployment(&f.store, &creation_identity, &create, NOW + 6).unwrap(),
        created
    );
}

#[test]
fn catalog_acceptance_replays_typed_result_after_later_replacement() {
    let f = fixture();
    let evidence = f
        .store
        .transaction(|connection| {
            let value: String = connection.query_row(
                "SELECT definition_json \
                FROM auth_accepted_apis \
                WHERE api_id=(SELECT api_id \
                FROM auth_capability_identities \
                WHERE capability_id=?1)",
                [&f.read],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str::<AuthPackageEvidence>(&value)?)
        })
        .unwrap();
    let api_id = f
        .store
        .transaction(|connection| {
            Ok(connection.query_row(
                "SELECT api_id FROM auth_capability_identities WHERE capability_id=?1",
                [&f.read],
                |row| row.get::<_, String>(0),
            )?)
        })
        .unwrap();
    let request = AuthApiReviewRequest {
        api_id: api_id.clone().into(),
        package_evidence: evidence.clone(),
        extra: Default::default(),
    };
    assert!(matches!(
        review_api(&f.store, &f.principal, &request, NOW),
        Err(AuthError::Denied)
    ));
    let review = review_api(&f.store, &f.admin, &request, NOW)
        .unwrap()
        .review;
    let input = AuthApiAcceptRequest {
        canonical_digest: review.canonical_digest,
        expected_generation: review.current_generation.unwrap().0,
        expected_accepted_revision: review.current_accepted_revision.unwrap().0,
        review_id: review.review_id,
        role_edits: vec![],
        mapping_edits: vec![],
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &input);
    let stale = AuthApiAcceptRequest {
        expected_generation: 0.into(),
        ..input.clone()
    };
    assert!(matches!(
        accept_api(&f.store, &mutation(&f.admin, &stale), &stale, NOW),
        Err(AuthError::Conflict)
    ));
    let accepted = accept_api(&f.store, &identity, &input, NOW).unwrap();
    let review = review_api(&f.store, &f.admin, &request, NOW + 1)
        .unwrap()
        .review;
    let force = AuthApiForceReplaceRequest {
        canonical_digest: review.canonical_digest,
        expected_generation: review.current_generation.unwrap(),
        expected_accepted_revision: review.current_accepted_revision.unwrap(),
        review_id: review.review_id,
        acknowledge_breakage: true,
        acknowledge_consent_impact: true,
        role_edits: vec![],
        mapping_edits: vec![],
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    let replacement =
        force_replace_api(&f.store, &mutation(&f.admin, &force), &force, NOW + 1).unwrap();
    assert!(replacement.api.generation > accepted.api.generation);
    assert_eq!(
        accept_api(&f.store, &identity, &input, NOW + 2).unwrap(),
        accepted
    );
    let current = f
        .store
        .transaction(|connection| {
            read_api(
                connection,
                &AuthApiGetRequest {
                    api_id: api_id.into(),
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(current.api, replacement.api);
    let changed = AuthApiAcceptRequest {
        canonical_digest: "different".to_string().into(),
        ..input
    };
    let changed_identity = Mutation {
        request_digest: super::super::sqlite::digest(&changed).unwrap(),
        ..identity
    };
    assert!(matches!(
        accept_api(&f.store, &changed_identity, &changed, NOW + 2),
        Err(AuthError::Conflict)
    ));
}

#[test]
fn provisioned_service_is_discoverable_without_leaking_into_device_listing() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let f = fixture();
    let evidence: trellis_idl::PackageEvidence = f
        .store
        .transaction(|connection| {
            let value: String = connection.query_row(
                "SELECT definition_json \
                FROM auth_accepted_apis \
                WHERE api_id=(SELECT api_id \
                FROM auth_capability_identities \
                WHERE capability_id=?1)",
                [&f.read],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str(&value)?)
        })
        .unwrap();
    let graph = trellis_idl::compile_evidence(evidence.clone()).unwrap();
    let participant_id = graph
        .root_package()
        .participants()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let input = super::super::DeploymentProvisioning {
        evidence,
        participant_id,
        expected_api_generations: Default::default(),
        identity_public_key: URL_SAFE_NO_PAD.encode(
            ed25519_dalek::SigningKey::from_bytes(&[12; 32])
                .verifying_key()
                .as_bytes(),
        ),
        resource_commitments: serde_json::json!({}),
        expires_at: None,
    };
    let provisioned = f
        .store
        .provision_deployment(&mutation(&f.admin, &input), &input, NOW)
        .unwrap();
    f.store
        .transaction(|connection| {
            let deployment = read_deployment(connection, &provisioned.deployment_id)?;
            assert_eq!(
                deployment.selection.required_capabilities[0].as_ref(),
                f.read
            );
            let request = AuthInstancesListRequest {
                deployment_id: Some(provisioned.deployment_id.parse().unwrap()),
                page: None,
                extra: Default::default(),
            };
            let services = read_instances(connection, &request, "service")?;
            assert_eq!(
                services.items[0].instance_id.to_string(),
                provisioned.instance_id
            );
            assert_eq!(
                services.items[0]
                    .identity_public_key
                    .as_ref()
                    .unwrap()
                    .as_ref(),
                input.identity_public_key
            );
            assert!(read_instances(connection, &request, "device")?
                .items
                .is_empty());
            Ok(())
        })
        .unwrap();
}
#[tokio::test]
async fn public_provision_materializes_empty_and_kv_declarations_before_signed_service_admission() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use sha2::{Digest, Sha256};
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;
    use trellis_rs::generated::ParticipantDescriptor;
    use trellis_runtime_apis::participants::{trellis_health_runtime, trellis_platform};
    let f = fixture();
    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut server = tokio::process::Command::new(
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into()),
    )
    .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
    .arg(directory.path())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let mut output = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if line.contains("Server is ready") {
                return;
            }
        }
        panic!("NATS exited before readiness");
    })
    .await
    .unwrap();
    let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .unwrap();
    let store = f.store;
    let packages = [
        (
            trellis_health_runtime::Participant::package_evidence(),
            "healthRuntime",
            91u8,
        ),
        (
            trellis_platform::Participant::package_evidence(),
            "platform",
            92u8,
        ),
    ];
    for (evidence, name, seed) in packages {
        let evidence = serde_json::to_value(evidence).unwrap();
        let graph =
            trellis_idl::compile_evidence(serde_json::from_value(evidence.clone()).unwrap())
                .unwrap();
        let participant = graph
            .root_package()
            .participants()
            .values()
            .find(|participant| participant.name() == name)
            .unwrap();
        let create = AuthDeploymentCreateRequest {
            package_evidence: serde_json::from_value(evidence).unwrap(),
            participant_id: participant.identity().to_string(),
            kind: AuthDeploymentKind::Service,
            title: format!("Public {name} deployment").into(),
            idempotency_key: super::super::sqlite::id().into(),
            extra: Default::default(),
        };
        let deployment =
            create_deployment(&store, &mutation(&f.admin, &create), &create, NOW).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let public = key.verifying_key();
        let input = AuthInstanceProvisionRequest {
            deployment_id: deployment.deployment.deployment_id,
            identity_public_key: Some(URL_SAFE_NO_PAD.encode(public.as_bytes()).into()),
            title: format!("Public {name} instance").into(),
            idempotency_key: super::super::sqlite::id().into(),
            extra: Default::default(),
        };
        let identity = mutation(&f.admin, &input);
        let provisioned = provision_service_instance(&store, &nats, &identity, &input, NOW)
            .await
            .unwrap();
        let ephemeral = nkeys::KeyPair::new_user();
        let signed = trellis_protocol::sign_provisioned_admission(
            trellis_protocol::ProvisionedAdmission {
                format: trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1.into(),
                trellis_instance_id: store.settings.instance_id.clone(),
                audience_nats_account: store.settings.account.clone(),
                credential_id: URL_SAFE_NO_PAD.encode(Sha256::digest(public.as_bytes())),
                runtime_id: URL_SAFE_NO_PAD.encode([seed; 16]),
                ephemeral_nkey: ephemeral.public_key(),
                issued_at: NOW,
                expires_at: NOW + 60,
            },
            &key,
        )
        .unwrap();
        let admitted = crate::platform::auth::tests::admit_credential(
            &store,
            &serde_json::to_vec(&signed).unwrap(),
            &ephemeral.public_key(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            admitted.authority.unsigned.principal_id,
            provisioned.instance.principal.principal_id.to_string()
        );
        // The actual bootstrap consumed by a normal service CONNECT must decode
        // the producer's binding projection, including empty declaration sets.
        let bootstrap = store
            .transaction(|connection| {
                let authority = &admitted.authority.unsigned;
                let trellis_protocol::SessionBinding::Service { instance_id, .. } =
                    &authority.binding
                else {
                    unreachable!("a service admission is service-bound");
                };
                let projection = crate::platform::auth::native_projection::project_instance(
                    connection,
                    instance_id,
                    authority.issued_at,
                )?;
                store.native_bootstrap(connection, authority, &projection)
            })
            .unwrap()
            .unwrap();
        let resources: trellis_rs::service::ServiceResourceBindings =
            serde_json::from_value(bootstrap["resourceRuntime"].clone()).unwrap();
        if name == "platform" {
            let bucket_name = resources.kv["connections"].bucket.clone();
            let bucket = async_nats::jetstream::new(nats.clone())
                .get_key_value(&bucket_name)
                .await
                .unwrap();
            bucket
                .put("public.provision", b"retained".as_slice().into())
                .await
                .unwrap();
            assert_eq!(
                provision_service_instance(&store, &nats, &identity, &input, NOW + 1)
                    .await
                    .unwrap(),
                provisioned
            );
            assert_eq!(
                bucket
                    .get("public.provision")
                    .await
                    .unwrap()
                    .unwrap()
                    .as_ref(),
                b"retained"
            );
        } else {
            assert_eq!(bootstrap["participantId"], create.participant_id);
            assert_eq!(
                provision_service_instance(&store, &nats, &identity, &input, NOW + 1)
                    .await
                    .unwrap(),
                provisioned
            );
        }
    }
    server.kill().await.unwrap();
}
