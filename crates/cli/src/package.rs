//! Source-package dependency resolution, installation, and publication.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};

use miette::{miette, IntoDiagnostic, Result, WrapErr};
use semver::{Version, VersionReq};
use serde::Serialize;

use crate::{
    cli::{AddArgs, OutputFormat, ProjectRootArgs, PublishArgs, RmArgs, UpdateArgs},
    oci, output,
    project::{
        read_lock, read_manifest, restore_project_files, write_lock, write_manifest_and_lock,
        LockedDependency, LockedPackage, LockedSource, PackageDependency, ProjectLock,
        ProjectManifest, RegistryConfig,
    },
};

#[derive(Debug, Serialize)]
struct PackageResult {
    installed_packages: usize,
    changed_dependencies: usize,
    generated_projects: usize,
}

struct Resolution {
    graph: trellis_idl::PackageGraph,
    packages: BTreeMap<String, LockedPackage>,
}

pub async fn add(format: OutputFormat, args: &AddArgs) -> Result<()> {
    let root = canonical_root(&args.project.root)?;
    let manifest_path = root.join("trellis.toml");
    let previous_manifest = fs::read(&manifest_path).into_diagnostic()?;
    let previous_lock = read_optional(&root.join("trellis.lock"))?;
    let mut manifest = read_manifest(&manifest_path)?;
    let source = Path::new(&args.source);
    let dependency = if root.join(source).is_dir() {
        miette::ensure!(
            !source.is_absolute(),
            "package paths must be relative to the project root"
        );
        let child = read_manifest(&root.join(source).join("trellis.toml"))?;
        PackageDependency {
            package: child.package.name,
            version: None,
            path: Some(args.source.clone()),
            registry: None,
        }
    } else {
        let registry = args
            .registry
            .clone()
            .or_else(|| manifest.default_registry.clone())
            .ok_or_else(|| miette!("remote add requires --registry or default-registry"))?;
        registry_config(&manifest, &registry)?;
        let requested = args
            .version
            .as_deref()
            .map(VersionReq::parse)
            .transpose()
            .map_err(|error| miette!("invalid version requirement: {error}"))?;
        let release = select_remote_version(
            registry_config(&manifest, &registry)?,
            &args.source,
            requested.as_ref().unwrap_or(&VersionReq::STAR),
        )
        .await?;
        let requirement = args
            .version
            .clone()
            .unwrap_or_else(|| format!("^{release}"));
        PackageDependency {
            package: args.source.clone(),
            version: Some(requirement),
            path: None,
            registry: Some(registry),
        }
    };
    let alias = dependency.package.replace('-', "_");
    miette::ensure!(
        !manifest.sources.contains_key(&alias),
        "dependency alias '{alias}' collides with a source alias"
    );
    manifest.dependencies.insert(alias.clone(), dependency);
    let edited = edit_manifest_dependency(
        &previous_manifest,
        &alias,
        manifest.dependencies.get(&alias),
    )?;
    let lock = resolve_lock(&root, &manifest).await?;
    let result = commit_and_install(
        &root,
        &manifest,
        &lock,
        &previous_manifest,
        previous_lock.as_deref(),
        Some(&edited),
    )
    .await?;
    print_result(
        format,
        &result,
        Some(format!("Added {}", manifest.dependencies[&alias].package)),
    )
}

pub async fn remove(format: OutputFormat, args: &RmArgs) -> Result<()> {
    let root = canonical_root(&args.project.root)?;
    let previous_manifest = fs::read(root.join("trellis.toml")).into_diagnostic()?;
    let previous_lock = read_optional(&root.join("trellis.lock"))?;
    let mut manifest = read_manifest(&root.join("trellis.toml"))?;
    let alias = manifest
        .dependencies
        .iter()
        .find_map(|(alias, dependency)| {
            (dependency.package == args.api_id || alias == &args.api_id).then(|| alias.clone())
        })
        .ok_or_else(|| miette!("package '{}' is not in trellis.toml", args.api_id))?;
    manifest.dependencies.remove(&alias);
    let edited = edit_manifest_dependency(&previous_manifest, &alias, None)?;
    let lock = resolve_lock(&root, &manifest).await?;
    let result = commit_and_install(
        &root,
        &manifest,
        &lock,
        &previous_manifest,
        previous_lock.as_deref(),
        Some(&edited),
    )
    .await?;
    print_result(format, &result, Some(format!("Removed {}", args.api_id)))
}

