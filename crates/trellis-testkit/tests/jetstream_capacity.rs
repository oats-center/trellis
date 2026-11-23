//! Trellis on a broker with little JetStream storage, as on a small VM whose
//! NATS sizes `max_file_store` from about a gigabyte of free disk.
use std::time::Duration;

use futures_util::StreamExt as _;
use runtime_trellis::participants::runtime_trellis_webhook_service::{Participant, Provider};
use serde_json::json;
use trellis_testkit::{TestNatsUser, TrellisTestRuntime};

const GIB: u64 = 1 << 30;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_small_broker_holds_every_stream_and_a_webhook_intake() {
    let mut runtime = TrellisTestRuntime::builder()
        .jetstream_max_file_store(GIB)
        .start()
        .await
        .unwrap();
    let identity = runtime
        .register_service::<Participant>("billing")
        .await
        .unwrap();
    runtime
        .set_config(
            &identity,
            json!({ "partnerUser": "partner", "relayRanges": ["127.0.0.0/8"] }),
            json!({
                "signing": "signing-secret",
                "hookToken": "right-token-value",
                "partnerPassword": "partner-password",
            }),
        )
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let provider = Provider::new(&mut service);
    let mut deliveries = provider.token().unwrap().subscribe().await.unwrap();
    let urls = runtime.webhook_urls(&identity).await.unwrap();
    let http = runtime.https_client_builder().build().unwrap();

    // An `async` endpoint stores the request in the deployment's intake
    // stream before it answers, so a 200 and the delivery mean the stream
    // was created next to every runtime stream.
    let response = http
        .post(&urls["token"])
        .header("x-hook-token", "right-token-value")
        .body("first")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let delivery = tokio::time::timeout(Duration::from_secs(20), deliveries.next())
        .await
        .expect("the Received event arrives")
        .unwrap()
        .unwrap();
    assert_eq!(delivery.body.0, b"first");

    // Together the streams reserve a small share of the gigabyte.
    let nats = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::Trellis).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let jetstream = async_nats::jetstream::new(nats);
    let account = jetstream.query_account().await.unwrap();
    assert!(
        account.reserved_storage <= GIB / 8,
        "the streams reserve {} bytes",
        account.reserved_storage
    );
    drop(deliveries);
    drop(service);
    runtime.shutdown().await.unwrap();
}
