//! The Trellis source-package distribution format, v1.
//!
//! A published release is an OCI image whose one layer is a normalized tar of
//! the package's `trellis.toml`, its listed `.trellis` sources, and its frozen
//! dependency resolution, and whose config names the package and its semantic
//! digest. Everything here is checked from bytes alone, so the CLI and Auth
//! verify a pulled release the same way whichever registry served it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use miette::{miette, IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::project::{LockedDependency, LockedPackage, PackageManifest};

/// Media type of the source layer, and the image's artifact type.
pub const SOURCE_MEDIA_TYPE: &str = "application/vnd.trellis.package.source.v1+tar";
/// Media type of the package config blob.
pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.trellis.package.source.v1+json";
/// Path of the frozen resolution inside the source layer.
pub const RESOLUTION_PATH: &str = "trellis.resolution.json";
/// Largest image manifest a release may have.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Largest config blob a release may have.
pub const MAX_CONFIG_BYTES: i64 = 4 * 1024;
const MAX_FILES: usize = 1_024;
const MAX_PATH_BYTES: usize = 255;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest source layer a release may have.
pub const MAX_ARCHIVE_BYTES: u64 = MAX_PACKAGE_BYTES + MAX_FILES as u64 * 4 * 512 + 1_024;

/// The config blob of a release: its identity and semantic digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageConfig {
    /// Always `trellis.package.source.v1`.
    pub format: String,
    /// Package name.
    pub name: String,
    /// Release version.
    pub version: String,
    /// Base64url SHA-256 semantic digest of the compiled package.
    pub package_digest: String,
}

impl PackageConfig {
    /// The config for one release.
    pub fn new(name: &str, version: &semver::Version, package_digest: &str) -> Self {
        Self {
            format: "trellis.package.source.v1".to_owned(),
            name: name.to_owned(),
            version: version.to_string(),
            package_digest: package_digest.to_owned(),
        }
    }

    /// Parse a config blob, refusing any that isn't canonical v1 JSON.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let config: Self = serde_json::from_slice(bytes)
            .into_diagnostic()
            .map_err(|error| miette!("invalid Trellis package config: {error}"))?;
        miette::ensure!(
            config.format == "trellis.package.source.v1",
            "unsupported Trellis package config format"
        );
        miette::ensure!(
            bytes == canonical_json(&config)?,
            "Trellis package config is not canonical JSON"
        );
        Ok(config)
    }
}

/// The exact dependency closure a release was published with.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenResolution {
    /// Always `trellis.package.resolution.v1`.
    pub format: String,
    /// The root package's direct dependencies.
    pub dependencies: Vec<LockedDependency>,
    /// Every package of the closure besides the root.
    pub packages: Vec<LockedPackage>,
}

/// Canonical JSON bytes of `value`.
pub fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(
        trellis_protocol::canonicalize_json(&serde_json::to_value(value).into_diagnostic()?)
            .map_err(|error| miette!(error.to_string()))?
            .into_bytes(),
    )
}

/// The `sha256:<hex>` digest of `bytes`.
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold("sha256:".to_owned(), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

/// Check that `value` is a base64url SHA-256 semantic digest.
pub fn validate_package_digest(value: &str) -> Result<()> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| miette!("package digest must be a base64url SHA-256 digest"))?;
    miette::ensure!(
        bytes.len() == 32,
        "package digest must be a base64url SHA-256 digest"
    );
    Ok(())
}

/// The normalized tar of a release's files.
pub fn archive(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<Vec<u8>> {
    validate_files(files)?;
    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        builder.mode(tar::HeaderMode::Deterministic);
        for (path, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            builder
                .append_data(&mut header, path, contents.as_slice())
                .into_diagnostic()?;
        }
        builder.finish().into_diagnostic()?;
    }
    Ok(bytes)
}

/// Read a release's files from its source layer, refusing any layer that is
/// not exactly the normalized tar of valid package files.
pub fn extract_archive(bytes: &[u8]) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    miette::ensure!(
        bytes.len() as u64 <= MAX_ARCHIVE_BYTES,
        "source package archive is too large"
    );
    let mut tar = tar::Archive::new(Cursor::new(bytes));
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for entry in tar.entries().into_diagnostic()? {
        let mut entry = entry.into_diagnostic()?;
        miette::ensure!(
            entry.header().entry_type().is_file(),
            "source package contains a non-file entry"
        );
        let path = entry.path().into_diagnostic()?.into_owned();
        validate_path(&path)?;
        let size = entry.size();
        total = total
            .checked_add(size)
            .ok_or_else(|| miette!("source package size overflow"))?;
        miette::ensure!(
            size <= MAX_FILE_BYTES && total <= MAX_PACKAGE_BYTES,
            "source package contents exceed size limits"
        );
        let mut contents = Vec::with_capacity(size as usize);
        entry.read_to_end(&mut contents).into_diagnostic()?;
        miette::ensure!(
            files.insert(path.clone(), contents).is_none(),
            "source package contains duplicate path {}",
            path.display()
        );
        miette::ensure!(
            files.len() <= MAX_FILES,
            "source package contains too many files"
        );
    }
    validate_files(&files)?;
    miette::ensure!(
        archive(&files)? == bytes,
        "source package tar is not normalized"
    );
    Ok(files)
}

