use super::*;
use trellis_protocol::{
    verify_issuer_metadata, IssuerMetadataQuery, IssuerMetadataRequest, ISSUER_METADATA_SUBJECT,
};

#[test]
fn public_metadata_retains_signed_cutoff_and_authenticates_rotated_issuer_without_new_trust() {
    let f = fixture();
    let issued =
        crate::platform::auth::tests::issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let original = f.store.issuer().unwrap();
    let scope =
        EnforcementScope::Session(issued.authority.unsigned.authorization_session_id.clone());
    crate::platform::auth::tests::revoke_root(
        &f.store,
        &mutation(&f.admin, &scope),
        &scope,
        NOW + 1,
    )
    .unwrap();
    let request = IssuerMetadataRequest {
        trellis_instance_id: f.store.settings.instance_id.clone(),
        audience_nats_account: f.store.settings.account.clone(),
        request_nonce: URL_SAFE_NO_PAD.encode([27; 16]),
        query: IssuerMetadataQuery::AuthorityStatus {
            authorization_session_id: issued.authority.unsigned.authorization_session_id.clone(),
        },
    };
    let reply = "_INBOX.TRELLIS.caller.request";
    let bytes = f
        .store
        .issuer_metadata(
            &serde_json::to_vec(&request).unwrap(),
            ISSUER_METADATA_SUBJECT,
            reply,
            NOW + 2,
        )
        .unwrap();
    let verified = verify_issuer_metadata(
        &bytes,
        &original,
        &request,
        ISSUER_METADATA_SUBJECT,
        reply,
        NOW + 2,
    )
    .unwrap();
    let Some(trellis_protocol::IssuerMetadataValue::Status(status)) = verified.value() else {
        panic!("retired status remains retained")
    };
    let cutoff: trellis_protocol::SignedSessionRevocation =
        serde_json::from_str(status.revocation.as_deref().unwrap()).unwrap();
    cutoff
        .verify(
            &original,
            &f.store.settings.instance_id,
            &f.store.settings.account,
        )
        .unwrap();
    assert_eq!(cutoff.effective_cutoff, NOW + 1);
    let policy = SessionAuthorityVerificationPolicy {
        now_unix_seconds: NOW + 2,
        allowed_clock_skew_seconds: 5,
        maximum_authority_lifetime_seconds: 60,
        maximum_authority_bytes: 65536,
        maximum_entries: 256,
    };
    assert!(verify_session_authority(SessionAuthorityVerificationInput {
        issuer: &original,
        trellis_instance_id: &f.store.settings.instance_id,
        audience_nats_account: &f.store.settings.account,
        authority: &issued.authority,
        policy: &policy,
        purpose: SessionAuthorityPurpose::Live,
        revocation_cutoff: Some(cutoff.effective_cutoff)
    })
    .is_err());
    assert!(f
        .store
        .issuer_metadata(
            &serde_json::to_vec(&request).unwrap(),
            ISSUER_METADATA_SUBJECT,
            "rpc.v1.control.Destroy",
            NOW + 2
        )
        .is_err());
    let rotation =
        crate::platform::auth::tests::rotate_issuer(&f.store, &f.admin, NOW + 3).unwrap();
    // Restart with the original bootstrap key; committed successor custody wins.
    let store = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let current = store.issuer().unwrap();
    let chain_request = IssuerMetadataRequest {
        query: IssuerMetadataQuery::IssuerChain {
            pinned_key_id: original.key_id.clone(),
            target_key_id: current.key_id.clone(),
        },
        ..request.clone()
    };
    let bytes = store
        .issuer_metadata(
            &serde_json::to_vec(&chain_request).unwrap(),
            ISSUER_METADATA_SUBJECT,
            reply,
            NOW + 4,
        )
        .unwrap();
    assert!(verify_issuer_metadata(
        &bytes,
        &original,
        &chain_request,
        ISSUER_METADATA_SUBJECT,
        reply,
        NOW + 4
    )
    .is_err());
    // The independently signed pinned-key rotation authenticates the target,
    // before its envelope can be admitted. No unsigned payload installs a key.
    let authenticated = rotation
        .verify_successor(
            &original,
            &store.settings.instance_id,
            &store.settings.account,
            0,
            NOW + 4,
        )
        .unwrap();
    assert_eq!(authenticated, current);
    let verified = verify_issuer_metadata(
        &bytes,
        &current,
        &chain_request,
        ISSUER_METADATA_SUBJECT,
        reply,
        NOW + 4,
    )
    .unwrap();
    let Some(trellis_protocol::IssuerMetadataValue::Chain(chain)) = verified.value() else {
        panic!("rotation proof is resolvable")
    };
    assert_eq!(chain.rotations.len(), 1);
    let resolved: trellis_protocol::SignedIssuerRotation =
        serde_json::from_str(&chain.rotations[0]).unwrap();
    assert_eq!(resolved, rotation);
}
