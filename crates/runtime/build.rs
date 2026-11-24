//! Generates the compile-time index for the embedded Trellis web application
//! and refuses a development build whose embedded Portal or Console is stale.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read embedded portal directory") {
        let path = entry.expect("read embedded portal entry").path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn generate_index(root: &Path, output_name: &str) {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    if root.is_dir() {
        collect_files(root, &mut files);
    }
    assert!(
        !files.is_empty(),
        "embedded browser assets are missing at {}; run `cargo xtask build` to \
        prepare them instead of invoking cargo directly",
        root.display()
    );
    files.sort();
    let entries = files
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .expect("embedded application path below root")
                .to_string_lossy()
                .replace('\\', "/");
            format!(
                "({relative:?}, include_bytes!({path:?}).as_slice()),",
                path = path.canonicalize().expect("canonical application path")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("build output directory")).join(output_name);
    fs::write(output, format!("&[{entries}]\n")).expect("write embedded application index");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Compares the sources an embedded app was built from with the current ones.
///
/// The asset build (`npm run browser:embedded`) writes `<app>.fingerprint`
/// beside the app: a digest, then the pathspecs it covers.
/// `ts/tools/embedded_fingerprint.ts` computes the digest; this recomputes it
/// the same way. A mismatch fails the build, so a development server never
/// serves a Portal or Console older than its sources.
fn check_fresh(repository: &Path, generated: &Path, app: &str) {
    let record = generated.join(format!("{app}.fingerprint"));
    println!("cargo:rerun-if-changed={}", record.display());
    let label = if app == "web" { "Console" } else { "Portal" };
    let rebuild = "run `npm run browser:embedded` to rebuild the embedded Portal and Console";
    let Ok(text) = fs::read_to_string(&record) else {
        panic!(
            "{} is missing, so the embedded {label} cannot be checked against \
            its sources; {rebuild}",
            record.display()
        );
    };
    let mut lines = text.lines();
    let recorded = lines.next().unwrap_or_default();
    let pathspecs = lines.filter(|line| !line.is_empty()).collect::<Vec<_>>();
    let listed = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .args(&pathspecs)
        .output();
    let listed = match listed {
        Ok(output) if output.status.success() => output.stdout,
        // Outside a Git checkout there is nothing to compare with.
        _ => {
            println!(
                "cargo:warning=cannot list the {label} sources with git; \
                skipping the stale-asset check"
            );
            return;
        }
    };
    let mut paths = listed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()).expect("UTF-8 source path"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let mut digest = Sha256::new();
    for path in &paths {
        let file = repository.join(path);
        // A tracked file deleted from the working tree is no longer an input.
        if !file.is_file() {
            continue;
        }
        let contents = fs::read(&file).expect("read embedded app source");
        digest.update(format!("{path}\0{}\n", hex(&Sha256::digest(&contents))));
    }
    let current = hex(&digest.finalize());
    assert!(
        current == recorded,
        "the embedded {label} in {} was built from other sources than the current \
        ones; {rebuild}",
        generated.display()
    );
    // Cargo reruns a failed build script regardless, so the sources are
    // declared only once they match, keeping the failure output short. A
    // directory is declared whole: Cargo then reruns this script when a file
    // in it is added or removed, not only when a listed file changes.
    for pathspec in &pathspecs {
        println!(
            "cargo:rerun-if-changed={}",
            repository.join(pathspec).display()
        );
    }
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let generated = manifest.join("generated");
    // Release builds embed the assets the Pipeline built for the same commit,
    // and their build context has no Git checkout to compare with.
    if env::var("PROFILE").as_deref() != Ok("release") {
        let repository = manifest.join("../..");
        check_fresh(&repository, &generated, "portal");
        check_fresh(&repository, &generated, "web");
    }
    generate_index(&generated.join("web"), "web_assets.rs");
}