pub async fn check(format: OutputFormat, args: &ProjectRootArgs) -> Result<()> {
    let root = canonical_root(&args.root)?;
    let manifest = read_manifest(&root.join("trellis.toml"))?;
    let lock = resolve_lock(&root, &manifest).await?;
    let package = lock
        .packages
        .iter()
        .find(|package| package.name == lock.root)
        .expect("resolved lock contains root package");
    if output::is_json(format) {
        output::print_json(&serde_json::json!({
            "package": package.name,
            "packageDigest": package.digest,
            "version": package.version,
        }))
    } else {
        println!("Checked {} {}", package.name, package.version);
        Ok(())
    }
}

pub async fn update(format: OutputFormat, args: &UpdateArgs) -> Result<()> {
    let root = canonical_root(&args.project.root)?;
    let manifest = read_manifest(&root.join("trellis.toml"))?;
    let previous_lock = read_optional(&root.join("trellis.lock"))?;
    let mut resolution_manifest = manifest.clone();
    if let Some(alias) = &args.dependency_alias {
        miette::ensure!(
            manifest.dependencies.contains_key(alias),
            "dependency alias '{alias}' is not in trellis.toml"
        );
        if previous_lock.is_none() {
            return Err(miette!("trellis.lock is missing; run `trellis update`"));
        }
        let current = read_lock(&root.join("trellis.lock"))?;
        for (current_alias, dependency) in &mut resolution_manifest.dependencies {
            if current_alias == alias || dependency.path.is_some() {
                continue;
            }
            let locked = current
                .packages
                .iter()
                .find(|package| package.name == dependency.package)
                .ok_or_else(|| {
                    miette!(
                        "package '{}' is absent from trellis.lock; run `trellis update`",
                        dependency.package
                    )
                })?;
            dependency.version = Some(format!("={}", locked.version));
        }
    }
    let lock = resolve_lock(&root, &resolution_manifest).await?;
    if let (Some(alias), Some(previous_lock)) = (&args.dependency_alias, previous_lock.as_deref()) {
        let previous = toml::from_slice::<ProjectLock>(previous_lock).into_diagnostic()?;
        previous.validate()?;
        let mut pending = manifest
            .dependencies
            .iter()
            .filter(|(current_alias, _)| *current_alias != alias)
            .map(|(_, dependency)| dependency.package.as_str())
            .collect::<Vec<_>>();
        let mut checked = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !checked.insert(name) {
                continue;
            }
            let old = previous
                .packages
                .iter()
                .find(|package| package.name == name)
                .ok_or_else(|| miette!("package '{name}' is absent from trellis.lock"))?;
            let new = lock
                .packages
                .iter()
                .find(|package| package.name == name)
                .ok_or_else(|| {
                    miette!("updating dependency alias '{alias}' changed locked package '{name}'")
                })?;
            miette::ensure!(
                old == new,
                "updating dependency alias '{alias}' changed locked package '{name}'; run `trellis update` to refresh every dependency"
            );
            pending.extend(
                old.dependencies
                    .iter()
                    .map(|dependency| dependency.name.as_str()),
            );
        }
    }
    write_lock(&root.join("trellis.lock"), &lock)?;
    match install_root(&root, &manifest, &lock).await {
        Ok(result) => print_result(format, &result, None),
        Err(error) => {
            restore_project_files(
                &root.join("trellis.toml"),
                &fs::read(root.join("trellis.toml")).into_diagnostic()?,
                &root.join("trellis.lock"),
                previous_lock.as_deref(),
            )?;
            Err(error)
        }
    }
}

