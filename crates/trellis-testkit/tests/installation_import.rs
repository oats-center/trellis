//! `trellis-server import` into an installation that already has what the
//! export carries, and its preview: what the preview reports is what the
//! import then does, and what an import cannot finish yet is reported with
//! the step that finishes it.
use std::time::Duration;

use runtime_trellis::apps::runtime_trellis_native_state_caller as state_caller;
use runtime_trellis::participants::runtime_trellis_moving_service::{
    types::Value as Payload, Participant as Moving, Provider as MovingProvider,
};
use serde_json::{json, Value};
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::generated::Client;
use trellis_rs::jobs::JobProcessError;
use trellis_rs::service::ServiceConnectOptions;
use trellis_runtime_apis::apis::trellis_auth_v1;
use trellis_testkit::{TestRegisteredNativeClient, TestUserIdentity, TrellisTestRuntime};

const PASSPHRASE: &str = "the passphrase of a moving installation";
/// What a person needs to keep State through an app.
const STATE_CAPABILITIES: [&str; 3] = [
    "trellis.state.v1:values.delete",
    "trellis.state.v1:values.read",
    "trellis.state.v1:values.write",
];

/// Whether `report`'s `list` has an entry of `kind` for `id`.
fn listed(report: &Value, list: &str, kind: &str, id: &str) -> bool {
    report[list].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["kind"] == kind && item["id"] == id)
    })
}

