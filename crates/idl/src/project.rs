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

/// The name of the built-in package that ships inside `trellis-server`.
///
/// Its authority is the server binary, not a registry, so it has no
/// workspace and its IDs carry none. The name is reserved in every workspace.
pub const BUILTIN_PACKAGE: &str = "trellis";

/// Where the Trellis project publishes the built-in package. It is a download
/// source only and never part of the package's identity.
pub const BUILTIN_PACKAGE_WORKSPACE: &str = "ghcr.io/oats-center";

/// The full identity of package `name` in `workspace`: `<workspace>/<name>`,
/// with [`trellis_protocol::LOCAL_WORKSPACE`] for a package that declares none,
/// and the bare name for the built-in package.
pub fn package_identity(workspace: Option<&str>, name: &str) -> String {
    if name == BUILTIN_PACKAGE && workspace.is_none() {
        return name.to_owned();
    }
    format!(
        "{}/{name}",
        workspace.unwrap_or(trellis_protocol::LOCAL_WORKSPACE)
    )
}

/// Split and validate a full package identity into its workspace and name.
///
/// The built-in package's identity is its bare name and has no workspace.
///
/// # Errors
///
/// Returns an error when the identity names no valid workspace and package,
/// or names the reserved `trellis` package in a workspace.
pub fn split_package_identity(identity: &str) -> Result<(Option<&str>, &str)> {
    let (workspace, name) = trellis_protocol::split_workspace(identity);
    validate_package_name(name)?;
    match workspace {
        None if name == BUILTIN_PACKAGE => Ok((None, name)),
        None => Err(miette!(
            "package identity '{identity}' names no workspace; expected <workspace>/<name>"
        )),
        Some(_) if name == BUILTIN_PACKAGE => Err(miette!(
            "the package name 'trellis' is reserved for the built-in package"
        )),
        Some(workspace) => {
            trellis_protocol::validate_workspace(workspace).map_err(|error| {
                miette!("package identity '{identity}' has an invalid workspace: {error}")
            })?;
            Ok((Some(workspace), name))
        }
    }
}

/// The registry repository a published package with this identity lives at:
/// the identity itself, or the built-in package's download location.
///
/// # Errors
///
/// Returns an error for a `localhost` package, which names no shared location.
pub fn package_repository(identity: &str) -> Result<String> {
    match split_package_identity(identity)? {
        (None, name) => Ok(format!("{BUILTIN_PACKAGE_WORKSPACE}/{name}")),
        (Some(trellis_protocol::LOCAL_WORKSPACE), name) => Err(miette!(
            "package '{name}' declares no workspace, so it cannot come from a registry; \
            add `workspace` to its trellis.toml"
        )),
        (Some(_), _) => Ok(identity.to_owned()),
    }
}

/// The workspace a published package with this identity is pulled from.
///
/// # Errors
///
/// Returns an error for a `localhost` package, which names no shared location.
pub fn package_source_workspace(identity: &str) -> Result<String> {
    Ok(
        trellis_protocol::split_workspace(&package_repository(identity)?)
            .0
            .unwrap_or_default()
            .to_owned(),
    )
}

/// A Trellis source-package manifest.
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
}

/// Logical package metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    /// Package name, unique within its workspace.
    pub name: String,
    /// The registry location the package is published from, such as
    /// `ghcr.io/acme`; absent for an unpublished package, which gets
    /// `localhost`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Authored package release version.
    pub version: Version,
}

/// One direct source-package dependency.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// Package name.
    pub package: String,
    /// Workspace of a registry dependency; absent for a path dependency,
    /// whose identity comes from its own `trellis.toml`, and for the built-in
    /// `trellis` package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Registry version requirement; absent for local paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Local package root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Dependency {
    /// The full identity of a registry dependency; `None` for a path
    /// dependency, whose identity its own manifest declares.
    pub fn identity(&self) -> Option<String> {
        self.path
            .is_none()
            .then(|| package_identity(self.workspace.as_deref(), &self.package))
    }
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
    /// The package's full identity, `<workspace>/<name>`.
    pub fn identity(&self) -> String {
        package_identity(self.package.workspace.as_deref(), &self.package.name)
    }

    /// Validate package identity, source aliases, dependencies, and outputs.
    pub fn validate(&self) -> Result<()> {
        validate_package_name(&self.package.name)?;
        validate_declared_workspace(&self.package.name, self.package.workspace.as_deref())?;
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
        let identity = self.identity();
        let mut packages = BTreeSet::new();
        for (alias, dependency) in &self.dependencies {
            validate_alias(alias)?;
            validate_package_name(&dependency.package)?;
            match (&dependency.path, &dependency.version) {
                (Some(path), None)
                    if !path.is_empty()
                        && !Path::new(path).is_absolute()
                        && dependency.workspace.is_none() => {}
                (None, Some(requirement)) => {
                    validate_declared_workspace(
                        &dependency.package,
                        dependency.workspace.as_deref(),
                    )?;
                    if dependency.workspace.is_none() && dependency.package != BUILTIN_PACKAGE {
                        return Err(miette!(
                            "registry dependency '{alias}' must name the workspace it is \
                            published from, such as workspace = \"ghcr.io/acme\""
                        ));
                    }
                    VersionReq::parse(requirement).map_err(|error| {
                        miette!("invalid version requirement for '{alias}': {error}")
                    })?;
                }
                _ => {
                    return Err(miette!(
                        "dependency '{alias}' must use either {{ package, path }} or {{ package, \
                        workspace, version }}"
                    ))
                }
            }
            if let Some(dependency) = dependency.identity() {
                if dependency == identity {
                    return Err(miette!("package cannot depend on itself"));
                }
                if !packages.insert(dependency.clone()) {
                    return Err(miette!(
                        "package '{dependency}' is present under more than one dependency alias"
                    ));
                }
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

/// Format-3 exact source-package resolution, keyed by full package identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PackageLock {
    /// Lock format, always `3`.
    pub format: u32,
    /// Root package's full identity.
    pub root: String,
    /// Complete package closure sorted by full identity.
    pub packages: Vec<LockedPackage>,
}

/// One exact package in a lock closure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LockedPackage {
    /// Full package identity, `<workspace>/<name>`.
    pub id: String,
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
    /// Full dependency identity.
    pub id: String,
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
        /// The workspace the package was pulled from.
        workspace: String,
        /// Exact OCI manifest digest.
        oci_digest: String,
    },
}

