//! A Rust service reads its settings as the generated `Config` struct and
//! follows live changes, while restart-only fields keep their connect value.
use std::time::Duration;

use runtime_trellis::participants::runtime_trellis_configured_service::{Participant, Provider};
use serde_json::json;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rust_service_reads_typed_settings_and_follows_live_changes() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("configured")
        .await
        .unwrap();
    runtime
        .set_config(
            &identity,
            json!({ "endpoint": "https://one.example" }),
            json!({ "apiKey": "k1" }),
        )
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let mut config = Provider::new(&mut service).config();
    let current = config.current().unwrap();
    assert_eq!(current.endpoint, "https://one.example");
    assert_eq!(current.api_key, "k1");
    assert_eq!(
        current.signing_key,
        trellis_rs::generated::Nullable::Null,
        "an optional secret reads null"
    );
    assert_eq!(current.port, 8080);
    assert_eq!(serde_json::to_value(&current.level).unwrap(), json!("info"));
    assert_eq!(config.revision().deployment, Some(1));

    runtime
        .set_config(
            &identity,
            json!({ "level": "debug", "port": 9090 }),
            json!({}),
        )
        .await
        .unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(10), config.changed())
        .await
        .expect("a live change arrives")
        .unwrap();
    assert_eq!(
        serde_json::to_value(&changed.level).unwrap(),
        json!("debug")
    );
    assert_eq!(
        changed.port, 8080,
        "a restart-only field waits for reconnect"
    );
    assert_eq!(config.revision().deployment, Some(2));
    runtime.shutdown().await.unwrap();
}
