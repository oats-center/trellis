//! SQLite projection checks following real policy mutations and catalog acceptance.

use super::{fixture, id, mutation, policy, AuthError, PolicyMutation, NOW};
use crate::platform::auth::rpc_policy::{
    assign_role, delete_role, grant_capability, put_role, read_assignments, read_capabilities,
    read_effective_access, read_grants, read_role, read_roles, revoke_assignment, revoke_grant,
};
use trellis_runtime_apis::__types::{trellis::*, CursorQuery};

#[test]
fn omitted_role_id_is_generated_once_and_replay_does_not_overwrite_an_update() {
    let f = fixture();
    let input = AuthRolePutRequest {
        role_id: None,
        expected_revision: 0.into(),
        title: "Reviewers".to_string().into(),
        description: "Read documents".to_string().into(),
        capability_ids: vec![f.read.clone().into()],
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &input);
    let created = put_role(&f.store, &identity, &input, NOW).unwrap();
    let role_id = created.role.role_id.to_string();
    let update = AuthRolePutRequest {
        role_id: Some(role_id.parse().unwrap()),
        expected_revision: 1.into(),
        title: "Editors".to_string().into(),
        description: "Write documents".to_string().into(),
        capability_ids: vec![f.write.clone().into()],
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    put_role(&f.store, &mutation(&f.admin, &update), &update, NOW + 1).unwrap();
    assert_eq!(
        put_role(&f.store, &identity, &input, NOW + 2).unwrap(),
        created
    );
    let current = f
        .store
        .transaction(|connection| read_role(connection, &role_id))
        .unwrap();
    assert_eq!(current.title, "Editors");
    assert_eq!(current.capabilities[0].capability_id.as_ref(), f.write);
    let listed = f
        .store
        .transaction(|connection| {
            read_roles(
                connection,
                &AuthPageRequest {
                    page: None,
                    search: None,
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    let delete = AuthRoleDeleteRequest {
        role_id: role_id.parse().unwrap(),
        expected_revision: AuthRevision(2.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let deletion_identity = mutation(&f.admin, &delete);
    let deleted = delete_role(&f.store, &deletion_identity, &delete, NOW + 3).unwrap();
    assert_eq!(
        put_role(&f.store, &identity, &input, NOW + 4).unwrap(),
        created
    );
    assert_eq!(
        delete_role(&f.store, &deletion_identity, &delete, NOW + 4).unwrap(),
        deleted
    );
    assert!(matches!(
        f.store
            .transaction(|connection| read_role(connection, &role_id)),
        Err(AuthError::NotFound)
    ));
}

#[test]
fn assignment_ids_survive_updates_replay_after_revoke_and_reject_stale_tuple_ids() {
    let f = fixture();
    let role = AuthRolePutRequest {
        role_id: None,
        expected_revision: 0.into(),
        title: "Reviewers".to_string().into(),
        description: "Read".to_string().into(),
        capability_ids: vec![f.read.clone().into()],
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let role_id = put_role(&f.store, &mutation(&f.admin, &role), &role, NOW)
        .unwrap()
        .role
        .role_id;
    let assign = AuthRoleAssignRequest {
        principal_id: f.principal.parse().unwrap(),
        role_id,
        expected_revision: 0.into(),
        expires_at: None,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &assign);
    let initial = assign_role(&f.store, &identity, &assign, NOW).unwrap();
    let update = AuthRoleAssignRequest {
        expected_revision: 1.into(),
        expires_at: Some(
            time::OffsetDateTime::from_unix_timestamp(NOW + 20)
                .unwrap()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
                .parse()
                .unwrap(),
        ),
        idempotency_key: id().into(),
        ..assign.clone()
    };
    let updated = assign_role(&f.store, &mutation(&f.admin, &update), &update, NOW + 1).unwrap();
    assert_eq!(
        updated.assignment.assignment_id,
        initial.assignment.assignment_id
    );
    assert_eq!(updated.assignment.expires_at, update.expires_at);
    assert_eq!(
        assign_role(&f.store, &identity, &assign, NOW + 2).unwrap(),
        initial
    );
    let revoke = AuthRoleRevokeRequest {
        assignment_id: updated.assignment.assignment_id,
        expected_revision: AuthRevision(2.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let revoke_identity = mutation(&f.admin, &revoke);
    let revoked = revoke_assignment(&f.store, &revoke_identity, &revoke, NOW + 2).unwrap();
    let recreated_input = AuthRoleAssignRequest {
        idempotency_key: id().into(),
        ..assign.clone()
    };
    let recreated = assign_role(
        &f.store,
        &mutation(&f.admin, &recreated_input),
        &recreated_input,
        NOW + 3,
    )
    .unwrap();
    assert_ne!(
        recreated.assignment.assignment_id,
        initial.assignment.assignment_id
    );
    assert_eq!(
        revoke_assignment(&f.store, &revoke_identity, &revoke, NOW + 4).unwrap(),
        revoked
    );
    assert_eq!(
        assign_role(&f.store, &identity, &assign, NOW + 4).unwrap(),
        initial
    );
    let stale = AuthRoleRevokeRequest {
        expected_revision: AuthRevision(1.into()),
        idempotency_key: id().into(),
        ..revoke
    };
    assert!(matches!(
        revoke_assignment(&f.store, &mutation(&f.admin, &stale), &stale, NOW + 4),
        Err(AuthError::NotFound)
    ));
    let listed = f
        .store
        .transaction(|connection| {
            read_assignments(
                connection,
                &AuthRoleAssignmentsListRequest {
                    principal_id: Some(assign.principal_id),
                    role_id: Some(role_id),
                    page: None,
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(listed.items, vec![recreated.assignment.clone()]);
    // A deleted/re-created role must not redirect an older assignment ID to
    // the new role generation, even when both tuple revisions are one.
    let deleted_role = AuthRoleDeleteRequest {
        role_id,
        expected_revision: AuthRevision(1.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    delete_role(
        &f.store,
        &mutation(&f.admin, &deleted_role),
        &deleted_role,
        NOW + 5,
    )
    .unwrap();
    let new_role = AuthRolePutRequest {
        role_id: Some(role_id),
        idempotency_key: id().into(),
        ..role
    };
    put_role(&f.store, &mutation(&f.admin, &new_role), &new_role, NOW + 6).unwrap();
    let new_assignment = AuthRoleAssignRequest {
        idempotency_key: id().into(),
        ..recreated_input
    };
    let current = assign_role(
        &f.store,
        &mutation(&f.admin, &new_assignment),
        &new_assignment,
        NOW + 6,
    )
    .unwrap();
    assert_ne!(
        current.assignment.role_identity_generation,
        recreated.assignment.role_identity_generation
    );
    let old_generation = AuthRoleRevokeRequest {
        assignment_id: recreated.assignment.assignment_id,
        expected_revision: AuthRevision(1.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        revoke_assignment(
            &f.store,
            &mutation(&f.admin, &old_generation),
            &old_generation,
            NOW + 7
        ),
        Err(AuthError::Conflict)
    ));
    let remaining = f
        .store
        .transaction(|connection| {
            read_assignments(
                connection,
                &AuthRoleAssignmentsListRequest {
                    principal_id: Some(new_assignment.principal_id),
                    role_id: Some(new_assignment.role_id),
                    page: None,
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert!(remaining.items.contains(&current.assignment));
}

#[test]
fn policy_lists_apply_filters_and_resume_without_duplicating_items() {
    let f = fixture();
    let mut assignments = Vec::new();
    for capability in [&f.read, &f.write] {
        let role = AuthRolePutRequest {
            role_id: None,
            expected_revision: 0.into(),
            title: "Team".to_string().into(),
            description: "Access".to_string().into(),
            capability_ids: vec![capability.clone().into()],
            idempotency_key: id().into(),
            extra: Default::default(),
        };
        let role_id = put_role(&f.store, &mutation(&f.admin, &role), &role, NOW)
            .unwrap()
            .role
            .role_id;
        let input = AuthRoleAssignRequest {
            principal_id: f.principal.parse().unwrap(),
            role_id,
            expected_revision: 0.into(),
            expires_at: None,
            idempotency_key: id().into(),
            extra: Default::default(),
        };
        assignments.push(
            assign_role(&f.store, &mutation(&f.admin, &input), &input, NOW)
                .unwrap()
                .assignment,
        );
    }
    assignments.sort_by_key(|left| left.assignment_id);
    let input = AuthRoleAssignmentsListRequest {
        principal_id: Some(f.principal.parse().unwrap()),
        role_id: None,
        page: Some(CursorQuery {
            cursor: None,
            limit: Some(1),
        }),
        extra: Default::default(),
    };
    let first = f
        .store
        .transaction(|connection| read_assignments(connection, &input))
        .unwrap();
    let second = f
        .store
        .transaction(|connection| {
            read_assignments(
                connection,
                &AuthRoleAssignmentsListRequest {
                    page: Some(CursorQuery {
                        cursor: first.page.next_cursor.clone(),
                        limit: Some(1),
                    }),
                    ..input
                },
            )
        })
        .unwrap();
    assert_eq!(first.items, vec![assignments[0].clone()]);
    assert_eq!(second.items, vec![assignments[1].clone()]);
    assert!(second.page.next_cursor.is_none());
    let filtered = f
        .store
        .transaction(|connection| {
            read_assignments(
                connection,
                &AuthRoleAssignmentsListRequest {
                    principal_id: Some(f.principal.parse().unwrap()),
                    role_id: Some(assignments[1].role_id),
                    page: None,
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(filtered.items, vec![assignments[1].clone()]);

    let input = AuthCapabilityGrantsListRequest {
        principal_id: Some(f.principal.parse().unwrap()),
        capability_id: None,
        page: Some(CursorQuery {
            cursor: None,
            limit: Some(1),
        }),
        extra: Default::default(),
    };
    let first = f
        .store
        .transaction(|connection| read_grants(connection, &input))
        .unwrap();
    let second = f
        .store
        .transaction(|connection| {
            read_grants(
                connection,
                &AuthCapabilityGrantsListRequest {
                    page: Some(CursorQuery {
                        cursor: first.page.next_cursor.clone(),
                        limit: Some(1),
                    }),
                    ..input
                },
            )
        })
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].grant_id, second.items[0].grant_id);
    assert!(second.page.next_cursor.is_none());
    let filtered = f
        .store
        .transaction(|connection| {
            read_grants(
                connection,
                &AuthCapabilityGrantsListRequest {
                    principal_id: Some(f.principal.parse().unwrap()),
                    capability_id: Some(f.read.clone().into()),
                    page: None,
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].capability.capability_id.as_ref(), f.read);
    let access = f
        .store
        .transaction(|connection| read_effective_access(connection, &f.principal, NOW))
        .unwrap();
    let read = access
        .capabilities
        .iter()
        .find(|capability| capability.definition.identity.capability_id.as_ref() == f.read)
        .unwrap();
    assert!(read
        .sources
        .iter()
        .any(|source| source.grant_id.as_ref() == Some(&filtered.items[0].grant_id)));
}

#[test]
fn grant_ids_survive_updates_replay_after_revoke_and_reject_stale_tuple_ids() {
    let f = fixture();
    let principal = crate::platform::auth::phase_two_tests::create_user(&f.store, &f.admin, NOW);
    let grant = AuthCapabilityGrantRequest {
        principal_id: principal.parse().unwrap(),
        capability_id: f.read.clone().into(),
        expected_revision: 0.into(),
        expires_at: None,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let identity = mutation(&f.admin, &grant);
    let initial = grant_capability(&f.store, &identity, &grant, NOW).unwrap();
    let update = AuthCapabilityGrantRequest {
        expected_revision: 1.into(),
        expires_at: Some(
            time::OffsetDateTime::from_unix_timestamp(NOW + 20)
                .unwrap()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
                .parse()
                .unwrap(),
        ),
        idempotency_key: id().into(),
        ..grant.clone()
    };
    let updated =
        grant_capability(&f.store, &mutation(&f.admin, &update), &update, NOW + 1).unwrap();
    assert_eq!(updated.grant.grant_id, initial.grant.grant_id);
    assert_eq!(updated.grant.expires_at, update.expires_at);
    assert_eq!(
        grant_capability(&f.store, &identity, &grant, NOW + 2).unwrap(),
        initial
    );
    let revoke = AuthCapabilityRevokeRequest {
        grant_id: updated.grant.grant_id,
        expected_revision: AuthRevision(2.into()),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    let revoke_identity = mutation(&f.admin, &revoke);
    let revoked = revoke_grant(&f.store, &revoke_identity, &revoke, NOW + 2).unwrap();
    let recreated_input = AuthCapabilityGrantRequest {
        idempotency_key: id().into(),
        ..grant.clone()
    };
    let recreated = grant_capability(
        &f.store,
        &mutation(&f.admin, &recreated_input),
        &recreated_input,
        NOW + 3,
    )
    .unwrap();
    assert_ne!(recreated.grant.grant_id, initial.grant.grant_id);
    assert_eq!(
        revoke_grant(&f.store, &revoke_identity, &revoke, NOW + 4).unwrap(),
        revoked
    );
    assert_eq!(
        grant_capability(&f.store, &identity, &grant, NOW + 4).unwrap(),
        initial
    );
    let stale = AuthCapabilityRevokeRequest {
        expected_revision: AuthRevision(1.into()),
        idempotency_key: id().into(),
        ..revoke
    };
    assert!(matches!(
        revoke_grant(&f.store, &mutation(&f.admin, &stale), &stale, NOW + 4),
        Err(AuthError::NotFound)
    ));
    let listed = f
        .store
        .transaction(|connection| {
            read_grants(
                connection,
                &AuthCapabilityGrantsListRequest {
                    principal_id: Some(grant.principal_id),
                    capability_id: Some(grant.capability_id),
                    page: None,
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(listed.items, vec![recreated.grant]);
}

#[test]
fn role_queries_follow_updates_paginate_and_hide_deleted_definitions() {
    let f = fixture();
    let mut ids = vec![id(), id()];
    ids.sort();
    for role_id in &ids {
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::RolePut {
                role_id: role_id.clone(),
                expected_revision: 0,
                title: "Reviewers".into(),
                description: "May read documents".into(),
                capabilities: vec![f.read.clone()],
            },
            NOW,
        );
    }
    let input = AuthPageRequest {
        page: Some(CursorQuery {
            cursor: None,
            limit: Some(1),
        }),
        search: Some("REVIEW".to_string().into()),
        extra: Default::default(),
    };
    let first = f
        .store
        .transaction(|connection| read_roles(connection, &input))
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].role_id.to_string(), ids[0]);
    assert_eq!(
        first.items[0].capabilities[0].capability_id.as_ref(),
        f.read
    );
    let second = f
        .store
        .transaction(|connection| {
            read_roles(
                connection,
                &AuthPageRequest {
                    page: Some(CursorQuery {
                        cursor: first.page.next_cursor,
                        limit: Some(1),
                    }),
                    ..input
                },
            )
        })
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].role_id.to_string(), ids[1]);
    assert!(second.page.next_cursor.is_none());

    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RolePut {
            role_id: ids[0].clone(),
            expected_revision: 1,
            title: "Editors".into(),
            description: "May write documents".into(),
            capabilities: vec![f.write.clone()],
        },
        NOW + 1,
    );
    let updated = f
        .store
        .transaction(|connection| read_role(connection, &ids[0]))
        .unwrap();
    assert_eq!(updated.title, "Editors");
    assert_eq!(updated.capabilities.len(), 1);
    assert_eq!(updated.capabilities[0].capability_id.as_ref(), f.write);
    let searched = f
        .store
        .transaction(|connection| {
            read_roles(
                connection,
                &AuthPageRequest {
                    page: None,
                    search: Some("editors".to_string().into()),
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(searched.items, vec![updated]);
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RoleDelete {
            role_id: ids[0].clone(),
            expected_revision: 2,
        },
        NOW + 2,
    );
    assert!(matches!(
        f.store
            .transaction(|connection| read_role(connection, &ids[0])),
        Err(AuthError::NotFound)
    ));
    let listed = f
        .store
        .transaction(|connection| {
            read_roles(
                connection,
                &AuthPageRequest {
                    page: None,
                    search: None,
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].role_id.to_string(), ids[1]);
}

#[test]
fn catalog_and_effective_access_project_accepted_actions_and_expiring_paths() {
    let f = fixture();
    let catalog = f
        .store
        .transaction(|connection| {
            read_capabilities(
                connection,
                &AuthCapabilitiesListRequest {
                    api_id: Some(f.api.clone().into()),
                    page: None,
                    search: Some("read".to_string().into()),
                    extra: Default::default(),
                },
            )
        })
        .unwrap();
    assert_eq!(catalog.items.len(), 1);
    assert_eq!(catalog.items[0].identity.capability_id.as_ref(), f.read);
    assert_eq!(catalog.items[0].allows.len(), 1);
    assert_eq!(catalog.items[0].allows[0].name.as_ref(), "Get");

    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: f.principal.clone(),
            capability_id: f.read.clone(),
            expected_revision: 1,
            expires_at: Some(NOW + 10),
        },
        NOW,
    );
    let before = f
        .store
        .transaction(|connection| read_effective_access(connection, &f.principal, NOW))
        .unwrap();
    assert!(before
        .capabilities
        .iter()
        .any(
            |cap| cap.definition.identity.capability_id.as_ref() == f.read
                && cap.expires_at.is_some()
        ));
    let after = f
        .store
        .transaction(|connection| read_effective_access(connection, &f.principal, NOW + 10))
        .unwrap();
    assert!(!after
        .capabilities
        .iter()
        .any(|cap| cap.definition.identity.capability_id.as_ref() == f.read));
    assert!(after
        .capabilities
        .iter()
        .any(|cap| cap.definition.identity.capability_id.as_ref() == f.write));
    assert!(after.platform_privileges.is_empty());
}