/// `user`'s State value through `app` on `runtime`, after writing `write`
/// when it is given.
async fn saved(
    runtime: &TrellisTestRuntime,
    app: &TestRegisteredNativeClient<state_caller::Request>,
    user: &TestUserIdentity,
    write: Option<&str>,
) -> Option<String> {
    let login = runtime
        .login_native_application_as(app, user)
        .await
        .unwrap();
    let caller = state_caller::Client::from_generated(
        Client::connect_user(login.connect_options()).await.unwrap(),
    );
    let (state, _) = caller
        .saved_resource("0", ulid::Ulid::generate().to_string().parse().unwrap())
        .await
        .unwrap();
    if let Some(value) = write {
        state
            .set(&state_caller::types::ResourceValue {
                value: value.into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
    }
    state.get().await.unwrap().map(|entry| entry.value.value)
}

/// A person's State moves once; when they have since changed it here, the
/// same export imported again leaves their value as it is, and its preview
/// reports that value as in the way rather than as one it would write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_preview_reports_state_stored_here_as_in_the_way() {
    let mut source = TrellisTestRuntime::builder().start().await.unwrap();
    let mut target = TrellisTestRuntime::builder().start().await.unwrap();
    let person = source.create_user("Moving person").await.unwrap();
    source
        .grant_user_capabilities(&person, &STATE_CAPABILITIES)
        .await
        .unwrap();
    let app = source
        .register_native_application::<state_caller::Request>("stateful")
        .await
        .unwrap();
    saved(&source, &app, &person, Some("the source's value")).await;
    let file = source.workdir().join("installation.trellis-installation");
    source
        .export_installation(None, &file, PASSPHRASE)
        .await
        .unwrap();
    let state = format!("{}/savedResource", person.principal_id());

    let first = target
        .import_installation(&file, PASSPHRASE, false, Some(&source))
        .await
        .unwrap();
    assert!(
        listed(&first["import"], "created", "state", &state),
        "{first}"
    );
    assert_eq!(
        saved(&target, &app, &person, Some("changed here")).await,
        Some("changed here".into())
    );

    let checked = target
        .import_installation(&file, PASSPHRASE, true, None)
        .await
        .unwrap();
    let again = target
        .import_installation(&file, PASSPHRASE, false, None)
        .await
        .unwrap();
    for report in [&checked["import"], &again["import"]] {
        assert!(listed(report, "skipped", "state", &state), "{report}");
        assert!(!listed(report, "created", "state", &state), "{report}");
    }
    assert_eq!(
        saved(&target, &app, &person, None).await,
        Some("changed here".into())
    );

    target.shutdown().await.unwrap();
    source.shutdown().await.unwrap();
}

fn wire<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn key() -> String {
    ulid::Ulid::generate().to_string()
}

/// `runtime`'s administrator, signed in through the CLI's device flow.
async fn administrator(runtime: &mut TrellisTestRuntime) -> trellis_auth_v1::Client {
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("native").join(key()),
        NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::generate(),
        },
        server.trust.clone(),
    )
    .await
    .unwrap();
    let device = native
        .start_device(&json!([{
            "type": "trellis_capabilities",
            "required": [
                "trellis.auth.v1:users.manage",
                "trellis.auth.v1:accounts.manage",
                "trellis.auth.v1:roles.manage",
                "trellis.auth.v1:clients.manage"
            ],
            "optional": []
        }]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    trellis_auth_v1::Client::from_generated(
        Client::connect_user(UserConnectOptions::new(
            server.nats_servers,
            30_000,
            device.complete().await.unwrap(),
        ))
        .await
        .unwrap(),
    )
}

/// The instances of `deployment` on the installation behind `admin`.
async fn instances(admin: &trellis_auth_v1::Client, deployment: &str) -> Vec<Value> {
    let page = admin
        .service_instances_list(&wire(json!({ "deploymentId": deployment })))
        .await
        .unwrap();
    serde_json::to_value(page).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

/// A deployment none of whose instances can run moves its records, and the
/// import reports its data as not finished with what finishes it. Once an
/// instance is enabled here, the same import finishes it: nothing is
/// pending, and the waiting job runs on the moved service.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deployment_data_waiting_for_an_instance_is_reported_until_a_retry_finishes_it() {
    let mut source = TrellisTestRuntime::builder().start().await.unwrap();
    let mut target = TrellisTestRuntime::builder().start().await.unwrap();
    let service = source.register_service::<Moving>("waiting").await.unwrap();
    let deployment = service.deployment_id().to_owned();
    source
        .set_config(
            &service,
            json!({ "label": "waiting label" }),
            json!({ "apiKey": "waiting-key-0123456789" }),
        )
        .await
        .unwrap();
    let mut running = Moving::connect(service.connect_options()).await.unwrap();
    let job = MovingProvider::new(&mut running)
        .jobs_client()
        .create_work(Payload {
            value: "work for later".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let job_id = job.identity().id.clone();
    drop((job, running));
    let admin = administrator(&mut source).await;
    let instance = instances(&admin, &deployment).await.remove(0);
    admin
        .service_instances_disable(&wire(json!({
            "instanceId": instance["instanceId"],
            "expectedRevision": instance["revision"],
            "idempotencyKey": key()
        })))
        .await
        .unwrap();
    drop(admin);
    let file = source.workdir().join("installation.trellis-installation");
    source
        .export_installation(None, &file, PASSPHRASE)
        .await
        .unwrap();

    let first = target
        .import_installation(&file, PASSPHRASE, false, Some(&source))
        .await
        .unwrap();
    assert!(
        listed(&first["import"], "created", "deployment", &deployment),
        "{first}"
    );
    assert!(
        listed(&first["import"], "pending", "deploymentData", &deployment),
        "{first}"
    );
    let admin = administrator(&mut target).await;
    let instance = instances(&admin, &deployment).await.remove(0);
    admin
        .service_instances_enable(&wire(json!({
            "instanceId": instance["instanceId"],
            "expectedRevision": instance["revision"],
            "idempotencyKey": key()
        })))
        .await
        .unwrap();
    drop(admin);
    let again = target
        .import_installation(&file, PASSPHRASE, false, None)
        .await
        .unwrap();
    assert_eq!(again["import"]["pending"], json!([]), "{again}");

    let server = discover_native_oauth(target.trellis_url(), &target.trust_roots())
        .await
        .unwrap();
    let mut moved = tokio::time::timeout(
        Duration::from_secs(60),
        Moving::connect(ServiceConnectOptions::new(
            server.nats_servers,
            server.trust,
            service.seed(),
        )),
    )
    .await
    .expect("the moved service connects")
    .unwrap();
    let mut provider = MovingProvider::new(&mut moved);
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    provider
        .register_work(move |job| {
            let ran = ran.clone();
            async move {
                let _ = ran.send(job.id().to_owned());
                Ok::<_, JobProcessError<String>>(job.payload().clone())
            }
        })
        .await
        .unwrap();
    provider
        .runtime_trellis_moving_v1()
        .register_ping(|_, input| async move { Ok(input) });
    let lifecycle = moved.lifecycle();
    let serving = tokio::spawn(moved.run());
    assert!(lifecycle.ready().await, "the moved service serves");
    let ran = tokio::time::timeout(Duration::from_secs(30), runs.recv())
        .await
        .expect("the waiting job runs once the import finishes")
        .unwrap();
    assert_eq!(ran, job_id);
    lifecycle.stop();
    serving.await.unwrap().unwrap();

    target.shutdown().await.unwrap();
    source.shutdown().await.unwrap();
}
