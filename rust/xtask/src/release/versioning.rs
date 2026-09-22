use std::fs;
use std::path::{Path, PathBuf};

use miette::{miette, IntoDiagnostic, Result, WrapErr};

use super::RELEASE_JS_INTERNAL_NPM_VERSION_FILES;

pub(super) fn check_versions(repo_root: &Path) -> Result<String> {
    let versions = collect_versions(repo_root)?;
    if versions.is_empty() {
        return Err(miette!("no release-managed Trellis versions were found"));
    }
    let expected = versions[0].version.clone();
    let mismatches: Vec<_> = versions
        .iter()
        .filter(|entry| entry.version != expected)
        .collect();
    if !mismatches.is_empty() {
        let mut message =
            format!("release-managed Trellis versions are inconsistent; expected {expected}");
        for mismatch in mismatches {
            message.push_str(&format!("\n- {} uses {}", mismatch.label, mismatch.version));
        }
        return Err(miette!(message));
    }
    Ok(expected)
}

pub(super) fn bump_versions(repo_root: &Path, from: &str, to: &str) -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    for path in release_manifest_paths(repo_root)? {
        let original = fs::read_to_string(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let updated = if is_json_manifest(&path) {
            let updated = rewrite_json_manifest_version(&original, from, to, &path)?;
            rewrite_json_manifest_internal_jsr_dependency_versions(&updated, from, to, &path)?
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            rewrite_cargo_manifest_versions(&original, from, to, &path)?
        } else if is_release_js_internal_npm_version_file(repo_root, &path) {
            rewrite_js_internal_npm_dependency_versions(&original, from, to, &path)?
        } else {
            original.clone()
        };
        if updated != original {
            fs::write(&path, updated)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write {}", path.display()))?;
            changed.push(path);
        }
    }
    Ok(changed)
}

pub(super) fn prepare_release(repo_root: &Path, release: &ReleaseVersion) -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    for path in release_manifest_paths(repo_root)? {
        let original = fs::read_to_string(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let updated = if is_json_manifest(&path) {
            let updated = rewrite_json_manifest_version_for_release(
                &original,
                &release.version,
                &release.base_version,
                &path,
            )?;
            rewrite_json_manifest_internal_jsr_dependency_versions(
                &updated,
                &release.base_version,
                &release.version,
                &path,
            )?
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            rewrite_cargo_manifest_versions_for_release(
                &original,
                &release.version,
                &release.base_version,
                &path,
            )?
        } else if is_release_js_internal_npm_version_file(repo_root, &path) {
            rewrite_js_internal_npm_dependency_versions(
                &original,
                &release.base_version,
                &release.version,
                &path,
            )?
        } else {
            original.clone()
        };
        if updated != original {
            fs::write(&path, updated)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write {}", path.display()))?;
            changed.push(path);
        }
    }
    Ok(changed)
}

pub(super) fn collect_versions(repo_root: &Path) -> Result<Vec<VersionEntry>> {
    let mut versions = Vec::new();
    for path in release_manifest_paths(repo_root)? {
        let contents = fs::read_to_string(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;
        if is_json_manifest(&path) {
            if let Some(version) = json_manifest_version(&contents) {
                if !is_non_release_sentinel_version(&version) {
                    versions.push(VersionEntry::new(
                        display_repo_path(repo_root, &path),
                        version,
                    ));
                }
            }
            collect_json_internal_jsr_dependency_versions(
                repo_root,
                &path,
                &contents,
                &mut versions,
            );
            continue;
        }

        if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            collect_cargo_versions(repo_root, &path, &contents, &mut versions);
            continue;
        }

        if is_release_js_internal_npm_version_file(repo_root, &path) {
            collect_js_internal_npm_versions(repo_root, &path, &contents, &mut versions);
        }
    }
    Ok(versions)
}

