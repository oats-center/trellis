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
        ProjectManifest,
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
            workspace: None,
            version: None,
            path: Some(args.source.clone()),
        }
    } else {
        // A registry package is named by its full identity, such as
        // ghcr.io/acme/orders; its workspace says where it is published.
        let (workspace, name) = trellis_idl::project::split_package_identity(&args.source)
            .wrap_err_with(|| {
                format!(
                    "'{}' is neither a project directory nor a published package such as \
                    ghcr.io/acme/orders",
                    args.source
                )
            })?;
        let identity = args.source.clone();
        oci::repository(&identity)?;
        let requested = args
            .version
            .as_deref()
            .map(VersionReq::parse)
            .transpose()
            .map_err(|error| miette!("invalid version requirement: {error}"))?;
        let release =
            select_remote_version(&identity, requested.as_ref().unwrap_or(&VersionReq::STAR))
                .await?;
        let requirement = args
            .version
            .clone()
            .unwrap_or_else(|| format!("^{release}"));
        PackageDependency {
            package: name.to_owned(),
            workspace: workspace.map(str::to_owned),
            version: Some(requirement),
            path: None,
        }
    };
    let alias = dependency.package.replace('-', "_");
    miette::ensure!(
        !manifest.sources.contains_key(&alias),
        "dependency alias '{alias}' collides with a source alias"
    );
    // Two packages with one name from different workspaces need their own
    // aliases, which only the author can choose.
    if let Some(existing) = manifest.dependencies.get(&alias) {
        miette::ensure!(
            existing.identity() == dependency.identity(),
            "dependency alias '{alias}' already names {}; add {} to trellis.toml under \
            another alias",
            existing
                .identity()
                .unwrap_or_else(|| existing.package.clone()),
            args.source
        );
    }
    manifest.dependencies.insert(alias.clone(), dependency);
    let edited = edit_manifest_dependency(
        &previous_manifest,
        &alias,
        manifest.dependencies.get(&alias),
    )?;
    let lock = resolve_lock(&root, &manifest, Some(&edited)).await?;
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
            (dependency.package == args.api_id
                || alias == &args.api_id
                || dependency.identity().as_deref() == Some(args.api_id.as_str()))
            .then(|| alias.clone())
        })
        .ok_or_else(|| miette!("package '{}' is not in trellis.toml", args.api_id))?;
    manifest.dependencies.remove(&alias);
    let edited = edit_manifest_dependency(&previous_manifest, &alias, None)?;
    let lock = resolve_lock(&root, &manifest, Some(&edited)).await?;
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
    let lock = resolve_lock(&root, &manifest, None).await?;
    let package = lock
        .packages
        .iter()
        .find(|package| package.id == lock.root)
        .expect("resolved lock contains root package");
    if output::is_json(format) {
        output::print_json(&serde_json::json!({
            "package": package.id,
            "packageDigest": package.digest,
            "version": package.version,
        }))
    } else {
        output::success(format!(
            "{} {} is valid",
            output::strong(trellis_protocol::short_id(&package.id)),
            package.version
        ));
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
            let identity = dependency.identity().expect("registry dependency identity");
            let locked = current
                .packages
                .iter()
                .find(|package| package.id == identity)
                .ok_or_else(|| {
                    miette!(
                        "package '{identity}' is absent from trellis.lock; run `trellis update`"
                    )
                })?;
            dependency.version = Some(format!("={}", locked.version));
        }
    }
    let lock = resolve_lock(&root, &resolution_manifest, None).await?;
    if let (Some(alias), Some(previous_lock)) = (&args.dependency_alias, previous_lock.as_deref()) {
        let previous = toml::from_slice::<ProjectLock>(previous_lock).into_diagnostic()?;
        previous.validate()?;
        let mut pending = manifest
            .dependencies
            .iter()
            .filter(|(current_alias, _)| *current_alias != alias)
            .map(|(_, dependency)| dependency_identity(&root, dependency))
            .collect::<Result<Vec<_>>>()?;
        let mut checked = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !checked.insert(name.clone()) {
                continue;
            }
            let old = previous
                .packages
                .iter()
                .find(|package| package.id == name)
                .ok_or_else(|| miette!("package '{name}' is absent from trellis.lock"))?;
            let new = lock
                .packages
                .iter()
                .find(|package| package.id == name)
                .ok_or_else(|| {
                    miette!("updating dependency alias '{alias}' changed locked package '{name}'")
                })?;
            miette::ensure!(
                old == new,
                "updating dependency alias '{alias}' changed locked package '{name}'; \
                run `trellis update` to refresh every dependency"
            );
            pending.extend(
                old.dependencies
                    .iter()
                    .map(|dependency| dependency.id.clone()),
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
        let lock = resolve_lock(&root, &manifest, None).await?;
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
                locked.id == lock.root || matches!(&locked.source, LockedSource::Registry { .. })
            }),
        "publish requires every dependency to be an exact registry package; \
        replace path dependencies first"
    );
    let graph = compile_project(&root, &manifest)?;
    validate_lock(&manifest, &lock, &graph)?;
    let package = package_from_lock(&root, &manifest, graph.root_digest(), &lock)?;
    // The package publishes to its own workspace; the registry's own access
    // control decides whether this publisher may write there.
    oci::repository(&package.id).wrap_err("only a package with a workspace is published")?;
    let versions = oci::versions(&package.id).await?;
    let existing = if versions.binary_search(&package.version).is_ok() {
        let remote = oci::pull_tag(&package.id, &package.version).await?;
        miette::ensure!(
            remote.package_digest == package.package_digest
                && remote.distribution_digest == package.distribution_digest,
            "release {} {} already exists with different content",
            package.id,
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
        None => oci::publish(&package).await?,
    };
    if output::is_json(format) {
        output::print_json(&serde_json::json!({
            "name": package.id,
            "version": package.version,
            "digest": digest,
            "changed": changed
        }))
    } else {
        let name = format!(
            "{} {}",
            output::strong(trellis_protocol::short_id(&package.id)),
            package.version
        );
        if changed {
            output::success(format!("Published {name}"));
        } else {
            output::unchanged(format!("{name} is already published"));
        }
        println!("{}@{digest}", oci::repository(&package.id)?);
        Ok(())
    }
}

