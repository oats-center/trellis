//! OCI distribution, Docker credentials, and the content-addressed source-package cache.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use docker_credential::{CredentialRetrievalError, DockerCredential};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use oci_client::{
    client::{Client, ClientConfig, ClientProtocol, Config, ImageLayer},
    errors::{OciDistributionError, OciErrorCode},
    manifest::{OciImageManifest, OCI_IMAGE_MEDIA_TYPE},
    secrets::RegistryAuth,
    Reference,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::project::{LockedDependency, LockedPackage, LockedSource, ProjectLock, RegistryConfig};

pub(crate) const SOURCE_MEDIA_TYPE: &str = "application/vnd.trellis.package.source.v1+tar";
const CONFIG_MEDIA_TYPE: &str = "application/vnd.trellis.package.source.v1+json";
const RESOLUTION_PATH: &str = "trellis.resolution.json";
const MAX_FILES: usize = 1_024;
const MAX_PATH_BYTES: usize = 255;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = MAX_PACKAGE_BYTES + MAX_FILES as u64 * 4 * 512 + 1_024;
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageConfig {
    format: String,
    name: String,
    version: String,
    package_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrozenResolution {
    format: String,
    dependencies: Vec<LockedDependency>,
    packages: Vec<LockedPackage>,
}

/// Validated source files and immutable distribution identity for one package release.
#[derive(Clone, Debug)]
pub(crate) struct SourcePackage {
    /// Stable package name and OCI repository suffix.
    pub name: String,
    /// Immutable package release version.
    pub version: Version,
    /// Base64url SHA-256 digest of the compiled package semantics.
    pub package_digest: String,
    /// SHA-256 digest of the exact normalized tar layer.
    pub distribution_digest: String,
    /// Explicit manifest and IDL source files.
    pub files: BTreeMap<PathBuf, Vec<u8>>,
    /// Exact OCI image manifest digest, empty before publication.
    pub manifest_digest: String,
}

/// Build a normalized source package from its explicit manifest and IDL sources.
pub(crate) fn source_package(
    name: &str,
    version: &Version,
    package_digest: &str,
    mut files: BTreeMap<PathBuf, Vec<u8>>,
    dependencies: Vec<LockedDependency>,
    packages: Vec<LockedPackage>,
) -> Result<SourcePackage> {
    miette::ensure!(
        !files.contains_key(Path::new(RESOLUTION_PATH)),
        "package source path '{RESOLUTION_PATH}' is reserved"
    );
    files.insert(
        RESOLUTION_PATH.into(),
        canonical_json(&FrozenResolution {
            format: "trellis.package.resolution.v1".into(),
            dependencies,
            packages,
        })?,
    );
    validate_files(&files)?;
    let manifest = package_manifest(&files)?;
    miette::ensure!(
        manifest.package.name == name && manifest.package.version == *version,
        "source package identity differs from trellis.toml"
    );
    let bytes = archive(&files)?;
    validate_package_digest(package_digest)?;
    Ok(SourcePackage {
        name: name.to_owned(),
        version: version.clone(),
        package_digest: package_digest.to_owned(),
        distribution_digest: sha256(&bytes),
        files,
        manifest_digest: String::new(),
    })
}

impl SourcePackage {
    pub(crate) fn lock(&self, source: LockedSource) -> Result<ProjectLock> {
        let resolution = frozen_resolution(&self.files)?;
        miette::ensure!(
            resolution
                .packages
                .iter()
                .all(|package| matches!(&package.source, LockedSource::Registry { .. })),
            "published frozen resolution contains a path dependency"
        );
        let mut packages = resolution.packages;
        miette::ensure!(
            packages.iter().all(|package| package.name != self.name),
            "frozen resolution repeats its root package"
        );
        packages.push(LockedPackage {
            name: self.name.clone(),
            version: self.version.clone(),
            digest: self.package_digest.clone(),
            distribution_digest: self.distribution_digest.clone(),
            dependencies: resolution.dependencies,
            source,
        });
        packages.sort_by(|left, right| left.name.cmp(&right.name));
        let lock = ProjectLock {
            format: 2,
            root: self.name.clone(),
            packages,
        };
        lock.validate()?;
        Ok(lock)
    }
}

/// Derive the one OCI repository owned by a package name.
pub(crate) fn repository(config: &RegistryConfig, name: &str) -> Result<String> {
    let repository = format!("{}/{name}", config.prefix.trim_end_matches('/'));
    Reference::from_str(&repository)
        .map_err(|error| miette!("invalid OCI repository for package '{name}': {error}"))?;
    Ok(repository)
}

/// List every valid SemVer release tag in one package repository.
pub(crate) async fn versions(config: &RegistryConfig, name: &str) -> Result<Vec<Version>> {
    let reference = Reference::from_str(&repository(config, name)?)
        .map_err(|error| miette!(error.to_string()))?;
    let auth = registry_auth(reference.resolve_registry())?;
    let client = client(reference.resolve_registry())?;
    let mut tags = Vec::new();
    let mut cursors = BTreeSet::new();
    let mut last = None;
    loop {
        let response = match client
            .list_tags(&reference, &auth, Some(100), last.as_deref())
            .await
        {
            Ok(response) => response,
            Err(error) if tags.is_empty() && missing_repository(&error) => return Ok(Vec::new()),
            Err(error) => return Err(miette!("failed to list releases for '{name}': {error}")),
        };
        let Some(next) = response.tags.last().cloned() else {
            break;
        };
        if !cursors.insert(next.clone()) {
            return Err(miette!(
                "registry returned a non-advancing tag page for '{name}'"
            ));
        }
        tags.extend(response.tags);
        last = Some(next);
    }
    let mut versions = tags
        .into_iter()
        .filter_map(|tag| Version::parse(&tag).ok())
        .collect::<Vec<_>>();
    versions.sort();
    versions.dedup();
    Ok(versions)
}

/// Pull and validate a tagged source-package release.
pub(crate) async fn pull_tag(
    config: &RegistryConfig,
    name: &str,
    version: &Version,
) -> Result<SourcePackage> {
    let reference = Reference::from_str(&format!("{}:{version}", repository(config, name)?))
        .map_err(|error| miette!(error.to_string()))?;
    let pulled = pull_reference(&reference).await?;
    validate_package(&pulled, name, &version.to_string(), None)?;
    write_cache(&pulled)?;
    Ok(pulled)
}

/// Read a locked package from cache or pull its exact OCI manifest digest.
pub(crate) async fn pull_locked(
    config: &RegistryConfig,
    name: &str,
    version: &str,
    package_digest: &str,
    distribution_digest: &str,
    manifest_digest: &str,
) -> Result<SourcePackage> {
    if let Ok(pulled) = read_locked(
        name,
        version,
        package_digest,
        distribution_digest,
        manifest_digest,
    ) {
        return Ok(pulled);
    }
    let _ = fs::remove_dir_all(cache_entry(manifest_digest)?);
    let reference =
        Reference::from_str(&format!("{}@{manifest_digest}", repository(config, name)?))
            .map_err(|error| miette!(error.to_string()))?;
    let pulled = pull_reference(&reference)
        .await
        .wrap_err_with(|| format!("locked source package {manifest_digest} is unavailable"))?;
    miette::ensure!(
        pulled.manifest_digest == manifest_digest,
        "OCI manifest digest differs from lock"
    );
    validate_package(&pulled, name, version, Some(package_digest))?;
    miette::ensure!(
        pulled.distribution_digest == distribution_digest,
        "Trellis package distribution digest differs from lock"
    );
    write_cache(&pulled)?;
    Ok(pulled)
}

/// Read and verify an exact locked package without credentials or network access.
pub(crate) fn read_locked(
    name: &str,
    version: &str,
    package_digest: &str,
    distribution_digest: &str,
    manifest_digest: &str,
) -> Result<SourcePackage> {
    let pulled = read_cache(manifest_digest)?;
    validate_package(&pulled, name, version, Some(package_digest))?;
    miette::ensure!(
        pulled.distribution_digest == distribution_digest,
        "Trellis package distribution digest differs from lock"
    );
    Ok(pulled)
}

/// Publish one deterministic source package.
pub(crate) async fn publish(config: &RegistryConfig, package: &SourcePackage) -> Result<String> {
    let reference = Reference::from_str(&format!(
        "{}:{}",
        repository(config, &package.name)?,
        package.version
    ))
    .map_err(|error| miette!(error.to_string()))?;
    let auth = registry_auth(reference.resolve_registry())?;
    let (layer, config_blob, manifest, expected_digest) = image(package)?;
    client(reference.resolve_registry())?
        .push(&reference, &[layer], config_blob, &auth, Some(manifest))
        .await
        .map_err(|error| miette!("failed to publish {}: {error}", package.name))?;
    let pulled = pull_reference(&reference).await?;
    miette::ensure!(
        pulled.manifest_digest == expected_digest,
        "registry stored a different OCI manifest digest"
    );
    validate_package(
        &pulled,
        &package.name,
        &package.version.to_string(),
        Some(&package.package_digest),
    )?;
    write_cache(&pulled)?;
    Ok(pulled.manifest_digest)
}

fn image(package: &SourcePackage) -> Result<(ImageLayer, Config, OciImageManifest, String)> {
    let bytes = archive(&package.files)?;
    miette::ensure!(
        sha256(&bytes) == package.distribution_digest,
        "source package distribution digest does not match its files"
    );
    let config_bytes = config_bytes(package)?;
    let layer = ImageLayer::new(bytes, SOURCE_MEDIA_TYPE.to_owned(), None);
    let config = Config::new(config_bytes, CONFIG_MEDIA_TYPE.to_owned(), None);
    let mut manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    manifest.media_type = Some(OCI_IMAGE_MEDIA_TYPE.to_owned());
    manifest.artifact_type = Some(SOURCE_MEDIA_TYPE.to_owned());
    let bytes = canonical_json(&manifest)?;
    Ok((layer, config, manifest, sha256(&bytes)))
}

async fn pull_reference(reference: &Reference) -> Result<SourcePackage> {
    let client = client(reference.resolve_registry())?;
    let auth = registry_auth(reference.resolve_registry())?;
    let (manifest_bytes, manifest_digest) = client
        .pull_manifest_raw(reference, &auth, &[OCI_IMAGE_MEDIA_TYPE])
        .await
        .map_err(|error| miette!("failed to pull {reference}: {error}"))?;
    miette::ensure!(
        manifest_bytes.len() <= 64 * 1024,
        "Trellis package manifest is too large"
    );
    miette::ensure!(
        sha256(&manifest_bytes) == manifest_digest,
        "registry returned a manifest with the wrong digest"
    );
    let manifest: OciImageManifest = serde_json::from_slice(&manifest_bytes)
        .into_diagnostic()
        .wrap_err("invalid OCI image manifest")?;
    validate_manifest(&manifest)?;
    let mut config_bytes = Vec::new();
    client
        .pull_blob(reference, &manifest.config, &mut config_bytes)
        .await
        .map_err(|error| miette!("failed to pull Trellis package config: {error}"))?;
    miette::ensure!(
        i64::try_from(config_bytes.len()).ok() == Some(manifest.config.size),
        "Trellis package config size mismatch"
    );
    miette::ensure!(
        sha256(&config_bytes) == manifest.config.digest,
        "Trellis package config digest mismatch"
    );
    let config: PackageConfig = serde_json::from_slice(&config_bytes)
        .into_diagnostic()
        .wrap_err("invalid Trellis package config")?;
    miette::ensure!(
        config.format == "trellis.package.source.v1",
        "unsupported Trellis package config format"
    );
    miette::ensure!(
        config_bytes == canonical_json(&config)?,
        "Trellis package config is not canonical JSON"
    );
    let mut bytes = Vec::new();
    client
        .pull_blob(reference, &manifest.layers[0], &mut bytes)
        .await
        .map_err(|error| miette!("failed to pull Trellis source package: {error}"))?;
    miette::ensure!(
        i64::try_from(bytes.len()).ok() == Some(manifest.layers[0].size),
        "Trellis source-package layer size mismatch"
    );
    miette::ensure!(
        sha256(&bytes) == manifest.layers[0].digest,
        "Trellis source-package layer digest mismatch"
    );
    let files = extract_archive(&bytes)?;
    Ok(SourcePackage {
        name: config.name,
        version: Version::parse(&config.version).into_diagnostic()?,
        package_digest: config.package_digest,
        distribution_digest: sha256(&bytes),
        files,
        manifest_digest,
    })
}

fn validate_manifest(manifest: &OciImageManifest) -> Result<()> {
    miette::ensure!(
        manifest.schema_version == 2
            && manifest.media_type.as_deref() == Some(OCI_IMAGE_MEDIA_TYPE)
            && manifest.artifact_type.as_deref() == Some(SOURCE_MEDIA_TYPE)
            && manifest.config.media_type == CONFIG_MEDIA_TYPE
            && manifest.config.size >= 0
            && manifest.config.size <= 4 * 1024
            && manifest.layers.len() == 1
            && manifest.layers[0].media_type == SOURCE_MEDIA_TYPE
            && manifest.layers[0].size >= 0
            && manifest.layers[0].size
                <= i64::try_from(MAX_ARCHIVE_BYTES)
                    .expect("package bounds fit OCI descriptor size"),
        "OCI image is not a Trellis source package v1 manifest"
    );
    Ok(())
}

fn validate_package(
    package: &SourcePackage,
    name: &str,
    version: &str,
    digest: Option<&str>,
) -> Result<()> {
    miette::ensure!(
        package.name == name,
        "remote package name differs from request"
    );
    miette::ensure!(
        package.version.to_string() == version,
        "remote package version differs from request"
    );
    validate_files(&package.files)?;
    let manifest = package_manifest(&package.files)?;
    miette::ensure!(
        manifest.package.name == package.name && manifest.package.version == package.version,
        "source package identity differs from trellis.toml"
    );
    validate_package_digest(&package.package_digest)?;
    let actual = sha256(&archive(&package.files)?);
    miette::ensure!(
        actual == package.distribution_digest,
        "Trellis source package distribution digest mismatch"
    );
    miette::ensure!(
        digest.is_none_or(|expected| expected == package.package_digest),
        "Trellis source package digest differs from lock"
    );
    Ok(())
}

fn config_bytes(package: &SourcePackage) -> Result<Vec<u8>> {
    canonical_json(&PackageConfig {
        format: "trellis.package.source.v1".to_owned(),
        name: package.name.clone(),
        version: package.version.to_string(),
        package_digest: package.package_digest.clone(),
    })
}

fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(
        trellis_protocol::canonicalize_json(&serde_json::to_value(value).into_diagnostic()?)
            .map_err(|error| miette!(error.to_string()))?
            .into_bytes(),
    )
}

