use std::fs;
use std::path::Path;
use std::process::Command;

use miette::{miette, IntoDiagnostic, Result, WrapErr};

pub(super) fn check_changelog(repo_root: &Path, version: &str, since: Option<&str>) -> Result<()> {
    let changelog_path = repo_root.join("CHANGELOG.md");
    let changelog = fs::read_to_string(&changelog_path)
        .into_diagnostic()
        .wrap_err("failed to read CHANGELOG.md")?;
    let section = extract_changelog_section(&changelog, version)?;
    if section.trim().is_empty() {
        return Err(miette!("CHANGELOG.md section for {version} is empty"));
    }
    if section.contains("TODO") || section.contains("TBD") {
        return Err(miette!(
            "CHANGELOG.md section for {version} still contains TODO/TBD text"
        ));
    }
    println!("CHANGELOG.md contains a release section for {version}.");
    if let Some(since) = since {
        print_changes_since(repo_root, since)?;
    }
    Ok(())
}

pub(super) fn write_release_notes(
    repo_root: &Path,
    version: &str,
    output_path: &Path,
) -> Result<()> {
    let changelog_path = repo_root.join("CHANGELOG.md");
    let changelog = fs::read_to_string(&changelog_path)
        .into_diagnostic()
        .wrap_err("failed to read CHANGELOG.md")?;
    let section = extract_changelog_section(&changelog, version)?;
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
    }
    fs::write(output_path, format!("{}\n", section.trim()))
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", output_path.display()))
}

pub(super) fn extract_changelog_section(changelog: &str, version: &str) -> Result<String> {
    let heading = format!("## [{version}]");
    let heading_with_date_prefix = format!("## [{version}] - ");
    let lines: Vec<_> = changelog
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect();
    let start = lines
        .iter()
        .position(|line| line == &heading || line.starts_with(&heading_with_date_prefix))
        .ok_or_else(|| miette!("CHANGELOG.md does not contain a section for {version}"))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| line.starts_with("## [").then_some(index))
        .unwrap_or(lines.len());
    Ok(lines[start + 1..end].join("\n"))
}

fn print_changes_since(repo_root: &Path, since: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("diff")
        .arg("--name-status")
        .arg(format!("{since}..HEAD"))
        .current_dir(repo_root)
        .output()
        .into_diagnostic()
        .wrap_err("failed to run git diff for changelog review")?;
    if !output.status.success() {
        return Err(miette!(
            "git diff {since}..HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        println!("No file changes found since {since}.");
    } else {
        println!("Files changed since {since}; verify CHANGELOG.md covers user-visible changes:");
        for line in stdout.lines() {
            println!("- {line}");
        }
    }
    Ok(())
}
