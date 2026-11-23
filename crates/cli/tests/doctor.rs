//! `trellis doctor` against a live runtime: it passes on a healthy one, and
//! on a broken one names each thing wrong and fails.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_testkit::{TestNatsUser, TrellisTestRuntime};

const CLI: &str = env!("CARGO_BIN_EXE_trellis");
const GIB: u64 = 1 << 30;
const MIB: i64 = 1 << 20;

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
        tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(CLI)
                .args(args)
                .env("XDG_CONFIG_HOME", &self.config)
                .env("NO_COLOR", "1")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("the command finishes")
        .unwrap()
    }

    /// `doctor --format json`: whether it passed, and its report.
    async fn doctor(&self) -> (bool, Value) {
        let output = self.run(&["--format", "json", "doctor"]).await;
        let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "doctor printed no report ({error}): {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.success(), report)
    }
}

/// Every finding's message in `report`, with its status.
fn findings(report: &Value) -> Vec<(String, String)> {
    report
        .as_object()
        .into_iter()
        .flat_map(|sections| sections.values())
        .flat_map(|section| section["findings"].as_array().cloned().unwrap_or_default())
        .map(|finding| {
            (
                finding["status"].as_str().unwrap_or_default().to_owned(),
                finding["message"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

// A healthy runtime passes. Then a service waits for a required secret no
// one set, a stream takes nearly all of JetStream's storage, and Auth's
// enforcement page for a sign-out fails for good (a trigger another process
// added refuses it); doctor names all three, Auth's own enforcement check
// among them, in its JSON and its text, and
// fails. This catches a doctor that misses any of them, or passes anyway.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_passes_a_healthy_trellis_and_names_what_needs_attention() {
    use runtime_trellis::participants::runtime_trellis_configured_service::Participant;

    let mut runtime = TrellisTestRuntime::builder()
        .jetstream_max_file_store(GIB)
        .start()
        .await
        .unwrap();
    let cli = Cli::login(&mut runtime).await;

    let (passed, report) = cli.doctor().await;
    assert!(passed, "{report:#}");
    assert_eq!(report["status"], "ok", "{report:#}");
    assert_eq!(
        report["server"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{report:#}"
    );
    assert_eq!(report["certificates"]["transport"], "tls", "{report:#}");
    assert_eq!(
        report["certificates"]["certificates"][0]["role"], "served",
        "{report:#}"
    );
    assert!(report["disk"]["free"].is_string(), "{report:#}");
    let text = cli.run(&["doctor"]).await;
    assert!(text.status.success());
    assert!(
        String::from_utf8_lossy(&text.stdout).contains("✓ Server"),
        "{}",
        String::from_utf8_lossy(&text.stdout)
    );

    // A service whose required secret is not set waits for it.
    let configured = runtime
        .register_service::<Participant>("configured")
        .await
        .unwrap();
    runtime
        .set_config(
            &configured,
            serde_json::json!({ "endpoint": "https://one.example" }),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    // The connect waits until the test ends, so its identity lives as long.
    let configured: &'static _ = Box::leak(Box::new(configured));
    let waiting = tokio::spawn(Participant::connect(configured.connect_options()));

    // A stream takes all but a few MiB of JetStream's storage.
    let nats = async_nats::ConnectOptions::with_credentials(
        &runtime.nats_creds(TestNatsUser::Trellis).await.unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let jetstream = async_nats::jetstream::new(nats);
    let reserved = jetstream.query_account().await.unwrap().reserved_storage as i64;
    let mut filler = GIB as i64 - reserved - 4 * MIB;
    while jetstream
        .create_stream(async_nats::jetstream::stream::Config {
            name: "FILLER".into(),
            subjects: vec!["filler".into()],
            max_bytes: filler,
            ..Default::default()
        })
        .await
        .is_err()
    {
        filler -= 4 * MIB;
        assert!(filler > 0, "no filler fits");
    }

    // A second login signs out while every enforcement page is refused.
    let leaving = Cli::login(&mut runtime).await;
    let database = rusqlite::Connection::open(platform_database(&runtime)).unwrap();
    database.busy_timeout(Duration::from_secs(10)).unwrap();
    database
        .execute_batch(
            "CREATE TRIGGER refuse_page BEFORE UPDATE OF state ON auth_enforcement_work \
            BEGIN SELECT RAISE(ABORT, 'page refused'); END",
        )
        .unwrap();
    let logout = leaving.run(&["logout"]).await;
    assert!(
        logout.status.success(),
        "{}",
        String::from_utf8_lossy(&logout.stderr)
    );

    let wanted = [
        ("problem", "configured: instance"),
        ("problem", "waits for settings: apiKey"),
        ("warning", "% committed: used"),
        ("warning", "platform: instance"),
        ("warning", "check enforcement degraded"),
        ("problem", "enforcement work"),
        ("problem", "page refused"),
    ];
    let mut last = Value::Null;
    let found = tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            let (passed, report) = cli.doctor().await;
            let messages = findings(&report);
            let named = |(status, text): &(&str, &str)| {
                messages
                    .iter()
                    .any(|(found, message)| found == status && message.contains(text))
            };
            if wanted.iter().all(named) {
                return (passed, report);
            }
            last = report;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    let (passed, report) =
        found.unwrap_or_else(|_| panic!("doctor never named all three: {last:#}"));
    assert!(!passed, "{report:#}");
    assert_eq!(report["status"], "problem", "{report:#}");
    assert_eq!(report["health"]["status"], "problem", "{report:#}");
    assert_eq!(report["jetstream"]["status"], "warning", "{report:#}");
    assert_eq!(report["work"]["status"], "problem", "{report:#}");

    let text = cli.run(&["doctor"]).await;
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(!text.status.success(), "{stdout}");
    for (_, phrase) in wanted {
        assert!(stdout.contains(phrase), "{phrase}: {stdout}");
    }
    assert!(
        String::from_utf8_lossy(&text.stderr).contains("Trellis needs attention"),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );

    database.execute_batch("DROP TRIGGER refuse_page").unwrap();
    waiting.abort();
    runtime.shutdown().await.unwrap();
}

fn platform_database(runtime: &TrellisTestRuntime) -> PathBuf {
    let mut pending = vec![runtime.workdir().to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name == "platform.sqlite")
            {
                return path;
            }
        }
    }
    panic!("the runtime has no platform database");
}