pub async fn install(format: OutputFormat, args: &ProjectRootArgs) -> Result<()> {
    let root = canonical_root(&args.root)?;
    let manifest = read_manifest(&root.join("trellis.toml"))?;
    let lock_path = root.join("trellis.lock");
    let created = !lock_path.exists();
    let lock = if created {
        let lock = resolve_lock(&root, &manifest).await?;
        write_lock(&lock_path, &lock)?;
        lock
    } else {
        read_lock(&lock_path)?
    };
    match install_root(&root, &manifest, &lock).await {
        Ok(result) => print_result(format, &result, None),
        Err(error) => {
            if created {
                fs::remove_file(lock_path).into_diagnostic()?;
            }
            Err(error)
        }
    }
}

pub async fn publish(format: OutputFormat, args: &PublishArgs) -> Result<()> {
    let root = canonical_root(&args.project.root)?;
    let manifest = read_manifest(&root.join("trellis.toml"))?;
    let lock = read_lock(&root.join("trellis.lock"))
        .wrap_err("trellis.lock is missing; run `trellis update`")?;
    miette::ensure!(
        manifest
            .dependencies
            .values()
            .all(|dependency| dependency.path.is_none())
            && lock.packages.iter().all(|locked| {
                locked.name == lock.root || matches!(&locked.source, LockedSource::Registry { .. })
            }),
        "publish requires every dependency to be an exact registry package; replace path dependencies first"
    );
    let graph = compile_project(&root, &manifest)?;
    validate_lock(&manifest, &lock, &graph)?;
    let package = package_from_lock(&root, &manifest, graph.root_digest(), &lock)?;
    let registry = args
        .registry
        .as_ref()
        .or(manifest.default_registry.as_ref())
        .ok_or_else(|| miette!("publish requires --registry or default-registry"))?;
    let config = registry_config(&manifest, registry)?;
    let versions = oci::versions(config, &package.name).await?;
    let existing = if versions.binary_search(&package.version).is_ok() {
        let remote = oci::pull_tag(config, &package.name, &package.version).await?;
        miette::ensure!(
            remote.package_digest == package.package_digest
                && remote.distribution_digest == package.distribution_digest,
            "release {} {} already exists with different content",
            package.name,
            package.version
        );
        Some(remote.manifest_digest)
    } else {
        if let Some(previous) = versions.last() {
            miette::ensure!(
                package.version > *previous,
                "release {} must be newer than {previous}",
                package.version
            );
        }
        None
    };
    let changed = existing.is_none();
    let digest = match existing {
        Some(digest) => digest,
        None => oci::publish(config, &package).await?,
    };
    if output::is_json(format) {
        output::print_json(
            &serde_json::json!({"name": package.name, "version": package.version, "digest": digest, "changed": changed}),
        )
    } else {
        println!(
            "{} {} {}",
            if changed {
                "Published"
            } else {
                "Already published"
            },
            package.name,
            package.version
        );
        println!("{}@{digest}", oci::repository(config, &package.name)?);
        Ok(())
    }
}

async fn resolve_lock(root: &Path, manifest: &ProjectManifest) -> Result<ProjectLock> {
    let mut stack = BTreeSet::new();
    let resolution = resolve_package(
        root,
        manifest,
        LockedSource::Path { path: ".".into() },
        &mut stack,
    )
    .await?;
    Ok(ProjectLock {
        format: 2,
        root: manifest.package.name.clone(),
        packages: resolution.packages.into_values().collect(),
    })
}

