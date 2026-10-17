//! State through ordinary OAuth admission, generated clients and the production provider.

use super::{administrator, login, wire};
use futures_util::StreamExt;
use serde_json::json;
use std::time::Duration;
use trellis_rs::generated::Client as GeneratedClient;
use trellis_runtime_apis::{
    apis::trellis_state_v1::{rpc, Client},
    types::{Bytes, StateRepresentation},
};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_state_registration_cas_watch_and_client_isolation() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let admin = administrator(&runtime).await;
    let required = rpc::Get::CALLER_CAPABILITIES
        .iter()
        .chain(rpc::Put::CALLER_CAPABILITIES)
        .chain(rpc::Delete::CALLER_CAPABILITIES)
        .copied()
        .collect::<Vec<_>>();
    let options = login(
        &runtime,
        "trellis.cli",
        json!([{"type":"trellis_capabilities","required":required,"optional":[]}]),
        runtime.admin_username(),
        runtime.admin_password(),
        None,
    )
    .await;
    let base = GeneratedClient::connect_user(options).await.unwrap();
    let client = Client::from_generated(base.clone());
    let evidence = <runtime_trellis::participants::runtime_trellis_provider::Participant as trellis_rs::generated::ParticipantDescriptor>::package_evidence();
    let graph =
        trellis_idl::compile_evidence(wire(serde_json::to_value(evidence).unwrap())).unwrap();
    let reference = trellis_idl::TypeRef {
        package: graph.root().clone(),
        id: graph
            .root_package()
            .types()
            .keys()
            .find(|id| id.as_str() == "ResourceValue")
            .unwrap()
            .clone(),
    };
    let schema = trellis_idl::json_schema(&graph, &reference).unwrap();
    let representation = StateRepresentation {
        version: 1.into(),
        schema: Bytes(serde_json::to_vec(&schema).unwrap()),
        extra: Default::default(),
    };
    let registration = wire(
        json!({"resourceName":"saved","representation":representation,"accepts":[],"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}),
    );
    client.register(&registration).await.unwrap();
    let mut stale_registration = registration.clone();
    stale_registration.representation.version = 2.into();
    stale_registration.idempotency_key = ulid::Ulid::new().to_string().parse().unwrap();
    let conflict = client.register(&stale_registration).await;
    assert!(
        matches!(
            &conflict,
            Err(trellis_rs::client::CallError::Declared(error))
                if matches!(**error, rpc::RegisterError::RegistrationConflict(_))
        ),
        "unexpected registration outcome: {conflict:?}"
    );
    let get = wire(json!({"resourceName":"saved"}));
    assert!(client.get(&get).await.unwrap().entry.is_none());
    let mut watch = client.watch(&get).await.unwrap();
    let initial = tokio::time::timeout(Duration::from_secs(10), watch.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(initial.entry.is_none());
    let value = Bytes(serde_json::to_vec(&json!({"value":"persisted"})).unwrap());
    let written = client
        .put(&wire(
            json!({"resourceName":"saved","representationVersion":1,"mode":"create","value":value}),
        ))
        .await
        .unwrap()
        .entry;
    let changed = tokio::time::timeout(Duration::from_secs(10), watch.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .entry
        .unwrap();
    assert_eq!(changed.value, value);
    assert_eq!(changed.revision, written.revision);
    assert!(client.put(&wire(json!({"resourceName":"saved","representationVersion":1,"mode":"replace","revision":"0","value":value}))).await.is_err());
    assert_eq!(
        client.get(&get).await.unwrap().entry.unwrap().revision,
        written.revision
    );
    let other_id = format!("state-{}", ulid::Ulid::new());
    admin.o_auth_clients_put(&wire(json!({"clientId":other_id,"title":"Independent State client","description":"Namespace isolation","kind":"native","redirectUris":[],"impliedConsent":[],"development":true,"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()}))).await.unwrap();
    let other_options = login(
        &runtime,
        &other_id,
        json!([{"type":"trellis_capabilities","required":required,"optional":[]}]),
        runtime.admin_username(),
        runtime.admin_password(),
        None,
    )
    .await;
    let other_base = GeneratedClient::connect_user(other_options).await.unwrap();
    let other = Client::from_generated(other_base.clone());
    let mut other_registration = registration;
    other_registration.idempotency_key = ulid::Ulid::new().to_string().parse().unwrap();
    other.register(&other_registration).await.unwrap();
    assert!(other.get(&get).await.unwrap().entry.is_none());
    client
        .delete(&wire(
            json!({"resourceName":"saved","revision":written.revision}),
        ))
        .await
        .unwrap();
    let deleted = tokio::time::timeout(Duration::from_secs(10), watch.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(deleted.entry.is_none());
    assert!(client.get(&get).await.unwrap().entry.is_none());
    watch.close().await.unwrap();
    drop((client, base, other, other_base));
    runtime.shutdown().await.unwrap();
}
