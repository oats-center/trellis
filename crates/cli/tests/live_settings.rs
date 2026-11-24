//! A Rust service checks its settings before they take effect, and the CLI
//! shows what each instance did with them: a revision a check or a derive
//! builder refuses leaves the service on the values it had, shows as
//! rejected in `config show` and `instances` and in the instance's health,
//! and can be rolled back. The revision an instance starts with shows as
//! applied only once it serves, so one a startup check refuses never does,
//! and a report Auth never answered is sent again once it answers.
//! One `config set` writes plain values and several secrets as one revision,
//! which the service hears as one change.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use runtime_trellis::participants::runtime_trellis_configured_service::{
    Config, Participant, Provider,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::generated::Nullable;
use trellis_rs::service::{ConfigHandle, Rejection, ServicePhase};
use trellis_testkit::{TestNatsUser, TrellisTestRuntime};

const CLI: &str = env!("CARGO_BIN_EXE_trellis");

/// The production CLI signed in to `runtime` as its administrator.
#[derive(Clone)]
struct Cli {
    config: PathBuf,
}

impl Cli {
    async fn login(runtime: &mut TrellisTestRuntime) -> Self {
        let config = runtime
            .workdir()
            .join(format!("cli-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&config).unwrap();
        let mut login = tokio::process::Command::new(CLI)
            .args([
                "--format",
                "json",
                "login",
                runtime.trellis_url(),
                "--ca-certificate",
            ])
            .arg(&runtime.trust_roots()[0])
            .env("XDG_CONFIG_HOME", &config)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(login.stderr.take().unwrap()).lines();
        let challenge: Value = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("login printed a challenge");
                if line.starts_with('{') {
                    return serde_json::from_str(&line).unwrap();
                }
            }
        })
        .await
        .unwrap();
        runtime
            .approve_device_login_with_administration(
                "trellis.cli",
                challenge["userCode"].as_str().unwrap(),
            )
            .await
            .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(30), login.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "login failed");
        Self { config }
    }

    async fn run(&self, args: &[&str]) -> std::process::Output {
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(CLI)
                .args(args)
                .env("XDG_CONFIG_HOME", &self.config)
                .env("NO_COLOR", "1")
                .env("COLUMNS", "400")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("the command finishes")
        .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// What a person reads from `args`.
    async fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.run(args).await.stdout).unwrap()
    }

    async fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--format", "json"];
        all.extend_from_slice(args);
        serde_json::from_slice(&self.run(&all).await.stdout).unwrap()
    }

    /// The instance's settings status in `config show` once `matches`.
    async fn settings(&self, deployment: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let mut last = Value::Null;
        let found = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let view = self.json(&["svc", deployment, "config"]).await;
                last = view["instances"][0]["settings"].clone();
                if matches(&last) {
                    return last.clone();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("settings never matched; last {last}"))
    }

    /// The instance's row in `instances` once `matches`.
    async fn instance(&self, deployment: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let mut last = Value::Null;
        let found = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let listed = self.json(&["svc", deployment, "instances"]).await;
                last = listed["instances"][0].clone();
                if matches(&last) {
                    return last.clone();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("instance never matched; last {last}"))
    }
}

/// Write `text` to `name` in `dir` and return the `@PATH` argument for it.
fn file(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    format!("@{}", path.display())
}

/// A client built from the service's keys. Dropping it records the key it
/// held and the key in effect at that moment.
struct Client {
    key: String,
    settings: ConfigHandle<Config>,
    dropped: Arc<Mutex<Vec<(String, String)>>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        let current = self.settings.current().unwrap().api_key;
        self.dropped
            .lock()
            .unwrap()
            .push((self.key.clone(), current));
    }
}

