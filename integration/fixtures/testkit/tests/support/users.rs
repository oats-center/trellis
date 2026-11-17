//! Public Rust setup parity: one app, three real independently authenticated logins.

use trellis_rs::generated::{AppRequestDescriptor, Client as GeneratedClient};
use trellis_test_fixture::apps::trellis_test_fixture_agent_caller::{Client, Request};
use trellis_test_fixture::participants::trellis_test_fixture_provider::{Participant, Provider};
use trellis_test_fixture::types::Value;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_native_directory_distinguishes_public_peer_profiles_from_self_accounts() {
    use runtime_trellis::apps::runtime_trellis_directory_caller::Request as Directory;
    use trellis_runtime_apis::{
        apis::trellis_auth_v1::Client as AuthClient,
        types::{AuthEmpty, AuthPageRequest, AuthUserGetRequest},
    };
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let alice = runtime.create_user("Native directory twin").await.unwrap();
    let bob = runtime.create_user("Native directory twin").await.unwrap();
    for user in [&alice, &bob] {
        runtime
            .grant_user_capabilities(user, Directory::REQUIRED_CAPABILITIES)
            .await
            .unwrap();
    }
    let app = runtime
        .register_native_application::<Directory>("native directory")
        .await
        .unwrap();
    for (user, peer) in [(&alice, &bob), (&bob, &alice)] {
        let login = runtime
            .login_native_application_as(&app, user)
            .await
            .unwrap();
        assert_eq!(login.client_id(), app.client_id());
        let client = GeneratedClient::connect_user(login.connect_options())
            .await
            .unwrap();
        let auth = AuthClient::from_generated(client.clone());
        let own = auth
            .users_resolve(&AuthEmpty {
                extra: Default::default(),
            })
            .await
            .unwrap()
            .user;
        assert_eq!(own.principal_id.to_string(), user.principal_id());
        assert_eq!(own.account.unwrap().username, user.username());
        let public = auth
            .users_get(&AuthUserGetRequest {
                user_id: peer.principal_id().parse().unwrap(),
                extra: Default::default(),
            })
            .await
            .unwrap()
            .user;
        assert_eq!(public.principal_id.to_string(), peer.principal_id());
        assert_eq!(public.display_name, "Native directory twin");
        assert!(public.account.is_none());
        let page = auth
            .users_list(&AuthPageRequest {
                search: Some("NATIVE DIRECTORY TWIN".to_owned().into()),
                page: None,
                extra: Default::default(),
            })
            .await
            .unwrap();
        let mut ids = page
            .items
            .iter()
            .map(|profile| profile.principal_id.to_string())
            .collect::<Vec<_>>();
        ids.sort();
        let mut expected = vec![
            alice.principal_id().to_owned(),
            bob.principal_id().to_owned(),
        ];
        expected.sort();
        assert_eq!(ids, expected);
        assert!(page.items.iter().all(|profile| profile.account.is_none()));
        drop(auth);
        drop(client);
    }
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registered_native_app_authenticates_multiple_users_and_sessions() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("user-identity-provider")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let mut provider = Provider::new(&mut service);
    provider
        .trellis_test_fixture_echo_v1()
        .register_echo(|context, _| async move {
            let caller = context
                .request()
                .caller
                .as_ref()
                .expect("generated RPC requires a verified caller");
            Ok(Value {
                value: serde_json::to_string(&caller).unwrap(),
                extra: Default::default(),
            })
        });
    drop(provider);
    let serving = tokio::spawn(service.run());
    let alice = runtime.create_user("Alice").await.unwrap();
    let bob = runtime.create_user("Bob").await.unwrap();
    runtime
        .grant_user_capabilities(&alice, Request::REQUIRED_CAPABILITIES)
        .await
        .unwrap();
    runtime
        .grant_user_capabilities(&bob, Request::REQUIRED_CAPABILITIES)
        .await
        .unwrap();
    let app = runtime
        .register_native_application::<Request>("shared native application")
        .await
        .unwrap();
    let mut observed = Vec::new();
    for user in [&alice, &bob, &alice] {
        let identity = runtime
            .login_native_application_as(&app, user)
            .await
            .unwrap();
        assert_eq!(identity.client_id(), app.client_id());
        let client = Client::from_generated(
            GeneratedClient::connect_user(identity.connect_options())
                .await
                .unwrap(),
        );
        let response = client
            .trellis_test_fixture_echo_v1()
            .echo(&Value {
                value: "who".into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
        let caller: trellis_protocol::AuthenticatedCaller =
            serde_json::from_str(&response.value).unwrap();
        assert_eq!(caller.principal_id, user.principal_id());
        let trellis_protocol::SessionBinding::Native {
            client_id,
            durable_dpop_jkt,
        } = caller.binding
        else {
            panic!("native login required")
        };
        assert_eq!(client_id, app.client_id());
        observed.push((
            caller.principal_id,
            durable_dpop_jkt,
            caller.authorization_session_id,
        ));
        drop(client);
    }
    assert_eq!(observed[0].0, observed[2].0);
    assert_ne!(observed[0].0, observed[1].0);
    assert_eq!(
        observed
            .iter()
            .map(|value| &value.1)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        observed
            .iter()
            .map(|value| &value.2)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    serving.abort();
    assert!(serving.await.unwrap_err().is_cancelled());
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn users_without_roles_use_their_own_account_and_not_anyone_elses() {
    use serde_json::json;
    use trellis_runtime_apis::apis::trellis_auth_v1::Client as AuthClient;
    use trellis_runtime_apis::apps::trellis_cli::Request as Cli;
    fn input<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }
    let key = || ulid::Ulid::generate().to_string();
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    // Neither user is given a role or a capability.
    let alice = runtime.create_user("Alice").await.unwrap();
    let bob = runtime.create_user("Bob").await.unwrap();
    let app = runtime
        .register_native_application::<Cli>("account self-service")
        .await
        .unwrap();
    let mut logins = Vec::new();
    for user in [&alice, &alice, &bob] {
        let login = runtime
            .login_native_application_as(&app, user)
            .await
            .unwrap();
        let id = login.login_session_id().to_owned();
        let client = GeneratedClient::connect_user(login.connect_options())
            .await
            .unwrap();
        logins.push((id, client));
    }
    let [(_, alice_client), (alice_second_login, alice_second), (bob_login, bob_client)] =
        <[_; 3]>::try_from(logins).ok().unwrap();
    let alice_auth = AuthClient::from_generated(alice_client.clone());
    let bob_auth = AuthClient::from_generated(bob_client.clone());

    // Alice reads and renames her own account.
    let own = alice_auth
        .users_resolve(&input(json!({})))
        .await
        .unwrap()
        .user;
    assert_eq!(own.principal_id.to_string(), alice.principal_id());
    let renamed = alice_auth
        .users_profile_update(&input(json!({
            "displayName": "Alice Renamed",
            "expectedRevision": own.account.unwrap().profile_revision,
            "idempotencyKey": key(),
        })))
        .await
        .unwrap()
        .user;
    assert_eq!(renamed.principal_id.to_string(), alice.principal_id());
    assert_eq!(renamed.display_name, "Alice Renamed");
    // She sees her own sign-ins and connected apps, and only hers.
    let sessions = alice_auth
        .sessions_list(&input(json!({})))
        .await
        .unwrap()
        .items;
    assert!(sessions
        .iter()
        .any(|login| login.login_session_id.to_string() == alice_second_login));
    assert!(sessions
        .iter()
        .all(|login| login.principal_id.to_string() == alice.principal_id()));
    let delegations = alice_auth
        .delegations_list(&input(json!({})))
        .await
        .unwrap()
        .items;
    assert!(!delegations.is_empty());
    assert!(delegations
        .iter()
        .all(|delegation| delegation.principal_id.to_string() == alice.principal_id()));
    alice_auth
        .user_identities_list(&input(json!({})))
        .await
        .unwrap();

    // Nothing reaches Bob's account: not his sign-ins, apps, or profile.
    assert!(alice_auth
        .sessions_revoke(&input(
            json!({"loginSessionId": bob_login, "idempotencyKey": key()})
        ))
        .await
        .is_err());
    assert!(alice_auth
        .delegations_list(&input(json!({"principalId": bob.principal_id()})))
        .await
        .is_err());
    let bob_delegation = bob_auth
        .delegations_list(&input(json!({})))
        .await
        .unwrap()
        .items
        .remove(0);
    assert!(alice_auth
        .delegations_review(&input(
            json!({"delegationId": bob_delegation.delegation_id})
        ))
        .await
        .is_err());
    assert!(alice_auth
        .users_get(&input(json!({"userId": bob.principal_id()})))
        .await
        .is_err());
    bob_client.refresh_authorization_context().await.unwrap();
    let bob_own = bob_auth
        .users_resolve(&input(json!({})))
        .await
        .unwrap()
        .user;
    assert_eq!(bob_own.display_name, "Bob");

    // Alice can end one of her own sign-ins.
    alice_auth
        .sessions_revoke(&input(json!({
            "loginSessionId": alice_second_login,
            "idempotencyKey": key(),
        })))
        .await
        .unwrap();
    assert!(alice_second.refresh_authorization_context().await.is_err());
    alice_client.refresh_authorization_context().await.unwrap();
    runtime.shutdown().await.unwrap();
}