fn release_manifest_paths(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_manifest_paths(&repo_root.join("generated"), &mut paths)?;
    collect_manifest_paths(&repo_root.join("ts"), &mut paths)?;
    collect_manifest_paths(&repo_root.join("rust"), &mut paths)?;
    for relative_path in RELEASE_JS_INTERNAL_NPM_VERSION_FILES {
        let path = repo_root.join(relative_path);
        if path.exists() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn is_release_js_internal_npm_version_file(repo_root: &Path, path: &Path) -> bool {
    path.strip_prefix(repo_root)
        .ok()
        .and_then(Path::to_str)
        .is_some_and(|relative| RELEASE_JS_INTERNAL_NPM_VERSION_FILES.contains(&relative))
}

fn collect_manifest_paths(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read directory {}", dir.display()))?
    {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type().into_diagnostic()?.is_dir() {
            if matches!(
                name.as_ref(),
                ".git" | "node_modules" | ".svelte-kit" | "target"
            ) {
                continue;
            }
            collect_manifest_paths(&path, paths)?;
            continue;
        }
        if is_json_manifest(&path) || path.file_name().is_some_and(|file| file == "Cargo.toml") {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_json_manifest(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        matches!(
            name.to_string_lossy().as_ref(),
            "deno.json" | "deno.npm.json" | "package.json"
        )
    })
}

fn json_manifest_version(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if !line.contains("\"version\"") {
            continue;
        }
        let colon = line.find(':')?;
        let after_colon = &line[colon + 1..];
        let start = after_colon.find('"')? + colon + 2;
        let end = line[start..].find('"')? + start;
        return Some(line[start..end].to_string());
    }
    None
}

pub(super) fn rewrite_json_manifest_version(
    contents: &str,
    from: &str,
    to: &str,
    path: &Path,
) -> Result<String> {
    let Some(version) = json_manifest_version(contents) else {
        return Ok(contents.to_string());
    };
    if is_non_release_sentinel_version(&version) {
        return Ok(contents.to_string());
    }
    if version == to {
        return Ok(contents.to_string());
    }
    if version != from {
        return Err(miette!(
            "{} uses version {}, expected {from} or {to}",
            path.display(),
            version
        ));
    }
    Ok(replace_first_version_literal(contents, &version, to))
}

pub(super) fn rewrite_json_manifest_version_for_release(
    contents: &str,
    release_version: &str,
    expected_base_version: &str,
    path: &Path,
) -> Result<String> {
    let Some(version) = json_manifest_version(contents) else {
        return Ok(contents.to_string());
    };
    if is_non_release_sentinel_version(&version) {
        return Ok(contents.to_string());
    }
    let actual_base_version = version_base(&version)?;
    if actual_base_version != expected_base_version {
        return Err(miette!(
            "{} uses version {}, but release tag requires base version {expected_base_version}",
            path.display(),
            version
        ));
    }
    if version == release_version {
        return Ok(contents.to_string());
    }
    Ok(replace_first_version_literal(
        contents,
        &version,
        release_version,
    ))
}

fn collect_js_internal_npm_versions(
    repo_root: &Path,
    path: &Path,
    contents: &str,
    versions: &mut Vec<VersionEntry>,
) {
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some((name, spec)) = json_like_string_property(trimmed) else {
            continue;
        };
        if !is_internal_npm_package(&name) {
            continue;
        }
        if let Some(version) = npm_dependency_spec_version(&spec) {
            versions.push(VersionEntry::new(
                format!("{} dependency {name}", display_repo_path(repo_root, path)),
                version,
            ));
        }
    }
}

pub(super) fn rewrite_js_internal_npm_dependency_versions(
    contents: &str,
    from: &str,
    to: &str,
    path: &Path,
) -> Result<String> {
    let mut lines = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some((name, spec)) = json_like_string_property(trimmed) else {
            lines.push(line.to_string());
            continue;
        };
        if !is_internal_npm_package(&name) {
            lines.push(line.to_string());
            continue;
        }

        let Some(version) = npm_dependency_spec_version(&spec) else {
            lines.push(line.to_string());
            continue;
        };
        if version != from {
            if version == to {
                lines.push(line.to_string());
                continue;
            }
            return Err(miette!(
                "{} dependency {name} uses version {}, expected {from} or {to}",
                path.display(),
                version
            ));
        }

        let replacement = replace_npm_dependency_spec_version(&spec, to);
        lines.push(line.replacen(&format!("\"{spec}\""), &format!("\"{replacement}\""), 1));
    }
    let mut updated = lines.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }
    Ok(updated)
}

