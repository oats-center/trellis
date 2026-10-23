use super::*;

#[test]
fn trusted_native_device_provisioning_issues_authority_and_replays_its_identity() {
    let f = fixture();
    let proposed = evidence(&SOURCE.replace("service Reader", "device Reader"));
    let graph = trellis_idl::compile_evidence(proposed.clone()).unwrap();
    let participant = graph
        .root_package()
        .participants()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let provisioning = DeploymentProvisioning {
        evidence: proposed,
        participant_id: participant.clone(),
        expected_api_generations: BTreeMap::new(),
        identity_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&[11; 32]).verifying_key().as_bytes()),
        resource_commitments: serde_json::json!({}),
        expires_at: None,
    };
    let command = mutation(&f.admin, &provisioning);
    let provisioned = f
        .store
        .provision_deployment(&command, &provisioning, NOW)
        .unwrap();
    let replayed = f
        .store
        .provision_deployment(&command, &provisioning, NOW)
        .unwrap();
    assert_eq!(replayed.identity_key_id, provisioned.identity_key_id);
    // A distinct provisioning command cannot make the same key select between
    // multiple independently owned bindings. The original remains usable below.
    assert!(f
        .store
        .provision_deployment(&mutation(&f.admin, &provisioning), &provisioning, NOW,)
        .is_err());
    let mut admission = f.admission.clone();
    admission.credential = Credential::ProvisionedIdentity(provisioned.identity_key_id.clone());
    admission.refresh_family_id = None;
    let issued = issue_authority(&f.store, &admission, None, NOW).unwrap();
    assert_eq!(
        issued.authority.unsigned.principal_kind,
        trellis_protocol::PrincipalKind::Device
    );
    assert_eq!(
        issued.authority.unsigned.principal_id,
        provisioned.principal_id
    );
    assert_eq!(
        issued.authority.unsigned.binding,
        trellis_protocol::SessionBinding::Device {
            deployment_id: provisioned.deployment_id,
            instance_id: provisioned.instance_id,
            participant_id: participant,
        }
    );
    let ephemeral = nkeys::KeyPair::new_user();
    let signed = trellis_protocol::sign_provisioned_admission(
        trellis_protocol::ProvisionedAdmission {
            format: trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1.into(),
            trellis_instance_id: f.store.settings.instance_id.clone(),
            audience_nats_account: f.store.settings.account.clone(),
            credential_id: provisioned.identity_key_id.clone(),
            runtime_id: URL_SAFE_NO_PAD.encode([17; 16]),
            ephemeral_nkey: ephemeral.public_key(),
            issued_at: NOW,
            expires_at: NOW + 60,
        },
        &SigningKey::from_bytes(&[11; 32]),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    let (_, public) = nkeys::from_public_key(&ephemeral.public_key()).unwrap();
    let admitted =
        issue_provisioned_authority(&f.store, &bytes, &ephemeral.public_key(), NOW).unwrap();
    assert_eq!(
        admitted.authority.unsigned.principal_id,
        provisioned.principal_id
    );
    assert_eq!(
        admitted.authority.unsigned.session_public_key,
        URL_SAFE_NO_PAD.encode(public)
    );
    // Auth derives the authoritative deployment/instance/participant binding
    // from the installed identity, not assertions submitted by the caller.
    assert_eq!(
        admitted.authority.unsigned.binding,
        issued.authority.unsigned.binding
    );
    let wrong_identity = trellis_protocol::sign_provisioned_admission(
        signed.credential.clone(),
        &SigningKey::from_bytes(&[12; 32]),
    )
    .unwrap();
    assert!(issue_provisioned_authority(
        &f.store,
        &serde_json::to_vec(&wrong_identity).unwrap(),
        &ephemeral.public_key(),
        NOW,
    )
    .is_err());
    let mut wrong_installation = signed.credential.clone();
    wrong_installation.trellis_instance_id = id();
    let wrong_installation = trellis_protocol::sign_provisioned_admission(
        wrong_installation,
        &SigningKey::from_bytes(&[11; 32]),
    )
    .unwrap();
    assert!(issue_provisioned_authority(
        &f.store,
        &serde_json::to_vec(&wrong_installation).unwrap(),
        &ephemeral.public_key(),
        NOW,
    )
    .is_err());
    assert!(issue_provisioned_authority(
        &f.store,
        &bytes,
        &nkeys::KeyPair::new_user().public_key(),
        NOW
    )
    .is_err());
    let mut forged = signed.clone();
    forged.credential.runtime_id = URL_SAFE_NO_PAD.encode([18; 16]);
    assert!(issue_provisioned_authority(
        &f.store,
        &serde_json::to_vec(&forged).unwrap(),
        &ephemeral.public_key(),
        NOW
    )
    .is_err());
    assert!(
        issue_provisioned_authority(&f.store, &bytes, &ephemeral.public_key(), NOW + 60).is_err()
    );
    let scope = EnforcementScope::Identity(provisioned.identity_key_id);
    revoke_root(&f.store, &mutation(&f.admin, &scope), &scope, NOW + 1).unwrap();
    assert!(matches!(
        issue_authority(&f.store, &admission, None, NOW + 1),
        Err(AuthError::Denied)
    ));
    assert!(
        issue_provisioned_authority(&f.store, &bytes, &ephemeral.public_key(), NOW + 1).is_err()
    );
}
