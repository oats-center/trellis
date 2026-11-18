//! Native Trellis package generation and offline filesystem watch orchestration.

use crate::output;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use crate::{
    cli::GenerateArgs,
    project::{read_manifest, ProjectManifest},
};
use miette::{miette, IntoDiagnostic, Result};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};

/// Run native IDL generation once or in watch mode.
pub fn run(format: crate::cli::OutputFormat, args: &GenerateArgs) -> Result<()> {
    let root = args.project.root.canonicalize().into_diagnostic()?;
    if args.watch {
        let manifest = read_manifest(&root.join("trellis.toml"))?;
        output_paths(&root, &manifest)?;
        return watch(&root);
    }
    generate_once(&root, args.check)?;
    let manifest = read_manifest(&root.join("trellis.toml"))?;
    let outputs = output_paths(&root, &manifest)?;
    if output::is_json(format) {
        return output::print_json(&serde_json::json!({
            "outputs": outputs
                .iter()
                .map(|(language, path)| serde_json::json!({
                    "language": language,
                    "path": path,
                }))
                .collect::<Vec<_>>(),
            "checked": args.check,
        }));
    }
    report_outputs(&root, &outputs, args.check);
    Ok(())
}

/// Say which outputs a generation wrote or checked, relative to the project.
fn report_outputs(root: &Path, outputs: &[(&'static str, PathBuf)], check: bool) {
    if outputs.is_empty() {
        output::warning(
            "Nothing to generate: the project has no Cargo.toml or package.json, \
            and trellis.toml names no [generate] outputs",
        );
        return;
    }
    let listed = outputs
        .iter()
        .map(|(language, path)| {
            let path = path.strip_prefix(root).unwrap_or(path);
            format!("{language} in {}", path.display())
        })
        .collect::<Vec<_>>()
        .join(", ");
    if check {
        output::success(format!("Generated code is up to date ({listed})"));
    } else {
        output::success(format!("Generated {listed}"));
    }
}

/// Compile current local sources and exact cached dependencies without network access.
pub fn generate_project(root: &Path) -> Result<()> {
    generate_once(root, false).map(|_| ())
}

pub(crate) fn generate_once(root: &Path, check: bool) -> Result<usize> {
    let root = root.canonicalize().into_diagnostic()?;
    let manifest = read_manifest(&root.join("trellis.toml"))?;
    output_paths(&root, &manifest)?;
    // An IDL edit changes the project's own locked digest; `--check` reports
    // that as stale instead of rewriting the lock.
    if !check {
        crate::package::relock_own_package(&root, &manifest)?;
    }
    let compiled = crate::package::compile_project(&root, &manifest)?;
    generate_compiled(&root, &manifest, &compiled, check)
}

pub(crate) fn output_paths(
    root: &Path,
    manifest: &ProjectManifest,
) -> Result<Vec<(&'static str, PathBuf)>> {
    let root = root.canonicalize().into_diagnostic()?;
    let has_rust = root.join("Cargo.toml").is_file();
    let has_ts = root.join("package.json").is_file();
    let config = &manifest.generate;
    // Explicit outputs select projections. Detect toolchains only when none are
    // configured, so a Cargo build script can own Rust generation independently.
    let configured = config.rust.is_some() || config.typescript.is_some();
    let targets = [
        ("rust", has_rust && !configured, config.rust.as_ref()),
        (
            "typescript",
            has_ts && !configured,
            config.typescript.as_ref(),
        ),
    ];
    let target_count = targets
        .iter()
        .filter(|(_, detected, selected)| *detected || selected.is_some())
        .count();
    if target_count == 0 {
        return Ok(Vec::new());
    }
    let name = manifest.package.name.as_str();
    miette::ensure!(
        name.len() <= 214
            && name.starts_with(|character: char| character.is_ascii_lowercase())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !name.ends_with('-')
            && !name.contains("--"),
        "invalid generated package name '{name}'; use at most 214 characters: a \
        lowercase ASCII letter followed by lowercase letters, digits, or \
        single hyphens"
    );
    let mut outputs = Vec::new();
    for (language, detected, selected) in targets {
        if !detected && selected.is_none() {
            continue;
        }
        let destination = match selected {
            Some(selected) => selected.output.as_str(),
            None => {
                // Only the sole detected language may fall back to the default
                // output; multiple targets must name their outputs explicitly.
                miette::ensure!(
                    target_count == 1,
                    "multiple languages detected; configure [generate.rust].output and \
                    [generate.typescript].output"
                );
                "trellis"
            }
        };
        miette::ensure!(
            !destination.is_empty(),
            "generated output must not be empty"
        );
        let destination = root.join(destination);
        // Resolve existing ancestors without following symlinks into user data.
        let mut resolved = PathBuf::new();
        for component in destination.components() {
            match component {
                std::path::Component::CurDir => continue,
                std::path::Component::ParentDir => {
                    resolved.pop();
                }
                _ => resolved.push(component.as_os_str()),
            }
            if let Ok(metadata) = fs::symlink_metadata(&resolved) {
                miette::ensure!(
                    !metadata.file_type().is_symlink() && metadata.is_dir(),
                    "generated output ancestor {} must be a real directory",
                    resolved.display()
                );
            }
        }
        miette::ensure!(
            resolved != root && resolved.starts_with(&root),
            "generated output {} must be a descendant of the project root",
            resolved.display()
        );
        for source in manifest.sources.values() {
            let source = root.join(source).canonicalize().into_diagnostic()?;
            miette::ensure!(
                !source.starts_with(&resolved) && !resolved.starts_with(&source),
                "generated output {} overlaps source {}",
                resolved.display(),
                source.display()
            );
        }
        for source in std::iter::once(root.to_path_buf()).chain(
            manifest
                .dependencies
                .values()
                .filter_map(|dependency| dependency.path.as_ref())
                .map(|path| root.join(path)),
        ) {
            let source = source.canonicalize().into_diagnostic()?;
            miette::ensure!(
                !source.starts_with(&resolved)
                    && (source == root || !resolved.starts_with(&source)),
                "generated output {} overlaps a source project",
                resolved.display()
            );
            for protected in ["contracts", ".git"] {
                miette::ensure!(
                    !resolved.starts_with(source.join(protected)),
                    "generated output {} overlaps source or reserved state",
                    resolved.display()
                );
            }
        }
        for (_, previous) in &outputs {
            miette::ensure!(
                !resolved.starts_with(previous) && !Path::new(previous).starts_with(&resolved),
                "generated package outputs overlap"
            );
        }
        outputs.push((language, resolved));
    }
    Ok(outputs)
}

pub(crate) fn generate_compiled(
    root: &Path,
    manifest: &ProjectManifest,
    compiled: &trellis_idl::PackageGraph,
    check: bool,
) -> Result<usize> {
    let outputs = output_paths(root, manifest)?;
    if outputs.is_empty() {
        return Ok(0);
    }
    let name = manifest.package.name.as_str();
    let mut staged = Vec::new();
    let mut changes = Vec::new();
    for (language, destination) in &outputs {
        let previous_change_count = changes.len();
        // Check staging never creates an output or its parents. Publication staging
        // stays on the destination filesystem without creating missing ancestors.
        let staging = if check {
            tempfile::tempdir()
        } else {
            let parent = destination
                .ancestors()
                .skip(1)
                .find(|path| path.is_dir())
                .expect("project root exists");
            tempfile::Builder::new()
                .prefix(".trellis-stage-")
                .tempdir_in(parent)
        }
        .into_diagnostic()?;
        let fresh = staging.path().join("new");
        match *language {
            "rust" => {
                let bundled: Vec<trellis_codegen_rust::RustModelBinding> = serde_json::from_str(
                    include_str!(concat!(env!("OUT_DIR"), "/rust-model-bindings.json")),
                )
                .into_diagnostic()?;
                let bindings = project_model_bindings(destination, bundled)?;
                for line in trellis_codegen_rust::describe_model_bindings(compiled, &bindings) {
                    tracing::info!("rust models: {line}");
                }
                trellis_codegen_rust::generate_rust_package(compiled, &fresh, name, &bindings)
                    .into_diagnostic()?;
            }
            _ => {
                trellis_codegen_ts::generate_ts_package(compiled, &fresh, name).into_diagnostic()?
            }
        }
        let mut files = BTreeSet::new();
        collect_files(&fresh, &fresh, &mut files)?;
        let mut existing = BTreeSet::new();
        if destination.exists() {
            collect_files(destination, destination, &mut existing)?;
        }
        let package_manifest = if *language == "rust" {
            "Cargo.toml"
        } else {
            "package.json"
        };
        let package_owned = existing.contains(Path::new(package_manifest))
            && generated_file(
                Path::new(package_manifest),
                &fs::read(destination.join(package_manifest)).into_diagnostic()?,
            );
        for relative in files.union(&existing) {
            let old = destination.join(relative);
            let new = fresh.join(relative);
            let previous = if old.exists() {
                Some(fs::read(&old).into_diagnostic()?)
            } else {
                None
            };
            let owned = package_owned
                && previous
                    .as_ref()
                    .is_some_and(|bytes| generated_file(relative, bytes));
            miette::ensure!(
                check || previous.is_none() || owned,
                "refusing to overwrite unrelated file {} (generated packages require an \
                explicit Trellis package marker)",
                old.display()
            );
            if new.is_file() {
                let next = fs::read(&new).into_diagnostic()?;
                if previous.as_deref() == Some(next.as_slice()) {
                    continue;
                }
            }
            changes.push((
                old,
                new,
                staging.path().join("backup").join(relative),
                previous.is_some(),
            ));
        }
        staged.push((
            destination.clone(),
            staging,
            changes.len() != previous_change_count,
        ));
    }

    if check {
        for (old, new, _, exists) in &changes {
            let kind = if !exists {
                "missing"
            } else if new.is_file() {
                "stale"
            } else {
                "extra"
            };
            eprintln!("{kind}: {}", old.display());
        }
        miette::ensure!(changes.is_empty(), "generated output is not up to date");
        return Ok(outputs.len());
    }

    let mut published = Vec::new();
    let publication = (|| -> std::io::Result<()> {
        for (destination, staging, changed) in &staged {
            if !changed {
                continue;
            }
            let fresh = staging.path().join("new");
            let backup = staging.path().join("old");
            let existed = destination.exists();
            fs::create_dir_all(destination.parent().expect("output parent"))?;
            if existed {
                fs::rename(destination, &backup)?;
            }
            published.push((destination, backup, existed));
            fs::rename(&fresh, destination)?;
        }
        Ok(())
    })();
    if let Err(error) = publication {
        let mut rollback_error = None;
        for (destination, backup, existed) in published.into_iter().rev() {
            if destination.exists() {
                if let Err(error) = fs::remove_dir_all(destination) {
                    rollback_error.get_or_insert(error);
                }
            }
            if existed {
                if let Err(error) = fs::rename(backup, destination) {
                    rollback_error.get_or_insert(error);
                }
            }
        }
        if let Some(rollback_error) = rollback_error {
            let recovery = staged
                .into_iter()
                .map(|(_, stage, _)| stage.keep().display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(miette!(
                "generation publication failed: {error}; rollback failed: \
                {rollback_error}; recoverable outputs retained at {recovery}"
            ));
        }
        return Err(error).into_diagnostic();
    }
    Ok(outputs.len())
}

/// Choose the model crates a generated Rust package may reuse.
///
/// Reused models must come from the `trellis-models` the consuming Cargo graph
/// builds, so the SDK's types are the ones `trellis-rs` exposes. The nearest
/// `Cargo.lock` above `output` names that version, and its models are read
/// from the crate source Cargo already holds. Without a lock, or without
/// `trellis-models` in it, the SDK itself brings in the CLI's version, so the
/// binding the CLI bundles applies. A resolved version whose source is not on
/// disk cannot be compared, so its models are emitted inline.
fn project_model_bindings(
    output: &Path,
    bundled: Vec<trellis_codegen_rust::RustModelBinding>,
) -> Result<Vec<trellis_codegen_rust::RustModelBinding>> {
    let bundled_version = bundled
        .iter()
        .find(|binding| binding.dependency() == "trellis-models")
        .map(|binding| binding.dependency_version().clone())
        .ok_or_else(|| miette!("the CLI bundles no trellis-models binding"))?;
    let Some(lock) = output
        .ancestors()
        .skip(1)
        .map(|directory| directory.join("Cargo.lock"))
        .find(|lock| lock.is_file())
    else {
        tracing::info!(
            "rust models: no Cargo.lock above {}; using trellis-models {bundled_version}, \
            which the CLI bundles",
            output.display()
        );
        return Ok(bundled);
    };
    let contents: toml::Value = toml::from_str(&fs::read_to_string(&lock).into_diagnostic()?)
        .map_err(|error| miette!("invalid Cargo lockfile {}: {error}", lock.display()))?;
    let packages = contents
        .get("package")
        .and_then(toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let locked = |name: &str| {
        let mut entries = packages
            .iter()
            .filter(|entry| entry.get("name").and_then(toml::Value::as_str) == Some(name))
            .filter_map(|entry| {
                let version = entry.get("version")?.as_str()?.parse::<semver::Version>();
                Some((version.ok()?, entry))
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| right.0.cmp(&left.0));
        entries
    };
    let models = locked("trellis-models");
    let runtime = locked("trellis-rs");
    // trellis-rs's entry names the trellis-models it links; Cargo adds the
    // version only when the lock holds more than one.
    let linked = runtime.first().and_then(|(runtime_version, entry)| {
        let dependency = entry
            .get("dependencies")?
            .as_array()?
            .iter()
            .filter_map(toml::Value::as_str)
            .find(|dependency| dependency.split(' ').next() == Some("trellis-models"))?;
        let version = match dependency.split(' ').nth(1) {
            Some(version) => version.parse().ok()?,
            None => models.first()?.0.clone(),
        };
        Some((version, runtime_version))
    });
    let (version, via) = match linked {
        Some((version, runtime_version)) => (version, format!(" for trellis-rs {runtime_version}")),
        None => match models.first() {
            Some((version, _)) => (version.clone(), String::new()),
            None => {
                tracing::info!(
                    "rust models: {} resolves no trellis-models; using trellis-models \
                    {bundled_version}, which the CLI bundles",
                    lock.display()
                );
                return Ok(bundled);
            }
        },
    };
    let resolved = format!("{} resolves trellis-models {version}{via}", lock.display());
    if version == bundled_version {
        tracing::info!("rust models: {resolved}, the version the CLI bundles");
        return Ok(bundled);
    }

    // Cargo's own resolution covers path overrides, vendoring, and registries,
    // but needs every member's sources, including a generated SDK not yet
    // written. The registry's unpacked sources answer for crates.io versions.
    let workspace = lock.with_file_name("Cargo.toml");
    let from_metadata =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["metadata", "--format-version", "1", "--offline", "--locked"])
            .arg("--manifest-path")
            .arg(&workspace)
            .output()
            .ok()
            .filter(|result| result.status.success())
            .and_then(|result| serde_json::from_slice::<serde_json::Value>(&result.stdout).ok())
            .and_then(|metadata| {
                metadata["packages"].as_array()?.iter().find_map(|package| {
                    (package["name"] == "trellis-models"
                        && package["version"] == version.to_string())
                    .then(|| package["manifest_path"].as_str().map(PathBuf::from))?
                })
            })
            .and_then(|manifest| Some((manifest.parent()?.to_path_buf(), "cargo metadata")));
    let from_registry = || {
        let home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cargo")))?;
        fs::read_dir(home.join("registry/src"))
            .ok()?
            .filter_map(|index| Some(index.ok()?.path().join(format!("trellis-models-{version}"))))
            .find(|source| source.join("Cargo.toml").is_file())
            .map(|source| (source, "Cargo registry sources"))
    };
    let Some((source, found_by)) = from_metadata.or_else(from_registry) else {
        tracing::info!(
            "rust models: {resolved}, whose source is not available offline (run `cargo \
            fetch`); its models cannot be compared, so they are emitted inline"
        );
        return Ok(Vec::new());
    };
    let bindings = fs::read_to_string(source.join("Cargo.toml"))
        .and_then(|manifest| {
            Ok((
                manifest,
                fs::read_to_string(source.join("generated/__types.rs"))?,
            ))
        })
        .map_err(|error| error.to_string())
        .and_then(|(manifest, types)| {
            trellis_codegen_rust::RustModelBinding::from_model_crate(&manifest, &types)
                .map_err(|error| error.to_string())
        });
    match bindings {
        Ok(bindings) => {
            tracing::info!(
                "rust models: {resolved}; read its models from {} ({found_by})",
                source.display()
            );
            Ok(bindings)
        }
        Err(error) => {
            tracing::info!(
                "rust models: {resolved}; its models in {} cannot be read ({error}), so they \
                are emitted inline",
                source.display()
            );
            Ok(Vec::new())
        }
    }
}

fn collect_files(root: &Path, directory: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).into_diagnostic()? {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        let kind = entry.file_type().into_diagnostic()?;
        miette::ensure!(
            !kind.is_symlink(),
            "generated output must not contain symlinks: {}",
            path.display()
        );
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else {
            miette::ensure!(
                kind.is_file(),
                "generated output contains a non-file: {}",
                path.display()
            );
            files.insert(
                path.strip_prefix(root)
                    .expect("output descendant")
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

fn generated_file(path: &Path, bytes: &[u8]) -> bool {
    if path == Path::new("package.json") {
        return serde_json::from_slice::<serde_json::Value>(bytes)
            .is_ok_and(|value| value["trellisGenerated"] == true);
    }
    if path == Path::new("Cargo.toml") {
        return std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(text).ok())
            .is_some_and(|value| {
                value
                    .get("package")
                    .and_then(|value| value.get("metadata"))
                    .and_then(|value| value.get("trellis"))
                    .and_then(|value| value.get("generated"))
                    .and_then(toml::Value::as_bool)
                    == Some(true)
            });
    }
    if path
        .extension()
        .is_some_and(|extension| extension == "trellis")
    {
        return false;
    }
    if path.extension().is_some_and(|extension| extension == "rs") {
        return true;
    }
    if bytes.starts_with(b"// Generated by Trellis.")
        || bytes.starts_with(b"# Generated by Trellis.")
    {
        return true;
    }
    false
}

fn watch(root: &Path) -> Result<()> {
    let (sender, receiver) = mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(200), move |events| {
        let _ = sender.send(events);
    })
    .into_diagnostic()?;
    let mut watched = BTreeSet::new();
    debouncer
        .watcher()
        .watch(root, RecursiveMode::Recursive)
        .into_diagnostic()?;
    watched.insert(root.to_path_buf());
    refresh_watch_roots(&mut debouncer, root, &mut watched);
    let generate = || match generate_once(root, false) {
        Ok(_) => {
            if let Ok(outputs) = read_manifest(&root.join("trellis.toml"))
                .and_then(|manifest| output_paths(root, &manifest))
            {
                report_outputs(root, &outputs, false);
            }
        }
        Err(error) => eprintln!("{error:?}"),
    };
    generate();
    output::hint(format!(
        "Watching {} for changes; press Ctrl-C to stop.",
        root.display()
    ));
    while let Ok(events) = receiver.recv() {
        match events {
            Ok(events) if events.iter().any(|event| relevant(&event.path)) => {
                refresh_watch_roots(&mut debouncer, root, &mut watched);
                generate();
            }
            Ok(_) => {}
            Err(error) => output::warning(format!("watching failed: {error}")),
        }
    }
    Ok(())
}

fn refresh_watch_roots(
    debouncer: &mut notify_debouncer_mini::Debouncer<
        notify_debouncer_mini::notify::RecommendedWatcher,
    >,
    root: &Path,
    watched: &mut BTreeSet<PathBuf>,
) {
    let result = read_manifest(&root.join("trellis.toml")).and_then(|manifest| {
        for path in manifest
            .dependencies
            .values()
            .filter_map(|dependency| dependency.path.as_deref())
        {
            let path = root.join(path).canonicalize().into_diagnostic()?;
            if watched.insert(path.clone()) {
                debouncer
                    .watcher()
                    .watch(&path, RecursiveMode::Recursive)
                    .into_diagnostic()?;
            }
        }
        Ok(())
    });
    if let Err(error) = result {
        eprintln!("{error:?}");
    }
}

fn relevant(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "trellis")
        || path
            .file_name()
            .is_some_and(|name| name == "trellis.toml" || name == "trellis.lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_output_must_stay_inside_the_project() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.0.0'\n",
        )
        .unwrap();
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname='fixture'\nversion='1.0.0'\n[sources]\n\
            main='contract.trellis'\n[generate.rust]\noutput='.'\n",
        )
        .unwrap();
        fs::write(root.path().join("contract.trellis"), "").unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        assert!(output_paths(root.path(), &manifest)
            .unwrap_err()
            .to_string()
            .contains("must be a descendant"));
    }

    #[test]
    fn configured_language_generates_without_its_toolchain_marker() {
        let root = tempfile::tempdir().unwrap();
        // A TypeScript app project with no Cargo.toml can still emit Rust.
        fs::write(root.path().join("package.json"), "{}").unwrap();
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname='fixture'\nversion='1.0.0'\n[sources]\n\
            main='contract.trellis'\n[generate.typescript]\noutput='ts-out'\n\
            [generate.rust]\noutput='rust-out'\n",
        )
        .unwrap();
        fs::write(root.path().join("contract.trellis"), "").unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        let outputs = output_paths(root.path(), &manifest).unwrap();
        let languages: BTreeSet<_> = outputs.iter().map(|(language, _)| *language).collect();
        assert_eq!(languages, BTreeSet::from(["rust", "typescript"]));
    }

    #[test]
    fn configured_language_generates_without_any_marker() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname='fixture'\nversion='1.0.0'\n[sources]\n\
            main='contract.trellis'\n[generate.rust]\noutput='rust-out'\n",
        )
        .unwrap();
        fs::write(root.path().join("contract.trellis"), "").unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        let outputs = output_paths(root.path(), &manifest).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "rust");
    }
}
