//! Commands that ask for missing input, driven through a pseudo-terminal
//! against a live runtime, the same commands run by scripts without one, and
//! the app commands that keep a disabled app disabled.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, BufReader};
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

    /// Run without a terminal, as a script does, with a deadline so a
    /// question that waits for an answer fails the test instead of hanging.
    async fn script(&self, args: &[&str]) -> std::process::Output {
        tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(CLI)
                .args(["--format", "json"])
                .args(args)
                .env("XDG_CONFIG_HOME", &self.config)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("a command without a terminal never waits for input")
        .unwrap()
    }

    async fn json(&self, args: &[&str]) -> Value {
        let output = self.script(args).await;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Start `args` on a pseudo-terminal, as a person at a terminal would.
    fn terminal(&self, args: &[&str]) -> rexpect::session::PtySession {
        let mut command = std::process::Command::new(CLI);
        command
            .args(args)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("NO_COLOR", "1");
        rexpect::session::spawn_command(command, Some(30_000)).unwrap()
    }
}

/// Press one key, such as `y` for a yes-or-no question.
fn press(session: &mut rexpect::session::PtySession, key: &str) {
    session.send(key).unwrap();
    session.flush().unwrap();
}

/// Wait for the terminal session to end and require a successful exit.
fn succeeded(mut session: rexpect::session::PtySession) {
    let transcript = session.exp_eof().unwrap();
    let status = format!("{:?}", session.process().wait().unwrap());
    assert!(
        status.starts_with("Exited(") && status.ends_with(", 0)"),
        "{status}: {transcript}"
    );
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../integration/fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
        .display()
        .to_string()
}