fn archive(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<Vec<u8>> {
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

fn extract_archive(bytes: &[u8]) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
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

fn validate_files(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
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

fn frozen_resolution(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<FrozenResolution> {
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

fn package_manifest(
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<trellis_idl::project::PackageManifest> {
    let manifest: trellis_idl::project::PackageManifest =
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

fn registry_auth(host: &str) -> Result<RegistryAuth> {
    let config = std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".docker")))
        .map(|directory| directory.join("config.json"));
    if config.as_ref().is_none_or(|path| !path.exists()) {
        return Ok(RegistryAuth::Anonymous);
    }
    credential_auth(docker_credential::get_credential(host), host)
}

fn credential_auth(
    credential: std::result::Result<DockerCredential, CredentialRetrievalError>,
    host: &str,
) -> Result<RegistryAuth> {
    match credential {
        Ok(DockerCredential::UsernamePassword(username, password)) => {
            Ok(RegistryAuth::Basic(username, password))
        }
        Ok(DockerCredential::IdentityToken(token)) => Ok(RegistryAuth::Bearer(token)),
        Err(
            CredentialRetrievalError::ConfigNotFound
            | CredentialRetrievalError::NoCredentialConfigured,
        ) => Ok(RegistryAuth::Anonymous),
        Err(CredentialRetrievalError::HelperFailure { .. }) => Err(miette!(
            "configured Docker credential helper failed for {host}"
        )),
        Err(CredentialRetrievalError::HelperCommunicationError) => Err(miette!(
            "failed to start the configured Docker credential helper for {host}"
        )),
        Err(_) => Err(miette!(
            "Docker credential configuration for {host} is malformed"
        )),
    }
}

fn client(host: &str) -> Result<Client> {
    let loopback = host.starts_with("localhost:") || host.starts_with("127.0.0.1:");
    Client::try_from(ClientConfig {
        protocol: if loopback {
            ClientProtocol::HttpsExcept(vec![host.to_owned()])
        } else {
            ClientProtocol::Https
        },
        ..ClientConfig::default()
    })
    .map_err(|error| miette!("failed to configure OCI client: {error}"))
}

fn missing_repository(error: &OciDistributionError) -> bool {
    match error {
        OciDistributionError::RegistryError { envelope, .. } => envelope
            .errors
            .iter()
            .all(|error| error.code == OciErrorCode::NameUnknown),
        OciDistributionError::ServerError { code: 404, .. } => true,
        _ => false,
    }
}

fn cache_root() -> PathBuf {
    std::env::var_os("TRELLIS_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME").map(|path| PathBuf::from(path).join("trellis"))
        })
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".cache/trellis")))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("trellis-{}", std::process::id())))
        .join("oci/source-v1")
}