fn https_only(settings: &Config) -> Result<(), Rejection> {
    if settings.endpoint.starts_with("https://") {
        Ok(())
    } else {
        Err(Rejection::new(["endpoint"], "must be an https URL"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_settings_show_in_the_cli_and_one_set_carries_several_secrets() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("billing")
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
    let deployment = identity.deployment_id().to_owned();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let config = Provider::new(&mut service).config();
    config.check(https_only).await.unwrap();
    let builds = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(Mutex::new(Vec::new()));
    let client = {
        let (builds, dropped, settings) =
            (Arc::clone(&builds), Arc::clone(&dropped), config.clone());
        config
            .derive(["api_key", "signing_key"], move |settings_now: &Config| {
                // Any error refuses the fields the derive is built from.
                if settings_now.api_key == "bad" {
                    Err::<(), _>(std::io::Error::other("the key is refused"))?;
                }
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(Client {
                    key: settings_now.api_key.clone(),
                    settings: settings.clone(),
                    dropped: Arc::clone(&dropped),
                })
            })
            .await
            .unwrap()
    };
    // Every accepted change, as the service hears it.
    let (heard, mut hearing) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = config.clone();
    let listening = tokio::spawn(async move {
        while let Ok(changed) = watcher.changed().await {
            if heard.send(changed).is_err() {
                break;
            }
        }
    });
    let cli = Cli::login(&mut runtime).await;
    let files = runtime.workdir().join("settings-files");
    std::fs::create_dir_all(&files).unwrap();
    let lifecycle = service.lifecycle();
    let running = tokio::spawn(service.run());
    assert!(lifecycle.ready().await, "the service serves");
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["revision"] == 1
    })
    .await;

    // A check refuses the revision: the service keeps what it had.
    cli.run(&[
        "svc",
        &deployment,
        "config",
        "set",
        "endpoint=http://plain.example",
    ])
    .await;
    let rejected = cli
        .settings(&deployment, |status| status["state"] == "rejected")
        .await;
    assert_eq!(rejected["revision"], 2);
    assert_eq!(rejected["fields"], json!(["endpoint"]));
    assert_eq!(rejected["reason"], "must be an https URL");
    assert_eq!(config.current().unwrap().endpoint, "https://one.example");
    assert_eq!(config.revision().deployment, Some(1));
    let shown = cli.text(&["svc", &deployment, "config"]).await;
    assert!(
        shown.contains("rejected r2: endpoint: must be an https URL"),
        "{shown}"
    );
    let listed = cli.text(&["svc", &deployment, "instances"]).await;
    assert!(
        listed.contains("rejected r2: endpoint: must be an https URL"),
        "{listed}"
    );
    let degraded = cli
        .instance(&deployment, |instance| {
            instance["status"]["health"]["failingChecks"]
                .as_array()
                .is_some_and(|checks| checks.iter().any(|check| check["name"] == "settings"))
        })
        .await;
    assert_eq!(
        degraded["status"]["health"]["failingChecks"][0]["summary"],
        "Settings rejected: endpoint: must be an https URL"
    );

    // Rolling the refused revision back is a revision the service accepts.
    cli.run(&["svc", &deployment, "config", "rollback", "1"])
        .await;
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["revision"] == 3
    })
    .await;
    cli.instance(&deployment, |instance| {
        instance["status"]["health"]["failingChecks"] == json!([])
    })
    .await;

    // A plain value and two secrets in one command are one revision, heard
    // as one change; the derived client is rebuilt and the old one dropped
    // only once the new keys are in effect.
    let saved = cli
        .json(&[
            "svc",
            &deployment,
            "config",
            "set",
            "level=debug",
            "--secret",
            &format!("apiKey={}", file(&files, "key.txt", "k2\n")),
            "--secret",
            &format!("signingKey={}", file(&files, "signing.txt", "s2")),
        ])
        .await;
    assert_eq!(saved["revision"], 4);
    let together = tokio::time::timeout(Duration::from_secs(30), hearing.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(together.api_key, "k2");
    assert_eq!(together.signing_key, Nullable::Value("s2".to_owned()));
    assert_eq!(
        serde_json::to_value(&together.level).unwrap(),
        json!("debug")
    );
    assert_eq!(client.current().key, "k2");
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert_eq!(
        *dropped.lock().unwrap(),
        [("k1".to_owned(), "k2".to_owned())]
    );

    // Another field changing leaves the derived client alone.
    cli.run(&["svc", &deployment, "config", "set", "level=info"])
        .await;
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["revision"] == 5
    })
    .await;
    assert_eq!(builds.load(Ordering::SeqCst), 2);

    // A builder that fails rejects its fields and keeps the old client.
    cli.run(&[
        "svc",
        &deployment,
        "config",
        "set-secret",
        &format!("apiKey={}", file(&files, "bad.txt", "bad")),
    ])
    .await;
    let refused = cli
        .settings(&deployment, |status| status["state"] == "rejected")
        .await;
    assert_eq!(refused["revision"], 6);
    assert_eq!(refused["fields"], json!(["apiKey", "signingKey"]));
    assert_eq!(refused["reason"], "the key is refused");
    assert_eq!(client.current().key, "k2");
    assert_eq!(config.current().unwrap().api_key, "k2");

    // An env file mixes both: a field the build declares secret is stored
    // as one without being marked.
    let env = files.join("fix.env");
    std::fs::write(&env, "# fixed\nexport level=debug\napiKey=\"k3\"\n").unwrap();
    let fixed = cli
        .json(&[
            "svc",
            &deployment,
            "config",
            "set",
            "--env-file",
            env.to_str().unwrap(),
        ])
        .await;
    assert_eq!(fixed["revision"], 7);
    let history = cli.json(&["svc", &deployment, "config", "history"]).await;
    assert_eq!(history["revisions"][0]["changed"], json!(["level"]));
    assert_eq!(history["revisions"][0]["changedSecrets"], json!(["apiKey"]));
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["revision"] == 7
    })
    .await;
    assert_eq!(client.current().key, "k3");
    // Only accepted revisions reached the watcher.
    let mut later = Vec::new();
    while let Ok(changed) = hearing.try_recv() {
        later.push(changed.api_key);
    }
    assert_eq!(later, ["k2", "k3"]);

    // A port applies only after a restart, so the instance applies the
    // revision while it still runs its old port, and the status says both.
    cli.run(&["svc", &deployment, "config", "set", "port=9090"])
        .await;
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["revision"] == 8
    })
    .await;
    assert_eq!(config.current().unwrap().port, 8080);
    let listed = cli.text(&["svc", &deployment, "instances"]).await;
    assert!(listed.contains("applied r8, restart pending"), "{listed}");
    let shown = cli.text(&["svc", &deployment, "config"]).await;
    assert!(shown.contains("applied r8, restart pending"), "{shown}");

    listening.abort();
    drop(client);
    drop(config);
    lifecycle.stop();
    running.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_check_that_refuses_the_starting_values_never_shows_them_applied() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("starting")
        .await
        .unwrap();
    runtime
        .set_config(
            &identity,
            json!({ "endpoint": "http://plain.example" }),
            json!({ "apiKey": "k1" }),
        )
        .await
        .unwrap();
    let deployment = identity.deployment_id().to_owned();
    let cli = Cli::login(&mut runtime).await;
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let lifecycle = service.lifecycle();
    let config = Provider::new(&mut service).config();
    // Every status `config show` gives from connect until the rejection.
    let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel::<String>();
    let watching = tokio::spawn({
        let (cli, deployment) = (cli.clone(), deployment.clone());
        async move {
            loop {
                let view = cli.json(&["svc", &deployment, "config"]).await;
                let status = &view["instances"][0]["settings"];
                let shown = format!(
                    "{} r{}",
                    status["state"].as_str().unwrap(),
                    status["revision"]
                );
                let _ = seen_tx.send(shown.clone());
                if status["state"] == "rejected" {
                    return;
                }
            }
        }
    });
    // The check registers only once the CLI has read the status.
    let first = tokio::time::timeout(Duration::from_secs(30), seen.recv())
        .await
        .unwrap()
        .unwrap();
    let registering = tokio::spawn({
        let config = config.clone();
        async move { config.check(https_only).await }
    });
    tokio::time::timeout(Duration::from_secs(30), watching)
        .await
        .expect("the rejection shows")
        .unwrap();
    let mut statuses = vec![first];
    while let Ok(shown) = seen.try_recv() {
        statuses.push(shown);
    }
    assert!(
        !statuses.iter().any(|shown| shown == "applied r1"),
        "revision 1 showed as applied before the check ran: {statuses:?}"
    );
    let rejected = cli.settings(&deployment, |_| true).await;
    assert_eq!(rejected["state"], "rejected");
    assert_eq!(rejected["revision"], 1);
    assert_eq!(rejected["fields"], json!(["endpoint"]));
    // Still starting: the check has not returned and nothing serves.
    assert!(!registering.is_finished());
    assert_eq!(lifecycle.phase(), ServicePhase::Connecting);
    let listed = cli.text(&["svc", &deployment, "instances"]).await;
    assert!(
        listed.contains("rejected r1: endpoint: must be an https URL"),
        "{listed}"
    );

    runtime
        .set_config(
            &identity,
            json!({ "endpoint": "https://one.example" }),
            json!({}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), registering)
        .await
        .expect("the check returns once a revision passes")
        .unwrap()
        .unwrap();
    assert_eq!(config.current().unwrap().endpoint, "https://one.example");
    // Accepted, but it shows as applied only once the service serves.
    let accepted = cli.settings(&deployment, |_| true).await;
    assert_eq!(accepted["state"], "pending");
    assert_eq!(accepted["revision"], 2);
    let running = tokio::spawn(service.run());
    assert!(lifecycle.ready().await, "the service serves");
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["revision"] == 2
    })
    .await;
    drop(config);
    lifecycle.stop();
    running.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_report_auth_never_answered_is_sent_again_once_it_does() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("reporting")
        .await
        .unwrap();
    runtime
        .set_config(
            &identity,
            json!({ "endpoint": "http://plain.example" }),
            json!({ "apiKey": "k1" }),
        )
        .await
        .unwrap();
    let deployment = identity.deployment_id().to_owned();
    let cli = Cli::login(&mut runtime).await;
    let mut service = Participant::connect(identity.connect_options().with_timeout_ms(3_000))
        .await
        .unwrap();
    let config = Provider::new(&mut service).config();
    // Sees the service's report on the broker, which keeps running while
    // the server is stopped.
    let nats = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::Trellis).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let mut reports = nats
        .subscribe(
            trellis_protocol::derive_rpc_subject("trellis.auth@v1", "Config.Own.Report").unwrap(),
        )
        .await
        .unwrap();
    nats.flush().await.unwrap();

    // The check refuses the starting values while Auth is stopped, so the
    // rejection's report goes unanswered.
    runtime.stop_server().await.unwrap();
    let registering = tokio::spawn({
        let config = config.clone();
        async move { config.check(https_only).await }
    });
    tokio::time::timeout(Duration::from_secs(30), reports.next())
        .await
        .expect("the rejection is reported")
        .unwrap();
    drop(reports);

    // Nothing is saved and the service's connection stays up: the report is
    // sent again until Auth takes it.
    runtime.restart().await.unwrap();
    let rejected = cli
        .settings(&deployment, |status| status["state"] == "rejected")
        .await;
    assert_eq!(rejected["revision"], 1);
    assert_eq!(rejected["fields"], json!(["endpoint"]));
    assert!(!registering.is_finished());

    registering.abort();
    drop(config);
    drop(service);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instance_that_rejects_its_own_override_is_told_to_roll_back_the_override() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("overridden")
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
    let deployment = identity.deployment_id().to_owned();
    let cli = Cli::login(&mut runtime).await;
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let config = Provider::new(&mut service).config();
    config.check(https_only).await.unwrap();
    let lifecycle = service.lifecycle();
    let running = tokio::spawn(service.run());
    assert!(lifecycle.ready().await, "the service serves");
    let view = cli.json(&["svc", &deployment, "config"]).await;
    let instance = view["instances"][0]["instanceId"]
        .as_str()
        .unwrap()
        .to_owned();
    let on_instance = |args: &[&'static str]| {
        let mut all = vec!["svc", deployment.as_str(), "config", "--instance"];
        all.push(instance.as_str());
        all.extend_from_slice(args);
        all.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    let run = |args: Vec<String>| {
        let cli = cli.clone();
        async move {
            let args = args.iter().map(String::as_str).collect::<Vec<_>>();
            cli.run(&args).await
        }
    };

    // The instance's own overrides: one it accepts, then one it refuses.
    run(on_instance(&["set", "level=debug"])).await;
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["instanceRevision"] == 1
    })
    .await;
    run(on_instance(&["set", "endpoint=http://plain.example"])).await;
    cli.settings(&deployment, |status| {
        status["state"] == "rejected" && status["instanceRevision"] == 2
    })
    .await;

    // What an operator is told to run is the contract here. The deployment's
    // view can't tell which layer holds the refused value, so it names both.
    let instance_rollback =
        format!("`trellis svc {deployment} config --instance {instance} rollback REVISION`");
    let deployment_rollback = format!("`trellis svc {deployment} config rollback REVISION`");
    let told = String::from_utf8(cli.run(&["svc", &deployment, "config"]).await.stderr).unwrap();
    assert!(told.contains(&instance_rollback), "{told}");
    assert!(told.contains(&deployment_rollback), "{told}");
    // The instance's own view knows the refused endpoint is its override.
    let told = String::from_utf8(run(on_instance(&[])).await.stderr).unwrap();
    assert!(told.contains(&instance_rollback), "{told}");
    assert!(!told.contains(&deployment_rollback), "{told}");

    // Following it brings the instance back, and the deployment's settings
    // stay as they were.
    run(on_instance(&["rollback", "1"])).await;
    let applied = cli
        .settings(&deployment, |status| status["state"] == "applied")
        .await;
    assert_eq!(applied["instanceRevision"], 3);
    assert_eq!(applied["revision"], 1);
    assert_eq!(config.current().unwrap().endpoint, "https://one.example");
    let history = cli.json(&["svc", &deployment, "config", "history"]).await;
    assert_eq!(history["revisions"].as_array().unwrap().len(), 1);

    drop(config);
    lifecycle.stop();
    running.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}