fn collect_json_internal_jsr_dependency_versions(
    repo_root: &Path,
    path: &Path,
    contents: &str,
    versions: &mut Vec<VersionEntry>,
) {
    for line in contents.lines() {
        let Some((_, spec)) = json_like_string_property(line.trim()) else {
            continue;
        };
        let Some(dependency) = internal_jsr_dependency_spec(&spec) else {
            continue;
        };
        versions.push(VersionEntry::new(
            format!(
                "{} dependency {}",
                display_repo_path(repo_root, path),
                dependency.name
            ),
            dependency.version,
        ));
    }
}

pub(super) fn rewrite_json_manifest_internal_jsr_dependency_versions(
    contents: &str,
    from: &str,
    to: &str,
    path: &Path,
) -> Result<String> {
    let mut lines = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some((_, spec)) = json_like_string_property(trimmed) else {
            lines.push(line.to_string());
            continue;
        };
        let Some(dependency) = internal_jsr_dependency_spec(&spec) else {
            lines.push(line.to_string());
            continue;
        };

        if dependency.version != from {
            if dependency.version == to {
                lines.push(line.to_string());
                continue;
            }
            return Err(miette!(
                "{} dependency {} uses version {}, expected {from} or {to}",
                path.display(),
                dependency.name,
                dependency.version
            ));
        }

        let replacement = replace_npm_dependency_spec_version(&dependency.version_spec, to);
        lines.push(line.replacen(&dependency.version_spec, &replacement, 1));
    }
    let mut updated = lines.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }
    Ok(updated)
}

fn internal_jsr_dependency_spec(spec: &str) -> Option<InternalJsrDependency> {
    let spec = spec.strip_prefix("jsr:")?;
    internal_js_package_names().iter().find_map(|name| {
        let rest = spec.strip_prefix(name)?;
        let version_and_path = rest.strip_prefix('@')?;
        let version_spec = version_and_path
            .split_once('/')
            .map(|(version, _)| version)
            .unwrap_or(version_and_path);
        let version = npm_dependency_spec_version(version_spec)?;
        Some(InternalJsrDependency {
            name: (*name).to_string(),
            version,
            version_spec: version_spec.to_string(),
        })
    })
}

fn internal_js_package_names() -> &'static [&'static str] {
    &[
        "@qlever-llc/result",
        "@qlever-llc/trellis",
        "@qlever-llc/trellis-svelte",
        "@qlever-llc/trellis-test",
    ]
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct InternalJsrDependency {
    name: String,
    version: String,
    version_spec: String,
}

fn json_like_string_property(trimmed: &str) -> Option<(String, String)> {
    let property_start = trimmed.find("\"@qlever-llc/")?;
    let rest = trimmed[property_start..].strip_prefix('"')?;
    let (name, after_name) = rest.split_once('"')?;
    let after_colon = after_name.trim_start().strip_prefix(':')?.trim_start();
    let value_rest = after_colon.strip_prefix('"')?;
    let (value, _) = value_rest.split_once('"')?;
    Some((name.to_string(), value.to_string()))
}

fn npm_dependency_spec_version(spec: &str) -> Option<String> {
    let version = spec.strip_prefix(['^', '~']).unwrap_or(spec);
    version_base(version).ok()?;
    Some(version.to_string())
}

fn replace_npm_dependency_spec_version(spec: &str, version: &str) -> String {
    let prefix = spec
        .chars()
        .next()
        .filter(|ch| matches!(ch, '^' | '~'))
        .map(|ch| ch.to_string())
        .unwrap_or_default();
    format!("{prefix}{version}")
}

fn replace_first_version_literal(contents: &str, from: &str, to: &str) -> String {
    let target = format!("\"version\": \"{from}\"");
    let replacement = format!("\"version\": \"{to}\"");
    if contents.contains(&target) {
        return contents.replacen(&target, &replacement, 1);
    }
    contents.replacen(
        &format!("\"version\":\"{from}\""),
        &format!("\"version\":\"{to}\""),
        1,
    )
}

fn collect_cargo_versions(
    repo_root: &Path,
    path: &Path,
    contents: &str,
    versions: &mut Vec<VersionEntry>,
) {
    let package_name = cargo_package_name(contents);
    let mut section = CargoSection::Other;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = match trimmed {
                "[package]" => CargoSection::Package,
                "[workspace.package]" => CargoSection::WorkspacePackage,
                _ => CargoSection::Other,
            };
            continue;
        }
        if matches!(section, CargoSection::WorkspacePackage) {
            if let Some(version) = cargo_version_assignment(trimmed) {
                if is_non_release_sentinel_version(&version) {
                    continue;
                }
                versions.push(VersionEntry::new(
                    "rust workspace version".to_string(),
                    version,
                ));
            }
        }
        if matches!(section, CargoSection::Package)
            && package_name.as_deref().is_some_and(is_internal_rust_crate)
        {
            if let Some(version) = cargo_version_assignment(trimmed) {
                if is_non_release_sentinel_version(&version) {
                    continue;
                }
                versions.push(VersionEntry::new(
                    format!("{} package version", display_repo_path(repo_root, path)),
                    version,
                ));
            }
        }
        if let Some((name, version)) = cargo_inline_dependency_version(trimmed) {
            if is_internal_rust_crate(&name) {
                versions.push(VersionEntry::new(
                    format!("{} dependency {name}", display_repo_path(repo_root, path)),
                    version,
                ));
            }
        }
    }
}

