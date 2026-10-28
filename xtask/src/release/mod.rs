use std::path::{Path, PathBuf};

#[cfg(test)]
use clap::Parser;
use clap::Subcommand;
use miette::{miette, IntoDiagnostic, Result};

mod changelog;
mod github;
mod package;
mod runner;
mod versioning;

use changelog::{check_changelog, write_release_notes};
use github::run_pretag_check;
use versioning::{
    bump_versions, check_versions, display_repo_path, parse_release_tag, prepare_release,
    require_stable_version, version_base, write_github_env,
};

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub(crate) enum ReleaseCommand {
    #[command(name = "check-versions")]
    CheckVersions,
    /// Require a publication archive to match the one Check accepted.
    #[command(name = "compare-crate")]
    CompareCrate {
        accepted: PathBuf,
        publication: PathBuf,
    },
    /// Write the testkit bundle's artifact manifest and pack the bundle.
    #[command(name = "stage-testkit-bundle")]
    StageTestkitBundle {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        source_sha: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        version: String,
    },
    #[command(name = "prepare")]
    Prepare {
        #[arg(long)]
        tag: Option<String>,
        /// Publish a new protocol implementation instead of retaining its current version.
        #[arg(long)]
        protocol_version: Option<String>,
        /// Publish a new model bundle instead of retaining its current version.
        #[arg(long)]
        models_version: Option<String>,
        /// Advance native built-in contracts; requires an explicitly selected model bundle.
        #[arg(long, requires = "models_version")]
        contracts_version: Option<String>,
    },
    #[command(name = "bump")]
    Bump {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    #[command(name = "changelog-check")]
    ChangelogCheck {
        #[arg(long)]
        version: String,
        #[arg(long)]
        since: Option<String>,
    },
    #[command(name = "write-notes")]
    WriteNotes {
        #[arg(long)]
        tag: String,
        #[arg(long)]
        output: PathBuf,
    },
    #[command(name = "check-metadata")]
    CheckMetadata {
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        since: Option<String>,
    },
    #[command(name = "pretag-check")]
    PretagCheck {
        #[arg(long)]
        tag: String,
        #[arg(long = "ref", default_value = "main", value_parser = normalize_git_ref)]
        git_ref: String,
    },
}

fn normalize_git_ref(value: &str) -> std::result::Result<String, String> {
    if value.trim().is_empty() {
        Ok("main".to_string())
    } else {
        Ok(value.to_string())
    }
}

#[cfg(test)]
#[derive(Debug, Parser)]
#[command(name = "release")]
struct ReleaseCli {
    #[command(subcommand)]
    command: ReleaseCommand,
}

#[cfg(test)]
pub(crate) fn parse_release_command<I>(args: I) -> Result<ReleaseCommand>
where
    I: Iterator<Item = String>,
{
    let argv = std::iter::once("release".to_string()).chain(args);
    let command = ReleaseCli::try_parse_from(argv)
        .map_err(|error| miette!("{error}"))?
        .command;
    validate_release_command(&command)?;
    Ok(command)
}

pub(crate) fn validate_release_command(command: &ReleaseCommand) -> Result<()> {
    if let ReleaseCommand::PretagCheck { tag, .. } = command {
        parse_release_tag(tag)?;
    }
    Ok(())
}

pub(crate) fn run_release(repo_root: &Path, command: ReleaseCommand) -> Result<()> {
    match command {
        ReleaseCommand::CheckVersions => {
            let version = check_versions(repo_root)?;
            println!("All release-managed Trellis versions are {version}.");
            Ok(())
        }
        ReleaseCommand::CompareCrate {
            accepted,
            publication,
        } => package::compare_crate(&accepted, &publication),
        ReleaseCommand::StageTestkitBundle {
            bundle,
            output,
            source_sha,
            target,
            version,
        } => package::stage_testkit_bundle(&bundle, &output, &source_sha, &target, &version),
        ReleaseCommand::Prepare {
            tag,
            protocol_version,
            models_version,
            contracts_version,
        } => {
            let Some(tag) = tag.filter(|tag| !tag.trim().is_empty()) else {
                println!("release tag is not set; skipping release version preparation.");
                return Ok(());
            };
            let release = parse_release_tag(&tag)?;
            for version in [&protocol_version, &models_version, &contracts_version]
                .into_iter()
                .flatten()
            {
                semver::Version::parse(version).into_diagnostic()?;
            }
            // A prerelease never publishes a component as a stable version:
            // a component keeping its checked-in version must already be a
            // prerelease, and a changed one needs an explicit prerelease.
            if release.version.contains('-') {
                for (name, path, flag, explicit) in [
                    (
                        "trellis-protocol",
                        "crates/protocol/Cargo.toml",
                        "--protocol-version",
                        &protocol_version,
                    ),
                    (
                        "trellis-models",
                        "crates/models/Cargo.toml",
                        "--models-version",
                        &models_version,
                    ),
                    (
                        "trellis",
                        "crates/builtin-contracts/trellis.toml",
                        "--contracts-version",
                        &contracts_version,
                    ),
                ] {
                    let version = match explicit {
                        Some(version) => version.clone(),
                        None => std::fs::read_to_string(repo_root.join(path))
                            .into_diagnostic()?
                            .parse::<toml_edit::DocumentMut>()
                            .into_diagnostic()?["package"]["version"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    };
                    if semver::Version::parse(&version)
                        .into_diagnostic()?
                        .pre
                        .is_empty()
                    {
                        return Err(miette!(
                            "prerelease {} would publish {name} as stable {version}; pass \
                            {flag} with a prerelease version",
                            release.version
                        ));
                    }
                }
            }
            let mut changed = prepare_release(repo_root, &release)?;
            for (name, path, version) in [
                (
                    "trellis-protocol",
                    "crates/protocol/Cargo.toml",
                    protocol_version,
                ),
                ("trellis-models", "crates/models/Cargo.toml", models_version),
                (
                    "trellis",
                    "crates/builtin-contracts/trellis.toml",
                    contracts_version,
                ),
            ] {
                if let Some(version) = version {
                    changed.extend(versioning::prepare_component(
                        repo_root, name, path, &version,
                    )?);
                }
            }
            write_github_env("TRELLIS_RELEASE_VERSION", &release.version)?;
            write_github_env("TRELLIS_RELEASE_BASE_VERSION", &release.base_version)?;
            println!(
                "Prepared release version {} from tag {tag} in {} file(s).",
                release.version,
                changed.len()
            );
            for path in changed {
                println!("- {}", display_repo_path(repo_root, &path));
            }
            Ok(())
        }
        ReleaseCommand::Bump { from, to } => {
            require_stable_version(&from, "--from")?;
            require_stable_version(&to, "--to")?;
            let changed = bump_versions(repo_root, &from, &to)?;
            println!(
                "Bumped release-managed Trellis versions from {from} to {to} in {} file(s).",
                changed.len()
            );
            for path in changed {
                println!("- {}", display_repo_path(repo_root, &path));
            }
            Ok(())
        }
        ReleaseCommand::ChangelogCheck { version, since } => {
            check_changelog(repo_root, &version, since.as_deref())?;
            Ok(())
        }
        ReleaseCommand::WriteNotes { tag, output } => {
            let release = parse_release_tag(&tag)?;
            write_release_notes(repo_root, &release.version, &output)?;
            println!("Wrote release notes for {tag} to {}.", output.display());
            Ok(())
        }
        ReleaseCommand::CheckMetadata { version, since } => {
            let checked_version = check_versions(repo_root)?;
            if let Some(version) = version {
                let version_base = version_base(&version)?;
                if version != checked_version && version_base != checked_version {
                    return Err(miette!(
                        "requested release version {version} has base version {version_base}, \
                        but release-managed versions use {checked_version}"
                    ));
                }
                check_changelog(repo_root, &version, since.as_deref())?;
            }
            println!("Release metadata verification passed for {checked_version}.");
            println!("Before publishing, require a successful Check run for the release base.");
            Ok(())
        }
        ReleaseCommand::PretagCheck { tag, git_ref } => run_pretag_check(repo_root, &tag, &git_ref),
    }
}

#[cfg(test)]
mod tests {
    use super::changelog::extract_changelog_section;
    use super::github::{pretag_dispatch_command, pretag_list_command, pretag_watch_command};
    use super::runner::command_text;
    use super::versioning::{
        collect_versions, prepare_release, rewrite_cargo_manifest_versions,
        rewrite_cargo_manifest_versions_for_release, rewrite_js_internal_npm_dependency_versions,
        rewrite_json_manifest_version, version_base, ReleaseVersion,
    };
    use super::{parse_release_command, run_release, ReleaseCommand};
    use std::fs;

    #[test]
    fn parse_release_bump_command() {
        let command = parse_release_command(
            ["bump", "--from", "0.8.2", "--to", "0.9.0"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("parse release bump");
        assert_eq!(
            command,
            ReleaseCommand::Bump {
                from: "0.8.2".to_string(),
                to: "0.9.0".to_string(),
            }
        );
    }

    #[test]
    fn native_contract_release_requires_explicit_model_bundle_selection() {
        assert!(parse_release_command(
            [
                "prepare",
                "--tag",
                "v0.9.0-rc.1",
                "--contracts-version",
                "0.2.0"
            ]
            .into_iter()
            .map(str::to_string)
        )
        .is_err());
    }

    #[test]
    fn parse_release_pretag_check_defaults_ref() {
        assert_eq!(
            parse_release_command(
                ["pretag-check", "--tag", "v0.9.0-rc.1"]
                    .into_iter()
                    .map(str::to_string)
            )
            .expect("parse release pretag-check"),
            ReleaseCommand::PretagCheck {
                tag: "v0.9.0-rc.1".to_string(),
                git_ref: "main".to_string(),
            }
        );
    }

    #[test]
    fn parse_release_pretag_check_rejects_invalid_tag() {
        let error = parse_release_command(
            ["pretag-check", "--tag", "0.9.0"]
                .into_iter()
                .map(str::to_string),
        )
        .expect_err("pretag-check should reject invalid tag");
        assert!(error.to_string().contains("invalid release tag"));
    }

    #[test]
    fn pretag_check_command_specs_construct_gh_invocations() {
        assert_eq!(
            command_text(&pretag_dispatch_command("v0.9.0", "main")),
            "gh workflow run .github/workflows/release.yml --ref main -f tag=v0.9.0 \
            -f publish=false"
        );
        assert_eq!(
            command_text(&pretag_list_command("main")),
            "gh run list --workflow .github/workflows/release.yml --event \
            workflow_dispatch --branch main --limit 20 --json databaseId --jq \
            '.[].databaseId'"
        );
        assert_eq!(
            command_text(&pretag_watch_command("12345")),
            "gh run watch 12345 --exit-status"
        );
    }

    #[test]
    fn rewrite_json_manifest_preserves_layout() {
        let original = "{\n  \"name\": \"@oatscenter/trellis\",\n  \"version\": \"0.8.2\"\n}\n";
        let updated = rewrite_json_manifest_version(
            original,
            "0.8.2",
            "0.9.0",
            std::path::Path::new("package.json"),
        )
        .expect("rewrite json version");
        assert_eq!(
            updated,
            "{\n  \"name\": \"@oatscenter/trellis\",\n  \"version\": \"0.9.0\"\n}\n"
        );
    }

    #[test]
    fn prepare_release_updates_internal_npm_dependency_versions() {
        let root = temp_repo_root();
        fs::create_dir_all(&root).expect("mkdir repo");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.8.2\"\n",
        )
        .expect("write workspace manifest");
        let manifest = root.join("ts/packages/trellis-testkit/package.json");
        fs::create_dir_all(manifest.parent().expect("manifest parent"))
            .expect("mkdir manifest parent");
        fs::write(
            &manifest,
            "{\n  \"name\": \"@oatscenter/trellis-testkit\",\n  \"version\": \
            \"0.8.2\",\n  \"dependencies\": {\n    \"@oatscenter/trellis\": \
            \"^0.8.2\",\n    \"zod\": \"^4.0.0\"\n  }\n}\n",
        )
        .expect("write manifest");
        prepare_release(
            &root,
            &ReleaseVersion {
                version: "0.8.2-rc.1".to_string(),
                base_version: "0.8.2".to_string(),
            },
        )
        .expect("prepare release");
        let updated: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(updated["version"], "0.8.2-rc.1");
        assert_eq!(
            updated["dependencies"]["@oatscenter/trellis"],
            "^0.8.2-rc.1"
        );
        assert_eq!(updated["dependencies"]["zod"], "^4.0.0");
        fs::remove_dir_all(root).expect("remove temp repo");
    }

    #[test]
    fn sdk_release_preserves_independent_contract_model_and_protocol_versions() {
        let root = temp_repo_root();
        fs::create_dir_all(&root).expect("mkdir repo");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.100.0\"\n[workspace.dependencies]\n\
            trellis-models = { path = \"crates/models\", version = \"0.100.0\" }\n\
            trellis-protocol = { path = \"crates/protocol\", version = \"0.100.0\" \
            }\n",
        )
        .expect("write workspace manifest");
        let projects = [
            ("crates/builtin-contracts", "trellis"),
            ("ts/packages/trellis-testkit", "trellis-testkit-generated"),
            ("web", "trellis-web-generated"),
        ];
        for (project, name) in projects {
            let dir = root.join(project);
            fs::create_dir_all(&dir).expect("mkdir project");
            fs::write(
                dir.join("trellis.toml"),
                format!("[package]\nname = \"{name}\"\nversion = \"0.100.0\"\n"),
            )
            .expect("write IDL manifest");
        }
        for (directory, name) in [
            ("models", "trellis-models"),
            ("protocol", "trellis-protocol"),
        ] {
            let path = root.join("crates").join(directory);
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("Cargo.toml"),
                format!("[package]\nname = {name:?}\nversion = \"0.100.0\"\n"),
            )
            .unwrap();
        }
        let generated = root.join("ts/packages/trellis/internal_sdk/generated/package.json");
        fs::create_dir_all(generated.parent().unwrap()).unwrap();
        fs::write(
            &generated,
            "{\n  \"name\": \"native-contract-client\",\n  \"version\": \"0.42.0\",\
            \n  \"trellisGenerated\": true,\n  \"dependencies\": {\n    \
            \"@oatscenter/trellis\": \"^0.100.0\"\n  }\n}\n",
        )
        .unwrap();
        prepare_release(
            &root,
            &ReleaseVersion {
                version: "0.100.0-rc.1".to_string(),
                base_version: "0.100.0".to_string(),
            },
        )
        .expect("prepare release");
        let client: serde_json::Value =
            serde_json::from_slice(&fs::read(&generated).unwrap()).unwrap();
        assert_eq!(client["version"], "0.42.0");
        assert_eq!(
            client["dependencies"]["@oatscenter/trellis"],
            "^0.100.0-rc.1"
        );
        for (project, name) in projects {
            let manifest = fs::read_to_string(root.join(project).join("trellis.toml"))
                .expect("read IDL manifest")
                .parse::<toml_edit::DocumentMut>()
                .expect("parse released IDL manifest");
            assert_eq!(
                manifest["package"]["version"].as_str(),
                Some(if name == "trellis" {
                    "0.100.0"
                } else {
                    "0.100.0-rc.1"
                })
            );
        }
        assert!(super::versioning::check_versions(&root).is_ok());
        super::versioning::prepare_component(
            &root,
            "trellis-models",
            "crates/models/Cargo.toml",
            "0.101.0-rc.1",
        )
        .unwrap();
        super::versioning::prepare_component(
            &root,
            "trellis",
            "crates/builtin-contracts/trellis.toml",
            "0.100.1",
        )
        .unwrap();
        let model = fs::read_to_string(root.join("crates/models/Cargo.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(model["package"]["version"].as_str(), Some("0.101.0-rc.1"));
        let workspace = fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(
            workspace["workspace"]["dependencies"]["trellis-models"]["version"].as_str(),
            Some("0.101.0-rc.1")
        );
        assert_eq!(
            workspace["workspace"]["dependencies"]["trellis-protocol"]["version"].as_str(),
            Some("0.100.0")
        );
        let contract = fs::read_to_string(root.join("crates/builtin-contracts/trellis.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(contract["package"]["version"].as_str(), Some("0.100.1"));
        assert!(super::versioning::check_versions(&root).is_ok());
        fs::remove_dir_all(root).expect("remove temp repo");
    }

    #[test]
    fn prepare_release_versions_testkit_fixture_manifests() {
        let root = temp_repo_root();
        fs::create_dir_all(&root).expect("mkdir repo");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.100.0\"\n",
        )
        .expect("write workspace manifest");
        for relative in [
            "integration/fixtures/testkit/Cargo.toml",
            "integration/fixtures/testkit/crates/trellis-test-fixture/Cargo.toml",
        ] {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir fixture");
            fs::write(
                &path,
                "[package]\nname = \"trellis-test-fixture\"\nversion = \"0.100.0\"\n",
            )
            .expect("write fixture manifest");
        }
        fs::write(
            root.join("integration/fixtures/testkit/trellis.toml"),
            "[package]\nname = \"trellis-test-fixture\"\nversion = \"0.100.0\"\n",
        )
        .expect("write IDL manifest");
        prepare_release(
            &root,
            &ReleaseVersion {
                version: "0.100.0-rc.1".to_string(),
                base_version: "0.100.0".to_string(),
            },
        )
        .expect("prepare release");
        for relative in [
            "integration/fixtures/testkit/Cargo.toml",
            "integration/fixtures/testkit/crates/trellis-test-fixture/Cargo.toml",
            "integration/fixtures/testkit/trellis.toml",
        ] {
            let manifest = fs::read_to_string(root.join(relative))
                .unwrap()
                .parse::<toml_edit::DocumentMut>()
                .unwrap();
            assert_eq!(
                manifest["package"]["version"].as_str(),
                Some("0.100.0-rc.1")
            );
        }
        fs::remove_dir_all(root).expect("remove temp repo");
    }

    #[test]
    fn rewrite_cargo_manifest_updates_workspace_and_internal_dependencies() {
        let original = "[workspace.package]\nversion = \"0.8.2\"\n\n[dependencies]\ntrellis-rs \
            = { path = \"../trellis\", version = \"0.8.2\" }\ntrellis-protocol = \
            \"0.8.2\"\nserde = { version = \"1.0\" }\n";
        let updated = rewrite_cargo_manifest_versions(
            original,
            "0.8.2",
            "0.9.0",
            std::path::Path::new("Cargo.toml"),
        )
        .expect("rewrite cargo versions");
        let manifest = updated.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            manifest["workspace"]["package"]["version"].as_str(),
            Some("0.9.0")
        );
        assert_eq!(
            manifest["dependencies"]["trellis-rs"]["version"].as_str(),
            Some("0.9.0")
        );
        assert_eq!(
            manifest["dependencies"]["trellis-protocol"].as_str(),
            Some("0.8.2")
        );
        assert_eq!(
            manifest["dependencies"]["serde"]["version"].as_str(),
            Some("1.0")
        );
    }

    #[test]
    fn rewrite_cargo_manifest_preserves_non_release_sentinel_version() {
        let original = "[package]\nname = \"console-trellis\"\nversion = \"0.0.0\"\n\n\
            [dependencies]\ntrellis-rs = { version = \"0.8.2\" }\n";
        let updated = rewrite_cargo_manifest_versions(
            original,
            "0.8.2",
            "0.9.0",
            std::path::Path::new("Cargo.toml"),
        )
        .expect("rewrite cargo versions");
        let manifest = updated.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(manifest["package"]["version"].as_str(), Some("0.0.0"));
        assert_eq!(
            manifest["dependencies"]["trellis-rs"]["version"].as_str(),
            Some("0.9.0")
        );
    }

    #[test]
    fn rewrite_cargo_manifest_for_release_preserves_local_generated_dependency_versions() {
        let original = "[workspace.package]\nversion = \"0.8.2\"\n\n[dependencies]\ntrellis-rs \
            = { version = \"0.8.2\" }\ntrellis-protocol = \"0.8.2\"\n\
            console-trellis = { path = \"trellis\", version = \"0.0.0\" }\n";
        let updated = rewrite_cargo_manifest_versions_for_release(
            original,
            "0.8.2-rc.1",
            "0.8.2",
            std::path::Path::new("Cargo.toml"),
        )
        .expect("rewrite cargo release versions");
        let manifest = updated.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            manifest["workspace"]["package"]["version"].as_str(),
            Some("0.8.2-rc.1")
        );
        assert_eq!(
            manifest["dependencies"]["trellis-rs"]["version"].as_str(),
            Some("0.8.2-rc.1")
        );
        assert_eq!(
            manifest["dependencies"]["trellis-protocol"].as_str(),
            Some("0.8.2")
        );
        assert_eq!(
            manifest["dependencies"]["console-trellis"]["version"].as_str(),
            Some("0.0.0")
        );
    }

    #[test]
    fn rewrite_npm_dependency_versions_preserves_ranges() {
        let original = "{\n  \"dependencies\": {\n    \"@oatscenter/result\": \"^0.8.2\",\
            \n    \"@oatscenter/trellis\": \"~0.8.2\"\n  }\n}\n";
        let updated = rewrite_js_internal_npm_dependency_versions(
            original,
            Some("0.8.2"),
            "0.9.0",
            std::path::Path::new("package.json"),
        )
        .expect("rewrite js internal npm dependencies");
        let updated: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(updated["dependencies"]["@oatscenter/result"], "^0.9.0");
        assert_eq!(updated["dependencies"]["@oatscenter/trellis"], "~0.9.0");
    }

    #[test]
    fn collect_versions_skips_zero_version_apps() {
        let root = temp_repo_root();
        fs::create_dir_all(root.join("ts/packages/trellis")).expect("mkdir package");
        fs::create_dir_all(root.join("web")).expect("mkdir app");
        fs::write(
            root.join("ts/packages/trellis/package.json"),
            "{\"version\":\"0.8.2\"}\n",
        )
        .expect("write package manifest");
        fs::write(root.join("web/package.json"), "{\"version\":\"0.0.0\"}\n")
            .expect("write app manifest");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.8.2\"\n",
        )
        .expect("write cargo manifest");
        let versions = collect_versions(&root).expect("collect versions");
        assert_eq!(versions.len(), 2);
        fs::remove_dir_all(root).expect("remove temp repo");
    }

    #[test]
    fn extract_changelog_section_finds_dated_heading() {
        let section = extract_changelog_section(
            "# Changelog\n\n## [0.9.0] - 2026-05-19\n\n### Added\n\n- Thing\n\n## [0.8.2]\n",
            "0.9.0",
        )
        .expect("extract changelog");
        assert!(section.contains("Thing"));
    }

    #[test]
    fn version_base_accepts_prerelease_versions() {
        assert_eq!(version_base("0.9.0-rc.1").expect("version base"), "0.9.0");
    }

    #[test]
    fn prerelease_refuses_a_stable_component_version() {
        let root = temp_repo_root();
        for (path, contents) in [
            (
                "crates/protocol/Cargo.toml",
                "[package]\nname = \"trellis-protocol\"\nversion = \"0.100.0-rc.3\"\n",
            ),
            (
                "crates/models/Cargo.toml",
                "[package]\nname = \"trellis-models\"\nversion = \"0.102.0-rc.4\"\n",
            ),
            (
                "crates/builtin-contracts/trellis.toml",
                "[package]\nname = \"trellis\"\nversion = \"0.100.0\"\n",
            ),
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let error = run_release(
            &root,
            ReleaseCommand::Prepare {
                tag: Some("v0.100.0-rc.4".into()),
                protocol_version: Some("0.100.0-rc.4".into()),
                models_version: None,
                contracts_version: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--contracts-version"), "{error}");
        // Nothing was written before the refusal.
        assert!(fs::read_to_string(root.join("crates/protocol/Cargo.toml"))
            .unwrap()
            .contains("0.100.0-rc.3"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepare_moves_the_previous_prerelease_and_leaves_generated_packages() {
        let root = temp_repo_root();
        let generated = "benchmarks/runtime/packages/performance-trellis/package.json";
        for (path, contents) in [
            (
                "Cargo.toml",
                "[workspace.package]\nversion = \"0.100.0-rc.3\"\n",
            ),
            (
                "ts/packages/trellis/package.json",
                "{\n  \"name\": \"@oatscenter/trellis\",\n  \"version\": \"0.100.0-rc.3\",\n  \"dependencies\": {\n    \"@oatscenter/result\": \"^0.100.0-rc.3\"\n  }\n}\n",
            ),
            (
                generated,
                "{\n  \"name\": \"performance-trellis\",\n  \"version\": \"0.0.0\",\n  \"dependencies\": {\n    \"@oatscenter/trellis\": \"0.100.0-rc.3\"\n  }\n}\n",
            ),
            (".gitignore", "/benchmarks/runtime/packages/performance-trellis/\n"),
            (
                "crates/protocol/Cargo.toml",
                "[package]\nname = \"trellis-protocol\"\nversion = \"0.100.0-rc.3\"\n",
            ),
            (
                "crates/models/Cargo.toml",
                "[package]\nname = \"trellis-models\"\nversion = \"0.100.0-rc.3\"\n",
            ),
            (
                "crates/builtin-contracts/trellis.toml",
                "[package]\nname = \"trellis\"\nversion = \"0.100.0-rc.3\"\n",
            ),
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        run_release(
            &root,
            ReleaseCommand::Prepare {
                tag: Some("v0.100.0-rc.4".into()),
                protocol_version: None,
                models_version: None,
                contracts_version: None,
            },
        )
        .unwrap();
        let trellis: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("ts/packages/trellis/package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(trellis["version"], "0.100.0-rc.4");
        assert_eq!(
            trellis["dependencies"]["@oatscenter/result"],
            "^0.100.0-rc.4"
        );
        // `cargo xtask install` regenerates the ignored package afterwards.
        assert!(fs::read_to_string(root.join(generated))
            .unwrap()
            .contains("0.100.0-rc.3"));
        fs::remove_dir_all(root).unwrap();
    }

    fn temp_repo_root() -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "trellis-release-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        path
    }
}
