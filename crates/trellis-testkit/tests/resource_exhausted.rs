//! Writes JetStream refuses for lack of room come back as a typed
//! `ResourceExhausted` naming the resource, from KV, store, State, jobs and
//! events alike, rather than as a generic backend failure.
use runtime_trellis::apps::runtime_trellis_native_state_caller as state_caller;
use runtime_trellis::participants::runtime_trellis_provider::{
    types::{ResourceValue, Value},
    Participant, Provider,
};
use trellis_rs::client::{ResourceExhausted, StateWriteError, TrellisClientError};
use trellis_rs::jobs::JobsError;
use trellis_rs::service::{KvResourceWriteError, ServerError};
use trellis_testkit::{TestNatsUser, TrellisTestRuntime};

const MIB: u64 = 1 << 20;

/// Asserts `exhausted` names the `kind` resource `name`.
fn names(exhausted: &ResourceExhausted, kind: &str, name: &str) {
    assert_eq!(
        (exhausted.kind.as_str(), exhausted.name.as_str()),
        (kind, name),
        "{exhausted}"
    );
    assert!(!exhausted.reason.is_empty(), "{exhausted}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_refused_for_lack_of_room_name_the_resource() {
    // A broker with little file storage, which the service then fills.
    let mut runtime = TrellisTestRuntime::builder()
        .jetstream_max_file_store(256 * MIB)
        .start()
        .await
        .unwrap();
    let identity = runtime
        .register_service::<Participant>("full broker")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let events = runtime_trellis::apis::runtime_trellis_runtime_v1::Client::from_generated(
        service.generated_client(),
    );
    let provider = Provider::new(&mut service);
    let client = provider.client();
    let records = client.records().await.unwrap();
    let files = client.files().await.unwrap();
    let jobs = provider.jobs_client();
    let caller = runtime
        .register_client::<state_caller::Request>("full broker state")
        .await
        .unwrap();
    let caller = state_caller::Client::from_generated(
        trellis_rs::generated::Client::connect_user(caller.connect_options())
            .await
            .unwrap(),
    );
    let (state, _) = caller
        .saved_resource("0", ulid::Ulid::generate().to_string().parse().unwrap())
        .await
        .unwrap();
    let value = |text: &str| Value {
        value: text.into(),
        extra: Default::default(),
    };

    // Half-megabyte values until JetStream has no room for the next one.
    let large = ResourceValue {
        value: "x".repeat(512 * 1024),
        extra: Default::default(),
    };
    let mut refused = None;
    for index in 0..2_000 {
        match records.put(&format!("fill.{index}"), &large).await {
            Ok(_) => {}
            Err(error) => {
                refused = Some(error);
                break;
            }
        }
    }
    match refused.expect("the broker ran out of room") {
        KvResourceWriteError::ResourceExhausted(exhausted) => {
            names(&exhausted, "kv", "records");
        }
        other => panic!("a KV write refused for room is typed: {other:?}"),
    }

    match files.write("full/object", vec![7u8; 64 * 1024]).await {
        Err(ServerError::ResourceExhausted(exhausted)) => names(&exhausted, "store", "files"),
        other => panic!("a store write refused for room is typed: {other:?}"),
    }
    match jobs.create_work(value("no room")).await {
        Err(JobsError::ResourceExhausted(exhausted)) => names(&exhausted, "jobs", "work"),
        other => panic!("a job refused for room is typed: {:?}", other.err()),
    }
    match events.publish_changed(&value("full")).await {
        Err(TrellisClientError::ResourceExhausted(exhausted)) => {
            assert_eq!(exhausted.kind, "events", "{exhausted}");
            // The subject carries the event's `value`, base64url-encoded.
            assert!(exhausted.name.ends_with(".Changed.ZnVsbA"), "{exhausted}");
        }
        other => panic!("an event refused for room is typed: {other:?}"),
    }
    match state
        .set(&state_caller::types::ResourceValue {
            value: "no room".into(),
            extra: Default::default(),
        })
        .await
    {
        Err(StateWriteError::ResourceExhausted(exhausted)) => {
            names(&exhausted, "state", "savedResource");
        }
        other => panic!("a State write refused for room is typed: {other:?}"),
    }

    // Making room, as an operator would by purging the filled bucket, lets
    // the same handle write again.
    let nats = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::Trellis).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    async_nats::jetstream::new(nats)
        .get_stream(format!("KV_{}", records.binding().bucket))
        .await
        .unwrap()
        .purge()
        .await
        .unwrap();
    records.put("after", &large).await.unwrap();
    drop(service);
    runtime.shutdown().await.unwrap();
}