fn cache_entry(digest: &str) -> Result<PathBuf> {
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| miette!("invalid OCI manifest digest '{digest}'"))?;
    Ok(cache_root().join("sha256").join(hex))
}

fn read_cache(digest: &str) -> Result<SourcePackage> {
    let entry = cache_entry(digest)?;
    let manifest_bytes = fs::read(entry.join("manifest.json")).into_diagnostic()?;
    miette::ensure!(
        sha256(&manifest_bytes) == digest,
        "cached OCI manifest digest mismatch"
    );
    let manifest: OciImageManifest = serde_json::from_slice(&manifest_bytes).into_diagnostic()?;
    validate_manifest(&manifest)?;
    let config_bytes = fs::read(entry.join("config.json")).into_diagnostic()?;
    miette::ensure!(
        i64::try_from(config_bytes.len()).ok() == Some(manifest.config.size),
        "cached package config size mismatch"
    );
    miette::ensure!(
        sha256(&config_bytes) == manifest.config.digest,
        "cached package config digest mismatch"
    );
    let config: PackageConfig = serde_json::from_slice(&config_bytes).into_diagnostic()?;
    miette::ensure!(
        config.format == "trellis.package.source.v1",
        "unsupported cached Trellis package config format"
    );
    miette::ensure!(
        config_bytes == canonical_json(&config)?,
        "cached package config is not canonical JSON"
    );
    let bytes = fs::read(entry.join("source.tar")).into_diagnostic()?;
    miette::ensure!(
        i64::try_from(bytes.len()).ok() == Some(manifest.layers[0].size),
        "cached source-package layer size mismatch"
    );
    miette::ensure!(
        sha256(&bytes) == manifest.layers[0].digest,
        "cached source-package layer digest mismatch"
    );
    Ok(SourcePackage {
        name: config.name,
        version: Version::parse(&config.version).into_diagnostic()?,
        package_digest: config.package_digest,
        distribution_digest: sha256(&bytes),
        files: extract_archive(&bytes)?,
        manifest_digest: digest.to_owned(),
    })
}