/// Resolve the project's lock. `manifest_bytes` supplies a proposed root
/// manifest that is not yet on disk; `None` reads the current file.
async fn resolve_lock(
    root: &Path,
    manifest: &ProjectManifest,
    manifest_bytes: Option<&[u8]>,
) -> Result<ProjectLock> {
    let mut stack = BTreeSet::new();
    let resolution = resolve_package(
        root,
        root,
        manifest,
        manifest_bytes,
        LockedSource::Path { path: ".".into() },
        &mut stack,
    )
    .await?;
    Ok(ProjectLock {
        format: 3,
        root: manifest.identity(),
        packages: resolution.packages.into_values().collect(),
    })
}

fn resolve_package<'a>(
    root: &'a Path,
    package_root: &'a Path,
    manifest: &'a ProjectManifest,
    manifest_bytes: Option<&'a [u8]>,
    source: LockedSource,
    stack: &'a mut BTreeSet<String>,
) -> Pin<Box<dyn Future<Output = Result<Resolution>> + 'a>> {
    Box::pin(async move {
        let identity = manifest.identity();
        miette::ensure!(
            stack.insert(identity.clone()),
            "package dependency cycle through '{identity}'"
        );
        let mut supplied = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut direct = Vec::new();
        for (alias, dependency) in &manifest.dependencies {
            let (child_root, child_source, pulled) = if let Some(path) = &dependency.path {
                let child = package_root.join(path).canonicalize().into_diagnostic()?;
                if matches!(&source, LockedSource::Registry { .. }) {
                    return Err(miette!(
                        "registry package '{identity}' contains a path dependency"
                    ));
                }
                // Record the canonical dependency location relative to the
                // project root, so the same local package reached through
                // different dependency edges records one exact source.
                let relative = pathdiff::diff_paths(&child, root)
                    .ok_or_else(|| {
                        miette!(
                            "local dependency {} cannot be expressed relative to the project root",
                            child.display()
                        )
                    })?
                    .to_string_lossy()
                    .into_owned();
                (child, LockedSource::Path { path: relative }, None)
            } else {
                let wanted = dependency.identity().expect("registry dependency identity");
                let requirement =
                    VersionReq::parse(dependency.version.as_deref().expect("validated version"))
                        .into_diagnostic()?;
                // The built-in package this CLI ships is the release the
                // Trellis project publishes; any other release is pulled from
                // the dependency's own workspace and must declare that same
                // identity.
                let builtin = (wanted == trellis_idl::project::BUILTIN_PACKAGE)
                    .then(oci::builtin_package)
                    .transpose()?
                    .filter(|package| requirement.matches(&package.version));
                let package = match builtin {
                    Some(package) => package,
                    None => {
                        let version = select_remote_version(&wanted, &requirement).await?;
                        oci::pull_tag(&wanted, &version).await?
                    }
                };
                let directory = materialize(&package)?;
                (
                    directory.path().to_path_buf(),
                    LockedSource::Registry {
                        workspace: trellis_idl::project::package_source_workspace(&wanted)?,
                        oci_digest: package.manifest_digest.clone(),
                    },
                    Some((package, directory)),
                )
            };
            let child_manifest = read_manifest(&child_root.join("trellis.toml"))?;
            let child_identity = child_manifest.identity();
            miette::ensure!(
                dependency.identity().map_or(
                    child_manifest.package.name == dependency.package,
                    |wanted| { wanted == child_identity }
                ),
                "dependency '{alias}' contains package '{child_identity}' instead of '{}'",
                dependency
                    .identity()
                    .unwrap_or_else(|| dependency.package.clone())
            );
            let child = if let Some((pulled, _)) = &pulled {
                let lock = pulled.lock(child_source)?;
                install_locked_packages(&lock).await?;
                let graph = compile_locked(
                    &child_root,
                    &child_root,
                    &child_manifest,
                    &lock,
                    false,
                    &mut BTreeSet::new(),
                )?;
                validate_lock(&child_manifest, &lock, &graph)?;
                miette::ensure!(
                    pulled.package_digest == graph.root_digest(),
                    "remote package '{child_identity}' semantic digest is invalid"
                );
                Resolution {
                    graph,
                    packages: lock
                        .packages
                        .into_iter()
                        .map(|package| (package.id.clone(), package))
                        .collect(),
                }
            } else {
                resolve_package(
                    root,
                    &child_root,
                    &child_manifest,
                    None,
                    child_source,
                    stack,
                )
                .await?
            };
            direct.push(LockedDependency {
                id: child_identity,
                version: child_manifest.package.version.clone(),
                digest: child.graph.root_digest().to_owned(),
            });
            miette::ensure!(
                !child.packages.contains_key(&identity),
                "package dependency cycle through '{identity}'"
            );
            merge_packages(&mut packages, child.packages)?;
            supplied.insert(alias.clone(), child.graph);
            drop(pulled);
        }
        direct.sort();
        let graph = compile_graph(package_root, manifest, supplied)?;
        let source_package = package_from_root(
            package_root,
            match manifest_bytes {
                Some(bytes) => bytes.to_vec(),
                None => fs::read(package_root.join("trellis.toml")).into_diagnostic()?,
            },
            manifest,
            graph.root_digest(),
            direct.clone(),
            packages.values().cloned().collect(),
        )?;
        packages.insert(
            identity.clone(),
            LockedPackage {
                id: identity.clone(),
                version: manifest.package.version.clone(),
                digest: graph.root_digest().to_owned(),
                distribution_digest: source_package.distribution_digest,
                dependencies: direct,
                source,
            },
        );
        stack.remove(&identity);
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
        let LockedSource::Registry { oci_digest, .. } = &package.source else {
            continue;
        };
        if oci::read_locked(
            &package.id,
            &package.version.to_string(),
            &package.digest,
            &package.distribution_digest,
            oci_digest,
        )
        .is_err()
        {
            changed += 1;
            if package.id == trellis_idl::project::BUILTIN_PACKAGE
                && oci::builtin_package()?.manifest_digest == *oci_digest
            {
                continue;
            }
            oci::pull_locked(
                &package.id,
                &package.version.to_string(),
                &package.digest,
                &package.distribution_digest,
                oci_digest,
            )
            .await?;
        }
    }
    Ok(changed)
}

