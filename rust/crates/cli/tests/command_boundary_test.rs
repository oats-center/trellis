use std::process::Command;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trellis"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run trellis")
}

#[test]
fn new_top_level_help_is_available() {
    for args in [
        &["login", "--help"][..],
        &["add", "--help"],
        &["rm", "--help"],
        &["check", "--help"],
        &["update", "--help"],
        &["install", "--help"],
        &["logout", "--help"],
        &["whoami", "--help"],
        &["identity", "--help"],
        &["identity", "grants", "--help"],
        &["participants", "--help"],
        &["issuers", "--help"],
        &["users", "--help"],
        &["svc", "--help"],
        &["dev", "--help"],
        &["init", "--help"],
        &["init", "config", "--help"],
        &["keys", "--help"],
        &["upgrade", "--help"],
        &["version", "--help"],
        &["completion", "--help"],
    ] {
        let output = run_cli(args);
        assert!(output.status.success(), "{args:?} help should succeed");
    }
}

#[test]
fn removed_top_level_commands_and_aliases_are_rejected() {
    for command in [
        "auth",
        "deploy",
        "deployment",
        "deployments",
        "dep",
        "d",
        "bootstrap",
        "local",
        "self",
        "keygen",
        "approvals",
        "grants",
        "server",
        "infra",
    ] {
        let output = run_cli(&[command, "--help"]);
        assert!(!output.status.success(), "{command} should fail");
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(
            stderr.contains(&format!("unrecognized subcommand '{command}'")),
            "unexpected stderr for {command}: {stderr}"
        );
    }
}

#[test]
fn login_help_uses_positional_trellis_url_and_hides_legacy_flags() {
    let output = run_cli(&["login", "--help"]);
    assert!(output.status.success(), "login help should succeed");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("<TRELLIS_URL>"));
    assert!(!stdout.contains("--auth-url"));
    assert!(!stdout.contains("--listen"));
    assert!(!stdout.contains("--nats-servers"));
    assert!(!stdout.contains("--servers <SERVERS>"));
    assert!(!stdout.contains("--creds <CREDS>"));
}

#[test]
fn init_admin_help_uses_identity_and_storage_path() {
    let output = run_cli(&["init", "admin", "--help"]);
    assert!(output.status.success(), "init admin help should succeed");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("--identity <PROVIDER:SUBJECT>"));
    assert!(stdout.contains("--db-path <DB_PATH>"));
    assert!(!stdout.contains("--provider"));
    assert!(!stdout.contains("--subject"));
    assert!(!stdout.contains("--capabilities"));
}

#[test]
fn resource_help_surfaces_resource_first_commands() {
    let output = run_cli(&["svc", "--help"]);
    assert!(output.status.success(), "svc help should succeed");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("list"));

    let output = run_cli(&["dev", "--help"]);
    assert!(output.status.success(), "dev help should succeed");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("list"));
}

#[test]
fn version_command_remains_available() {
    let output = run_cli(&["version"]);
    assert!(output.status.success(), "version should succeed");
}

#[test]
fn legacy_completions_command_is_rejected() {
    let output = run_cli(&["completions", "bash"]);
    assert!(!output.status.success(), "legacy completions should fail");
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("unrecognized subcommand 'completions'"));
}
