//! Registration behavior at the production SQLite transaction boundary.

use super::*;
use serde_json::json;
use trellis_protocol::SessionBinding;
use trellis_runtime_apis::types::{Bytes, StateRegisterRequest, StateRepresentation};

fn request(source: &str, version: u32, revision: &str) -> StateRegisterRequest {
    let graph = trellis_idl::compile_evidence(evidence(source)).unwrap();
    let schema = trellis_idl::json_schema(
        &graph,
        &trellis_idl::TypeRef {
            package: graph.root().clone(),
            id: graph
                .root_package()
                .types()
                .keys()
                .find(|id| id.as_str() == "Saved")
                .unwrap()
                .clone(),
        },
    )
    .unwrap();
    StateRegisterRequest {
        resource_name: "settings".to_owned().into(),
        representation: StateRepresentation {
            version: version.into(),
            schema: Bytes(serde_json::to_vec(&schema).unwrap()),
            extra: Default::default(),
        },
        accepts: vec![],
        expected_revision: revision.to_owned().into(),
        idempotency_key: id().parse().unwrap(),
        extra: Default::default(),
    }
}

#[test]
fn state_registration_is_namespace_owned_and_survives_native_key_replacement() {
    let fixture = fixture();
    let browser = SessionBinding::Browser {
        client_id: "browser-client".into(),
        origin: "https://app.example".into(),
    };
    let input = request("model Saved { value: string; }", 1, "0");
    fixture
        .store
        .register_state(&fixture.principal, &browser, &input, NOW)
        .unwrap()
        .unwrap();
    let (registered, _) =
        SqliteAuthorizationStore::state_namespace(&fixture.principal, &browser, "settings")
            .unwrap();
    let declaration = fixture.store.state_declaration(&registered).unwrap();
    assert!(jsonschema::validator_for(&declaration.schema)
        .unwrap()
        .is_valid(&json!({"value":"persisted"})));
    for (principal, binding) in [
        (fixture.admin.as_str(), browser.clone()),
        (
            fixture.principal.as_str(),
            SessionBinding::Browser {
                client_id: "other-client".into(),
                origin: "https://app.example".into(),
            },
        ),
        (
            fixture.principal.as_str(),
            SessionBinding::Browser {
                client_id: "browser-client".into(),
                origin: "https://other.example".into(),
            },
        ),
        (
            fixture.principal.as_str(),
            SessionBinding::Native {
                client_id: "browser-client".into(),
                durable_dpop_jkt: "native-key".into(),
            },
        ),
    ] {
        let (resource, _) =
            SqliteAuthorizationStore::state_namespace(principal, &binding, "settings").unwrap();
        assert!(matches!(
            fixture.store.state_declaration(&resource),
            Err(AuthError::NotFound)
        ));
    }
    let native = SessionBinding::Native {
        client_id: "native-client".into(),
        durable_dpop_jkt: "first-key".into(),
    };
    let mut native_input = input.clone();
    native_input.idempotency_key = id().parse().unwrap();
    fixture
        .store
        .register_state(&fixture.principal, &native, &native_input, NOW)
        .unwrap()
        .unwrap();
    let replacement = SessionBinding::Native {
        client_id: "native-client".into(),
        durable_dpop_jkt: "replacement-key".into(),
    };
    fixture
        .store
        .register_state(&fixture.principal, &replacement, &native_input, NOW + 1)
        .unwrap()
        .unwrap();
    let (resource, _) =
        SqliteAuthorizationStore::state_namespace(&fixture.principal, &replacement, "settings")
            .unwrap();
    drop(fixture.store);
    let reopened = open(&fixture.directory, SigningKey::from_bytes(&[42; 32]));
    assert!(
        jsonschema::validator_for(&reopened.state_declaration(&resource).unwrap().schema)
            .unwrap()
            .is_valid(&json!({"value":"after restart"}))
    );
}

#[test]
fn state_registration_cas_replay_and_history_prevent_schema_redefinition() {
    let fixture = fixture();
    let binding = SessionBinding::Native {
        client_id: "native-client".into(),
        durable_dpop_jkt: "key".into(),
    };
    let first = request("model Saved { value: string; }", 1, "0");
    let registered = fixture
        .store
        .register_state(&fixture.principal, &binding, &first, NOW)
        .unwrap()
        .unwrap();
    let stale = request("model Saved { count: int32; }", 2, "0");
    let conflict = fixture
        .store
        .register_state(&fixture.principal, &binding, &stale, NOW)
        .unwrap()
        .unwrap_err();
    assert_eq!(conflict.current_revision, registered.revision);
    let mut replacement = stale.clone();
    replacement.idempotency_key = id().parse().unwrap();
    replacement.expected_revision = registered.revision;
    fixture
        .store
        .register_state(&fixture.principal, &binding, &replacement, NOW + 1)
        .unwrap()
        .unwrap();
    let replay = fixture
        .store
        .register_state(&fixture.principal, &binding, &stale, NOW + 2)
        .unwrap()
        .unwrap_err();
    assert_eq!(replay.current_revision, conflict.current_revision);
    let (resource, _) =
        SqliteAuthorizationStore::state_namespace(&fixture.principal, &binding, "settings")
            .unwrap();
    let current = fixture.store.state_declaration(&resource).unwrap();
    let validator = jsonschema::validator_for(&current.schema).unwrap();
    assert!(validator.is_valid(&json!({"count":7})));
    assert!(!validator.is_valid(&json!({"value":"old"})));
    let redefine_retired = request("model Saved { count: int32; }", 1, "2");
    assert!(matches!(
        fixture
            .store
            .register_state(&fixture.principal, &binding, &redefine_retired, NOW + 3),
        Err(AuthError::Invalid(_))
    ));
}
