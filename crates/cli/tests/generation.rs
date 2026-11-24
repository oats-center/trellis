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
    assert_eq!(lock.root, "localhost/fixture");
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
    let target = repository.join("target/codegen-consumers/cli-models");
    fs::create_dir_all(&target).unwrap();
    // Cargo fingerprints registry crates by source path. Keep this private
    // Cargo home's path stable across runs, while giving each consumer fresh
    // registry contents and its own project. A concurrent invocation must not
    // replace the registry while this consumer is still compiling.
    let home_lock = fs::File::options()
        .create(true)
        .append(true)
        .open(target.join("consumer-home.lock"))
        .unwrap();
    home_lock.lock().unwrap();
    let cargo_home = target.join("consumer-home");
    if cargo_home.exists() {
        fs::remove_dir_all(&cargo_home).unwrap();
    }
    let home_directory = tempfile::Builder::new()
        .prefix("consumer-home")
        .rand_bytes(0)
        .tempdir_in(&target)
        .unwrap();
    let cargo_home = home_directory.path();
    let cli_version = env!("CARGO_PKG_VERSION");
    // trellis-models keeps its own version, which a release need not bump.
    let models_version = toml::from_str::<toml::Table>(
        &fs::read_to_string(repository.join("crates/models/Cargo.toml")).unwrap(),
    )
    .unwrap()["package"]["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut later = semver::Version::parse(&models_version).unwrap();
    if later.pre.is_empty() {
        later.patch += 1;
    } else {
        later.pre = semver::Prerelease::new(&format!("{}.1", later.pre)).unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    // Cargo and generation share a Cargo home that holds the real registry
    // crates plus the copies this test adds. A dead proxy keeps every step
    // offline; only the local file:// git repository below is fetched.
    let real_home = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(&std::env::var_os("HOME").unwrap()).join(".cargo"));
    for kind in ["index", "cache", "src"] {
        fs::create_dir_all(cargo_home.join("registry").join(kind)).unwrap();
        for entry in fs::read_dir(real_home.join("registry").join(kind)).unwrap() {
            let entry = entry.unwrap();
            std::os::unix::fs::symlink(
                entry.path(),
                cargo_home
                    .join("registry")
                    .join(kind)
                    .join(entry.file_name()),
            )
            .unwrap();
        }
    }
    let cargo = |root: &std::path::Path, args: &[&str]| {
        let result = std::process::Command::new(env!("CARGO"))
            .args(args)
            .current_dir(root)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .env("CARGO_INCREMENTAL", "1")
            .env("CARGO_HOME", cargo_home)
            .env("CARGO_HTTP_PROXY", "http://127.0.0.1:9")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "cargo {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let trellis = |root: &std::path::Path, command: &str| {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_trellis"))
            .args([command, "--root"])
            .arg(root)
            .env("CARGO_HOME", cargo_home)
            .env("CARGO_HTTP_PROXY", "http://127.0.0.1:9")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "trellis {command}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
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
model Seen { at: timestamp; size: int64; }
api who@v1 {
  title "Who";
  description "Return a built-in model.";
  rpc Me { input Empty; output AuthUser; }
  rpc LastSeen { input Empty; output Seen; }
  capabilities { public { allows { rpc Me; rpc LastSeen; } } }
}
"#,
    )
    .unwrap();

    // The published trellis-rs archive names trellis-models by registry
    // version, so Cargo resolves it to whatever the project patches in.
    // Packaging checks the registry index, so it runs with the ordinary Cargo
    // home rather than through the offline `cargo` above.
    let packaged = std::process::Command::new(env!("CARGO"))
        .args([
            "package",
            "-p",
            "trellis-rs",
            "--no-verify",
            "--allow-dirty",
        ])
        .args(["--config", ".cargo/publication.toml"])
        .current_dir(&repository)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert!(
        packaged.status.success(),
        "cargo package: {}",
        String::from_utf8_lossy(&packaged.stderr)
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
    // trellis-protocol keeps its own version, which a release need not bump.
    let protocol_version = toml::from_str::<toml::Table>(
        &fs::read_to_string(repository.join("crates/protocol/Cargo.toml")).unwrap(),
    )
    .unwrap()["package"]["version"]
        .as_str()
        .unwrap()
        .to_owned();
    // A model crate is the repository's models source under a given version,
    // carrying a given generated snapshot.
    let model_crate = |directory: &std::path::Path, version: &str, types: &str| {
        fs::create_dir_all(directory.join("src")).unwrap();
        fs::create_dir_all(directory.join("generated")).unwrap();
        fs::copy(
            repository.join("crates/models/src/lib.rs"),
            directory.join("src/lib.rs"),
        )
        .unwrap();
        fs::write(
            directory.join("Cargo.toml"),
            format!(
                "[package]\nname = \"trellis-models\"\nversion = \"{version}\"\nedition = \
                \"2021\"\n[dependencies]\ntrellis-protocol = \"{protocol_version}\"\nserde = {{ \
                version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n"
            ),
        )
        .unwrap();
        fs::write(directory.join("generated/__types.rs"), types).unwrap();
    };
    let original =
        fs::read_to_string(repository.join("crates/models/generated/__types.rs")).unwrap();
    let changed = |types: &str| types.replacen("digest: \"", "digest: \"changed-", 1);
    let snapshot = original.replacen(
        &format!("version: \"{models_version}\""),
        &format!("version: \"{later}\""),
        1,
    );
    let models = root.path().join("trellis-models-later");
    model_crate(&models, &later.to_string(), &snapshot);

    // `identity` passes trellis-models values through the SDK's types, which
    // compiles only when the SDK reuses the crate Cargo builds.
    let identity = r#"
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
"#;
    let standalone = r#"
fn main() {
    use std::any::TypeId;
    use builtin_consumer::types::AuthUser;
    assert_ne!(TypeId::of::<AuthUser>(), TypeId::of::<trellis_models::trellis::AuthUser>());
    let wire = serde_json::json!({
        "principalId": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "displayName": "Peer",
        "emailVerified": true
    });
    let user: AuthUser = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(user).unwrap(), wire);
}
"#;
    fs::create_dir(root.path().join("src")).unwrap();
    let consumer = |patch: &str, main: &str| {
        fs::write(root.path().join("src/main.rs"), main).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"shared-model-consumer\"\nversion = \"1.0.0\"\nedition = \
                \"2021\"\n[workspace]\n[dependencies]\nbuiltin-consumer = {{ path = \"sdk\" }}\n\
                trellis-models = \"{models_version}\"\nserde_json = \"1\"\n[patch.crates-io]\n\
                trellis-rs = {{ path = {:?} }}\ntrellis-protocol = {{ path = {:?} }}\n\
                trellis-models = {patch}\n",
                root.path().join(format!("trellis-rs-{cli_version}")),
                repository.join("crates/protocol"),
            ),
        )
        .unwrap();
    };

    // The CLI bundles its version's crates.io release. The same version from
    // another source may carry other models, as this path copy does, so the
    // bundled binding would fail the SDK's compile-time model check. The
    // trellis-rs built here expects that copy's models, as a trellis-rs from
    // the same source would.
    let bundled_version_copy = root.path().join("trellis-models-bundled-version");
    model_crate(&bundled_version_copy, &models_version, &changed(&original));
    let runtime_types = root
        .path()
        .join(format!("trellis-rs-{cli_version}/generated/__types.rs"));
    let runtime_snapshot = fs::read_to_string(&runtime_types).unwrap();
    fs::write(&runtime_types, changed(&runtime_snapshot)).unwrap();
    consumer(
        &format!("{{ path = {bundled_version_copy:?} }}"),
        standalone,
    );
    trellis(root.path(), "install");
    cargo(root.path(), &["generate-lockfile", "--offline"]);
    trellis(root.path(), "generate");
    cargo(root.path(), &["run", "--quiet", "--offline"]);
    fs::write(&runtime_types, &runtime_snapshot).unwrap();
    fs::remove_file(root.path().join("Cargo.lock")).unwrap();

    consumer(&format!("{{ path = {models:?} }}"), identity);
    // With no Cargo graph yet, the SDK brings in the trellis-rs the CLI
    // matches, and with it that release's models, so the bundled binding applies.
    trellis(root.path(), "install");
    // Cargo resolves the later release, whose identical snapshot satisfies the
    // SDK's compile-time model check.
    cargo(root.path(), &["run", "--quiet", "--offline"]);

    // Once Cargo.lock names the later release, generation binds that crate.
    trellis(root.path(), "generate");
    cargo(root.path(), &["run", "--quiet", "--offline"]);
    // A trellis-rs that links sibling path trellis-models and trellis-protocol
    // crates, with no patch for either anywhere, still gives the SDK those
    // exact crates: values of the types trellis-rs exposes pass through the
    // SDK's types.
    let runtime_manifest = root
        .path()
        .join(format!("trellis-rs-{cli_version}/Cargo.toml"));
    let published_runtime = fs::read_to_string(&runtime_manifest).unwrap();
    let mut sibling: toml::Table = toml::from_str(&published_runtime).unwrap();
    for (dependency, path) in [
        ("trellis-models", models.clone()),
        ("trellis-protocol", repository.join("crates/protocol")),
    ] {
        sibling["dependencies"][dependency]
            .as_table_mut()
            .unwrap()
            .insert("path".into(), path.display().to_string().into());
    }
    fs::write(&runtime_manifest, toml::to_string(&sibling).unwrap()).unwrap();
    let models_manifest = models.join("Cargo.toml");
    let published_models = fs::read_to_string(&models_manifest).unwrap();
    fs::write(
        &models_manifest,
        published_models.replace(
            &format!("trellis-protocol = \"{protocol_version}\""),
            &format!(
                "trellis-protocol = {{ path = {:?} }}",
                repository.join("crates/protocol")
            ),
        ),
    )
    .unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"shared-model-consumer\"\nversion = \"1.0.0\"\nedition = \
            \"2021\"\n[workspace]\n[dependencies]\nbuiltin-consumer = {{ path = \"sdk\" }}\n\
            trellis-rs = \"{cli_version}\"\nserde_json = \"1\"\n[patch.crates-io]\n\
            trellis-rs = {{ path = {:?} }}\n",
            root.path().join(format!("trellis-rs-{cli_version}")),
        ),
    )
    .unwrap();
    fs::write(
        root.path().join("src/main.rs"),
        identity
            .replace("trellis_models::", "trellis_rs::models::")
            .replace(
                "\n}\n",
                r#"
    let wire = serde_json::json!({ "at": "2026-11-22T08:30:00Z", "size": "9007199254740993" });
    let seen: builtin_consumer::types::Seen = serde_json::from_value(wire.clone()).unwrap();
    let at: trellis_rs::protocol::model::Timestamp = seen.at;
    let size: trellis_rs::protocol::model::Int64 = seen.size;
    let back = builtin_consumer::types::Seen { at, size, extra: seen.extra };
    assert_eq!(serde_json::to_value(back).unwrap(), wire);
}
"#,
            ),
    )
    .unwrap();
    fs::remove_file(root.path().join("Cargo.lock")).unwrap();
    cargo(root.path(), &["generate-lockfile", "--offline"]);
    trellis(root.path(), "generate");
    cargo(root.path(), &["run", "--quiet", "--offline"]);
    fs::write(&runtime_manifest, published_runtime).unwrap();
    fs::write(&models_manifest, published_models).unwrap();
    fs::remove_file(root.path().join("Cargo.lock")).unwrap();
    consumer(&format!("{{ path = {models:?} }}"), identity);
    cargo(root.path(), &["generate-lockfile", "--offline"]);

    // This runtime and its linked models agree; the generated SDK needs its own types.
    fs::write(&runtime_types, changed(&runtime_snapshot)).unwrap();
    fs::write(models.join("generated/__types.rs"), changed(&snapshot)).unwrap();
    trellis(root.path(), "generate");
    fs::write(root.path().join("src/main.rs"), standalone).unwrap();
    cargo(root.path(), &["run", "--quiet", "--offline"]);
    fs::write(&runtime_types, &runtime_snapshot).unwrap();
    fs::write(models.join("generated/__types.rs"), &snapshot).unwrap();
    trellis(root.path(), "generate");

    // A project locked to a git commit binds that commit's checkout, not a
    // same-version registry copy whose models differ.
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Trellis",
                "-c",
                "user.email=trellis@example.com",
            ])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&models)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    fs::create_dir_all(models.join("anchor/src")).unwrap();
    fs::write(models.join("anchor/src/lib.rs"), "").unwrap();
    fs::write(
        models.join("anchor/Cargo.toml"),
        "[package]\nname = \"git-anchor\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git(&["init", "--quiet"]);
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "models"]);
    let registry_copy = |index: &str, types: &str, archive: &str| {
        let unpacked = cargo_home.join(format!("registry/src/{index}/trellis-models-{later}"));
        model_crate(&unpacked, &later.to_string(), types);
        fs::write(unpacked.join(".cargo-ok"), "{\"v\":1}").unwrap();
        let cache = cargo_home.join(format!("registry/cache/{index}"));
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join(format!("trellis-models-{later}.crate")), archive).unwrap();
    };
    registry_copy("index.crates.io-test", &changed(&snapshot), "impostor");
    consumer(
        &format!("{{ git = \"file://{}\" }}", models.display()),
        identity,
    );
    // Resolving a git crate with registry dependencies, or a changed patch,
    // would query the registry index. Fetching a dependency-free crate in the
    // same repository instead brings the commit into Cargo's git cache, from
    // which the consumer then resolves offline.
    let fetcher = root.path().join("git-fetch");
    fs::create_dir_all(fetcher.join("src")).unwrap();
    fs::write(fetcher.join("src/lib.rs"), "").unwrap();
    fs::write(
        fetcher.join("Cargo.toml"),
        format!(
            "[package]\nname = \"git-fetch\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
            [workspace]\n[dependencies]\ngit-anchor = {{ git = \"file://{}\" }}\n",
            models.display(),
        ),
    )
    .unwrap();
    cargo(&fetcher, &["fetch", "--quiet"]);
    cargo(root.path(), &["generate-lockfile", "--offline"]);
    trellis(root.path(), "generate");
    cargo(root.path(), &["run", "--quiet", "--offline"]);

    // A registry lock names the crate archive by checksum, so of two
    // same-version registry copies generation reads the one locked.
    registry_copy("example.com-test", &snapshot, "genuine");
    let lock = fs::read_to_string(root.path().join("Cargo.lock")).unwrap();
    let relock = |archive: &[u8]| {
        use sha2::Digest as _;
        let checksum = sha2::Sha256::digest(archive)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let relocked = lock
            .lines()
            .map(|line| {
                if line.starts_with("source = \"git+") {
                    format!(
                        "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                        checksum = \"{checksum}\""
                    )
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.path().join("Cargo.lock"), relocked).unwrap();
        trellis(root.path(), "generate")
    };
    relock(b"genuine");
    fs::write(root.path().join("Cargo.lock"), &lock).unwrap();
    cargo(root.path(), &["run", "--quiet", "--offline"]);
    relock(b"impostor");
    fs::write(root.path().join("Cargo.lock"), &lock).unwrap();
    fs::write(root.path().join("src/main.rs"), standalone).unwrap();
    cargo(root.path(), &["run", "--quiet", "--offline"]);
}
