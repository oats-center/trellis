//! The IDs Trellis makes name their kind with a short prefix before the
//! ULID: a user's starts with `U`, a service deployment's with `S`, its
//! instance's with `SI` and a job's with `J`. An ID chosen from outside, as
//! an installation move or an administrator recreating a person gives one,
//! is kept exactly as it is given: bare, as earlier releases made them, or
//! with its kind's prefix. One with another kind's prefix, in lowercase, or
//! with characters that would change a NATS subject is refused.
use runtime_trellis::participants::runtime_trellis_moving_service::{
    types::Value as Payload, Participant as Moving, Provider as MovingProvider,
};
use trellis_protocol::model::{parse_id, IdKind};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_ids_name_their_kind_and_chosen_ones_are_kept_as_given() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let made = |id: &str, kind: IdKind| {
        assert!(
            id.starts_with(kind.prefix()) && parse_id(id, &[kind]).is_ok(),
            "{id} is not a new {kind:?} ID"
        );
    };

    let person = runtime.create_user("Pat").await.unwrap();
    made(person.principal_id(), IdKind::User);
    let service = runtime.register_service::<Moving>("typed").await.unwrap();
    made(service.deployment_id(), IdKind::ServiceDeployment);
    made(service.instance_id(), IdKind::ServiceInstance);
    // The service waits to connect until its required settings are set.
    runtime
        .set_config(
            &service,
            serde_json::json!({ "label": "typed label" }),
            serde_json::json!({ "apiKey": "typed-key-0123456789" }),
        )
        .await
        .unwrap();
    let mut running = Moving::connect(service.connect_options()).await.unwrap();
    let job = MovingProvider::new(&mut running)
        .jobs_client()
        .create_work(Payload {
            value: "typed work".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    made(&job.identity().id, IdKind::Job);
    drop((job, running));

    let bare = ulid::Ulid::generate().to_string();
    let prefixed = format!("U{}", ulid::Ulid::generate());
    for (username, chosen) in [("bare-user", &bare), ("prefixed-user", &prefixed)] {
        let kept = runtime
            .create_user_as(username, username, Some(chosen))
            .await
            .unwrap();
        assert_eq!(kept.principal_id(), chosen.as_str());
    }
    for (username, refused) in [
        ("another-kind", format!("S{}", ulid::Ulid::generate())),
        (
            "lowercase",
            format!("u{}", ulid::Ulid::generate()).to_lowercase(),
        ),
        ("wildcard", format!("U{}.*", ulid::Ulid::generate())),
    ] {
        assert!(
            runtime
                .create_user_as(username, username, Some(&refused))
                .await
                .is_err(),
            "user ID {refused} was accepted"
        );
    }
    runtime.shutdown().await.unwrap();
}