/// Re-record the project's own package in `trellis.lock` after its IDL or
/// manifest changed, from local sources only. Dependencies are untouched and
/// still need `trellis update`. Returns whether the lock changed.
pub(crate) fn relock_own_package(root: &Path, manifest: &ProjectManifest) -> Result<bool> {
    let path = root.join("trellis.lock");
    let mut lock = read_lock(&path).wrap_err("trellis.lock is missing; run `trellis update`")?;
    let graph = compile_locked(root, root, manifest, &lock, true, &mut BTreeSet::new())?;
    let source = package_from_lock(root, manifest, graph.root_digest(), &lock)?;
    let identity = manifest.identity();
    let own = lock
        .packages
        .iter_mut()
        .find(|package| package.id == identity)
        .ok_or_else(|| miette!("package '{identity}' is absent from trellis.lock"))?;
    if own.digest == graph.root_digest() && own.distribution_digest == source.distribution_digest {
        return Ok(false);
    }
    own.digest = graph.root_digest().to_owned();
    own.distribution_digest = source.distribution_digest;
    crate::project::write_lock(&path, &lock)?;
    Ok(true)
}

/// Compile the project from local paths and exact validated cache entries only.
pub(crate) fn compile_project(
    root: &Path,
    manifest: &ProjectManifest,
) -> Result<trellis_idl::PackageGraph> {
    let lock = read_lock(&root.join("trellis.lock"))
        .wrap_err("trellis.lock is missing; run `trellis update`")?;
    // A pulled registry package is compiled as its own root too, but its lock
    // records its registry source; only a project's own lock names itself ".".
    miette::ensure!(
        lock.packages
            .iter()
            .any(|package| package.id == manifest.identity()
                && matches!(&package.source, LockedSource::Path { path } if path == ".")),
        "root package must use the exact project path in trellis.lock"
    );
    let graph = compile_locked(root, root, manifest, &lock, false, &mut BTreeSet::new())?;
    validate_lock(manifest, &lock, &graph)?;
    Ok(graph)
}

