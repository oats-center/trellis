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