fn write_cache(package: &SourcePackage) -> Result<()> {
    let destination = cache_entry(&package.manifest_digest)?;
    if destination.is_dir() {
        if read_cache(&package.manifest_digest).is_ok() {
            return Ok(());
        }
        fs::remove_dir_all(&destination).into_diagnostic()?;
    }
    let bytes = archive(&package.files)?;
    let config_bytes = config_bytes(package)?;
    let (_, _, manifest, digest) = image(package)?;
    miette::ensure!(
        digest == package.manifest_digest,
        "package manifest digest mismatch"
    );
    let manifest_bytes = canonical_json(&manifest)?;
    let parent = destination
        .parent()
        .ok_or_else(|| miette!("invalid cache path"))?;
    fs::create_dir_all(parent).into_diagnostic()?;
    let staging = tempfile::Builder::new()
        .prefix(".pull-")
        .tempdir_in(parent)
        .into_diagnostic()?;
    for (name, contents) in [
        ("source.tar", bytes.as_slice()),
        ("config.json", config_bytes.as_slice()),
        ("manifest.json", manifest_bytes.as_slice()),
    ] {
        let path = staging.path().join(name);
        fs::write(&path, contents).into_diagnostic()?;
        fs::File::open(path)
            .into_diagnostic()?
            .sync_all()
            .into_diagnostic()?;
    }
    if let Err(error) = fs::rename(staging.path(), &destination) {
        if !destination.is_dir() {
            return Err(error).into_diagnostic();
        }
    }
    read_cache(&package.manifest_digest)?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold("sha256:".to_owned(), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

fn validate_package_digest(value: &str) -> Result<()> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| miette!("package digest must be a base64url SHA-256 digest"))?;
    miette::ensure!(
        bytes.len() == 32,
        "package digest must be a base64url SHA-256 digest"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> BTreeMap<PathBuf, Vec<u8>> {
        BTreeMap::from([
            (PathBuf::from("main.trellis"), b"api test@v1 {}\n".to_vec()),
            (
                PathBuf::from("trellis.toml"),
                b"[package]\nname='acme-orders'\nversion='1.2.3'\n[sources]\nmain='main.trellis'\n"
                    .to_vec(),
            ),
        ])
    }

    fn archived_files() -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = files();
        files.insert(
            RESOLUTION_PATH.into(),
            canonical_json(&FrozenResolution {
                format: "trellis.package.resolution.v1".into(),
                dependencies: Vec::new(),
                packages: Vec::new(),
            })
            .unwrap(),
        );
        files
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

    #[test]
    fn package_config_has_required_v1_identity() {
        let package = source_package(
            "acme-orders",
            &Version::parse("1.2.3").unwrap(),
            &URL_SAFE_NO_PAD.encode([0_u8; 32]),
            files(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&config_bytes(&package).unwrap()).unwrap();
        assert_eq!(value["format"], "trellis.package.source.v1");
        assert_eq!(value["name"], "acme-orders");
        assert_eq!(value["version"], "1.2.3");
        assert_eq!(value["packageDigest"], package.package_digest);
        let (_, _, manifest, manifest_digest) = image(&package).unwrap();
        assert_eq!(manifest.layers[0].digest, package.distribution_digest);
        assert_ne!(package.package_digest, package.distribution_digest);
        assert_ne!(manifest_digest, package.distribution_digest);
        let mut invalid_manifest = manifest;
        invalid_manifest.layers[0].size = -1;
        assert!(validate_manifest(&invalid_manifest).is_err());
        let lock = package
            .lock(LockedSource::Path { path: ".".into() })
            .unwrap();
        assert_eq!(lock.format, 2);
        assert_eq!(lock.root, "acme-orders");
        assert_eq!(lock.packages.len(), 1);
    }

    #[tokio::test]
    async fn exact_cache_revalidates_all_content() {
        let _guard = TEST_ENV_LOCK.lock().await;
        let cache = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("TRELLIS_CACHE", cache.path()) };
        let mut package = source_package(
            "acme-orders",
            &Version::parse("1.2.3").unwrap(),
            &URL_SAFE_NO_PAD.encode([0_u8; 32]),
            files(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        package.manifest_digest = image(&package).unwrap().3;
        write_cache(&package).unwrap();
        assert_eq!(
            cache_entry(&package.manifest_digest).unwrap(),
            cache
                .path()
                .join("oci/source-v1/sha256")
                .join(package.manifest_digest.strip_prefix("sha256:").unwrap())
        );
        assert_eq!(
            read_locked(
                &package.name,
                &package.version.to_string(),
                &package.package_digest,
                &package.distribution_digest,
                &package.manifest_digest,
            )
            .unwrap()
            .files,
            package.files
        );
        fs::write(
            cache_entry(&package.manifest_digest)
                .unwrap()
                .join("source.tar"),
            b"corrupt",
        )
        .unwrap();
        assert!(read_locked(
            &package.name,
            &package.version.to_string(),
            &package.package_digest,
            &package.distribution_digest,
            &package.manifest_digest,
        )
        .is_err());
        unsafe { std::env::remove_var("TRELLIS_CACHE") };
    }

    #[test]
    fn maps_docker_credentials_without_exposing_secrets() {
        assert_eq!(
            credential_auth(
                Ok(DockerCredential::UsernamePassword(
                    "user".into(),
                    "secret".into()
                )),
                "registry.example"
            )
            .unwrap(),
            RegistryAuth::Basic("user".into(), "secret".into())
        );
        assert_eq!(
            credential_auth(
                Ok(DockerCredential::IdentityToken("token".into())),
                "registry.example"
            )
            .unwrap(),
            RegistryAuth::Bearer("token".into())
        );
        let error = credential_auth(
            Err(CredentialRetrievalError::HelperFailure {
                helper: "test".into(),
                stdout: "secret-token".into(),
                stderr: "secret-password".into(),
            }),
            "registry.example",
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains("secret"));
    }
}
