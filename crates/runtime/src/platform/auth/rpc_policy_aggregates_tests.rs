//! Real SQLite aggregate replacement, CAS, identity, and replay behavior.

use super::{fixture, id, mutation, policy, AuthError, PolicyMutation, NOW};
use crate::platform::auth::{
    rpc_policy::read_effective_access,
    rpc_policy_aggregates::{assign_privileges, put_mapping, read_assignments, revoke_privileges},
    rpc_users::{delete_mapping, read_mapping},
};
use trellis_runtime_apis::__types::trellis::*;

#[test]
fn privilege_replace_preserves_identity_cas_and_replay_cannot_revoke_recreated_assignment() {
    let f = fixture();
    for capability in [&f.read, &f.write] {
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::CapabilityRevoke {
                principal_id: f.principal.clone(),
                capability_id: capability.clone(),
                expected_revision: 1,
            },
            NOW,
        );
    }
    let input = AuthPlatformAssignRequest {
        principal_id: f.principal.parse().unwrap(),
        privileges: vec![
            AuthPlatformPrivilege::RolesManage,
            AuthPlatformPrivilege::ClientsManage,
        ],
        expected_revision: 0.into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &input);
    let created = assign_privileges(&f.store, &identity, &input, NOW).unwrap();
    let update = AuthPlatformAssignRequest {
        privileges: vec![AuthPlatformPrivilege::ApisAccept],
        expected_revision: 1.into(),
        idempotency_key: id().into(),
        ..input.clone()
    };
    let updated =
        assign_privileges(&f.store, &mutation(&f.admin, &update), &update, NOW + 1).unwrap();
    assert_eq!(
        created.assignment.assignment_id,
        updated.assignment.assignment_id
    );
    assert_eq!(
        assign_privileges(&f.store, &identity, &input, NOW + 2).unwrap(),
        created
    );
    assert!(matches!(
        assign_privileges(&f.store, &mutation(&f.admin, &update), &update, NOW + 2),
        Err(AuthError::Conflict)
    ));
    let access = f
        .store
        .transaction(|connection| read_effective_access(connection, &f.principal, NOW + 2))
        .unwrap();
    assert_eq!(
        access.platform_privileges,
        vec![AuthPlatformPrivilege::ApisAccept]
    );
    // Finite administrative privileges never expand capability entitlements.
    assert!(access.capabilities.is_empty());
    let page = AuthPageRequest {
        page: None,
        search: Some(f.principal.clone().into()),
        extra: Default::default(),
    };
    let listed = f
        .store
        .transaction(|connection| read_assignments(connection, &page))
        .unwrap();
    assert_eq!(listed.items, vec![updated.assignment.clone()]);
    let revoke = AuthPlatformRevokeRequest {
        assignment_id: updated.assignment.assignment_id,
        expected_revision: updated.assignment.revision,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let revoke_identity = mutation(&f.admin, &revoke);
    let revoked = revoke_privileges(&f.store, &revoke_identity, &revoke, NOW + 3).unwrap();
    assert!(f
        .store
        .transaction(|connection| read_effective_access(connection, &f.principal, NOW + 3))
        .unwrap()
        .platform_privileges
        .is_empty());
    let recreated =
        assign_privileges(&f.store, &mutation(&f.admin, &input), &input, NOW + 4).unwrap();
    assert_ne!(
        recreated.assignment.assignment_id,
        created.assignment.assignment_id
    );
    assert_eq!(
        revoke_privileges(&f.store, &revoke_identity, &revoke, NOW + 5).unwrap(),
        revoked
    );
    let stale = AuthPlatformRevokeRequest {
        expected_revision: AuthRevision(1.into()),
        ..revoke
    };
    assert!(matches!(
        revoke_privileges(&f.store, &mutation(&f.admin, &stale), &stale, NOW + 5),
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        f.store
            .transaction(|connection| read_assignments(connection, &page))
            .unwrap()
            .items,
        vec![recreated.assignment]
    );
}

#[test]
fn bootstrap_assignment_cannot_be_demoted_or_removed() {
    let f = fixture();
    let page = AuthPageRequest {
        page: None,
        search: Some(f.admin.clone().into()),
        extra: Default::default(),
    };
    let admin = f
        .store
        .transaction(|connection| read_assignments(connection, &page))
        .unwrap()
        .items
        .remove(0);
    let demote = AuthPlatformAssignRequest {
        principal_id: f.admin.parse().unwrap(),
        privileges: vec![AuthPlatformPrivilege::PrivilegesManage],
        expected_revision: admin.revision.0,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        assign_privileges(&f.store, &mutation(&f.admin, &demote), &demote, NOW),
        Err(AuthError::Denied)
    ));
    let revoke = AuthPlatformRevokeRequest {
        assignment_id: admin.assignment_id,
        expected_revision: admin.revision,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        revoke_privileges(&f.store, &mutation(&f.admin, &revoke), &revoke, NOW),
        Err(AuthError::Denied)
    ));
    assert_eq!(
        f.store
            .transaction(|connection| read_assignments(connection, &page))
            .unwrap()
            .items,
        vec![admin]
    );
}

