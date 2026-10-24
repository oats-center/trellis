use std::{fs, process::Command};

#[test]
fn installed_project_checks_and_reports_invalid_authored_types() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("trellis.toml"),
        "[package]\nname = \"command-fixture\"\nversion = \
        \"1.0.0\"\n[sources]\nmain = \
        \"main.trellis\"\n[generate.typescript]\noutput = \"generated\"\n",
    )
    .unwrap();
    let source = "model Value { value: string; }\napi ping@v1 { title \"Ping\"; \
        description \"Command fixture\"; rpc Get { input Value; output Value; \
        } capabilities { public { allows { rpc Get; } } } \
        }\n";
    fs::write(root.path().join("main.trellis"), source).unwrap();

    for command in ["install", "check"] {
        let output = Command::new(env!("CARGO_BIN_EXE_trellis"))
            .args([command, "--root"])
            .arg(root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fs::write(
        root.path().join("main.trellis"),
        source.replace("input Value", "input Missing"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_trellis"))
        .args(["check", "--root"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "invalid authored types must fail check"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Missing"));
}
