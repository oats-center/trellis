//! Real SQLite aggregate checks; transport acceptance remains in the live suite.

use super::{
    fixture, id, mutation, policy, AuthError, ClientRegistration, Credential, EnforcementScope,
    PolicyMutation, NOW,
};
use crate::platform::auth::{
    rpc_credentials::{change_client_state, kick_connection, put_client, read_client},
    Attachment,
};
use trellis_runtime_apis::__types::trellis::*;

#[test]
fn client_put_replay_preserves_original_result_without_overwriting_later_edits() {
    let f = fixture();
    let input = AuthOAuthClientPutRequest {
        client_id: "rpc-native".to_string().into(),
        description: "Public native application".to_string().into(),
        development: false,
        expected_revision: 0.into(),
        idempotency_key: id().into(),
        implied_consent: vec![],
        kind: AuthPublicClientKind::Native,
        redirect_uris: vec!["http://127.0.0.1:9823/callback".into()],
        title: "Original".to_string().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &input);
    let first = put_client(&f.store, &identity, &input, NOW).unwrap();
    let update = AuthOAuthClientPutRequest {
        title: "Updated".to_string().into(),
        expected_revision: 1.into(),
        idempotency_key: id().into(),
        ..input.clone()
    };
    put_client(&f.store, &mutation(&f.admin, &update), &update, NOW + 1).unwrap();
    assert_eq!(
        put_client(&f.store, &identity, &input, NOW + 2).unwrap(),
        first
    );
    let current = f
        .store
        .transaction(|connection| read_client(connection, &input.client_id))
        .unwrap();
    assert_eq!(current.title, "Updated");
    let stale = AuthOAuthClientPutRequest {
        title: "Lost edit".to_string().into(),
        idempotency_key: id().into(),
        ..update
    };
    assert!(matches!(
        put_client(&f.store, &mutation(&f.admin, &stale), &stale, NOW + 3),
        Err(AuthError::Conflict)
    ));
    let disable = AuthOAuthClientMutationRequest {
        client_id: input.client_id.clone(),
        expected_revision: AuthRevision(2.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &disable);
    let disabled = change_client_state(&f.store, &identity, &disable, "disabled", NOW + 4).unwrap();
    assert_eq!(
        change_client_state(&f.store, &identity, &disable, "disabled", NOW + 5).unwrap(),
        disabled
    );
    assert!(
        !f.store
            .transaction(|connection| read_client(connection, &input.client_id))
            .unwrap()
            .enabled
    );
}

#[test]
fn physical_kick_queues_exact_attachment_without_retiring_its_logical_session() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let session = issued.authority.unsigned.authorization_session_id.clone();
    let nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
        .unwrap()
        .public_key();
    let policy = f
        .store
        .transaction(|connection| {
            f.store
                .compile_transport(connection, &issued.authority.unsigned)
        })
        .unwrap();
    let attachment = Attachment {
        attachment_id: id(),
        server_id: "server-a".into(),
        broker_client_id: 17,
        broker_user_nkey: nkey.clone(),
        authorization_session_id: session.clone(),
        context_digest: issued.digest,
        ephemeral_nkey: nkey,
        transport_policy_digest: policy.digest().unwrap(),
    };
    f.store.admit_attachment(&attachment, NOW).unwrap();
    let input = AuthConnectionsKickRequest {
        attachment_id: attachment.attachment_id.parse().unwrap(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &input);
    let kicked = kick_connection(&f.store, &identity, &input, NOW + 1).unwrap();
    assert_eq!(
        kick_connection(&f.store, &identity, &input, NOW + 2).unwrap(),
        kicked
    );
    while f.store.enforce_next_page(NOW + 2).unwrap().is_some() {}
    let renewed = f
        .store
        .issue_authority(&f.admission, Some(&session), NOW + 3)
        .unwrap();
    assert_eq!(renewed.authority.unsigned.authorization_session_id, session);
    let effects = f.store.claim_effects(256, NOW + 3, 30).unwrap();
    let kicks: Vec<_> = effects
        .iter()
        .filter(|effect| effect.kind == "kick")
        .collect();
    assert_eq!(kicks.len(), 1);
    assert_eq!(kicks[0].payload["attachmentId"], attachment.attachment_id);
    assert_eq!(kicks[0].payload["serverId"], "server-a");
    assert_eq!(kicks[0].payload["brokerClientId"], 17);
}

#[test]
fn client_manager_can_edit_metadata_without_changing_existing_privilege_eligibility() {
    use trellis_protocol::PlatformPrivilege;
    let f = fixture();
    let registration = ClientRegistration {
        client_id: "eligible-client".into(),
        kind: "native".into(),
        display_name: "Eligible".into(),
        redirect_uris: vec!["http://127.0.0.1:9823/callback".into()],
        development: false,
        implied_capabilities: vec![],
        eligible_privileges: vec![PlatformPrivilege::ClientsManage],
        metadata: serde_json::json!({}),
        expected_revision: 0,
    };
    f.store
        .register_client(&mutation(&f.admin, &registration), &registration, NOW)
        .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::PrivilegeAssign {
            principal_id: f.principal.clone(),
            privileges: vec![PlatformPrivilege::ClientsManage],
            expected_revision: 0,
        },
        NOW,
    );
    let input = AuthOAuthClientPutRequest {
        client_id: registration.client_id.clone().into(),
        description: "Updated by a client manager".to_string().into(),
        development: false,
        expected_revision: 1.into(),
        idempotency_key: id().into(),
        implied_consent: vec![],
        kind: AuthPublicClientKind::Native,
        redirect_uris: registration.redirect_uris.clone(),
        title: "Renamed".to_string().into(),
        extra: Default::default(),
    };
    assert_eq!(
        put_client(&f.store, &mutation(&f.principal, &input), &input, NOW + 1)
            .unwrap()
            .client
            .title,
        "Renamed"
    );
    let mut changed = registration;
    changed.expected_revision = 2;
    changed
        .eligible_privileges
        .push(PlatformPrivilege::PrincipalsManage);
    assert!(matches!(
        f.store
            .register_client(&mutation(&f.principal, &changed), &changed, NOW + 2),
        Err(AuthError::Denied)
    ));
}

#[test]
fn ordinary_user_can_revoke_own_grant_but_cannot_revoke_another_login() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let Credential::OAuthGrant(grant) = &f.admission.credential else {
        panic!("OAuth fixture")
    };
    let scope = EnforcementScope::Grant(grant.clone());
    f.store
        .revoke_root(&mutation(&f.principal, &scope), &scope, NOW + 1)
        .unwrap();
    assert!(matches!(
        f.store.issue_authority(&f.admission, None, NOW + 2),
        Err(AuthError::Denied)
    ));
    while f.store.enforce_next_page(NOW + 2).unwrap().is_some() {}
    assert!(matches!(
        f.store.issue_authority(
            &f.admission,
            Some(&issued.authority.unsigned.authorization_session_id),
            NOW + 3
        ),
        Err(AuthError::Retired)
    ));
    let login = f
        .store
        .record_verified_login(
            &super::VerifiedLogin {
                principal_id: f.admin.clone(),
                provider_id: None,
                upstream_issuer: None,
                upstream_subject: None,
                verified_claims: None,
                expires_at: NOW + 3600,
            },
            NOW,
        )
        .unwrap();
    let other = EnforcementScope::Login(login);
    assert!(matches!(
        f.store
            .revoke_root(&mutation(&f.principal, &other), &other, NOW + 4),
        Err(AuthError::Denied)
    ));
}