impl PackageLock {
    /// Validate format, deterministic ordering, exact edges, and digest syntax.
    pub fn validate(&self) -> Result<()> {
        if self.format != 3 {
            return Err(miette!(
                "lock format must equal 3; run `trellis update` to write a new trellis.lock"
            ));
        }
        split_package_identity(&self.root)?;
        let mut previous = None;
        let by_id = self
            .packages
            .iter()
            .map(|package| (package.id.as_str(), package))
            .collect::<BTreeMap<_, _>>();
        for package in &self.packages {
            split_package_identity(&package.id)?;
            if previous.is_some_and(|id| id >= package.id.as_str()) {
                return Err(miette!(
                    "locked packages must be uniquely sorted by identity"
                ));
            }
            previous = Some(package.id.as_str());
            validate_digest("package digest", &package.digest)?;
            validate_distribution_digest(&package.distribution_digest)?;
            if !package
                .dependencies
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
            {
                return Err(miette!(
                    "dependencies of '{}' must be uniquely sorted",
                    package.id
                ));
            }
            for dependency in &package.dependencies {
                split_package_identity(&dependency.id)?;
                validate_digest("dependency digest", &dependency.digest)?;
                let Some(locked) = by_id.get(dependency.id.as_str()) else {
                    return Err(miette!(
                        "locked dependency '{}' is missing from the closure",
                        dependency.id
                    ));
                };
                if dependency.version != locked.version || dependency.digest != locked.digest {
                    return Err(miette!(
                        "locked edge '{} -> {}' disagrees with the target package",
                        package.id,
                        dependency.id
                    ));
                }
            }
            match &package.source {
                LockedSource::Path { path }
                    if !path.is_empty() && !Path::new(path).is_absolute() => {}
                // A registry package comes from its own workspace; there are
                // no mirrors.
                LockedSource::Registry {
                    workspace,
                    oci_digest,
                } if package_source_workspace(&package.id)
                    .is_ok_and(|expected| expected == *workspace) =>
                {
                    validate_distribution_digest(oci_digest)?
                }
                _ => {
                    return Err(miette!(
                        "locked package '{}' has an invalid acquisition source",
                        package.id
                    ))
                }
            }
        }
        if !by_id.contains_key(self.root.as_str()) {
            return Err(miette!("root package '{}' is absent from lock", self.root));
        }
        fn visit<'a>(
            id: &'a str,
            packages: &BTreeMap<&'a str, &'a LockedPackage>,
            visiting: &mut BTreeSet<&'a str>,
            complete: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if complete.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err(miette!("package dependency cycle includes '{id}'"));
            }
            for dependency in &packages[id].dependencies {
                visit(&dependency.id, packages, visiting, complete)?;
            }
            visiting.remove(id);
            complete.insert(id);
            Ok(())
        }
        let mut complete = BTreeSet::new();
        visit(&self.root, &by_id, &mut BTreeSet::new(), &mut complete)?;
        if complete.len() != by_id.len() {
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

/// Read and validate format-3 `trellis.lock`.
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

/// A declared workspace must be canonical, and the built-in package's name is
/// reserved in every workspace. `localhost` is never written: a package
/// without a workspace already has it.
fn validate_declared_workspace(name: &str, workspace: Option<&str>) -> Result<()> {
    let Some(workspace) = workspace else {
        return Ok(());
    };
    if name == BUILTIN_PACKAGE {
        return Err(miette!(
            "the package name 'trellis' is reserved for the built-in package"
        ));
    }
    if workspace == trellis_protocol::LOCAL_WORKSPACE {
        return Err(miette!(
            "leave out `workspace` for an unpublished package; `localhost` is what it gets"
        ));
    }
    trellis_protocol::validate_workspace(workspace)
        .map_err(|error| miette!("invalid workspace '{workspace}': {error}"))
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
