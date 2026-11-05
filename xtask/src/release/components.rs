//! Versions of the components that are not released at the release's own
//! version every time: `trellis-protocol`, `trellis-models` and the built-in
//! contracts. Each keeps its version until what it would publish changes, then
//! takes the release's version. Registries never replace a published version,
//! so a changed component published under its old version would silently
//! never reach anyone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use miette::{miette, IntoDiagnostic, Result, WrapErr};

use super::package::{published_differences, PUBLIC_CRATES};

/// A component and the manifest that holds its version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Component {
    pub(super) name: &'static str,
    pub(super) manifest: &'static str,
}

pub(super) const PROTOCOL: Component = Component {
    name: "trellis-protocol",
    manifest: "crates/protocol/Cargo.toml",
};
pub(super) const MODELS: Component = Component {
    name: "trellis-models",
    manifest: "crates/models/Cargo.toml",
};
pub(super) const CONTRACTS: Component = Component {
    name: "trellis",
    manifest: "crates/builtin-contracts/trellis.toml",
};

const USER_AGENT: &str = "trellis-release (https://github.com/oats-center/trellis)";

/// The components whose contents differ from their published version.
///
/// The models are generated from the built-in contracts, embed their
/// version, and depend on the protocol, so either of those changing changes
/// the models too. The models comparison uses the generated files on disk;
/// the Pipeline's `check-published` repeats it against freshly generated ones.
pub(super) fn changed_components(root: &Path) -> Result<Vec<Component>> {
    let versions = crate_versions(root)?;
    let contracts = contracts_changed(root, &contracts_version(root)?)?;
    let protocol = crate_changed(root, PROTOCOL.name, &versions[PROTOCOL.name])?;
    let models = contracts || protocol || crate_changed(root, MODELS.name, &versions[MODELS.name])?;
    Ok([
        (CONTRACTS, contracts),
        (PROTOCOL, protocol),
        (MODELS, models),
    ]
    .into_iter()
    .filter_map(|(component, changed)| changed.then_some(component))
    .collect())
}

/// Require each packaged public crate whose version crates.io already has to
/// carry the same contents as the published one.
pub(super) fn check_published(root: &Path, packages: &Path) -> Result<()> {
    let versions = crate_versions(root)?;
    let downloads = tempfile::tempdir().into_diagnostic()?;
    let mut failures = Vec::new();
    for name in PUBLIC_CRATES {
        let version = &versions[name];
        let packaged = packages.join(format!("{name}-{version}.crate"));
        miette::ensure!(
            packaged.is_file(),
            "{} is missing; package the public crates first",
            packaged.display()
        );
        let Some(published) = download_published_crate(name, version, downloads.path())? else {
            println!("{name} {version} is not published yet");
            continue;
        };
        let differences = published_differences(&published, &packaged)?;
        if differences.is_empty() {
            println!("{name} {version} matches the published crate");
        } else {
            failures.push(format!(
                "{name} {version} is already on crates.io with different contents ({})",
                differences.join(", ")
            ));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    Err(miette!(
        "{}\nRun `cargo xtask release prepare --tag <tag>` so a changed component takes the release's version.",
        failures.join("\n")
    ))
}

/// Whether the built-in contract package differs from what release
/// `v<version>` published. The package is its manifest, lock and `.trellis`
/// sources, all tracked; the crate that embeds them is not part of it.
pub(super) fn contracts_changed(root: &Path, version: &str) -> Result<bool> {
    let tag = format!("v{version}");
    let tagged = Command::new("git")
        .args(["rev-parse", "--quiet", "--verify"])
        .arg(format!("refs/tags/{tag}^{{commit}}"))
        .current_dir(root)
        .output()
        .into_diagnostic()?;
    if !tagged.status.success() {
        println!(
            "No local tag {tag} records what the built-in contracts {version} published; \
            treating them as changed (fetch tags to compare)"
        );
        return Ok(true);
    }
    let diff = Command::new("git")
        .args(["diff", "--quiet", &tag, "--"])
        .args([
            "crates/builtin-contracts/trellis.toml",
            "crates/builtin-contracts/trellis.lock",
            ":(glob)crates/builtin-contracts/**/*.trellis",
        ])
        .current_dir(root)
        .status()
        .into_diagnostic()?;
    match diff.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(miette!("git diff against {tag} failed")),
    }
}

/// Whether a crate's package differs from its published version; an
/// unpublished version has nothing to differ from.
fn crate_changed(root: &Path, name: &str, version: &str) -> Result<bool> {
    let work = tempfile::tempdir().into_diagnostic()?;
    let Some(published) = download_published_crate(name, version, work.path())? else {
        return Ok(false);
    };
    let target = work.path().join("target");
    let status = Command::new("cargo")
        .args(["package", "--quiet", "--manifest-path", "Cargo.toml"])
        .args(["--allow-dirty", "--locked", "--no-verify"])
        .args(["--config", ".cargo/publication.toml", "-p", name])
        .arg("--target-dir")
        .arg(&target)
        .current_dir(root)
        .status()
        .into_diagnostic()?;
    miette::ensure!(status.success(), "cargo package -p {name} failed");
    let packaged = target.join(format!("package/{name}-{version}.crate"));
    let differences = published_differences(&published, &packaged)?;
    if !differences.is_empty() {
        println!(
            "{name} {version} changed since it was published: {}",
            differences.join(", ")
        );
    }
    Ok(!differences.is_empty())
}

/// Download `name` `version` from crates.io into `directory`, or `None` when
/// that version is not published.
fn download_published_crate(
    name: &str,
    version: &str,
    directory: &Path,
) -> Result<Option<PathBuf>> {
    // crates.io refuses API requests without a User-Agent (403), which must
    // never read as unpublished.
    let status = Command::new("curl")
        .args(["--silent", "--show-error", "--output", "/dev/null"])
        .args(["--write-out", "%{http_code}", "--user-agent", USER_AGENT])
        .arg(format!("https://crates.io/api/v1/crates/{name}/{version}"))
        .output()
        .into_diagnostic()?;
    match String::from_utf8_lossy(&status.stdout).trim() {
        "404" => return Ok(None),
        "200" => {}
        code => {
            return Err(miette!(
                "crates.io answered {code} for {name} {version}: {}",
                String::from_utf8_lossy(&status.stderr).trim()
            ))
        }
    }
    let path = directory.join(format!("published-{name}-{version}.crate"));
    let download = Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--location"])
        .args(["--user-agent", USER_AGENT, "--output"])
        .arg(&path)
        .arg(format!(
            "https://crates.io/api/v1/crates/{name}/{version}/download"
        ))
        .status()
        .into_diagnostic()?;
    miette::ensure!(download.success(), "downloading {name} {version} failed");
    Ok(Some(path))
}

/// The version of every public crate, as Cargo resolves the workspace.
fn crate_versions(root: &Path) -> Result<BTreeMap<String, String>> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .args(["--manifest-path", "Cargo.toml"])
        .current_dir(root)
        .output()
        .into_diagnostic()?;
    miette::ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).into_diagnostic()?;
    let versions = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|package| {
            let name = package["name"].as_str()?;
            PUBLIC_CRATES
                .contains(&name)
                .then(|| Some((name.to_owned(), package["version"].as_str()?.to_owned())))?
        })
        .collect::<BTreeMap<_, _>>();
    for name in PUBLIC_CRATES {
        miette::ensure!(versions.contains_key(name), "the workspace has no {name}");
    }
    Ok(versions)
}

