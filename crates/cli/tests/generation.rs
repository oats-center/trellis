use std::{collections::BTreeSet, fs};

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
// `trellis-models` crate the project's Cargo graph builds rather than emitting a
// second copy of those models: otherwise a value from another Trellis crate is a
// different Rust type and the consumer below does not compile. The project here
// resolves a later trellis-models release than the CLI bundles, carrying the
// same snapshot, as a project ahead of its CLI does.
#[test]
fn generated_rust_sdk_reuses_the_trellis_models_cargo_resolves() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let target = repository.join("target/codegen-consumers");
    let cli_version = env!("CARGO_PKG_VERSION");
    let mut later = semver::Version::parse(cli_version).unwrap();
    if later.pre.is_empty() {
        later.patch += 1;
    } else {
        later.pre = semver::Prerelease::new(&format!("{}.1", later.pre)).unwrap();
    }
    let cargo = |root: &std::path::Path, args: &[&str]| {
        let result = std::process::Command::new(env!("CARGO"))
            .args(args)
            .current_dir(root)
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "cargo {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    };
    let trellis = |root: &std::path::Path, command: &str| {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_trellis"))
            .args(["-v", command, "--root"])
            .arg(root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
        assert!(result.status.success(), "trellis {command}: {stderr}");
        stderr
    };
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

    // The published trellis-rs archive names trellis-models by registry
    // version, so Cargo resolves it to whatever the project patches in.
    cargo(
        &repository,
        &[
            "package",
            "-p",
            "trellis-rs",
            "--no-verify",
            "--allow-dirty",
            "--config",
            ".cargo/publication.toml",
        ],
    );
    // `cargo package` stamps a fixed 2006 mtime; extract with current mtimes so
    // the shared target directory does not reuse a stale build.
    assert!(std::process::Command::new("tar")
        .arg("-xzmf")
        .arg(target.join(format!("package/trellis-rs-{cli_version}.crate")))
        .arg("-C")
        .arg(root.path())
        .status()
        .unwrap()
        .success());
    let models = root.path().join("trellis-models-later");
    fs::create_dir_all(models.join("src")).unwrap();
    fs::create_dir_all(models.join("generated")).unwrap();
    fs::copy(
        repository.join("crates/models/src/lib.rs"),
        models.join("src/lib.rs"),
    )
    .unwrap();
    fs::write(
        models.join("Cargo.toml"),
        format!(
            "[package]\nname = \"trellis-models\"\nversion = \"{later}\"\nedition = \
            \"2021\"\n[dependencies]\ntrellis-protocol = \"{cli_version}\"\nserde = {{ \
            version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n"
        ),
    )
    .unwrap();
    let snapshot = fs::read_to_string(repository.join("crates/models/generated/__types.rs"))
        .unwrap()
        .replacen(
            &format!("version: \"{cli_version}\""),
            &format!("version: \"{later}\""),
            1,
        );
    fs::write(models.join("generated/__types.rs"), &snapshot).unwrap();

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
            trellis-models = \"{cli_version}\"\nserde_json = \"1\"\n[patch.crates-io]\n\
            trellis-rs = {{ path = {:?} }}\ntrellis-protocol = {{ path = {:?} }}\n\
            trellis-models = {{ path = {models:?} }}\n",
            root.path().join(format!("trellis-rs-{cli_version}")),
            repository.join("crates/protocol"),
        ),
    )
    .unwrap();
    let sdk_manifest = || fs::read_to_string(root.path().join("sdk/Cargo.toml")).unwrap();
    let sdk_types = || fs::read_to_string(root.path().join("sdk/src/__types.rs")).unwrap();

    // With no Cargo graph yet, the SDK brings in the trellis-models the CLI
    // bundles, so that binding applies.
    trellis(root.path(), "install");
    assert!(sdk_manifest().contains(&format!("\"trellis-models\" = \"{cli_version}\"")));
    assert!(sdk_types().contains("pub use trellis_models::trellis::*;"));
    // Cargo resolves the later release, whose identical snapshot satisfies the
    // SDK's compile-time model check.
    cargo(root.path(), &["run", "--quiet", "--offline"]);

    // Once Cargo.lock names the later release, generation binds that crate.
    let stderr = trellis(root.path(), "generate");
    assert!(
        stderr.contains(&format!(
            "resolves trellis-models {later} for trellis-rs {cli_version}"
        )),
        "{stderr}"
    );
    assert!(sdk_manifest().contains(&format!("\"trellis-models\" = \"{later}\"")));
    assert!(sdk_types().contains("pub use trellis_models::trellis::*;"));
    cargo(root.path(), &["run", "--quiet", "--offline"]);
    let tree = cargo(
        root.path(),
        &["tree", "--offline", "--prefix", "none", "--format", "{p}"],
    );
    let copies = tree
        .lines()
        .filter(|line| line.starts_with("trellis-models "))
        .map(|line| line.trim_end_matches(" (*)"))
        .collect::<BTreeSet<_>>();
    assert_eq!(copies.len(), 1, "{tree}");
    assert!(
        copies.iter().all(|copy| copy.contains(&later.to_string())),
        "{tree}"
    );

    // A fresh checkout has Cargo.lock but no generated SDK yet, so Cargo cannot
    // load the graph; the release's unpacked registry source still binds it.
    fs::remove_dir_all(root.path().join("sdk")).unwrap();
    let cargo_home = root.path().join("cargo-home");
    let unpacked = cargo_home.join(format!(
        "registry/src/index.crates.io-test/trellis-models-{later}"
    ));
    for file in ["Cargo.toml", "src/lib.rs", "generated/__types.rs"] {
        fs::create_dir_all(unpacked.join(file).parent().unwrap()).unwrap();
        fs::copy(models.join(file), unpacked.join(file)).unwrap();
    }
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_trellis"))
        .args(["-v", "generate", "--root"])
        .arg(root.path())
        .env("CARGO_HOME", &cargo_home)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success() && stderr.contains("(Cargo registry sources)"),
        "{stderr}"
    );
    assert!(sdk_manifest().contains(&format!("\"trellis-models\" = \"{later}\"")));
    assert!(sdk_types().contains("pub use trellis_models::trellis::*;"));

    // A resolved release with a different snapshot is not reused.
    fs::write(
        models.join("generated/__types.rs"),
        snapshot.replacen("digest: \"", "digest: \"changed-", 1),
    )
    .unwrap();
    let stderr = trellis(root.path(), "generate");
    assert!(
        stderr.contains(&format!(
            "trellis-models {later} provides trellis@{later} with digest changed-"
        )) && stderr.contains("emitting models inline"),
        "{stderr}"
    );
    assert!(!sdk_manifest().contains("trellis-models"));
    assert!(sdk_types().contains("pub struct AuthUser"));
}
