//! A service lists the live keys of its KV resource.
use futures_util::TryStreamExt;
use runtime_trellis::participants::runtime_trellis_provider::{
    types::ResourceValue, Participant, Provider,
};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keys_lists_live_keys_matching_a_filter() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("kv keys")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let records = Provider::new(&mut service)
        .client()
        .records()
        .await
        .unwrap();
    for key in ["runs.a.one", "runs.a.two", "runs.b.one", "other"] {
        let value = ResourceValue {
            value: key.into(),
            extra: Default::default(),
        };
        records.put(key, &value).await.unwrap();
    }
    records.delete("runs.a.two", None).await.unwrap();

    let list = |filter: &'static str| {
        let records = records.clone();
        async move {
            let mut keys: Vec<String> = records
                .keys(filter)
                .await
                .unwrap()
                .try_collect()
                .await
                .unwrap();
            keys.sort();
            keys
        }
    };
    assert_eq!(list(">").await, ["other", "runs.a.one", "runs.b.one"]);
    assert_eq!(list("runs.>").await, ["runs.a.one", "runs.b.one"]);
    assert_eq!(list("runs.*.one").await, ["runs.a.one", "runs.b.one"]);
    assert_eq!(list("runs.a.*").await, ["runs.a.one"]);
    drop(service);
    runtime.shutdown().await.unwrap();
}
