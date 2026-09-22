use std::fs;

use trellis_cli::{
    cli::{GenerateArgs, OutputFormat, ProjectRootArgs},
    generate, package,
};

#[tokio::test]
async fn install_creates_lock_and_generation_is_staged_and_repeatable() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("trellis.toml"),
        r#"
[package]
name = "fixture"
version = "1.2.3"

[sources]
main = "main.trellis"

[generate.typescript]
output = "generated"
"#,
    )
    .unwrap();
    fs::write(root.path().join("deno.json"), "{}\n").unwrap();
    fs::write(root.path().join("deno.lock"), "lock\n").unwrap();
    fs::write(root.path().join("package-lock.json"), "lock\n").unwrap();
    fs::write(
        root.path().join("main.trellis"),
        r#"
model Empty {}
api ping@v1 {
  title "Ping";
  description "A generation fixture.";
  rpc Get { input Empty; output Empty; }
  capabilities { public { allows { rpc Get; } } }
}
"#,
    )
    .unwrap();
    let project = ProjectRootArgs {
        root: root.path().to_path_buf(),
    };

    package::install(OutputFormat::Text, &project)
        .await
        .unwrap();

    let lock = trellis_idl::project::read_lock(&root.path().join("trellis.lock")).unwrap();
    assert_eq!(lock.format, 2);
    assert_eq!(lock.root, "fixture");
    assert_eq!(lock.packages.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("generated/package.json")).unwrap())
            .unwrap();
    assert_eq!(metadata["name"], "fixture");
    assert_eq!(metadata["version"], "1.2.3");
    assert_eq!(fs::read(root.path().join("deno.lock")).unwrap(), b"lock\n");
    assert_eq!(
        fs::read(root.path().join("package-lock.json")).unwrap(),
        b"lock\n"
    );

    let args = GenerateArgs {
        watch: false,
        check: true,
        project,
    };
    generate::run(&args).unwrap();
    let generated = root.path().join("generated/index.js");
    let original = fs::read(&generated).unwrap();
    fs::create_dir(root.path().join("generated/obsolete-empty")).unwrap();
    fs::write(&generated, [original.as_slice(), b"// stale\n"].concat()).unwrap();
    assert!(generate::run(&args).is_err());
    assert_ne!(fs::read(&generated).unwrap(), original);

    generate::generate_project(root.path()).unwrap();
    assert_eq!(fs::read(generated).unwrap(), original);
    assert!(!root.path().join("generated/obsolete-empty").exists());
}
