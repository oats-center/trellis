//! An installation lost with its disk and its broker's store comes back from
//! a backup taken while it ran: the same people sign in, sessions keep
//! refreshing against the same signing keys, and services find their finished
//! jobs and stored files.
use std::time::Duration;

use runtime_trellis::apps::runtime_trellis_account_caller::Request as AccountCaller;
use runtime_trellis::apps::runtime_trellis_jobs_admin::Request as JobsAdmin;
use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::jobs::{JobProcessError, JobState};
use trellis_runtime_apis::apis::{trellis_auth_v1, trellis_jobs_v1};
use trellis_testkit::TrellisTestRuntime;

/// The signed-in principal, as Auth reports it to the session's own caller.
async fn signed_in_as(options: UserConnectOptions<'static>) -> serde_json::Value {
    let auth = trellis_auth_v1::Client::from_generated(
        trellis_rs::generated::Client::connect_user(options)
            .await
            .unwrap(),
    );
    let me = auth
        .sessions_me(&serde_json::from_value(serde_json::json!({})).unwrap())
        .await
        .unwrap();
    serde_json::to_value(me).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backup_taken_while_running_brings_back_users_sessions_jobs_and_files() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();

    // A service with a finished job and a stored file, larger than one
    // snapshot chunk and not compressible.
    let identity = runtime
        .register_service::<Participant>("drill")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let mut provider = Provider::new(&mut service);
    provider
        .register_work(|job| async move { Ok::<_, JobProcessError<String>>(job.payload().clone()) })
        .await
        .unwrap();
    let jobs = provider.jobs_client();
    let files = provider.client().files().await.unwrap();
    let mut state = 0x9e37_79b9_u32;
    let object: Vec<u8> = (0..300_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect();
    files.write("drill/object", object.clone()).await.unwrap();
    let job = jobs
        .create_work(Value {
            value: "kept".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let job_id = job.identity().id.clone();
    assert_eq!(job.wait().await.unwrap().state, JobState::Completed);

    // A person who signs in with a password.
    let user = runtime.create_user("Drill user").await.unwrap();
    runtime
        .grant_user_capabilities(&user, &["trellis.auth.v1:account.read"])
        .await
        .unwrap();
    let app = runtime
        .register_native_application::<AccountCaller>("account")
        .await
        .unwrap();
    let before = runtime
        .login_native_application_as(&app, &user)
        .await
        .unwrap();
    let me = signed_in_as(before.connect_options()).await;
    assert_eq!(me["principal"]["principalId"], user.principal_id(), "{me}");

    // A CLI session whose refresh token is not used again until after the
    // restore.
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("cli-custody"),
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
        .start_device(&serde_json::json!([{
            "type": "trellis_capabilities",
            "required": ["trellis.auth.v1:account.read"],
            "optional": []
        }]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    let mut session = device.complete().await.unwrap();
    let login = session.admission.credential.login_session_id.clone();
    let principal = session.admission.credential.principal_id.clone();

    let backup = runtime.workdir().join("backup");
    runtime.backup(&backup).await.unwrap();
    drop(files);
    drop(jobs);
    drop(service);
    runtime.restore(&backup).await.unwrap();

    // The installation signs with the same keys, so the session refreshes
    // with the token it held before the backup, and stays the same sign-in.
    let after = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    assert_eq!(after.trust.instance_id, server.trust.instance_id);
    assert_eq!(
        after.trust.issuer.public_key,
        server.trust.issuer.public_key
    );
    session.refresh(after.trust.clone()).await.unwrap();
    assert_eq!(session.admission.credential.login_session_id, login);
    let cli = signed_in_as(UserConnectOptions::new(after.nats_servers, 5_000, session)).await;
    assert_eq!(cli["principal"]["principalId"], principal.as_str());

    // The same person signs in with the same password.
    let again = runtime
        .login_native_application_as(&app, &user)
        .await
        .unwrap();
    assert_eq!(
        signed_in_as(again.connect_options()).await["principal"]["principalId"],
        user.principal_id()
    );

    // The service connects as before and reads its file back unchanged.
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let files = Provider::new(&mut service).client().files().await.unwrap();
    let restored = files
        .read("drill/object")
        .await
        .unwrap()
        .expect("stored file");
    assert!(
        restored.as_ref() == object.as_slice(),
        "the stored file changed"
    );

    // The finished job is still listed, finished.
    let admin = runtime
        .register_client::<JobsAdmin>("jobs admin")
        .await
        .unwrap();
    let admin = trellis_jobs_v1::Client::from_generated(
        trellis_rs::generated::Client::connect_user(admin.connect_options())
            .await
            .unwrap(),
    );
    let request = serde_json::from_value(serde_json::json!({})).unwrap();
    let listed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(listed) = admin.query(&request).await {
                let listed = serde_json::to_value(&listed).unwrap();
                if let Some(job) = listed["items"]
                    .as_array()
                    .and_then(|items| items.iter().find(|job| job["id"] == job_id.as_str()))
                {
                    break job.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the finished job is listed after the restore");
    assert_eq!(listed["state"], "completed");

    drop(service);
    runtime.shutdown().await.unwrap();
}

/// A recovery drill whose backup is missing, or lies in a folder the drill
/// deletes, is refused before anything is stopped or deleted: the backup is
/// still there, and the installation starts again on its own data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_from_a_backup_it_would_delete_leaves_the_installation_as_it_was() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let inside = runtime.workdir().join("certificates/backup");
    runtime.backup(&inside).await.unwrap();

    let refused = runtime.restore(&inside).await;
    assert!(refused.is_err(), "restored from a backup it deletes");
    assert!(
        inside.join("manifest.json").is_file(),
        "the restore deleted its own backup"
    );
    let missing = runtime.workdir().join("no-backup-here");
    let refused = runtime.restore(&missing).await;
    assert!(refused.is_err(), "restored from a missing backup");

    runtime.restart().await.unwrap();
    runtime.shutdown().await.unwrap();
}
