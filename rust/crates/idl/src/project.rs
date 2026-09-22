//! Trellis source-package manifest, lock, and explicit source loading.
use crate::semantic::SourceUnit;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

/// A format-2 Trellis source-package manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    /// Logical package identity and version.
    pub package: PackageMetadata,
    /// Explicit source aliases and package-relative files.
    pub sources: BTreeMap<String, String>,
    /// Direct dependencies keyed by source import alias.
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
    /// Generated package outputs.
    #[serde(default)]
    pub generate: GenerateConfig,
    /// Registry used when tooling does not name one.
    #[serde(rename = "default-registry", skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    /// Named registry addresses retained from the existing project configuration.
    #[serde(default)]
    pub registries: BTreeMap<String, RegistryConfig>,
}

/// Logical package metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    /// Logical package identity.
    pub name: String,
    /// Authored package release version.
    pub version: Version,
}

/// One direct source-package dependency.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// Logical package identity.
    pub package: String,
    /// Registry version requirement; absent for local paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Local package root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Registry acquisition address or configured registry name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
}

/// One named registry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryConfig {
    /// OCI registry/repository prefix.
    pub prefix: String,
}

/// Configured generated package destinations.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateConfig {
    /// Optional Rust package target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rust: Option<GenerateTarget>,
    /// Optional TypeScript package target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typescript: Option<GenerateTarget>,
}

/// One generated language package target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateTarget {
    /// Package-relative output directory.
    pub output: String,
    /// Runtime dependency overrides used for repository-local development.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
}

impl PackageManifest {
    /// Validate package identity, source aliases, dependencies, and outputs.
    pub fn validate(&self) -> Result<()> {
        validate_package_name(&self.package.name)?;
        if self.sources.is_empty() {
            return Err(miette!("manifest must list at least one source"));
        }
        let mut files = BTreeSet::new();
        for (alias, path) in &self.sources {
            validate_alias(alias)?;
            if self.dependencies.contains_key(alias) {
                return Err(miette!("source and dependency alias '{alias}' collide"));
            }
            let path = Path::new(path);
            if !package_relative(path) {
                return Err(miette!("source '{alias}' must be a relative path"));
            }
            if !files.insert(path) {
                return Err(miette!(
                    "source file '{}' is listed more than once",
                    path.display()
                ));
            }
        }
        let mut packages = BTreeSet::new();
        for (alias, dependency) in &self.dependencies {
            validate_alias(alias)?;
            validate_package_name(&dependency.package)?;
            if dependency.package == self.package.name {
                return Err(miette!("package cannot depend on itself"));
            }
            if !packages.insert(&dependency.package) {
                return Err(miette!(
                    "package '{}' is present under more than one dependency alias",
                    dependency.package
                ));
            }
            match (&dependency.path, &dependency.version, &dependency.registry) {
                (Some(path), None, None) if !path.is_empty() && !Path::new(path).is_absolute() => {}
                (None, Some(requirement), registry)
                    if match registry {
                        Some(value) => !value.is_empty(),
                        None => self.default_registry.is_some(),
                    } =>
                {
                    VersionReq::parse(requirement).map_err(|error| miette!("invalid version requirement for '{alias}': {error}"))?;
                }
                _ => return Err(miette!("dependency '{alias}' must use either {{ package, path }} or {{ package, version, registry }}")),
            }
        }
        if let Some(default) = &self.default_registry {
            if !self.registries.contains_key(default) {
                return Err(miette!("default registry '{default}' is not configured"));
            }
        }
        for (name, registry) in &self.registries {
            validate_alias(name)?;
            if registry.prefix.is_empty() || registry.prefix.chars().any(char::is_whitespace) {
                return Err(miette!("registry '{name}' has an invalid prefix"));
            }
        }
        for target in [
            self.generate.rust.as_ref(),
            self.generate.typescript.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !package_relative(Path::new(&target.output)) {
                return Err(miette!("generated output must be a package-relative path"));
            }
        }
        Ok(())
    }
}

/// Format-2 exact source-package resolution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PackageLock {
    /// Lock format, always `2`.
    pub format: u32,
    /// Root package identity.
    pub root: String,
    /// Complete package closure sorted by identity.
    pub packages: Vec<LockedPackage>,
}

/// One exact package in a lock closure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LockedPackage {
    /// Logical package identity.
    pub name: String,
    /// Exact package version.
    pub version: Version,
    /// Semantic package digest.
    pub digest: String,
    /// OCI source-distribution digest.
    pub distribution_digest: String,
    /// Exact direct dependency edges.
    #[serde(default)]
    pub dependencies: Vec<LockedDependency>,
    /// Acquisition location.
    pub source: LockedSource,
}

/// One exact direct edge in a locked package.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LockedDependency {
    /// Logical dependency identity.
    pub name: String,
    /// Exact dependency version.
    pub version: Version,
    /// Exact semantic dependency digest.
    pub digest: String,
}

/// Exact acquisition location, distinct from logical identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum LockedSource {
    /// Local development package root.
    Path {
        /// Lock-root-relative package path.
        path: String,
    },
    /// Immutable cached OCI source package.
    Registry {
        /// Registry configuration name or address.
        registry: String,
        /// Exact OCI manifest digest.
        oci_digest: String,
    },
}