fn contracts_version(root: &Path) -> Result<String> {
    let path = root.join(CONTRACTS.manifest);
    std::fs::read_to_string(&path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {}", path.display()))?
        .parse::<toml_edit::DocumentMut>()
        .into_diagnostic()?["package"]["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| miette!("{} has no package version", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn released_contracts() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let contracts = root.path().join("crates/builtin-contracts");
        fs::create_dir_all(contracts.join("src/builtin_source")).unwrap();
        fs::write(
            contracts.join("trellis.toml"),
            "[package]\nname = \"trellis\"\nversion = \"0.100.0-rc.20\"\n",
        )
        .unwrap();
        fs::write(
            contracts.join("Cargo.toml"),
            "version = \"0.100.0-rc.20\"\n",
        )
        .unwrap();
        fs::write(
            contracts.join("src/builtin_source/auth.trellis"),
            "api auth@v1 {}\n",
        )
        .unwrap();
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-q", "-m", "release"]);
        git(root.path(), &["tag", "v0.100.0-rc.20"]);
        root
    }

    #[test]
    fn contracts_are_unchanged_when_only_their_crate_moved_on() {
        let root = released_contracts();
        fs::write(
            root.path().join("crates/builtin-contracts/Cargo.toml"),
            "version = \"0.100.0-rc.21\"\n",
        )
        .unwrap();
        assert!(!contracts_changed(root.path(), "0.100.0-rc.20").unwrap());
    }

    #[test]
    fn an_edited_source_changes_the_contracts() {
        let root = released_contracts();
        fs::write(
            root.path()
                .join("crates/builtin-contracts/src/builtin_source/auth.trellis"),
            "api auth@v1 { rpc Me {} }\n",
        )
        .unwrap();
        assert!(contracts_changed(root.path(), "0.100.0-rc.20").unwrap());
    }

    #[test]
    fn contracts_without_a_release_tag_count_as_changed() {
        let root = released_contracts();
        assert!(contracts_changed(root.path(), "0.100.0-rc.19").unwrap());
    }
}