/// Check that `files` are exactly a manifest, its sources, and a frozen
/// resolution, within the size limits.
pub fn validate_files(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
    miette::ensure!(
        files.contains_key(Path::new("trellis.toml")),
        "source package is missing trellis.toml"
    );
    let manifest = package_manifest(files)?;
    let expected = std::iter::once(PathBuf::from("trellis.toml"))
        .chain(std::iter::once(PathBuf::from(RESOLUTION_PATH)))
        .chain(manifest.sources.values().map(PathBuf::from))
        .collect::<BTreeSet<_>>();
    miette::ensure!(
        files.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "source package content does not exactly match its manifest sources"
    );
    let mut total = 0_u64;
    for (path, contents) in files {
        validate_path(path)?;
        total = total
            .checked_add(contents.len() as u64)
            .ok_or_else(|| miette!("source package size overflow"))?;
        miette::ensure!(
            contents.len() as u64 <= MAX_FILE_BYTES && total <= MAX_PACKAGE_BYTES,
            "source package contents exceed size limits"
        );
    }
    miette::ensure!(
        files.len() <= MAX_FILES,
        "source package contains too many files"
    );
    frozen_resolution(files)?;
    Ok(())
}

/// The frozen resolution among a release's files.
pub fn frozen_resolution(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<FrozenResolution> {
    let bytes = files
        .get(Path::new(RESOLUTION_PATH))
        .ok_or_else(|| miette!("source package is missing frozen resolution metadata"))?;
    let resolution: FrozenResolution = serde_json::from_slice(bytes).into_diagnostic()?;
    miette::ensure!(
        resolution.format == "trellis.package.resolution.v1",
        "unsupported frozen resolution format"
    );
    miette::ensure!(
        bytes == &canonical_json(&resolution)?,
        "frozen resolution is not canonical JSON"
    );
    miette::ensure!(
        resolution
            .dependencies
            .windows(2)
            .all(|pair| pair[0].name < pair[1].name)
            && resolution
                .packages
                .windows(2)
                .all(|pair| pair[0].name < pair[1].name),
        "frozen resolution entries must be uniquely sorted"
    );
    Ok(resolution)
}

/// The parsed `trellis.toml` among a release's files.
pub fn package_manifest(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<PackageManifest> {
    let manifest: PackageManifest =
        toml::from_slice(&files[Path::new("trellis.toml")]).into_diagnostic()?;
    manifest.validate()?;
    Ok(manifest)
}

fn validate_path(path: &Path) -> Result<()> {
    miette::ensure!(
        !path.as_os_str().is_empty()
            && !path.is_absolute()
            && path
                .to_str()
                .is_some_and(|path| path.len() <= MAX_PATH_BYTES)
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "invalid source package path {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archived_files() -> BTreeMap<PathBuf, Vec<u8>> {
        BTreeMap::from([
            (PathBuf::from("main.trellis"), b"api test@v1 {}\n".to_vec()),
            (
                PathBuf::from("trellis.toml"),
                b"[package]\nname='acme-orders'\nversion='1.2.3'\n[sources]\nmain='main.trellis'\n"
                    .to_vec(),
            ),
            (
                RESOLUTION_PATH.into(),
                canonical_json(&FrozenResolution {
                    format: "trellis.package.resolution.v1".into(),
                    dependencies: Vec::new(),
                    packages: Vec::new(),
                })
                .unwrap(),
            ),
        ])
    }

    #[test]
    fn source_archive_is_normalized_and_rejects_unsafe_content() {
        let bytes = archive(&archived_files()).unwrap();
        assert_eq!(archive(&archived_files()).unwrap(), bytes);
        assert_eq!(extract_archive(&bytes).unwrap(), archived_files());
        let mut unexpected = archived_files();
        unexpected.insert(PathBuf::from("README.md"), Vec::new());
        assert!(archive(&unexpected)
            .unwrap_err()
            .to_string()
            .contains("exactly match"));

        let mut bytes = Vec::new();
        let mut builder = tar::Builder::new(&mut bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "contract.trellis", &[][..])
            .unwrap();
        builder.finish().unwrap();
        drop(builder);
        assert!(extract_archive(&bytes)
            .unwrap_err()
            .to_string()
            .contains("non-file"));
    }
}