fn resolve_package<'a>(
    package_root: &'a Path,
    manifest: &'a ProjectManifest,
    source: LockedSource,
    stack: &'a mut BTreeSet<String>,
) -> Pin<Box<dyn Future<Output = Result<Resolution>> + 'a>> {
    Box::pin(async move {
        miette::ensure!(
            stack.insert(manifest.package.name.clone()),
            "package dependency cycle through '{}'",
            manifest.package.name
        );
        let mut supplied = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut direct = Vec::new();
        for (alias, dependency) in &manifest.dependencies {
            let (child_root, child_source, pulled) = if let Some(path) = &dependency.path {
                let child = package_root.join(path).canonicalize().into_diagnostic()?;
                let relative = match &source {
                    LockedSource::Path { path: package_path } => Path::new(package_path)
                        .join(path)
                        .to_string_lossy()
                        .into_owned(),
                    LockedSource::Registry { .. } => {
                        return Err(miette!(
                            "registry package '{}' contains a path dependency",
                            manifest.package.name
                        ));
                    }
                };
                (child, LockedSource::Path { path: relative }, None)
            } else {
                let registry = dependency
                    .registry
                    .as_deref()
                    .expect("validated registry dependency");
                let config = registry_config(manifest, registry)?;
                let requirement =
                    VersionReq::parse(dependency.version.as_deref().expect("validated version"))
                        .into_diagnostic()?;
                let version =
                    select_remote_version(config, &dependency.package, &requirement).await?;
                let package = oci::pull_tag(config, &dependency.package, &version).await?;
                let directory = materialize(&package)?;
                (
                    directory.path().to_path_buf(),
                    LockedSource::Registry {
                        registry: config.prefix.clone(),
                        oci_digest: package.manifest_digest.clone(),
                    },
                    Some((package, directory)),
                )
            };
            let child_manifest = read_manifest(&child_root.join("trellis.toml"))?;
            miette::ensure!(
                child_manifest.package.name == dependency.package,
                "dependency '{alias}' contains package '{}' instead of '{}'",
                child_manifest.package.name,
                dependency.package
            );
            let child = if let Some((pulled, _)) = &pulled {
                let lock = pulled.lock(child_source)?;
                install_locked_packages(&lock).await?;
                let graph = compile_locked(
                    &child_root,
                    &child_root,
                    &child_manifest,
                    &lock,
                    &mut BTreeSet::new(),
                )?;
                validate_lock(&child_manifest, &lock, &graph)?;
                miette::ensure!(
                    pulled.package_digest == graph.root_digest(),
                    "remote package '{}' semantic digest is invalid",
                    dependency.package
                );
                Resolution {
                    graph,
                    packages: lock
                        .packages
                        .into_iter()
                        .map(|package| (package.name.clone(), package))
                        .collect(),
                }
            } else {
                resolve_package(&child_root, &child_manifest, child_source, stack).await?
            };
            direct.push(LockedDependency {
                name: dependency.package.clone(),
                version: child_manifest.package.version.clone(),
                digest: child.graph.root_digest().to_owned(),
            });
            miette::ensure!(
                !child.packages.contains_key(&manifest.package.name),
                "package dependency cycle through '{}'",
                manifest.package.name
            );
            merge_packages(&mut packages, child.packages)?;
            supplied.insert(alias.clone(), child.graph);
            drop(pulled);
        }
        direct.sort();
        let graph = compile_graph(package_root, manifest, supplied)?;
        let source_package = package_from_root(
            package_root,
            manifest,
            graph.root_digest(),
            direct.clone(),
            packages.values().cloned().collect(),
        )?;
        packages.insert(
            manifest.package.name.clone(),
            LockedPackage {
                name: manifest.package.name.clone(),
                version: manifest.package.version.clone(),
                digest: graph.root_digest().to_owned(),
                distribution_digest: source_package.distribution_digest,
                dependencies: direct,
                source,
            },
        );
        stack.remove(&manifest.package.name);
        Ok(Resolution { graph, packages })
    })
}

async fn install_root(
    root: &Path,
    manifest: &ProjectManifest,
    lock: &ProjectLock,
) -> Result<PackageResult> {
    lock.validate()?;
    let changed = install_locked_packages(lock).await?;
    let graph = compile_project(root, manifest)?;
    validate_lock(manifest, lock, &graph)?;
    let generated = crate::generate::generate_compiled(root, manifest, &graph, false)?;
    Ok(PackageResult {
        installed_packages: lock.packages.len().saturating_sub(1),
        changed_dependencies: changed,
        generated_projects: generated,
    })
}

