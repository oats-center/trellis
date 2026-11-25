//! `trellis-server import` into an installation that already has what the
//! export carries, and its preview: what the preview reports is what the
//! import then does, and what an import cannot finish yet is reported with
//! the step that finishes it.
use runtime_trellis::apps::runtime_trellis_native_state_caller as state_caller;
use serde_json::Value;
use trellis_rs::generated::Client;
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
    assert!(listed(&first["import"], "created", "state", &state), "{first}");
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
