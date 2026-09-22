use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TRELLIS_OFFICIAL_BUILD");
    println!("cargo:rerun-if-env-changed=TRELLIS_BUILD_SHA");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    let manifest_dir = Path::new(&manifest_dir);
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../../.git/HEAD").display()
    );

    let package_version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    let build_version = build_version(manifest_dir, &package_version);
    println!("cargo:rustc-env=TRELLIS_BUILD_VERSION={build_version}");
}

fn build_version(manifest_dir: &Path, package_version: &str) -> String {
    if std::env::var("TRELLIS_OFFICIAL_BUILD").as_deref() == Ok("1") {
        return package_version.to_string();
    }

    let sha = std::env::var("TRELLIS_BUILD_SHA")
        .ok()
        .map(|value| short_sha(&value))
        .or_else(|| git_output(manifest_dir, &["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = if is_dirty(manifest_dir) { ".dirty" } else { "" };
    format!("{package_version}+local.{sha}{dirty}")
}

fn short_sha(value: &str) -> String {
    value.chars().take(12).collect()
}

fn is_dirty(manifest_dir: &Path) -> bool {
    git_output(manifest_dir, &["status", "--porcelain"]).is_some_and(|output| !output.is_empty())
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