fn compile_locked(
    root: &Path,
    package_root: &Path,
    manifest: &ProjectManifest,
    lock: &ProjectLock,
    refresh_root: bool,
    stack: &mut BTreeSet<String>,
) -> Result<trellis_idl::PackageGraph> {
    let identity = manifest.identity();
    miette::ensure!(
        stack.insert(identity.clone()),
        "package dependency cycle through '{identity}'"
    );
    let locked = lock
        .packages
        .iter()
        .find(|package| package.id == identity)
        .ok_or_else(|| miette!("package '{identity}' is absent from trellis.lock"))?;
    miette::ensure!(
        manifest.package.version == locked.version,
        "package '{identity}' version differs from trellis.lock"
    );
    let mut supplied = BTreeMap::new();
    let mut exact_dependencies = Vec::new();
    for (alias, dependency) in &manifest.dependencies {
        let child_identity = dependency_identity(package_root, dependency)?;
        let child_lock = lock
            .packages
            .iter()
            .find(|package| package.id == child_identity)
            .ok_or_else(|| miette!("package '{child_identity}' is absent from trellis.lock"))?;
        match (&dependency.path, &dependency.version, &child_lock.source) {
            (Some(path), None, LockedSource::Path { path: locked_path }) => {
                let declared = package_root.join(path).canonicalize().into_diagnostic()?;
                let acquired = root.join(locked_path).canonicalize().into_diagnostic()?;
                miette::ensure!(
                    declared == acquired,
                    "locked path for dependency '{alias}' differs from trellis.toml"
                );
            }
            (None, Some(requirement), LockedSource::Registry { .. }) => {
                let requirement = VersionReq::parse(requirement).into_diagnostic()?;
                miette::ensure!(
                    requirement.matches(&child_lock.version),
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
                    &child_lock.id,
                    &child_lock.version.to_string(),
                    &child_lock.digest,
                    &child_lock.distribution_digest,
                    oci_digest,
                )
                .map_err(|error| {
                    miette!(
                        "cached package '{}' {} is unavailable: {error}; run `trellis install`",
                        child_lock.id,
                        child_lock.version
                    )
                })?;
                let temporary = materialize(&package)?;
                (temporary.path().to_path_buf(), Some(temporary))
            }
        };
        let child_manifest = read_manifest(&child_root.join("trellis.toml"))?;
        miette::ensure!(
            child_manifest.identity() == child_lock.id
                && child_manifest.package.version == child_lock.version,
            "locked identity for dependency '{alias}' differs from its source package"
        );
        let graph = compile_locked(root, &child_root, &child_manifest, lock, false, stack)?;
        exact_dependencies.push(LockedDependency {
            id: child_lock.id.clone(),
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
        locked.id
    );
    let graph = compile_graph(package_root, manifest, supplied)?;
    // Generation may re-record the project's own edited sources; every
    // dependency stays exactly as locked.
    if refresh_root && package_root == root {
        stack.remove(&identity);
        return Ok(graph);
    }
    miette::ensure!(
        graph.root_digest() == locked.digest,
        "package '{}' semantic digest differs from trellis.lock; run `trellis generate` or `trellis update`",
        locked.id
    );
    let source = package_from_lock(package_root, manifest, graph.root_digest(), lock)?;
    miette::ensure!(
        source.distribution_digest == locked.distribution_digest,
        "package '{}' source differs from trellis.lock; run `trellis generate` or `trellis update`",
        locked.id
    );
    stack.remove(&identity);
    Ok(graph)
}

/// The full identity a dependency resolves to: a registry dependency names
/// it, and a path dependency's own manifest declares it.
fn dependency_identity(package_root: &Path, dependency: &PackageDependency) -> Result<String> {
    match (&dependency.path, dependency.identity()) {
        (_, Some(identity)) => Ok(identity),
        (Some(path), None) => {
            Ok(read_manifest(&package_root.join(path).join("trellis.toml"))?.identity())
        }
        (None, None) => unreachable!("a dependency without a path has an identity"),
    }
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
        lock.root == manifest.identity() && graph.root().as_str() == lock.root,
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
                    .find(|id| id.as_str() == package.id)
                    .ok_or_else(|| miette!(
                        "locked package '{}' is absent from compiled graph",
                        package.id
                    ))?
            ) == Some(package.digest.as_str()),
            "locked digest for '{}' differs from compiled graph",
            package.id
        );
    }
    Ok(())
}