async fn install_locked_packages(lock: &ProjectLock) -> Result<usize> {
    let mut changed = 0;
    for package in &lock.packages {
        let LockedSource::Registry {
            registry,
            oci_digest,
        } = &package.source
        else {
            continue;
        };
        let config = RegistryConfig {
            prefix: registry.clone(),
        };
        if oci::read_locked(
            &package.name,
            &package.version.to_string(),
            &package.digest,
            &package.distribution_digest,
            oci_digest,
        )
        .is_err()
        {
            oci::pull_locked(
                &config,
                &package.name,
                &package.version.to_string(),
                &package.digest,
                &package.distribution_digest,
                oci_digest,
            )
            .await?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Compile the project from local paths and exact validated cache entries only.
pub(crate) fn compile_project(
    root: &Path,
    manifest: &ProjectManifest,
) -> Result<trellis_idl::PackageGraph> {
    let lock = read_lock(&root.join("trellis.lock"))
        .wrap_err("trellis.lock is missing; run `trellis update`")?;
    let graph = compile_locked(root, root, manifest, &lock, &mut BTreeSet::new())?;
    validate_lock(manifest, &lock, &graph)?;
    Ok(graph)
}

fn compile_locked(
    root: &Path,
    package_root: &Path,
    manifest: &ProjectManifest,
    lock: &ProjectLock,
    stack: &mut BTreeSet<String>,
) -> Result<trellis_idl::PackageGraph> {
    miette::ensure!(
        stack.insert(manifest.package.name.clone()),
        "package dependency cycle through '{}'",
        manifest.package.name
    );
    let locked = lock
        .packages
        .iter()
        .find(|package| package.name == manifest.package.name)
        .ok_or_else(|| {
            miette!(
                "package '{}' is absent from trellis.lock",
                manifest.package.name
            )
        })?;
    miette::ensure!(
        manifest.package.version == locked.version,
        "package '{}' version differs from trellis.lock",
        manifest.package.name
    );
    if package_root == root {
        miette::ensure!(
            matches!(&locked.source, LockedSource::Path { path } if path == "."),
            "root package must use the exact project path in trellis.lock"
        );
    }
    let mut supplied = BTreeMap::new();
    let mut exact_dependencies = Vec::new();
    for (alias, dependency) in &manifest.dependencies {
        let child_lock = lock
            .packages
            .iter()
            .find(|package| package.name == dependency.package)
            .ok_or_else(|| {
                miette!(
                    "package '{}' is absent from trellis.lock",
                    dependency.package
                )
            })?;
        match (&dependency.path, &dependency.version, &child_lock.source) {
            (Some(path), None, LockedSource::Path { path: locked_path }) => {
                let declared = package_root.join(path).canonicalize().into_diagnostic()?;
                let acquired = root.join(locked_path).canonicalize().into_diagnostic()?;
                miette::ensure!(
                    declared == acquired,
                    "locked path for dependency '{alias}' differs from trellis.toml"
                );
            }
            (None, Some(requirement), LockedSource::Registry { registry, .. }) => {
                let requirement = VersionReq::parse(requirement).into_diagnostic()?;
                let configured = dependency
                    .registry
                    .as_deref()
                    .or(manifest.default_registry.as_deref())
                    .expect("validated registry dependency");
                miette::ensure!(
                    requirement.matches(&child_lock.version)
                        && registry_config(manifest, configured)?.prefix == *registry,
                    "locked registry release for dependency '{alias}' differs from trellis.toml"
                );
            }
            _ => {
                return Err(miette!(
                    "locked acquisition source for dependency '{alias}' differs from trellis.toml"
                ))
            }
        }
        let (child_root, temporary) = match &child_lock.source {
            LockedSource::Path { path } => {
                (root.join(path).canonicalize().into_diagnostic()?, None)
            }
            LockedSource::Registry { oci_digest, .. } => {
                let package = oci::read_locked(
                    &child_lock.name,
                    &child_lock.version.to_string(),
                    &child_lock.digest,
                    &child_lock.distribution_digest,
                    oci_digest,
                )
                .map_err(|error| {
                    miette!(
                        "cached package '{}' {} is unavailable: {error}; run `trellis install`",
                        child_lock.name,
                        child_lock.version
                    )
                })?;
                let temporary = materialize(&package)?;
                (temporary.path().to_path_buf(), Some(temporary))
            }
        };
        let child_manifest = read_manifest(&child_root.join("trellis.toml"))?;
        miette::ensure!(
            child_manifest.package.name == child_lock.name
                && child_manifest.package.version == child_lock.version,
            "locked identity for dependency '{alias}' differs from its source package"
        );
        let graph = compile_locked(root, &child_root, &child_manifest, lock, stack)?;
        exact_dependencies.push(LockedDependency {
            name: child_lock.name.clone(),
            version: child_lock.version.clone(),
            digest: child_lock.digest.clone(),
        });
        supplied.insert(alias.clone(), graph);
        drop(temporary);
    }
    exact_dependencies.sort();
    miette::ensure!(
        exact_dependencies == locked.dependencies,
        "locked dependencies for '{}' differ from trellis.toml",
        locked.name
    );
    let graph = compile_graph(package_root, manifest, supplied)?;
    miette::ensure!(
        graph.root_digest() == locked.digest,
        "package '{}' semantic digest differs from trellis.lock",
        locked.name
    );
    let source = package_from_lock(package_root, manifest, graph.root_digest(), lock)?;
    miette::ensure!(
        source.distribution_digest == locked.distribution_digest,
        "package '{}' source differs from trellis.lock",
        locked.name
    );
    stack.remove(&manifest.package.name);
    Ok(graph)
}

fn compile_graph(
    root: &Path,
    manifest: &ProjectManifest,
    dependencies: BTreeMap<String, trellis_idl::PackageGraph>,
) -> Result<trellis_idl::PackageGraph> {
    trellis_idl::compile_project(
        manifest,
        trellis_idl::project::load_sources(root, manifest)?,
        dependencies,
    )
}

fn validate_lock(
    manifest: &ProjectManifest,
    lock: &ProjectLock,
    graph: &trellis_idl::PackageGraph,
) -> Result<()> {
    lock.validate()?;
    miette::ensure!(
        lock.root == manifest.package.name && graph.root().as_str() == lock.root,
        "trellis.lock belongs to a different root package"
    );
    miette::ensure!(
        lock.packages.len() == graph.packages().len(),
        "trellis.lock does not contain the exact package closure; run `trellis update`"
    );
    for package in &lock.packages {
        miette::ensure!(
            graph.digest(
                graph
                    .packages()
                    .keys()
                    .find(|id| id.as_str() == package.name)
                    .ok_or_else(|| miette!(
                        "locked package '{}' is absent from compiled graph",
                        package.name
                    ))?
            ) == Some(package.digest.as_str()),
            "locked digest for '{}' differs from compiled graph",
            package.name
        );
    }
    Ok(())
}

fn package_from_root(
    root: &Path,
    manifest: &ProjectManifest,
    digest: &str,
    dependencies: Vec<LockedDependency>,
    packages: Vec<LockedPackage>,
) -> Result<oci::SourcePackage> {
    let mut files = BTreeMap::from([(
        PathBuf::from("trellis.toml"),
        fs::read(root.join("trellis.toml")).into_diagnostic()?,
    )]);
    for relative in manifest.sources.values() {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).into_diagnostic()?;
        miette::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "package source {} must be a regular file",
            path.display()
        );
        files.insert(PathBuf::from(relative), fs::read(path).into_diagnostic()?);
    }
    oci::source_package(
        &manifest.package.name,
        &manifest.package.version,
        digest,
        files,
        dependencies,
        packages,
    )
}

fn package_from_lock(
    root: &Path,
    manifest: &ProjectManifest,
    digest: &str,
    lock: &ProjectLock,
) -> Result<oci::SourcePackage> {
    let locked = lock
        .packages
        .iter()
        .find(|package| package.name == manifest.package.name)
        .ok_or_else(|| {
            miette!(
                "package '{}' is absent from trellis.lock",
                manifest.package.name
            )
        })?;
    let mut names = BTreeSet::new();
    let mut pending = locked
        .dependencies
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        if !names.insert(name) {
            continue;
        }
        let package = lock
            .packages
            .iter()
            .find(|package| package.name == name)
            .ok_or_else(|| miette!("package '{name}' is absent from trellis.lock"))?;
        pending.extend(
            package
                .dependencies
                .iter()
                .map(|dependency| dependency.name.as_str()),
        );
    }
    package_from_root(
        root,
        manifest,
        digest,
        locked.dependencies.clone(),
        lock.packages
            .iter()
            .filter(|package| names.contains(package.name.as_str()))
            .cloned()
            .collect(),
    )
}

