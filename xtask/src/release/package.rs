//! Release packaging checks: native archive pins, publication archive
//! comparison, and the artifact-only testkit verification bundle.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use miette::{miette, IntoDiagnostic, Result};
use sha2::{Digest, Sha256};

/// Native targets the testkit acquires published CLI/server archives for.
const NATIVE_TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

/// The public crates a testkit verification bundle must carry.
const BUNDLE_CRATES: [&str; 4] = [
    "trellis-protocol",
    "trellis-models",
    "trellis-rs",
    "trellis-testkit",
];

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Write the testkit's native archive pins after checking each archive
/// against its published checksum.
pub(super) fn native_pins(archives: &Path, output: &Path, version: &str) -> Result<()> {
    let prefix = format!("trellis-{version}-");
    let mut targets = BTreeMap::new();
    let mut entries = fs::read_dir(archives)
        .into_diagnostic()?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    entries.sort();
    for name in entries {
        let Some(target) = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".tar.gz"))
        else {
            continue;
        };
        if !NATIVE_TARGETS.contains(&target) {
            return Err(miette!("unsupported native target: {target}"));
        }
        let checksum_file = archives.join(format!("checksum-{version}-{target}-trellis.sha256"));
        let checksum = fs::read_to_string(&checksum_file)
            .into_diagnostic()?
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        let actual = sha256_hex(&fs::read(archives.join(&name)).into_diagnostic()?);
        if actual != checksum {
            return Err(miette!("native archive checksum mismatch: {name}"));
        }
        targets.insert(
            target.to_owned(),
            serde_json::json!({ "archive": name, "sha256": actual }),
        );
    }
    if targets.is_empty() {
        return Err(miette!("no native archives for {version}"));
    }
    let staged = output.with_extension("tmp");
    fs::write(
        &staged,
        serde_json::to_string_pretty(&serde_json::json!({
            "version": version,
            "targets": targets,
        }))
        .into_diagnostic()?
            + "\n",
    )
    .into_diagnostic()?;
    fs::rename(staged, output).into_diagnostic()
}

/// Each regular file of a `.crate` archive with its mode and SHA-256.
fn crate_contents(path: &Path) -> Result<BTreeMap<String, (u32, String)>> {
    let file = fs::File::open(path).into_diagnostic()?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut files = BTreeMap::new();
    for entry in archive.entries().into_diagnostic()? {
        let mut entry = entry.into_diagnostic()?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        let name = entry
            .path()
            .into_diagnostic()?
            .to_string_lossy()
            .into_owned();
        if !kind.is_file() {
            return Err(miette!("unsupported archive entry: {name}"));
        }
        let mode = entry.header().mode().into_diagnostic()?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).into_diagnostic()?;
        if name.is_empty()
            || files
                .insert(name.clone(), (mode, sha256_hex(&bytes)))
                .is_some()
        {
            return Err(miette!("invalid or duplicate archive entry: {name}"));
        }
    }
    Ok(files)
}

/// Require Cargo's publication archive to carry exactly the file contents and
/// modes of the archive Check accepted; archive metadata such as timestamps
/// may differ.
pub(super) fn compare_crate(accepted: &Path, publication: &Path) -> Result<()> {
    let accepted_files = crate_contents(accepted)?;
    let publication_files = crate_contents(publication)?;
    let differences = accepted_files
        .keys()
        .chain(publication_files.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|name| accepted_files.get(*name) != publication_files.get(*name))
        .cloned()
        .collect::<Vec<_>>();
    if !differences.is_empty() {
        return Err(miette!(
            "publication differs from accepted Check: {}",
            differences.join(", ")
        ));
    }
    println!(
        "Publication contents match accepted Check: {}",
        accepted.display()
    );
    Ok(())
}

/// The `[package]` name and version recorded in a `.crate` archive.
fn crate_identity(path: &Path) -> Result<(String, String)> {
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".crate"))
        .ok_or_else(|| miette!("not a .crate archive: {}", path.display()))?;
    let manifest_path = format!("{stem}/Cargo.toml");
    let file = fs::File::open(path).into_diagnostic()?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries().into_diagnostic()? {
        let mut entry = entry.into_diagnostic()?;
        if entry.path().into_diagnostic()?.to_string_lossy() != manifest_path {
            continue;
        }
        let mut source = String::new();
        entry.read_to_string(&mut source).into_diagnostic()?;
        let manifest = source.parse::<toml_edit::DocumentMut>().into_diagnostic()?;
        let field = |key: &str| {
            manifest["package"][key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| miette!("{} has no package {key}", path.display()))
        };
        return Ok((field("name")?, field("version")?));
    }
    Err(miette!("{} has no {manifest_path}", path.display()))
}