impl PackageLock {
    /// Validate format, deterministic ordering, exact edges, and digest syntax.
    pub fn validate(&self) -> Result<()> {
        if self.format != 2 {
            return Err(miette!("lock format must equal 2"));
        }
        validate_package_name(&self.root)?;
        let mut previous = None;
        let identities = self
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect::<BTreeSet<_>>();
        let by_name = self
            .packages
            .iter()
            .map(|package| (package.name.as_str(), package))
            .collect::<BTreeMap<_, _>>();
        for package in &self.packages {
            validate_package_name(&package.name)?;
            if previous.is_some_and(|name| name >= package.name.as_str()) {
                return Err(miette!(
                    "locked packages must be uniquely sorted by identity"
                ));
            }
            previous = Some(package.name.as_str());
            validate_digest("package digest", &package.digest)?;
            validate_distribution_digest(&package.distribution_digest)?;
            match &package.source {
                LockedSource::Path { path }
                    if !path.is_empty() && !Path::new(path).is_absolute() => {}
                LockedSource::Registry {
                    registry,
                    oci_digest,
                } if !registry.is_empty() => validate_distribution_digest(oci_digest)?,
                _ => return Err(miette!("locked package has an invalid acquisition source")),
            }
            if !package
                .dependencies
                .windows(2)
                .all(|pair| pair[0].name < pair[1].name)
            {
                return Err(miette!(
                    "dependencies of '{}' must be uniquely sorted",
                    package.name
                ));
            }
            for dependency in &package.dependencies {
                validate_package_name(&dependency.name)?;
                validate_digest("dependency digest", &dependency.digest)?;
                if !identities.contains(dependency.name.as_str()) {
                    return Err(miette!(
                        "locked dependency '{}' is missing from the closure",
                        dependency.name
                    ));
                }
                let locked = by_name[dependency.name.as_str()];
                if dependency.version != locked.version || dependency.digest != locked.digest {
                    return Err(miette!(
                        "locked edge '{} -> {}' disagrees with the target package",
                        package.name,
                        dependency.name
                    ));
                }
            }
            match &package.source {
                LockedSource::Path { path }
                    if !path.is_empty() && !Path::new(path).is_absolute() => {}
                LockedSource::Registry {
                    registry,
                    oci_digest,
                } if !registry.is_empty() => validate_distribution_digest(oci_digest)?,
                _ => {
                    return Err(miette!(
                        "locked package '{}' has an invalid acquisition source",
                        package.name
                    ))
                }
            }
        }
        if !identities.contains(self.root.as_str()) {
            return Err(miette!("root package '{}' is absent from lock", self.root));
        }
        fn visit<'a>(
            name: &'a str,
            packages: &BTreeMap<&'a str, &'a LockedPackage>,
            visiting: &mut BTreeSet<&'a str>,
            complete: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if complete.contains(name) {
                return Ok(());
            }
            if !visiting.insert(name) {
                return Err(miette!("package dependency cycle includes '{name}'"));
            }
            for dependency in &packages[name].dependencies {
                visit(&dependency.name, packages, visiting, complete)?;
            }
            visiting.remove(name);
            complete.insert(name);
            Ok(())
        }
        let mut complete = BTreeSet::new();
        visit(&self.root, &by_name, &mut BTreeSet::new(), &mut complete)?;
        if complete.len() != identities.len() {
            return Err(miette!(
                "lock contains packages outside the root dependency closure"
            ));
        }
        Ok(())
    }
}

/// Read and validate `trellis.toml`.
pub fn read_manifest(path: &Path) -> Result<PackageManifest> {
    let manifest = toml::from_str::<PackageManifest>(&fs::read_to_string(path).into_diagnostic()?)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Read and validate format-2 `trellis.lock`.
pub fn read_lock(path: &Path) -> Result<PackageLock> {
    let lock = toml::from_str::<PackageLock>(&fs::read_to_string(path).into_diagnostic()?)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
    lock.validate()?;
    Ok(lock)
}

/// Load only the source files explicitly listed by a validated manifest.
pub fn load_sources(root: &Path, manifest: &PackageManifest) -> Result<Vec<SourceUnit>> {
    manifest.validate()?;
    let root = root
        .canonicalize()
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to resolve {}", root.display()))?;
    let mut canonical = BTreeSet::<PathBuf>::new();
    manifest
        .sources
        .iter()
        .map(|(alias, relative)| {
            let path = root
                .join(relative)
                .canonicalize()
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to resolve source '{alias}'"))?;
            if !path.starts_with(&root) {
                return Err(miette!("source '{alias}' escapes the package root"));
            }
            if !path.is_file() {
                return Err(miette!("source '{alias}' is not a regular file"));
            }
            if !canonical.insert(path.clone()) {
                return Err(miette!(
                    "source '{alias}' duplicates another canonical file"
                ));
            }
            let source = fs::read_to_string(&path)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to read {}", path.display()))?;
            Ok(SourceUnit {
                alias: alias.clone(),
                path: PathBuf::from(relative),
                source,
            })
        })
        .collect()
}

fn validate_package_name(value: &str) -> Result<()> {
    if value.is_empty()
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(miette!(
            "invalid package name '{value}'; expected [a-z][a-z0-9-]*"
        ));
    }
    Ok(())
}

fn package_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

fn validate_alias(value: &str) -> Result<()> {
    if value.is_empty()
        || !value.as_bytes()[0].is_ascii_alphabetic()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(miette!("invalid alias '{value}'"));
    }
    Ok(())
}

fn validate_digest(name: &str, value: &str) -> Result<()> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    if !matches!(URL_SAFE_NO_PAD.decode(value), Ok(bytes) if bytes.len() == 32) {
        return Err(miette!("{name} must be a base64url SHA-256 digest"));
    }
    Ok(())
}

fn validate_distribution_digest(value: &str) -> Result<()> {
    if !value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err(miette!("distribution digest must be a sha256 OCI digest"));
    }
    Ok(())
}