fn materialize(package: &oci::SourcePackage) -> Result<tempfile::TempDir> {
    let directory = tempfile::tempdir().into_diagnostic()?;
    for (relative, bytes) in &package.files {
        let path = directory.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).into_diagnostic()?;
        }
        fs::write(path, bytes).into_diagnostic()?;
    }
    Ok(directory)
}

fn merge_packages(
    target: &mut BTreeMap<String, LockedPackage>,
    packages: BTreeMap<String, LockedPackage>,
) -> Result<()> {
    for (name, package) in packages {
        if let Some(existing) = target.insert(name.clone(), package.clone()) {
            miette::ensure!(
                existing == package,
                "package '{name}' resolves to conflicting exact releases"
            );
        }
    }
    Ok(())
}

fn registry_config<'a>(
    manifest: &'a ProjectManifest,
    registry: &str,
) -> Result<&'a RegistryConfig> {
    manifest
        .registries
        .get(registry)
        .ok_or_else(|| miette!("registry '{registry}' is not configured"))
}

async fn select_remote_version(
    config: &RegistryConfig,
    name: &str,
    requirement: &VersionReq,
) -> Result<Version> {
    oci::versions(config, name)
        .await?
        .into_iter()
        .filter(|version| requirement.matches(version))
        .max()
        .ok_or_else(|| miette!("no release of {name} satisfies {requirement}"))
}

