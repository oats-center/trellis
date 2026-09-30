//! Partial lifecycle grants come from native participant resolution and real
//! Auth.Grants.Set. Negative admission must produce an authorization denial,
//! not merely a request timeout, and must leave no durable reservation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use trellis_rs::auth::{
    complete_local_login, connect_admin_client_async, start_agent_login, StartAgentLoginOpts,
};
use trellis_rs::client::{
    CallError, OperationInvoker, OperationState, TrellisClient, TrellisClientError,
};
use trellis_test_fixture::__types::{Int64, Nullable, Uint64};
use trellis_test_fixture::apis::trellis_auth_v1::Client as AuthClient;
use trellis_test_fixture::apis::trellis_test_fixture_echo_v1::operations::OptionalSilent;
use trellis_test_fixture::participants::trellis_test_fixture_operation_authorization_caller::Participant as FullCallerParticipant;
use trellis_test_fixture::participants::trellis_test_fixture_optional_operation_caller::{
    Client as CallerClient, Participant as CallerParticipant,
};
use trellis_test_fixture::participants::trellis_test_fixture_provider::{
    Participant as ProviderParticipant, Provider,
};
use trellis_test_fixture::types::{
    AuthGrantSet, AuthGrantsGetRequest, AuthGrantsGetRequestOwnerKind, AuthGrantsSetRequest,
    AuthGrantsSetRequestOwnerKind, AuthPermissionAtomAction, AuthSessionsMeRequest, Value,
};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_admission_requires_invoke_and_cancel_without_reserving_denied_ids() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let provider_identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .unwrap();
    let mut service = ProviderParticipant::connect(provider_identity.connect_options())
        .await
        .unwrap();
    let entries = Arc::new(AtomicUsize::new(0));
    let business = Arc::new(AtomicUsize::new(0));
    let handler_entries = Arc::clone(&entries);
    let handler_business = Arc::clone(&business);
    Provider::new(&mut service)
        .trellis_test_fixture_echo_v1()
        .register_optional_silent(move |_context, input, operation| {
            let entries = Arc::clone(&handler_entries);
            let business = Arc::clone(&handler_business);
            async move {
                entries.fetch_add(1, Ordering::SeqCst);
                if !operation.cancellation().is_cancelled() {
                    business.fetch_add(1, Ordering::SeqCst);
                    operation.complete(input).await?;
                }
                Ok(())
            }
        });
    let task = tokio::spawn(async move { service.run().await });
    let identity = runtime
        .register_client::<CallerParticipant>("optional-caller")
        .await
        .unwrap();
    let full_identity = runtime
        .register_client::<FullCallerParticipant>("full-caller")
        .await
        .unwrap();

    // The administrator logs in through the same ordinary portal as testkit.
    let challenge = start_agent_login(&StartAgentLoginOpts {
        trellis_url: runtime.trellis_url(),
        participant_id: "trellis.cli",
    })
    .await
    .unwrap();
    complete_local_login(
        runtime.trellis_url(),
        challenge.login_url(),
        runtime.admin_username(),
        runtime.admin_password(),
    )
    .await
    .unwrap();
    let state = challenge
        .complete_session(runtime.trellis_url())
        .await
        .unwrap();
    let admin = AuthClient::from_generated(connect_admin_client_async(&state).await.unwrap());
    let me = admin
        .sessions_me(&AuthSessionsMeRequest {
            extra: Default::default(),
        })
        .await
        .unwrap();
    let Nullable::Value(user) = me.user else {
        panic!("administrator has no user")
    };
    let owner_id = user.user_id.as_ref().to_owned();
    let initial = admin
        .grants_get(&AuthGrantsGetRequest {
            owner_id: owner_id.clone().into(),
            owner_kind: AuthGrantsGetRequestOwnerKind::User,
            participant_id: identity.participant_id().to_owned().into(),
            extra: Default::default(),
        })
        .await
        .unwrap()
        .binding;
    let Nullable::Value(initial) = initial else {
        panic!("caller grant missing")
    };
    let full_binding = admin
        .grants_get(&AuthGrantsGetRequest {
            owner_id: owner_id.clone().into(),
            owner_kind: AuthGrantsGetRequestOwnerKind::User,
            participant_id: full_identity.participant_id().to_owned().into(),
            extra: Default::default(),
        })
        .await
        .unwrap()
        .binding;
    let Nullable::Value(full_binding) = full_binding else {
        panic!("full caller grant missing")
    };
    // This sibling selects the same native operation as required. Its real
    // consent binding supplies the complete atom set; Set checks the optional
    // participant's own installed ceiling before accepting that subset.
    let full = full_binding.grants;
    assert!(full
        .permissions
        .iter()
        .any(|atom| atom.action == AuthPermissionAtomAction::Invoke));
    assert!(full
        .permissions
        .iter()
        .any(|atom| atom.action == AuthPermissionAtomAction::Cancel));

    let denied_ids = ["01J00000000000000000000031", "01J00000000000000000000032"];
    let input = Value {
        value: "permission proof".to_owned(),
        extra: Default::default(),
    };
    for (index, actions) in [
        vec![AuthPermissionAtomAction::Invoke],
        vec![AuthPermissionAtomAction::Cancel],
        vec![
            AuthPermissionAtomAction::Invoke,
            AuthPermissionAtomAction::Cancel,
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let binding = admin
            .grants_get(&AuthGrantsGetRequest {
                owner_id: owner_id.clone().into(),
                owner_kind: AuthGrantsGetRequestOwnerKind::User,
                participant_id: identity.participant_id().to_owned().into(),
                extra: Default::default(),
            })
            .await
            .unwrap()
            .binding;
        let Nullable::Value(binding) = binding else {
            panic!("caller grant missing")
        };
        // Copy whole native atoms, including opaque targets, without manufacturing them.
        // Restore Observe only for the final fully authorized absence checks.
        let grants = AuthGrantSet {
            permissions: full
                .permissions
                .iter()
                .filter(|atom| index == 2 || actions.contains(&atom.action))
                .cloned()
                .collect(),
            ..full.clone()
        };
        let changed = admin
            .grants_set(&AuthGrantsSetRequest {
                expected_revision: Uint64(u64::try_from(binding.revision.0 .0).unwrap()).into(),
                expires_at: Nullable::Null,
                grants,
                idempotency_key: format!("cancelled-admission-permissions-{index}").into(),
                installed_revision: Int64(initial.installed_revision.0 .0).into(),
                owner_id: owner_id.clone().into(),
                owner_kind: AuthGrantsSetRequestOwnerKind::User,
                participant_id: identity.participant_id().to_owned().into(),
                platform_privileges: vec![],
                extra: Default::default(),
            })
            .await
            .unwrap();
        let client = TrellisClient::connect_user(identity.connect_options())
            .await
            .unwrap();
        let issued = client.authorization_context().unwrap().unwrap();
        let issued_permissions = issued.context["grants"]["permissions"].as_array().unwrap();
        assert_eq!(
            issued_permissions.len(),
            changed.binding.grants.permissions.len()
        );
        for action in ["invoke", "cancel"] {
            assert_eq!(
                issued_permissions
                    .iter()
                    .any(|atom| atom["action"] == action),
                actions.iter().any(|granted| granted.as_str() == action)
            );
        }
        let invoker =
            OperationInvoker::<_, trellis_rs::generated::OperationAdapter<OptionalSilent>>::new(
                &client,
            );
        if index < 2 {
            if index == 0 {
                let error = invoker
                    .start_cancelled_with_invocation_id(denied_ids[index], &input)
                    .await
                    .unwrap_err();
                let TrellisClientError::RpcError(payload) = error else {
                    panic!("expected provider authorization denial, got {error:?}")
                };
                assert!(
                    payload.value().unwrap()["context"]["causeMessage"]
                        .as_str()
                        .unwrap()
                        .starts_with("request denied"),
                    "{payload:?}"
                );
            } else {
                // Direct low-level Invoke traffic without Invoke authority may be
                // broker-suppressed. The generated facade is the supported
                // permission-gated boundary; this proves client, not provider, denial.
                let generated = CallerClient::connect(identity.connect_options())
                    .await
                    .unwrap();
                let error = generated
                    .trellis_test_fixture_echo_v1()
                    .optional_silent()
                    .start_cancelled_with_invocation_id(denied_ids[index], &input)
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, CallError::AuthorizationUnavailable(_)),
                    "{error:?}"
                );
            }
            assert_eq!(entries.load(Ordering::SeqCst), 0);
            assert_eq!(business.load(Ordering::SeqCst), 0);
        } else {
            for id in denied_ids {
                let error = invoker.control(id).unwrap().get().await.unwrap_err();
                let TrellisClientError::RpcError(payload) = error else {
                    panic!("expected authoritative missing operation, got {error:?}")
                };
                assert_eq!(
                    payload.value().unwrap()["context"]["causeMessage"]
                        .as_str()
                        .unwrap(),
                    format!("operation '{id}' was not found")
                );
            }
            let operation = invoker
                .start_cancelled_with_invocation_id(denied_ids[0], &input)
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(20), operation.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                OperationState::Cancelled
            );
            assert_eq!(entries.load(Ordering::SeqCst), 1);
            assert_eq!(business.load(Ordering::SeqCst), 0);
        }
    }
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.unwrap();
}
