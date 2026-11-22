//! Commands that ask for missing input, driven through a pseudo-terminal
//! against a live runtime, the same commands run by scripts without one, the
//! app commands that keep a disabled app disabled, and color that appears only
//! on a terminal.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_testkit::{TestUserIdentity, TrellisTestRuntime};

const CLI: &str = env!("CARGO_BIN_EXE_trellis");

/// The production CLI signed in to `runtime` as its administrator, or as
/// `user` with only what that user was granted.
struct Cli {
    config: PathBuf,
}

impl Cli {
    async fn login(runtime: &mut TrellisTestRuntime, user: Option<&TestUserIdentity>) -> Self {
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
        let code = challenge["userCode"].as_str().unwrap();
        match user {
            None => {
                runtime
                    .approve_device_login_with_administration("trellis.cli", code)
                    .await
            }
            Some(user) => {
                runtime
                    .approve_device_login_as("trellis.cli", code, user)
                    .await
            }
        }
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

/// `text` without whitespace or the report box's borders. The report wraps
/// long lines, even inside an ID at a hyphen, so a refusal is compared as
/// squashed text.
fn squashed(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '│')
        .collect()
}

/// Whether the failed run `output` reported `message`.
fn refused_with(output: &std::process::Output, message: &str) -> bool {
    !output.status.success()
        && squashed(&String::from_utf8_lossy(&output.stderr)).contains(&squashed(message))
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

/// The secrets `config` reports as stored, by name.
fn secrets(view: &Value) -> Vec<String> {
    view["unused"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["secret"] == true)
        .map(|item| item["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_secret_asks_without_echo_on_a_terminal_and_reads_piped_input_without_one() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime, None).await;
    // A service added without its IDL takes secrets by any name.
    let name = format!("secretive-{}", ulid::Ulid::generate()).to_lowercase();
    cli.json(&["svc", &name, "create"]).await;

    // Asked on a terminal, the value is typed twice and never shown; a
    // mismatched second entry is caught and asked again.
    let typed = format!("typed-{}", ulid::Ulid::generate());
    let mut session = cli.terminal(&["svc", &name, "config", "set-secret", "apiKey"]);
    session.exp_string("Value for apiKey").unwrap();
    session.send_line(&typed).unwrap();
    session.exp_string("Type it again").unwrap();
    session.send_line("something else").unwrap();
    session.exp_string("don't match").unwrap();
    session.exp_string("Value for apiKey").unwrap();
    session.send_line(&typed).unwrap();
    session.exp_string("Type it again").unwrap();
    session.send_line(&typed).unwrap();
    let transcript = session.exp_eof().unwrap();
    let status = format!("{:?}", session.process().wait().unwrap());
    assert!(
        status.starts_with("Exited(") && status.ends_with(", 0)"),
        "{status}: {transcript}"
    );
    assert!(
        !transcript.contains(&typed),
        "the secret was echoed: {transcript}"
    );
    let view = cli.json(&["svc", &name, "config"]).await;
    assert_eq!(secrets(&view), ["apiKey"]);
    assert!(!view.to_string().contains(&typed));

    // A script pipes the value in, as before, and nothing is asked.
    let mut piped = tokio::process::Command::new(CLI)
        .args([
            "--format",
            "json",
            "svc",
            &name,
            "config",
            "set-secret",
            "token",
        ])
        .env("XDG_CONFIG_HOME", &cli.config)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    {
        use tokio::io::AsyncWriteExt as _;
        let mut stdin = piped.stdin.take().unwrap();
        stdin.write_all(b"piped-value\n").await.unwrap();
    }
    let output = tokio::time::timeout(Duration::from_secs(30), piped.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(secrets(&saved), ["apiKey", "token"]);

    // With questions turned off, a terminal with no value fails at once,
    // naming what is missing, and stores nothing.
    let mut session = cli.terminal(&["--no-input", "svc", &name, "config", "set-secret", "other"]);
    session
        .exp_string("missing required input: --from-file")
        .unwrap();
    session.exp_eof().unwrap();
    let after = cli.json(&["svc", &name, "config"]).await;
    assert_eq!(secrets(&after), ["apiKey", "token"]);

    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_ask_for_missing_input_on_a_terminal_and_fail_fast_without_one() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime, None).await;

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
            "localhost/client-upgrade-fixture.Provider",
        ])
        .await["apis"][0]
        .clone();
    let api_id = accepted["apiId"].as_str().unwrap().to_owned();
    let mut session = cli.terminal(&[
        "apis",
        "force-replace",
        "--participant",
        "localhost/client-upgrade-fixture.Provider",
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

/// An app named in its source project is registered as the kind its IDL
/// declares, so a `native app` never becomes a browser client by mistake.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_from_its_source_is_registered_as_its_declared_kind() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime, None).await;
    let source = fixture("client-upgrade");
    let native = format!("tool-{}", ulid::Ulid::generate()).to_lowercase();
    // No --kind and no redirect URI: `native app Caller` signs in with a code.
    let added = cli
        .json(&[
            "clients",
            "add",
            &native,
            "--title",
            "Command-line tool",
            "--source",
            &source,
            "--app",
            "localhost/client-upgrade-fixture.Caller",
        ])
        .await["client"]
        .clone();
    assert_eq!(added["kind"], "native");
    assert_eq!(added["redirectUris"], serde_json::json!([]));

    let browser = format!("web-{}", ulid::Ulid::generate()).to_lowercase();
    let refused = cli
        .script(&[
            "clients",
            "add",
            &browser,
            "--kind",
            "browser",
            "--title",
            "Mistaken web app",
            "--redirect-uri",
            "https://tool.example.test/callback",
            "--source",
            &source,
            "--app",
            "localhost/client-upgrade-fixture.Caller",
        ])
        .await;
    assert!(!refused.status.success());
    // The report wraps long lines in a box; compare its words.
    let stderr = String::from_utf8_lossy(&refused.stderr);
    let words = stderr
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        words.contains(
            "localhost/client-upgrade-fixture.Caller is declared as a native app, but app"
        ) && words.contains("is a browser app")
            && words.contains("Use --kind native"),
        "{stderr}"
    );
    assert!(!cli
        .script(&["clients", "get", &browser])
        .await
        .status
        .success());

    runtime.shutdown().await.unwrap();
}

/// Turning an app off is what an administrator chose; saving its settings,
/// by `edit` or by `put` from a file, must not turn it back on. Only
/// `enable` does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saving_a_disabled_app_keeps_it_disabled_until_enabled() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime, None).await;
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

/// Someone delegated only accounts.manage links a person's OIDC sign-in by
/// choosing the provider from the enabled ones, without the authority to
/// manage the providers themselves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_manager_chooses_the_provider_when_linking_a_sign_in() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let admin = Cli::login(&mut runtime, None).await;
    let provider = admin
        .json(&[
            "sign-in",
            "providers",
            "add",
            "--name",
            "Company",
            "--issuer",
            "https://identity.example/",
            "--client-id",
            "trellis",
        ])
        .await["provider"]["providerId"]
        .as_str()
        .unwrap()
        .to_owned();
    let person = runtime.create_user("Linked Person").await.unwrap();
    let manager = runtime.create_user("Account manager").await.unwrap();
    runtime
        .grant_user_capabilities(&manager, &["trellis.auth.v1:accounts.manage"])
        .await
        .unwrap();
    let cli = Cli::login(&mut runtime, Some(&manager)).await;

    let mut session = cli.terminal(&["users", "link", person.principal_id()]);
    session.exp_string("Identity provider").unwrap();
    session.exp_string("Company").unwrap();
    session.send_line("").unwrap();
    session.exp_string("subject (sub)").unwrap();
    session.send_line("auth0|linked").unwrap();
    session.exp_string("Go ahead?").unwrap();
    press(&mut session, "y");
    succeeded(session);
    let found = cli
        .json(&[
            "users",
            "find",
            "--provider",
            &provider,
            "--subject",
            "auth0|linked",
        ])
        .await;
    assert_eq!(found["user"]["principalId"], person.principal_id());
}

/// `svc <ID> instances` colors a failing check's state on a terminal, and
/// writes plain text when piped, with `NO_COLOR`, or with `TERM=dumb`, so
/// scripts and log files never see escape codes unless `CLICOLOR_FORCE` asks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn instance_listings_are_colored_only_where_color_is_wanted() {
    use runtime_trellis::participants::runtime_trellis_provider::Participant;
    use trellis_rs::service::ServiceHealthCheck;

    const RED_FAILED: &str = "\x1b[31mfailed\x1b[39m: connection refused";

    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("colored checks")
        .await
        .unwrap();
    let service = Participant::connect(
        identity
            .connect_options()
            .with_health_heartbeat_interval_ms(1_000),
    )
    .await
    .unwrap();
    let _check = service.health().add("database", || async {
        ServiceHealthCheck::failed("connection refused")
    });
    let cli = Cli::login(&mut runtime, None).await;
    let deployment = identity.deployment_id();
    let args = ["--format", "text", "svc", deployment, "instances"];
    let line = format!(
        "{}: check database failed: connection refused",
        identity.instance_id()
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let listed = cli.json(&["svc", deployment, "instances"]).await;
            if listed["instances"][0]["status"]["health"]["status"] == "unhealthy" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the instance reports its failing check");

    let command = |env: &[(&str, &str)]| {
        let mut command = std::process::Command::new(CLI);
        command
            .args(args)
            .env("XDG_CONFIG_HOME", &cli.config)
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .envs(env.iter().copied());
        command
    };
    let piped = |env: &[(&str, &str)]| {
        let output = command(env)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    for env in [&[][..], &[("NO_COLOR", "1")]] {
        let stdout = piped(env);
        assert!(stdout.lines().any(|text| text == line), "{env:?}: {stdout}");
        assert!(!stdout.contains('\x1b'), "{env:?}: {stdout:?}");
    }
    let forced = piped(&[("CLICOLOR_FORCE", "1")]);
    assert!(forced.contains(RED_FAILED), "{forced:?}");

    let on_terminal = |env: &[(&str, &str)]| {
        let mut session = rexpect::session::spawn_command(command(env), Some(30_000)).unwrap();
        session.exp_eof().unwrap()
    };
    let colored = on_terminal(&[]);
    assert!(colored.contains(RED_FAILED), "{colored:?}");
    for env in [[("NO_COLOR", "1")], [("TERM", "dumb")]] {
        let plain = on_terminal(&env);
        assert!(plain.contains("failed: connection refused"), "{plain:?}");
        assert!(!plain.contains('\x1b'), "{env:?}: {plain:?}");
    }

    drop(service);
    runtime.shutdown().await.unwrap();
}

/// A Rust service holding only an enrollment secret enrolls itself and serves;
/// revoking the secret with `--retire-instances` retires it, says how many of
/// its connections are still being closed, and disconnects it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_a_secret_with_its_instances_disconnects_them() {
    use runtime_trellis::participants::runtime_trellis_provider::Participant;

    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("enrolled pods")
        .await
        .unwrap();
    let cli = Cli::login(&mut runtime, None).await;
    let deployment = identity.deployment_id();
    let issued = cli
        .json(&["svc", deployment, "enrollment", "issue", "--title", "pods"])
        .await;
    let text = issued["text"].as_str().unwrap().to_owned();
    let secret = issued["secret"]["secretId"].as_str().unwrap().to_owned();
    let service = Participant::connect(identity.enrolling_options(&text))
        .await
        .unwrap();
    let running = tokio::spawn(service.run());
    let listed = cli.json(&["svc", deployment, "instances"]).await;
    let enrolled = listed["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|instance| instance["origin"]["kind"] == "enrolled")
        .expect("the process enrolled itself")
        .clone();
    assert_eq!(enrolled["origin"]["secretId"], secret.as_str());
    assert!(enrolled["connectedAt"].is_string());

    let revoked = std::process::Command::new(CLI)
        .args([
            "--format",
            "text",
            "svc",
            deployment,
            "enrollment",
            "revoke",
        ])
        .args([secret.as_str(), "--retire-instances"])
        .env("XDG_CONFIG_HOME", &cli.config)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&revoked.stderr).into_owned();
    assert!(revoked.status.success(), "{said}");
    assert!(
        said.contains("Retired 1 instance; 1 connection still being closed."),
        "{said}"
    );
    // The enforcement pipeline closes its connection, and its retired key
    // cannot be admitted again, so the service stops.
    let ended = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .expect("the retired instance is disconnected");
    assert!(ended.unwrap().is_err());
    let all = cli.json(&["svc", deployment, "instances", "--all"]).await;
    let retired = all["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|instance| instance["instanceId"] == enrolled["instanceId"])
        .unwrap()
        .clone();
    assert_eq!(retired["state"], "retired");
    assert_eq!(retired["retiredReason"], "secretRevoked");
    // A retired instance is hidden from the ordinary listing.
    let live = cli.json(&["svc", deployment, "instances"]).await;
    assert!(live["instances"]
        .as_array()
        .unwrap()
        .iter()
        .all(|instance| instance["instanceId"] != enrolled["instanceId"]));
    runtime.shutdown().await.unwrap();
}

/// `svc <name> webhooks` lists a service's endpoints with their URLs, shows
/// what each request to an endpoint became, and disables an endpoint so its
/// URL answers 503.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_endpoints_and_their_requests_are_shown_and_managed() {
    use runtime_trellis::participants::runtime_trellis_webhook_service::Participant;

    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("billing")
        .await
        .unwrap();
    runtime
        .set_config(
            &identity,
            serde_json::json!({ "partnerUser": "partner" }),
            serde_json::json!({
                "signing": "signing-secret",
                "hookToken": "right-token-value",
                "partnerPassword": "partner-password",
            }),
        )
        .await
        .unwrap();
    let cli = Cli::login(&mut runtime, None).await;
    let deployment = identity.deployment_id();

    let listed = cli.json(&["svc", deployment, "webhooks"]).await;
    let endpoints = listed["endpoints"].as_array().unwrap();
    let mut names: Vec<_> = endpoints
        .iter()
        .map(|endpoint| endpoint["endpoint"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "handshake",
            "layered",
            "partner",
            "query",
            "receipt",
            "relayed",
            "relayedToken",
            "relayedTokens",
            "signed",
            "token"
        ]
    );
    let token = cli.json(&["svc", deployment, "webhooks", "token"]).await;
    let url = token["url"].as_str().unwrap().to_owned();
    assert!(url.contains("/webhooks/billing/"), "{url}");
    assert_eq!(
        token["verifiers"],
        serde_json::json!(["Token in the x-hook-token header"])
    );

    let http = runtime.https_client_builder().build().unwrap();
    let refused = http
        .post(&url)
        .header("x-hook-token", "wrong-token-value")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
    let attempts = cli
        .json(&[
            "svc",
            deployment,
            "webhooks",
            "token",
            "attempts",
            "--outcome",
            "rejected",
        ])
        .await;
    let attempt = &attempts["attempts"][0];
    assert_eq!(attempt["reason"], "token does not match");
    assert_eq!(attempt["stored"], false);
    let shown = cli
        .json(&[
            "svc",
            deployment,
            "webhooks",
            "token",
            "attempt",
            attempt["attemptId"].as_str().unwrap(),
        ])
        .await;
    assert!(!shown.to_string().contains("wrong-token-value"));

    cli.json(&["svc", deployment, "webhooks", "token", "disable"])
        .await;
    let disabled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = http
                .post(&url)
                .header("x-hook-token", "right-token-value")
                .body("{}")
                .send()
                .await
                .unwrap();
            if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a disabled endpoint answers 503");
    assert_eq!(disabled.text().await.unwrap(), "");
    let shown = cli.json(&["svc", deployment, "webhooks", "token"]).await;
    assert_eq!(shown["enabled"], false);
}

/// Generated key IDs and public keys are base64url, so one in 64 starts with
/// `-`. Given as an argument, such a value is the value rather than an
/// option, and `--help` still prints help.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_values_starting_with_a_hyphen_are_arguments() {
    use runtime_trellis::participants::runtime_trellis_provider::Participant;

    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("hyphen keys")
        .await
        .unwrap();
    let cli = Cli::login(&mut runtime, None).await;

    // An issuer key ID is the runtime's to choose, so this fixed one names no
    // key, and Auth itself says so: the ID reached Auth instead of being read
    // as options.
    let refused = cli
        .script(&[
            "issuers",
            "revoke",
            "-q9TkWbQ1d0pZ_3vXo8yRk2eLmA7cHnB4sUfJ6gYtVw",
        ])
        .await;
    assert!(
        refused_with(
            &refused,
            "no issuer key -q9TkWbQ1d0pZ_3vXo8yRk2eLmA7cHnB4sUfJ6gYtVw"
        ),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let help = cli.script(&["issuers", "revoke", "--help"]).await;
    let usage = String::from_utf8_lossy(&help.stdout);
    assert!(help.status.success(), "{usage}");
    assert!(usage.contains("Usage: trellis issuers revoke"), "{usage}");

    let public_key = "-EYsmSdw8ZJ7tTednbtP5iMZVtGjNslfmLz_gseNdk0";
    let deployment = identity.deployment_id();
    let provisioned = cli
        .json(&["svc", deployment, "provision", "--public-key", public_key])
        .await;
    assert_eq!(provisioned["reused"], false);
    assert_eq!(provisioned["instance"]["identityPublicKey"], public_key);
    let listed = cli.json(&["svc", deployment, "instances"]).await;
    assert!(listed["instances"]
        .as_array()
        .unwrap()
        .iter()
        .any(|instance| instance["identityPublicKey"] == public_key));
    runtime.shutdown().await.unwrap();
}

/// The current signing key can't be revoked, and the refusal says how to get
/// there: rotate, then revoke the retired key. A key revoked once says so on
/// a second try.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issuer_keys_rotate_and_revoke_from_the_cli() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime, None).await;
    let metadata: Value = runtime
        .https_client_builder()
        .build()
        .unwrap()
        .get(format!(
            "{}/.well-known/oauth-authorization-server",
            runtime.trellis_url().trim_end_matches('/')
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first = metadata["trellis"]["issuers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["state"] == "active")
        .and_then(|key| key["keyId"].as_str())
        .expect("one key signs")
        .to_owned();

    let refused = cli.script(&["issuers", "revoke", &first]).await;
    assert!(
        refused_with(
            &refused,
            &format!(
                "issuer key {first} is the current signing key and cannot be revoked; rotate \
                to a new key first with `trellis issuers rotate {first}`"
            )
        ),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let rotated = cli.json(&["issuers", "rotate", &first]).await;
    let second = rotated["keyId"].as_str().unwrap().to_owned();
    assert_ne!(second, first);
    // Rotating from a key that is no longer current names the one that is.
    let stale = cli.script(&["issuers", "rotate", &first]).await;
    assert!(
        refused_with(&stale, &format!("the current key is {second}")),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );

    let revoked = cli.json(&["issuers", "revoke", &first]).await;
    assert_eq!(revoked["keyId"], first.as_str());
    let again = cli.script(&["issuers", "revoke", &first]).await;
    assert!(
        refused_with(&again, &format!("issuer key {first} is already revoked")),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    // The new key still signs: commands keep working.
    assert_eq!(cli.json(&["whoami"]).await["loggedIn"], true);
    runtime.shutdown().await.unwrap();
}