fn edit_manifest_dependency(
    previous: &[u8],
    alias: &str,
    dependency: Option<&PackageDependency>,
) -> Result<Vec<u8>> {
    let mut document = std::str::from_utf8(previous)
        .into_diagnostic()?
        .parse::<toml_edit::DocumentMut>()
        .into_diagnostic()?;
    if !document.contains_key("dependencies") {
        document["dependencies"] = toml_edit::table();
    }
    let dependencies = document
        .get_mut("dependencies")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| miette!("trellis.toml dependencies must be a table"))?;
    match dependency {
        Some(value) => {
            dependencies.insert(
                alias,
                toml_edit::ser::to_document(value)
                    .into_diagnostic()?
                    .into_item(),
            );
        }
        None => {
            dependencies.remove(alias);
        }
    }
    let bytes = document.to_string().into_bytes();
    toml::from_slice::<ProjectManifest>(&bytes)
        .into_diagnostic()?
        .validate()?;
    Ok(bytes)
}

async fn commit_and_install(
    root: &Path,
    manifest: &ProjectManifest,
    lock: &ProjectLock,
    previous_manifest: &[u8],
    previous_lock: Option<&[u8]>,
    manifest_bytes: Option<&[u8]>,
) -> Result<PackageResult> {
    let manifest_path = root.join("trellis.toml");
    let lock_path = root.join("trellis.lock");
    write_manifest_and_lock(&manifest_path, manifest, manifest_bytes, &lock_path, lock)?;
    match install_root(root, manifest, lock).await {
        Ok(result) => Ok(result),
        Err(error) => {
            restore_project_files(&manifest_path, previous_manifest, &lock_path, previous_lock)?;
            Err(error)
        }
    }
}

