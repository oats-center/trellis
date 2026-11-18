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
    fs::write(root.path().join("package.json"), "{\"type\":\"module\"}\n").unwrap();
    // Rust is built independently; its marker must not override explicit TS output.
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\n",
    )
    .unwrap();
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
    assert_eq!(lock.root, "fixture");
    assert_eq!(lock.packages.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("generated/package.json")).unwrap())
            .unwrap();
    assert_eq!(metadata["name"], "fixture");
    assert_eq!(metadata["version"], "1.2.3");
    assert_eq!(
        fs::read(root.path().join("package.json")).unwrap(),
        b"{\"type\":\"module\"}\n"
    );
    assert_eq!(
        fs::read(root.path().join("package-lock.json")).unwrap(),
        b"lock\n"
    );

    let args = GenerateArgs {
        watch: false,
        check: true,
        project,
    };
    generate::run(OutputFormat::Text, &args).unwrap();
    let generated = root.path().join("generated/index.js");
    let original = fs::read(&generated).unwrap();
    fs::create_dir(root.path().join("generated/obsolete-empty")).unwrap();
    fs::write(&generated, [original.as_slice(), b"// stale\n"].concat()).unwrap();
    assert!(generate::run(OutputFormat::Text, &args).is_err());
    assert_ne!(fs::read(&generated).unwrap(), original);

    generate::generate_project(root.path()).unwrap();
    assert_eq!(fs::read(generated).unwrap(), original);
    assert!(!root.path().join("generated/obsolete-empty").exists());
}

// An SDK whose contract depends on the built-in `trellis` package must reuse the
// published `trellis-models` crate rather than emitting a second copy of those
// models: otherwise a value from another Trellis crate is a different Rust type
// and the consumer below does not compile.
#[tokio::test]
async fn generated_rust_sdk_shares_builtin_model_types_with_trellis_models() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("trellis.toml"),
        format!(
            r#"
[package]
name = "builtin-consumer"
version = "1.0.0"

[sources]
main = "main.trellis"

[dependencies]
trellis = {{ package = "trellis", path = {:?} }}

[generate.rust]
output = "sdk"
"#,
            // Dependency paths are relative to the project.
            pathdiff::diff_paths(
                repository.join("crates/builtin-contracts"),
                root.path().canonicalize().unwrap()
            )
            .unwrap()
        ),
    )
    .unwrap();
    fs::write(
        root.path().join("main.trellis"),
        r#"
import { AuthUser } from trellis;
model Empty {}
api who@v1 {
  title "Who";
  description "Return a built-in model.";
  rpc Me { input Empty; output AuthUser; }
  capabilities { public { allows { rpc Me; } } }
}
"#,
    )
    .unwrap();
    package::install(
        OutputFormat::Text,
        &ProjectRootArgs {
            root: root.path().to_path_buf(),
        },
    )
    .await
    .unwrap();

    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("src/main.rs"),
        r#"
fn main() {
    let wire = serde_json::json!({
        "principalId": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "displayName": "Peer",
        "emailVerified": true
    });
    let shared: trellis_models::trellis::AuthUser = serde_json::from_value(wire.clone()).unwrap();
    let generated: builtin_consumer::types::AuthUser = shared;
    let back: trellis_models::trellis::AuthUser = generated;
    assert_eq!(serde_json::to_value(back).unwrap(), wire);
}
"#,
    )
    .unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"shared-model-consumer\"\nversion = \"1.0.0\"\nedition = \
            \"2021\"\n[workspace]\n[dependencies]\nbuiltin-consumer = {{ path = \"sdk\" }}\n\
            trellis-models = {{ path = {models:?} }}\nserde_json = \"1\"\n[patch.crates-io]\n\
            trellis-rs = {{ path = {:?} }}\ntrellis-protocol = {{ path = {:?} }}\n\
            trellis-models = {{ path = {models:?} }}\n",
            repository.join("crates/trellis"),
            repository.join("crates/protocol"),
            models = repository.join("crates/models"),
        ),
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--offline"])
        .current_dir(root.path())
        .env(
            "CARGO_TARGET_DIR",
            repository.join("target/codegen-consumers"),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
