//! A generated publish fills the event's subject parameters, so a consumer
//! filtering on one parameter value receives that event and no other.
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::StreamExt as _;
use runtime_trellis::apis::runtime_trellis_runtime_v1::{events::Changed, Client};
use runtime_trellis::participants::runtime_trellis_provider::{types::Value, Participant};
use trellis_testkit::{TestNatsUser, TrellisTestRuntime};

/// `Changed`'s concrete subject for `value`, which the subject carries as its
/// last token, base64url-encoded.
fn changed_subject(value: &str) -> String {
    Changed::SUBJECT.replace("{/value}", &URL_SAFE_NO_PAD.encode(value))
}

/// The next message `subscription` receives.
async fn next(subscription: &mut async_nats::Subscriber) -> async_nats::Message {
    tokio::time::timeout(Duration::from_secs(10), subscription.next())
        .await
        .expect("an event arrives")
        .expect("the subscription stays open")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consumer_filtering_on_a_parameter_receives_only_that_value() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("event subjects")
        .await
        .unwrap();
    let service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let events = Client::from_generated(service.generated_client());
    let broker = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::Trellis).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let wanted_subject = changed_subject("wanted");
    let mut wanted = broker.subscribe(wanted_subject.clone()).await.unwrap();
    let mut all = broker.subscribe(Changed::SUBSCRIBE_SUBJECT).await.unwrap();
    broker.flush().await.unwrap();

    for value in ["other", "wanted"] {
        events
            .publish_changed(&Value {
                value: value.into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
    }

    // Every publish lands on its own value's subject.
    let first = next(&mut all).await;
    let second = next(&mut all).await;
    assert_eq!(
        [first.subject.as_str(), second.subject.as_str()],
        [changed_subject("other").as_str(), wanted_subject.as_str()]
    );
    // The filtered consumer gets the `wanted` event and nothing from `other`.
    let received = next(&mut wanted).await;
    let payload: serde_json::Value = serde_json::from_slice(&received.payload).unwrap();
    assert_eq!(payload["value"], "wanted", "{payload}");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), wanted.next())
            .await
            .is_err(),
        "the filtered consumer received an event for another value"
    );

    drop(events);
    drop(service);
    runtime.shutdown().await.unwrap();
}
