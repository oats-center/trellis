//! Native Trellis package generation and offline filesystem watch orchestration.

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
pub fn run(args: &GenerateArgs) -> Result<()> {
    let root = args.project.root.canonicalize().into_diagnostic()?;
    if args.watch {
        let manifest = read_manifest(&root.join("trellis.toml"))?;
        output_paths(&root, &manifest)?;
        watch(&root)
    } else {
        generate_once(&root, args.check).map(|_| ())
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
    let compiled = crate::package::compile_project(&root, &manifest)?;
    generate_compiled(&root, &manifest, &compiled, check)
}

pub(crate) fn output_paths(
    root: &Path,
    manifest: &ProjectManifest,
) -> Result<Vec<(&'static str, PathBuf)>> {
    let root = root.canonicalize().into_diagnostic()?;
    let has_rust = root.join("Cargo.toml").is_file();
    let has_ts = ["package.json", "deno.json", "deno.jsonc"]
        .iter()
        .any(|file| root.join(file).is_file());
    let config = &manifest.generate;
    // A language generates when it is configured or its toolchain marker is
    // present. Configuration alone is enough: a TypeScript app participant can
    // emit a Rust projection for a Rust test harness, and vice versa.
    let targets = [
        ("rust", has_rust, config.rust.as_ref()),
        ("typescript", has_ts, config.typescript.as_ref()),
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
            && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !name.ends_with('-') && !name.contains("--"),
        "invalid generated package name '{name}'; use at most 214 characters: a lowercase ASCII letter followed by lowercase letters, digits, or single hyphens"
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
                    "multiple languages detected; configure [generate.rust].output and [generate.typescript].output"
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
            "rust" => trellis_codegen_rust::generate_rust_package(compiled, &fresh, name)
                .into_diagnostic()?,
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
                "refusing to overwrite unrelated file {} (generated packages require an explicit Trellis package marker)",
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
            return Err(miette!("generation publication failed: {error}; rollback failed: {rollback_error}; recoverable outputs retained at {recovery}"));
        }
        return Err(error).into_diagnostic();
    }
    Ok(outputs.len())
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
    if let Err(error) = generate_once(root, false) {
        eprintln!("{error:?}");
    }
    while let Ok(events) = receiver.recv() {
        match events {
            Ok(events) if events.iter().any(|event| relevant(&event.path)) => {
                refresh_watch_roots(&mut debouncer, root, &mut watched);
                if let Err(error) = generate_once(root, false) {
                    eprintln!("{error:?}");
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("{error}"),
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
            "[package]\nname='fixture'\nversion='1.0.0'\n[sources]\nmain='contract.trellis'\n[generate.rust]\noutput='.'\n",
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
            "[package]\nname='fixture'\nversion='1.0.0'\n[sources]\nmain='contract.trellis'\n[generate.typescript]\noutput='ts-out'\n[generate.rust]\noutput='rust-out'\n",
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
            "[package]\nname='fixture'\nversion='1.0.0'\n[sources]\nmain='contract.trellis'\n[generate.rust]\noutput='rust-out'\n",
        )
        .unwrap();
        fs::write(root.path().join("contract.trellis"), "").unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        let outputs = output_paths(root.path(), &manifest).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "rust");
    }

    #[test]
    fn multiple_targets_require_explicit_outputs() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("package.json"), "{}").unwrap();
        fs::write(
            root.path().join("trellis.toml"),
            "[package]\nname='fixture'\nversion='1.0.0'\n[sources]\nmain='contract.trellis'\n[generate.rust]\noutput='rust-out'\n",
        )
        .unwrap();
        fs::write(root.path().join("contract.trellis"), "").unwrap();
        let manifest = read_manifest(&root.path().join("trellis.toml")).unwrap();
        assert!(output_paths(root.path(), &manifest)
            .unwrap_err()
            .to_string()
            .contains("multiple languages detected"));
    }
}