fn canonical_root(root: &Path) -> Result<PathBuf> {
    root.canonicalize()
        .into_diagnostic()
        .wrap_err_with(|| format!("invalid project root {}", root.display()))
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).into_diagnostic(),
    }
}

fn print_result(
    format: OutputFormat,
    result: &PackageResult,
    headline: Option<String>,
) -> Result<()> {
    if output::is_json(format) {
        return output::print_json(result);
    }
    if let Some(headline) = headline {
        println!("{headline}");
    }
    if result.changed_dependencies == 0 && result.generated_projects == 0 {
        println!("Installed 0 changes");
    } else {
        println!("Installed {} package(s)", result.installed_packages);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PublishArgs;

    fn write_package(root: &Path, name: &str, field: &str) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("trellis.toml"),
            format!(
                "[package]\nname = {name:?}\nversion = \"1.0.0\"\n[sources]\nmain = \"main.trellis\"\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join("main.trellis"),
            format!(
                "model Value {{ {field}: string; }}\napi ping@v1 {{ title \"Ping\"; description \"Test\"; rpc Get {{ input Value; output Value; }} capabilities {{ public {{ allows {{ rpc Get; }} }} }} }}\n"
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn targeted_update_preserves_every_other_locked_path_package() {
        let root = tempfile::tempdir().unwrap();
        write_package(&root.path().join("alpha"), "alpha", "alpha");
        write_package(&root.path().join("beta"), "beta", "before");
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname = \"root\"\nversion = \"1.0.0\"\n[sources]\nmain = \"main.trellis\"\n[dependencies.alpha]\npackage = \"alpha\"\npath = \"alpha\"\n[dependencies.beta]\npackage = \"beta\"\npath = \"beta\"\n",
        )
        .unwrap();
        fs::write(root.path().join("main.trellis"), "model Root {}\n").unwrap();
        let project = ProjectRootArgs {
            root: root.path().to_path_buf(),
        };
        install(OutputFormat::Text, &project).await.unwrap();
        let before = fs::read(root.path().join("trellis.lock")).unwrap();

        write_package(&root.path().join("beta"), "beta", "after");
        let error = update(
            OutputFormat::Text,
            &UpdateArgs {
                dependency_alias: Some("alpha".into()),
                project,
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("changed locked package 'beta'"), "{error}");
        assert_eq!(fs::read(root.path().join("trellis.lock")).unwrap(), before);
    }

    #[tokio::test]
    async fn locked_acquisition_cannot_hide_an_authored_path_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_package(&root.path().join("child"), "child", "value");
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname = \"root\"\nversion = \"1.0.0\"\n[sources]\nmain = \"main.trellis\"\n[dependencies.child]\npackage = \"child\"\npath = \"child\"\n",
        )
        .unwrap();
        fs::write(root.path().join("main.trellis"), "model Root {}\n").unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        let mut lock = resolve_lock(root.path(), &manifest).await.unwrap();
        let child = lock
            .packages
            .iter_mut()
            .find(|package| package.name == "child")
            .unwrap();
        child.source = LockedSource::Registry {
            registry: "registry.example/trellis".into(),
            oci_digest: format!("sha256:{}", "0".repeat(64)),
        };
        write_lock(&root.path().join("trellis.lock"), &lock).unwrap();

        let error = compile_project(root.path(), &manifest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("differs from trellis.toml"), "{error}");
        let error = publish(
            OutputFormat::Text,
            &PublishArgs {
                registry: None,
                project: ProjectRootArgs {
                    root: root.path().to_path_buf(),
                },
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("replace path dependencies first"), "{error}");
    }
}