/// Every regular file under `root`, relative to it, in sorted order.
fn bundle_files(root: &Path, directory: &Path, files: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(directory).into_diagnostic()? {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        if entry.file_type().into_diagnostic()?.is_dir() {
            bundle_files(root, &path, files)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .into_diagnostic()?
                .to_string_lossy()
                .into_owned();
            files.push(relative);
        }
    }
    Ok(())
}

/// Write the bundle's `artifact-manifest.json` (file hashes, crate versions
/// and executables) and pack the whole directory, preserving modes.
pub(super) fn stage_testkit_bundle(
    bundle: &Path,
    output: &Path,
    source_sha: &str,
    target: &str,
    version: &str,
) -> Result<()> {
    for directory in ["packages", "nats", "native", "consumer"] {
        if !bundle.join(directory).is_dir() {
            return Err(miette!(
                "missing staging directory: {}",
                bundle.join(directory).display()
            ));
        }
    }
    if !bundle.join("verify-rust-test-package.sh").is_file() {
        return Err(miette!(
            "missing {}/verify-rust-test-package.sh",
            bundle.display()
        ));
    }
    let mut names = Vec::new();
    bundle_files(bundle, bundle, &mut names)?;
    names.sort();
    let mut files = BTreeMap::new();
    for name in names {
        if name == "artifact-manifest.json" {
            continue;
        }
        let digest = sha256_hex(&fs::read(bundle.join(&name)).into_diagnostic()?);
        files.insert(name, digest);
    }
    let mut packages = BTreeMap::new();
    for entry in fs::read_dir(bundle.join("packages")).into_diagnostic()? {
        let path = entry.into_diagnostic()?.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "crate")
        {
            continue;
        }
        let (name, crate_version) = crate_identity(&path)?;
        if packages.insert(name.clone(), crate_version).is_some() {
            return Err(miette!("duplicate archive for {name}"));
        }
    }
    for package in BUNDLE_CRATES {
        if !packages.contains_key(package) {
            return Err(miette!("missing {package} .crate in packages/"));
        }
    }
    let manifest = serde_json::json!({
        "source_sha": source_sha,
        "target": target,
        "version": version,
        "packages": packages,
        "files": files,
        "executables": ["nats/nats-server", "native/trellis", "native/trellis-server"],
    });
    fs::write(
        bundle.join("artifact-manifest.json"),
        serde_json::to_string_pretty(&manifest).into_diagnostic()? + "\n",
    )
    .into_diagnostic()?;
    let file = fs::File::create(output).into_diagnostic()?;
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        file,
        flate2::Compression::default(),
    ));
    archive.append_dir_all(".", bundle).into_diagnostic()?;
    archive
        .into_inner()
        .into_diagnostic()?
        .finish()
        .into_diagnostic()?;
    println!("staged {} for {target} at {source_sha}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_with(directory: &Path, name: &str, body: &[u8], mtime: u64) -> std::path::PathBuf {
        let path = directory.join(name);
        let file = fs::File::create(&path).unwrap();
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::default(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        // Recompression metadata is not application content.
        header.set_mtime(mtime);
        header.set_cksum();
        archive
            .append_data(
                &mut header,
                "trellis-runtime-1.0.0/generated/web/index.html",
                body,
            )
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn identical_content_survives_repackaging() {
        let directory = tempfile::tempdir().unwrap();
        let accepted = crate_with(directory.path(), "a.crate", b"<html>accepted</html>", 1);
        let published = crate_with(directory.path(), "b.crate", b"<html>accepted</html>", 2);
        compare_crate(&accepted, &published).unwrap();
    }

    #[test]
    fn changed_content_blocks_publication() {
        let directory = tempfile::tempdir().unwrap();
        let accepted = crate_with(directory.path(), "a.crate", b"<html>accepted</html>", 1);
        let published = crate_with(directory.path(), "b.crate", b"<html>different</html>", 1);
        let error = compare_crate(&accepted, &published)
            .unwrap_err()
            .to_string();
        assert!(error.contains("generated/web/index.html"), "{error}");
    }
}
