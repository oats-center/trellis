//! CLI project-file writers and shared package model exports.

use std::{fs, path::Path};

use miette::{miette, IntoDiagnostic, Result};

pub(crate) use trellis_idl::project::{
    read_lock, read_manifest, Dependency as PackageDependency, LockedDependency, LockedPackage,
    LockedSource, PackageLock as ProjectLock, PackageManifest as ProjectManifest, RegistryConfig,
};

pub(crate) fn write_lock(path: &Path, lock: &ProjectLock) -> Result<()> {
    lock.validate()?;
    write_atomic_if_changed(path, toml::to_string(lock).into_diagnostic()?.as_bytes())
}

pub(crate) fn write_manifest_and_lock(
    manifest_path: &Path,
    manifest: &ProjectManifest,
    manifest_source: Option<&[u8]>,
    lock_path: &Path,
    lock: &ProjectLock,
) -> Result<()> {
    manifest.validate()?;
    lock.validate()?;
    let manifest_bytes = manifest_source
        .map(ToOwned::to_owned)
        .unwrap_or(toml::to_string(manifest).into_diagnostic()?.into_bytes());
    let lock_bytes = toml::to_string(lock).into_diagnostic()?.into_bytes();
    let parent = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).into_diagnostic()?;
    let old_manifest = fs::read(manifest_path).ok();
    let mut manifest_temp = tempfile::NamedTempFile::new_in(parent).into_diagnostic()?;
    let mut lock_temp = tempfile::NamedTempFile::new_in(parent).into_diagnostic()?;
    std::io::Write::write_all(&mut manifest_temp, &manifest_bytes).into_diagnostic()?;
    std::io::Write::write_all(&mut lock_temp, &lock_bytes).into_diagnostic()?;
    manifest_temp.as_file().sync_all().into_diagnostic()?;
    lock_temp.as_file().sync_all().into_diagnostic()?;
    manifest_temp
        .persist(manifest_path)
        .map_err(|error| miette!(error))?;
    if let Err(error) = lock_temp.persist(lock_path) {
        match old_manifest {
            Some(bytes) => write_atomic_if_changed(manifest_path, &bytes)?,
            None => {
                let _ = fs::remove_file(manifest_path);
            }
        }
        return Err(miette!(error));
    }
    Ok(())
}

pub(crate) fn restore_project_files(
    manifest_path: &Path,
    manifest: &[u8],
    lock_path: &Path,
    lock: Option<&[u8]>,
) -> Result<()> {
    write_atomic_if_changed(manifest_path, manifest)?;
    match lock {
        Some(bytes) => write_atomic_if_changed(lock_path, bytes),
        None if lock_path.exists() => fs::remove_file(lock_path).into_diagnostic(),
        None => Ok(()),
    }
}

fn write_atomic_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).into_diagnostic()?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).into_diagnostic()?;
    std::io::Write::write_all(&mut temporary, bytes).into_diagnostic()?;
    temporary.as_file().sync_all().into_diagnostic()?;
    temporary.persist(path).map_err(|error| miette!(error))?;
    Ok(())
}