pub(super) fn rewrite_cargo_manifest_versions(
    contents: &str,
    from: &str,
    to: &str,
    path: &Path,
) -> Result<String> {
    let package_name = cargo_package_name(contents);
    let mut section = CargoSection::Other;
    let mut lines = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = match trimmed {
                "[package]" => CargoSection::Package,
                "[workspace.package]" => CargoSection::WorkspacePackage,
                _ => CargoSection::Other,
            };
            lines.push(line.to_string());
            continue;
        }

        let should_update_package_version = matches!(section, CargoSection::WorkspacePackage)
            || (matches!(section, CargoSection::Package)
                && package_name.as_deref().is_some_and(is_internal_rust_crate));
        if should_update_package_version {
            if let Some(version) = cargo_version_assignment(trimmed) {
                if is_non_release_sentinel_version(&version) {
                    lines.push(line.to_string());
                    continue;
                }
                if version != from {
                    if version == to {
                        lines.push(line.to_string());
                        continue;
                    }
                    return Err(miette!(
                        "{} uses version {}, expected {from} or {to}",
                        path.display(),
                        version
                    ));
                }
                lines.push(line.replacen(&format!("\"{from}\""), &format!("\"{to}\""), 1));
                continue;
            }
        }

        if let Some((name, version)) = cargo_inline_dependency_version(trimmed) {
            if is_internal_rust_crate(&name) {
                if version != from {
                    if version == to {
                        lines.push(line.to_string());
                        continue;
                    }
                    return Err(miette!(
                        "{} dependency {name} uses version {}, expected {from} or {to}",
                        path.display(),
                        version
                    ));
                }
                lines.push(line.replacen(
                    &format!("version = \"{from}\""),
                    &format!("version = \"{to}\""),
                    1,
                ));
                continue;
            }
        }
        lines.push(line.to_string());
    }
    let mut updated = lines.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }
    Ok(updated)
}

pub(super) fn rewrite_cargo_manifest_versions_for_release(
    contents: &str,
    release_version: &str,
    expected_base_version: &str,
    path: &Path,
) -> Result<String> {
    let package_name = cargo_package_name(contents);
    let mut section = CargoSection::Other;
    let mut lines = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = match trimmed {
                "[package]" => CargoSection::Package,
                "[workspace.package]" => CargoSection::WorkspacePackage,
                _ => CargoSection::Other,
            };
            lines.push(line.to_string());
            continue;
        }

        let should_update_package_version = matches!(section, CargoSection::WorkspacePackage)
            || (matches!(section, CargoSection::Package)
                && package_name.as_deref().is_some_and(is_internal_rust_crate));
        if should_update_package_version {
            if let Some(version) = cargo_version_assignment(trimmed) {
                if is_non_release_sentinel_version(&version) {
                    lines.push(line.to_string());
                    continue;
                }
                require_version_base(&version, expected_base_version, path, "version")?;
                lines.push(line.replacen(
                    &format!("\"{version}\""),
                    &format!("\"{release_version}\""),
                    1,
                ));
                continue;
            }
        }

        if let Some((name, version)) = cargo_inline_dependency_version(trimmed) {
            if is_internal_rust_crate(&name) {
                require_version_base(
                    &version,
                    expected_base_version,
                    path,
                    &format!("dependency {name}"),
                )?;
                lines.push(line.replacen(
                    &format!("version = \"{version}\""),
                    &format!("version = \"{release_version}\""),
                    1,
                ));
                continue;
            }
        }
        lines.push(line.to_string());
    }
    let mut updated = lines.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }
    Ok(updated)
}

