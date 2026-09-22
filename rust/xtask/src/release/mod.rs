use std::path::{Path, PathBuf};

#[cfg(test)]
use clap::Parser;
use clap::Subcommand;
use miette::{miette, Result};

mod changelog;
mod github;
mod runner;
mod versioning;

use changelog::{check_changelog, write_release_notes};
use github::run_pretag_check;
use versioning::{
    bump_versions, check_versions, display_repo_path, parse_release_tag, prepare_release,
    require_stable_version, version_base, write_github_env,
};

const RELEASE_JS_INTERNAL_NPM_VERSION_FILES: &[&str] = &[
    "ts/packages/trellis/scripts/build_npm.ts",
    "ts/packages/trellis-svelte/scripts/build_npm.ts",
    "ts/packages/trellis/tests/publishing_targets_test.ts",
];
#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub(crate) enum ReleaseCommand {
    #[command(name = "check-versions")]
    CheckVersions,
    #[command(name = "prepare")]
    Prepare {
        #[arg(long)]
        tag: Option<String>,
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
        ReleaseCommand::Prepare { tag } => {
            let Some(tag) = tag.filter(|tag| !tag.trim().is_empty()) else {
                println!("release tag is not set; skipping release version preparation.");
                return Ok(());
            };
            let release = parse_release_tag(&tag)?;
            let changed = prepare_release(repo_root, &release)?;
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
                        "requested release version {version} has base version {version_base}, but release-managed versions use {checked_version}"
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
        rewrite_json_manifest_internal_jsr_dependency_versions, rewrite_json_manifest_version,
        version_base, ReleaseVersion,
    };
    use super::{parse_release_command, ReleaseCommand};
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
    fn parse_release_prepare_command() {
        assert_eq!(
            parse_release_command(
                ["prepare", "--tag", "v0.9.0-rc.1"]
                    .into_iter()
                    .map(str::to_string)
            )
            .expect("parse release prepare"),
            ReleaseCommand::Prepare {
                tag: Some("v0.9.0-rc.1".to_string())
            }
        );
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
            "gh workflow run .github/workflows/release.yml --ref main -f tag=v0.9.0 -f publish=false"
        );
        assert_eq!(
            command_text(&pretag_list_command("main")),
            "gh run list --workflow .github/workflows/release.yml --event workflow_dispatch --branch main --limit 20 --json databaseId --jq '.[].databaseId'"
        );
        assert_eq!(
            command_text(&pretag_watch_command("12345")),
            "gh run watch 12345 --exit-status"
        );
    }

    #[test]
    fn rewrite_json_manifest_preserves_layout() {
        let original = "{\n  \"name\": \"@qlever-llc/trellis\",\n  \"version\": \"0.8.2\"\n}\n";
        let updated = rewrite_json_manifest_version(
            original,
            "0.8.2",
            "0.9.0",
            std::path::Path::new("deno.json"),
        )
        .expect("rewrite json version");
        assert_eq!(
            updated,
            "{\n  \"name\": \"@qlever-llc/trellis\",\n  \"version\": \"0.9.0\"\n}\n"
        );
    }

    #[test]
    fn rewrite_json_manifest_updates_internal_jsr_dependencies() {
        let original = "{\n  \"imports\": {\n    \"@qlever-llc/trellis\": \"jsr:@qlever-llc/trellis@^0.8.2\",\n    \"@qlever-llc/trellis/sdk/auth\": \"jsr:@qlever-llc/trellis@^0.8.2/sdk/auth\",\n    \"@std/path\": \"jsr:@std/path@^1.1.4\"\n  }\n}\n";
        let updated = rewrite_json_manifest_internal_jsr_dependency_versions(
            original,
            "0.8.2",
            "0.8.2-rc.1",
            std::path::Path::new("deno.json"),
        )
        .expect("rewrite jsr dependencies");
        assert!(updated.contains("@qlever-llc/trellis@^0.8.2-rc.1"));
        assert!(updated.contains("@std/path@^1.1.4"));
    }

    #[test]
    fn prepare_release_updates_internal_jsr_dependency_versions() {
        let root = temp_repo_root();
        let manifest = root.join("ts/packages/trellis-test/deno.json");
        fs::create_dir_all(manifest.parent().expect("manifest parent"))
            .expect("mkdir manifest parent");
        fs::write(
            &manifest,
            "{\n  \"name\": \"@qlever-llc/trellis-test\",\n  \"version\": \"0.8.2\",\n  \"imports\": {\n    \"@qlever-llc/trellis\": \"jsr:@qlever-llc/trellis@^0.8.2\"\n  }\n}\n",
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
        assert!(fs::read_to_string(&manifest)
            .expect("read updated manifest")
            .contains("0.8.2-rc.1"));
        fs::remove_dir_all(root).expect("remove temp repo");
    }

    #[test]
    fn rewrite_cargo_manifest_updates_workspace_and_internal_dependencies() {
        let original = "[workspace.package]\nversion = \"0.8.2\"\n\n[dependencies]\ntrellis-rs = { path = \"../trellis\", version = \"0.8.2\" }\nserde = { version = \"1.0\" }\n";
        let updated = rewrite_cargo_manifest_versions(
            original,
            "0.8.2",
            "0.9.0",
            std::path::Path::new("Cargo.toml"),
        )
        .expect("rewrite cargo versions");
        assert!(updated.contains("version = \"0.9.0\""));
        assert!(updated.contains("serde = { version = \"1.0\" }"));
    }

    #[test]
    fn rewrite_cargo_manifest_preserves_non_release_sentinel_version() {
        let original = "[package]\nname = \"console-trellis\"\nversion = \"0.0.0\"\n\n[dependencies]\ntrellis-rs = { version = \"0.8.2\" }\n";
        let updated = rewrite_cargo_manifest_versions(
            original,
            "0.8.2",
            "0.9.0",
            std::path::Path::new("Cargo.toml"),
        )
        .expect("rewrite cargo versions");
        assert!(updated.contains("version = \"0.0.0\""));
        assert!(updated.contains("version = \"0.9.0\""));
    }

    #[test]
    fn rewrite_cargo_manifest_for_release_preserves_local_generated_dependency_versions() {
        let original = "[workspace.package]\nversion = \"0.8.2\"\n\n[dependencies]\ntrellis-rs = { version = \"0.8.2\" }\nconsole-trellis = { path = \"trellis\", version = \"0.0.0\" }\n";
        let updated = rewrite_cargo_manifest_versions_for_release(
            original,
            "0.8.2-rc.1",
            "0.8.2",
            std::path::Path::new("Cargo.toml"),
        )
        .expect("rewrite cargo release versions");
        assert!(updated.contains("0.8.2-rc.1"));
        assert!(updated.contains("console-trellis = { path = \"trellis\", version = \"0.0.0\" }"));
    }

    #[test]
    fn rewrite_js_internal_npm_dependency_versions_updates_build_scripts() {
        let original = "const dependencies = {\n  \"@qlever-llc/result\": \"^0.8.2\",\n  \"@qlever-llc/trellis\": \"~0.8.2\",\n};\n";
        let updated = rewrite_js_internal_npm_dependency_versions(
            original,
            "0.8.2",
            "0.9.0",
            std::path::Path::new("build_npm.ts"),
        )
        .expect("rewrite js internal npm dependencies");
        assert!(updated.contains("^0.9.0"));
        assert!(updated.contains("~0.9.0"));
    }

    #[test]
    fn collect_versions_skips_zero_version_apps() {
        let root = temp_repo_root();
        fs::create_dir_all(root.join("ts/packages/trellis")).expect("mkdir package");
        fs::create_dir_all(root.join("web")).expect("mkdir app");
        fs::create_dir_all(root.join("rust")).expect("mkdir rust");
        fs::write(
            root.join("ts/packages/trellis/deno.json"),
            "{\"version\":\"0.8.2\"}\n",
        )
        .expect("write package manifest");
        fs::write(root.join("web/deno.json"), "{\"version\":\"0.0.0\"}\n")
            .expect("write app manifest");
        fs::write(
            root.join("rust/Cargo.toml"),
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