fn package_from_root(
    root: &Path,
    manifest_bytes: Vec<u8>,
    manifest: &ProjectManifest,
    digest: &str,
    dependencies: Vec<LockedDependency>,
    packages: Vec<LockedPackage>,
) -> Result<oci::SourcePackage> {
    let mut files = BTreeMap::from([(PathBuf::from("trellis.toml"), manifest_bytes)]);
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
        &manifest.identity(),
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
    let identity = manifest.identity();
    let locked = lock
        .packages
        .iter()
        .find(|package| package.id == identity)
        .ok_or_else(|| miette!("package '{identity}' is absent from trellis.lock"))?;
    let mut names = BTreeSet::new();
    let mut pending = locked
        .dependencies
        .iter()
        .map(|dependency| dependency.id.as_str())
        .collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        if !names.insert(name) {
            continue;
        }
        let package = lock
            .packages
            .iter()
            .find(|package| package.id == name)
            .ok_or_else(|| miette!("package '{name}' is absent from trellis.lock"))?;
        pending.extend(
            package
                .dependencies
                .iter()
                .map(|dependency| dependency.id.as_str()),
        );
    }
    package_from_root(
        root,
        fs::read(root.join("trellis.toml")).into_diagnostic()?,
        manifest,
        digest,
        locked.dependencies.clone(),
        lock.packages
            .iter()
            .filter(|package| names.contains(package.id.as_str()))
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

async fn select_remote_version(name: &str, requirement: &VersionReq) -> Result<Version> {
    oci::versions(name)
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
        output::success(headline);
    }
    if result.changed_dependencies == 0 && result.generated_projects == 0 {
        output::unchanged("Dependencies and generated code are up to date");
    } else {
        let generated = if result.generated_projects > 0 {
            " and generated code"
        } else {
            ""
        };
        match result.installed_packages {
            0 => output::success(format!(
                "Installed the project{generated}; it has no dependencies"
            )),
            count => output::success(format!(
                "Installed {}{generated}",
                output::plural(count, "package")
            )),
        }
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
                "[package]\nname = {name:?}\nversion = \"1.0.0\"\n[sources]\nmain = \
                \"main.trellis\"\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join("main.trellis"),
            format!(
                "model Value {{ {field}: string; }}\napi ping@v1 {{ title \"Ping\"; \
                description \"Test\"; rpc Get {{ input Value; output Value; }} \
                capabilities {{ public {{ allows {{ rpc Get; }} }} \
                }} }}\n"
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn add_then_remove_locks_the_edited_manifest() {
        let root = tempfile::tempdir().unwrap();
        write_package(&root.path().join("child"), "child", "value");
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname = \"root\"\nversion = \"1.0.0\"\n[sources]\nmain = \
            \"main.trellis\"\n",
        )
        .unwrap();
        fs::write(root.path().join("main.trellis"), "model Root {}\n").unwrap();
        let project = ProjectRootArgs {
            root: root.path().to_path_buf(),
        };
        install(OutputFormat::Text, &project).await.unwrap();

        add(
            OutputFormat::Text,
            &AddArgs {
                source: "child".into(),
                version: None,
                project: ProjectRootArgs {
                    root: root.path().to_path_buf(),
                },
            },
        )
        .await
        .unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        assert!(manifest.dependencies.contains_key("child"));
        compile_project(root.path(), &manifest).unwrap();

        remove(
            OutputFormat::Text,
            &RmArgs {
                api_id: "child".into(),
                project: ProjectRootArgs {
                    root: root.path().to_path_buf(),
                },
            },
        )
        .await
        .unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        assert!(manifest.dependencies.is_empty());
        compile_project(root.path(), &manifest).unwrap();
    }

    #[tokio::test]
    async fn registry_dependency_is_pulled_from_the_workspace_it_names() {
        let _guard = oci::TEST_ENV_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        // Nothing listens on port 1, so resolution reaches the dependency's
        // own workspace and reports that it cannot list releases there.
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname = \"root\"\nversion = \"1.0.0\"\n[sources]\nmain = \
            \"main.trellis\"\n[dependencies.acme]\npackage = \"acme\"\nworkspace = \
            \"127.0.0.1:1/trellis\"\nversion = \"^1\"\n",
        )
        .unwrap();
        fs::write(root.path().join("main.trellis"), "model Root {}\n").unwrap();

        let error = update(
            OutputFormat::Text,
            &UpdateArgs {
                dependency_alias: None,
                project: ProjectRootArgs {
                    root: root.path().to_path_buf(),
                },
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("failed to list releases for '127.0.0.1:1/trellis/acme'"),
            "{error}"
        );
        assert!(!root.path().join("trellis.lock").exists());
    }

    #[tokio::test]
    async fn a_package_without_a_workspace_is_never_published() {
        let _guard = oci::TEST_ENV_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        write_package(root.path(), "local", "value");
        let project = ProjectRootArgs {
            root: root.path().to_path_buf(),
        };
        install(OutputFormat::Text, &project).await.unwrap();

        let error = format!(
            "{:?}",
            publish(OutputFormat::Text, &PublishArgs { project })
                .await
                .unwrap_err()
        );
        assert!(error.contains("declares no workspace"), "{error}");
    }

    #[tokio::test]
    async fn targeted_update_preserves_every_other_locked_path_package() {
        let root = tempfile::tempdir().unwrap();
        write_package(&root.path().join("alpha"), "alpha", "alpha");
        write_package(&root.path().join("beta"), "beta", "before");
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname = \"root\"\nversion = \"1.0.0\"\n[sources]\nmain = \
            \"main.trellis\"\n[dependencies.alpha]\npackage = \"alpha\"\npath = \
            \"alpha\"\n[dependencies.beta]\npackage = \"beta\"\npath = \"beta\"\n",
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
        assert!(
            error.contains("changed locked package 'localhost/beta'"),
            "{error}"
        );
        assert_eq!(fs::read(root.path().join("trellis.lock")).unwrap(), before);
    }

    #[tokio::test]
    async fn locked_acquisition_cannot_hide_an_authored_path_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_package(&root.path().join("child"), "child", "value");
        let child_manifest = root.path().join("child/trellis.toml");
        let text = fs::read_to_string(&child_manifest).unwrap().replace(
            "name = \"child\"\n",
            "name = \"child\"\nworkspace = \"registry.example/trellis\"\n",
        );
        fs::write(&child_manifest, text).unwrap();
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname = \"root\"\nversion = \"1.0.0\"\n[sources]\nmain = \
            \"main.trellis\"\n[dependencies.child]\npackage = \"child\"\npath = \
            \"child\"\n",
        )
        .unwrap();
        fs::write(root.path().join("main.trellis"), "model Root {}\n").unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        let mut lock = resolve_lock(root.path(), &manifest, None).await.unwrap();
        let child = lock
            .packages
            .iter_mut()
            .find(|package| package.id == "registry.example/trellis/child")
            .unwrap();
        child.source = LockedSource::Registry {
            workspace: "registry.example/trellis".into(),
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