fn require_version_base(
    version: &str,
    expected_base_version: &str,
    path: &Path,
    label: &str,
) -> Result<()> {
    let actual_base_version = version_base(version)?;
    if actual_base_version == expected_base_version {
        Ok(())
    } else {
        Err(miette!(
            "{} {label} uses version {version}, but release tag requires base version {expected_base_version}",
            path.display()
        ))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum CargoSection {
    Package,
    WorkspacePackage,
    Other,
}

fn cargo_package_name(contents: &str) -> Option<String> {
    let mut in_package = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            return quoted_value_after_equals(trimmed);
        }
    }
    None
}

fn cargo_version_assignment(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("version") || trimmed.contains("workspace") {
        return None;
    }
    quoted_value_after_equals(trimmed)
}

fn cargo_inline_dependency_version(trimmed: &str) -> Option<(String, String)> {
    let (name, rest) = trimmed.split_once('=')?;
    if !rest.contains("version") {
        return None;
    }
    let version_index = rest.find("version")?;
    let version_rest = &rest[version_index..];
    Some((
        name.trim().to_string(),
        quoted_value_after_equals(version_rest)?,
    ))
}

fn quoted_value_after_equals(value: &str) -> Option<String> {
    let equals = value.find('=')?;
    let rest = &value[equals + 1..];
    let start = rest.find('"')? + equals + 2;
    let end = value[start..].find('"')? + start;
    Some(value[start..end].to_string())
}

fn is_internal_rust_crate(name: &str) -> bool {
    name.starts_with("trellis-")
}

fn is_internal_npm_package(name: &str) -> bool {
    matches!(
        name,
        "@qlever-llc/result" | "@qlever-llc/trellis" | "@qlever-llc/trellis-svelte"
    )
}

pub(super) fn require_stable_version(version: &str, label: &str) -> Result<()> {
    if is_stable_semver(version) {
        Ok(())
    } else {
        Err(miette!(
            "{label} must be a stable semver version like 0.9.0"
        ))
    }
}

pub(super) fn parse_release_tag(tag: &str) -> Result<ReleaseVersion> {
    let tag = tag.trim();
    let Some(version) = tag.strip_prefix('v') else {
        return Err(miette!(
            "invalid release tag `{tag}`; expected a tag like v0.9.0 or v0.9.0-rc.1"
        ));
    };
    let base_version = version_base(version)?;
    Ok(ReleaseVersion {
        version: version.to_string(),
        base_version,
    })
}

pub(super) fn version_base(version: &str) -> Result<String> {
    let version = version.trim();
    let suffix_start = [version.find('-'), version.find('+')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(version.len());
    let base = &version[..suffix_start];
    if is_stable_semver(base) {
        if !version
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'))
        {
            return Err(miette!("invalid release version `{version}`"));
        }
        Ok(base.to_string())
    } else {
        Err(miette!("invalid release version `{version}`"))
    }
}

pub(super) fn write_github_env(name: &str, value: &str) -> Result<()> {
    let Some(path) = std::env::var_os("GITHUB_ENV") else {
        return Ok(());
    };
    if path.is_empty() {
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .into_diagnostic()
        .wrap_err("failed to open GITHUB_ENV")?;
    use std::io::Write;
    writeln!(file, "{name}={value}")
        .into_diagnostic()
        .wrap_err("failed to write GITHUB_ENV")
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct ReleaseVersion {
    pub(super) version: String,
    pub(super) base_version: String,
}

fn is_stable_semver(version: &str) -> bool {
    let mut parts = version.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [major, minor, patch]
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

pub(super) fn is_non_release_sentinel_version(version: &str) -> bool {
    version == "0.0.0" || version.starts_with("0.0.0-")
}

pub(super) fn display_repo_path(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct VersionEntry {
    pub(super) label: String,
    pub(super) version: String,
}

impl VersionEntry {
    fn new(label: String, version: String) -> Self {
        Self { label, version }
    }
}