/// The parts of a registration an administrator chose.
fn chosen(client: &Value) -> Value {
    let mut client = client.clone();
    for generated in ["clientId", "revision"] {
        client.as_object_mut().unwrap().remove(generated);
    }
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_ask_for_missing_input_on_a_terminal_and_fail_fast_without_one() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime).await;

    // An administrator registers an app by answering questions; an unsafe
    // redirect URI is refused as it is typed, and a safe one accepted.
    let asked = format!("asked-{}", ulid::Ulid::generate()).to_lowercase();
    let mut session = cli.terminal(&["clients", "add"]);
    session.exp_string("App ID").unwrap();
    session.send_line(&asked).unwrap();
    session.exp_string("Kind of app").unwrap();
    session.send_line("").unwrap();
    session.exp_string("Title users see").unwrap();
    session.send_line("Prompted app").unwrap();
    session.exp_string("Description").unwrap();
    session.send_line("Asked for").unwrap();
    session.exp_string("Is this a development app?").unwrap();
    press(&mut session, "y");
    session.exp_string("Redirect URIs").unwrap();
    session.send_line("http://example.com/callback").unwrap();
    session.exp_string("must use https").unwrap();
    session
        .send_line("http://localhost:49761/callback")
        .unwrap();
    session.exp_string("Allowed without asking").unwrap();
    session.send_line("").unwrap();
    session
        .exp_string("username-and-password account?")
        .unwrap();
    press(&mut session, "n");
    session.exp_string("create the account?").unwrap();
    press(&mut session, "y");
    session.exp_string("Source project").unwrap();
    session.send_line("").unwrap();
    session.exp_string("Go ahead?").unwrap();
    press(&mut session, "y");
    succeeded(session);
    let registered = cli.json(&["clients", "get", &asked]).await["client"].clone();
    assert_eq!(registered["title"], "Prompted app");
    assert_eq!(registered["description"], "Asked for");
    assert_eq!(registered["kind"], "browser");
    assert_eq!(registered["development"], true);
    assert_eq!(
        registered["redirectUris"],
        serde_json::json!(["http://localhost:49761/callback"])
    );
    assert_eq!(
        registered["signUp"],
        serde_json::json!({ "password": false, "providers": true })
    );

    // A script gives the same answers as flags and gets the same registration.
    let flagged = format!("flagged-{}", ulid::Ulid::generate()).to_lowercase();
    let saved = cli
        .json(&[
            "clients",
            "add",
            &flagged,
            "--kind",
            "browser",
            "--title",
            "Prompted app",
            "--description",
            "Asked for",
            "--development",
            "--redirect-uri",
            "http://localhost:49761/callback",
            "--sign-up-providers",
        ])
        .await;
    assert_eq!(saved["client"]["clientId"], flagged.as_str());
    let flagged_client = cli.json(&["clients", "get", &flagged]).await["client"].clone();
    assert_eq!(chosen(&flagged_client), chosen(&registered));

    // Without a terminal, missing required input fails at once, naming every
    // missing flag, and registers nothing. `--no-input` does the same on one.
    let missing = format!("missing-{}", ulid::Ulid::generate()).to_lowercase();
    let refused = cli
        .script(&["clients", "add", &missing, "--kind", "browser"])
        .await;
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("--title") && stderr.contains("--redirect-uri"),
        "{stderr}"
    );
    let mut session = cli.terminal(&["--no-input", "clients", "add", &missing]);
    session.exp_string("missing required input").unwrap();
    session.exp_eof().unwrap();
    assert!(!cli
        .script(&["clients", "get", &missing])
        .await
        .status
        .success());

    // An edit without flags asks each question starting from the current
    // answer, so only the answer that changes needs typing.
    let mut session = cli.terminal(&["clients", "edit", &asked]);
    for question in [
        "Title users see",
        "Description",
        "Is this a development app?",
        "Redirect URIs",
        "Allowed without asking",
    ] {
        session.exp_string(question).unwrap();
        session.send_line("").unwrap();
    }
    session
        .exp_string("username-and-password account?")
        .unwrap();
    press(&mut session, "y");
    for question in ["create the account?", "Source project"] {
        session.exp_string(question).unwrap();
        session.send_line("").unwrap();
    }
    session.exp_string("Go ahead?").unwrap();
    press(&mut session, "y");
    succeeded(session);
    let edited = cli.json(&["clients", "get", &asked]).await["client"].clone();
    assert_eq!(
        edited["signUp"],
        serde_json::json!({ "password": true, "providers": true })
    );
    let mut unchanged = chosen(&edited);
    unchanged["signUp"] = registered["signUp"].clone();
    assert_eq!(unchanged, chosen(&registered));

    // An edit given a flag changes that setting and keeps the rest.
    cli.json(&["clients", "edit", &asked, "--title", "Renamed app"])
        .await;
    let renamed = cli.json(&["clients", "get", &asked]).await["client"].clone();
    assert_eq!(renamed["title"], "Renamed app");
    assert_eq!(renamed["redirectUris"], registered["redirectUris"]);
    assert_eq!(renamed["signUp"], edited["signUp"]);

    // A role and a user are created by answering questions too.
    let mut session = cli.terminal(&["roles", "create"]);
    session.exp_string("Role title").unwrap();
    session.send_line("Prompted readers").unwrap();
    session.exp_string("Description").unwrap();
    session.send_line("").unwrap();
    session.exp_string("Capabilities").unwrap();
    session.send_line("trellis.auth.v1:users.read").unwrap();
    session
        .exp_string("every user created from now on?")
        .unwrap();
    press(&mut session, "n");
    session.exp_string("Go ahead?").unwrap();
    press(&mut session, "y");
    succeeded(session);
    let roles = cli.json(&["roles", "list"]).await;
    let role = roles["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|role| role["title"] == "Prompted readers")
        .expect("the prompted role exists");
    assert_eq!(
        role["capabilities"][0]["capabilityId"],
        "trellis.auth.v1:users.read"
    );
    assert_eq!(role["assignToNewUsers"], false);

    let username = format!("asked-{}", ulid::Ulid::generate()).to_lowercase();
    let mut session = cli.terminal(&["users", "create"]);
    session.exp_string("Username").unwrap();
    session.send_line(&username).unwrap();
    session.exp_string("Name shown for the user").unwrap();
    session.send_line("Asked User").unwrap();
    session.exp_string("Go ahead?").unwrap();
    press(&mut session, "y");
    succeeded(session);
    let users = cli.json(&["users", "list"]).await;
    let user = users["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|user| user["account"]["username"] == username.as_str())
        .expect("the prompted user exists");
    assert_eq!(user["displayName"], "Asked User");

    // An incompatible replacement shows the review and asks for both
    // acknowledgements before replacing the accepted API.
    let accepted = cli
        .json(&[
            "apis",
            "accept",
            "--source",
            &fixture("client-upgrade"),
            "--participant",
            "client-upgrade-fixture.Provider",
        ])
        .await["apis"][0]
        .clone();
    let api_id = accepted["apiId"].as_str().unwrap().to_owned();
    let mut session = cli.terminal(&[
        "apis",
        "force-replace",
        "--participant",
        "client-upgrade-fixture.Provider",
    ]);
    session.exp_string("Source project directory").unwrap();
    session
        .send_line(&fixture("client-upgrade-breaking"))
        .unwrap();
    session.exp_string("incompatible").unwrap();
    session.exp_string("Replace it anyway?").unwrap();
    press(&mut session, "y");
    session
        .exp_string("may now mean something different?")
        .unwrap();
    press(&mut session, "y");
    succeeded(session);
    let replaced = cli.json(&["apis", "get", &api_id]).await["api"].clone();
    assert_ne!(replaced["canonicalDigest"], accepted["canonicalDigest"]);

    runtime.shutdown().await.unwrap();
}

/// Turning an app off is what an administrator chose; saving its settings,
/// by `edit` or by `put` from a file, must not turn it back on. Only
/// `enable` does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saving_a_disabled_app_keeps_it_disabled_until_enabled() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime).await;
    let client_id = format!("off-{}", ulid::Ulid::generate()).to_lowercase();
    let revision = |client: &Value| match &client["revision"] {
        Value::String(revision) => revision.clone(),
        revision => revision.to_string(),
    };
    let added = cli
        .json(&[
            "clients",
            "add",
            &client_id,
            "--kind",
            "browser",
            "--title",
            "Switched off",
            "--description",
            "Turned off by an administrator",
            "--development",
            "--redirect-uri",
            "http://localhost:49762/callback",
        ])
        .await["client"]
        .clone();
    assert_eq!(added["enabled"], true);
    cli.json(&[
        "clients",
        "disable",
        &client_id,
        "--expected-revision",
        &revision(&added),
    ])
    .await;

    cli.json(&["clients", "edit", &client_id, "--title", "Renamed"])
        .await;
    let edited = cli.json(&["clients", "get", &client_id]).await["client"].clone();
    assert_eq!(edited["title"], "Renamed");
    assert_eq!(edited["enabled"], false);

    let request = runtime.workdir().join(format!("{client_id}.json"));
    std::fs::write(
        &request,
        serde_json::to_vec(&serde_json::json!({
            "clientId": client_id, "kind": "browser", "title": "Put again",
            "description": "Turned off by an administrator",
            "redirectUris": ["http://localhost:49762/callback"],
            "development": true, "impliedConsent": [],
            "expectedRevision": revision(&edited),
            "idempotencyKey": ulid::Ulid::generate().to_string()
        }))
        .unwrap(),
    )
    .unwrap();
    cli.json(&["clients", "put", request.to_str().unwrap()])
        .await;
    let put = cli.json(&["clients", "get", &client_id]).await["client"].clone();
    assert_eq!(put["title"], "Put again");
    assert_eq!(put["enabled"], false);

    cli.json(&[
        "clients",
        "enable",
        &client_id,
        "--expected-revision",
        &revision(&put),
    ])
    .await;
    let enabled = cli.json(&["clients", "get", &client_id]).await["client"].clone();
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["title"], "Put again");

    runtime.shutdown().await.unwrap();
}
