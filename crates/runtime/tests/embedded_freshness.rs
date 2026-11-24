//! The runtime build script refuses a development build whose embedded Portal
//! or Console was built from other sources than the current ones.
//!
//! Each test builds a small package with the runtime's own `build.rs` in a
//! scratch repository, its fingerprints written by the same
//! `ts/tools/embedded_fingerprint.ts` the asset builds use.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A scratch repository holding a package built by the runtime's build
/// script, with one Portal and one Console source file.
fn fixture(git: bool) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("fixture directory");
    let package = root.path().join("crates/runtime");
    fs::create_dir_all(package.join("src")).unwrap();
    fs::create_dir_all(package.join("generated/web")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"),
        package.join("build.rs"),
    )
    .unwrap();
    // The workspace lock pins the build script's dependencies to versions
    // already downloaded, so the fixture builds offline.
    fs::copy(workspace().join("Cargo.lock"), package.join("Cargo.lock")).unwrap();
    fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"embedded-freshness-fixture\"\nversion = \"0.0.0\"\n\
        edition = \"2021\"\n\n[workspace]\n\n[build-dependencies]\nsha2 = \"0.11.1\"\n",
    )
    .unwrap();
    fs::write(package.join("src/lib.rs"), "").unwrap();
    fs::write(package.join("generated/web/index.html"), "console").unwrap();
    for app in ["portal", "web"] {
        fs::create_dir_all(root.path().join(app).join("src")).unwrap();
        fs::write(root.path().join(app).join("src/main.js"), app).unwrap();
    }
    if git {
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .arg(root.path())
            .status()
            .unwrap();
        assert!(init.success());
    }
    root
}

/// Records the current sources as the ones the embedded apps were built from.
fn fingerprint(root: &Path) {
    let producer = workspace().join("ts/tools/embedded_fingerprint.ts");
    let script = format!(
        "import {{ sourceFingerprint, writeFingerprint }} from {producer:?};\n\
        const root = {root:?};\n\
        for (const [app, input] of [[\"portal\", \"portal/src\"], [\"web\", \"web/src\"]]) {{\n\
          writeFingerprint(sourceFingerprint(root, [input]), [input], \
          `${{root}}/crates/runtime/generated/${{app}}.fingerprint`);\n\
        }}\n",
        producer = producer.canonicalize().unwrap().display().to_string(),
        root = root.display().to_string(),
    );
    let output = Command::new("node")
        .args(["--input-type=module", "--eval", &script])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", report(&output));
}

fn build(root: &Path) -> Output {
    let package = root.join("crates/runtime");
    Command::new(option_env!("CARGO").unwrap_or("cargo"))
        .args(["build", "--offline", "--quiet", "--manifest-path"])
        .arg(package.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .unwrap()
}

fn report(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_source_file_added_after_the_asset_build_refuses_the_next_build() {
    let root = fixture(true);
    fingerprint(root.path());
    let built = build(root.path());
    assert!(built.status.success(), "{}", report(&built));

    // Only a new file: no file the last build read changed.
    fs::write(root.path().join("portal/src/added.js"), "added").unwrap();
    let stale = build(root.path());
    assert!(
        !stale.status.success(),
        "a build with a new Portal source and the old assets succeeded"
    );

    // Rebuilding the assets records the new file, and the build goes on.
    fingerprint(root.path());
    let rebuilt = build(root.path());
    assert!(rebuilt.status.success(), "{}", report(&rebuilt));
}