#[test]
fn multi_role_mapping_atomic_replace_delete_and_typed_replay() {
    let f = fixture();
    let provider_id = id();
    let provider = crate::platform::auth::lifecycle::OidcProviderRegistration {
        provider_id: provider_id.clone(),
        issuer: "https://identity.example".into(),
        display_name: "Identity".into(),
        client_id: "trellis".into(),
        sealed_client_secret: None,
        config: serde_json::json!({"roleClaim":"/groups"}),
        claim_freshness_seconds: 3600,
        expected_revision: 0,
    };
    f.store
        .transaction(|connection| {
            f.store.register_oidc_provider_in_transaction(
                connection,
                &mutation(&f.admin, &provider),
                &provider,
                NOW,
            )
        })
        .unwrap();
    let roles = [id(), id()];
    for (role_id, capability) in roles.iter().zip([&f.read, &f.write]) {
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::RolePut {
                role_id: role_id.clone(),
                title: "Mapped role".into(),
                description: String::new(),
                capabilities: vec![capability.clone()],
                expected_revision: 0,
            },
            NOW,
        );
    }
    let input = AuthOIDCRoleMappingPutRequest {
        mapping_id: None,
        provider_id: provider_id.parse().unwrap(),
        external_role: "team".to_string().into(),
        role_ids: roles.iter().map(|role| role.parse().unwrap()).collect(),
        expected_revision: 0.into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &input);
    let created = put_mapping(&f.store, &identity, &input, NOW).unwrap();
    assert_eq!(created.mapping.role_ids.len(), 2);
    let update = AuthOIDCRoleMappingPutRequest {
        mapping_id: Some(created.mapping.mapping_id),
        role_ids: vec![roles[1].parse().unwrap()],
        expected_revision: 1.into(),
        idempotency_key: id().into(),
        ..input.clone()
    };
    let updated = put_mapping(&f.store, &mutation(&f.admin, &update), &update, NOW + 1).unwrap();
    assert_eq!(updated.mapping.role_ids, update.role_ids);
    assert!(matches!(
        put_mapping(&f.store, &mutation(&f.admin, &update), &update, NOW + 1),
        Err(AuthError::Conflict)
    ));
    let invalid = AuthOIDCRoleMappingPutRequest {
        role_ids: vec![roles[0].parse().unwrap(), id().parse().unwrap()],
        expected_revision: 2.into(),
        idempotency_key: id().into(),
        ..update.clone()
    };
    assert!(matches!(
        put_mapping(&f.store, &mutation(&f.admin, &invalid), &invalid, NOW + 2),
        Err(AuthError::NotFound)
    ));
    assert_eq!(
        f.store
            .transaction(|connection| read_mapping(
                connection,
                &created.mapping.mapping_id.to_string()
            ))
            .unwrap(),
        updated.mapping
    );
    let delete = AuthOIDCRoleMappingDeleteRequest {
        mapping_id: created.mapping.mapping_id,
        expected_revision: updated.mapping.revision,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let delete_identity = mutation(&f.admin, &delete);
    let deleted = delete_mapping(&f.store, &delete_identity, &delete, NOW + 3).unwrap();
    assert_eq!(
        delete_mapping(&f.store, &delete_identity, &delete, NOW + 4).unwrap(),
        deleted
    );
    assert_eq!(
        put_mapping(&f.store, &identity, &input, NOW + 4).unwrap(),
        created
    );
    assert!(matches!(
        f.store
            .transaction(|connection| read_mapping(connection, &delete.mapping_id.to_string())),
        Err(AuthError::NotFound)
    ));
}
