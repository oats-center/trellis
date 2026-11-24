//! A Rust service checks its settings before they take effect, and the CLI
//! shows what each instance did with them: a revision a check or a derive
//! builder refuses leaves the service on the values it had, shows as
//! rejected in `config show` and `instances` and in the instance's health,
//! and can be rolled back. One `config set` writes plain values and several
//! secrets as one revision, which the service hears as one change.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_trellis::participants::runtime_trellis_configured_service::{
    Config, Participant, Provider,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::generated::Nullable;
use trellis_rs::service::{ConfigHandle, Rejection, ServicePhase};
use trellis_testkit::TrellisTestRuntime;

const CLI: &str = env!("CARGO_BIN_EXE_trellis");

/// The production CLI signed in to `runtime` as its administrator.
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

    listening.abort();
    drop(client);
    drop(config);
    drop(service);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_check_that_refuses_the_starting_values_waits_for_a_fix() {
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
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let lifecycle = service.lifecycle();
    let config = Provider::new(&mut service).config();
    let registering = tokio::spawn({
        let config = config.clone();
        async move { config.check(https_only).await }
    });
    let cli = Cli::login(&mut runtime).await;
    let rejected = cli
        .settings(&deployment, |status| status["state"] == "rejected")
        .await;
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
    cli.settings(&deployment, |status| {
        status["state"] == "applied" && status["revision"] == 2
    })
    .await;
    drop(config);
    drop(service);
    runtime.shutdown().await.unwrap();
}
