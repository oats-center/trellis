use std::process::Command;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trellis"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run trellis")
}

#[test]
fn top_level_help_is_available() {
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
fn version_command_remains_available() {
    let output = run_cli(&["version"]);
    assert!(output.status.success(), "version should succeed");
}
