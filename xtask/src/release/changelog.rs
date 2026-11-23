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

/// Keep a Changelog's subsection order; other subsections follow in the order
/// they first appear.
const SUBSECTION_ORDER: [&str; 6] = [
    "Added",
    "Changed",
    "Deprecated",
    "Removed",
    "Fixed",
    "Security",
];

/// Move the Unreleased entries of `CHANGELOG.md` into the `version` section
/// dated `date`, and say whether the file changed.
pub(super) fn release_changelog(repo_root: &Path, version: &str, date: &str) -> Result<bool> {
    let path = repo_root.join("CHANGELOG.md");
    let original = fs::read_to_string(&path)
        .into_diagnostic()
        .wrap_err("failed to read CHANGELOG.md")?;
    let updated = release_unreleased(&original, version, date)?;
    if updated == original {
        return Ok(false);
    }
    fs::write(&path, updated)
        .into_diagnostic()
        .wrap_err("failed to write CHANGELOG.md")?;
    Ok(true)
}

/// A changelog section's text before its first subsection, and each
/// subsection's name and trimmed body.
struct Section {
    intro: String,
    subsections: Vec<(String, String)>,
}

impl Section {
    fn parse(lines: &[&str]) -> Self {
        let mut intro = Vec::new();
        let mut subsections: Vec<(String, Vec<&str>)> = Vec::new();
        for line in lines {
            if let Some(name) = line.strip_prefix("### ") {
                subsections.push((name.trim().to_string(), Vec::new()));
            } else if let Some((_, body)) = subsections.last_mut() {
                body.push(line);
            } else {
                intro.push(*line);
            }
        }
        Self {
            intro: intro.join("\n").trim().to_string(),
            subsections: subsections
                .into_iter()
                .map(|(name, body)| (name, body.join("\n").trim().to_string()))
                .filter(|(_, body)| !body.is_empty())
                .collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.intro.is_empty() && self.subsections.is_empty()
    }

    /// Put `newer`'s entries ahead of this section's, subsection by
    /// subsection, in Keep a Changelog order.
    fn merge_newer(self, newer: Section) -> Self {
        let join = |newer: &str, older: &str| {
            [newer, older]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        let mut names: Vec<String> = SUBSECTION_ORDER
            .iter()
            .map(|name| name.to_string())
            .collect();
        for (name, _) in newer.subsections.iter().chain(&self.subsections) {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        let body = |section: &Section, name: &str| {
            section
                .subsections
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, body)| body.clone())
                .unwrap_or_default()
        };
        let subsections = names
            .into_iter()
            .map(|name| {
                let merged = join(&body(&newer, &name), &body(&self, &name));
                (name, merged)
            })
            .filter(|(_, body)| !body.is_empty())
            .collect();
        Self {
            intro: join(&newer.intro, &self.intro),
            subsections,
        }
    }

    fn render(&self, heading: &str) -> String {
        let mut text = format!("{heading}\n\n");
        if !self.intro.is_empty() {
            text.push_str(&self.intro);
            text.push_str("\n\n");
        }
        for (name, body) in &self.subsections {
            text.push_str(&format!("### {name}\n\n{body}\n\n"));
        }
        text
    }
}

/// Release the Unreleased section as `version` on `date`: a new
/// `## [version] - date` section, or the entries merged ahead of an existing
/// `version` section, whose date moves to `date`. Unreleased is left empty.
pub(super) fn release_unreleased(changelog: &str, version: &str, date: &str) -> Result<String> {
    let changelog = changelog.replace("\r\n", "\n");
    let lines: Vec<&str> = changelog.lines().collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("## [").then_some(index))
        .collect();
    let section_lines = |position: usize| {
        let end = starts.get(position + 1).copied().unwrap_or(lines.len());
        &lines[starts[position] + 1..end]
    };
    let unreleased = starts
        .iter()
        .position(|&start| lines[start].trim_end() == "## [Unreleased]")
        .ok_or_else(|| miette!("CHANGELOG.md has no `## [Unreleased]` section"))?;
    let heading = format!("## [{version}]");
    let existing = starts.iter().position(|&start| {
        let line = lines[start].trim_end();
        line == heading || line.starts_with(&format!("{heading} - "))
    });
    let entries = Section::parse(section_lines(unreleased));
    let released = match existing {
        Some(position) => Section::parse(section_lines(position)).merge_newer(entries),
        None if entries.is_empty() => {
            return Err(miette!(
                "CHANGELOG.md has nothing under Unreleased and no section for {version}"
            ))
        }
        None => Section::parse(&[]).merge_newer(entries),
    };
    let released = released.render(&format!("{heading} - {date}"));

    let preamble = lines[..starts[0]].join("\n");
    let mut text = String::new();
    if !preamble.trim().is_empty() {
        text.push_str(preamble.trim_end());
        text.push_str("\n\n");
    }
    for position in 0..starts.len() {
        if position == unreleased {
            text.push_str("## [Unreleased]\n\n");
            if existing.is_none() {
                text.push_str(&released);
            }
        } else if Some(position) == existing {
            text.push_str(&released);
        } else {
            let body = section_lines(position).join("\n");
            text.push_str(lines[starts[position]]);
            text.push_str("\n\n");
            if !body.trim().is_empty() {
                text.push_str(body.trim());
                text.push_str("\n\n");
            }
        }
    }
    Ok(format!("{}\n", text.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::release_unreleased;

    const HEAD: &str = "# Changelog\n\nAll notable changes.\n\n";

    #[test]
    fn a_new_release_takes_the_unreleased_entries() {
        let changelog = format!(
            "{HEAD}## [Unreleased]\n\n### Fixed\n\n- A fix.\n\n### Added\n\n- A feature\n  over two lines.\n\n\
             ## [0.9.0] - 2026-01-02\n\n### Added\n\n- Old.\n"
        );
        let released = release_unreleased(&changelog, "0.10.0", "2026-03-04").unwrap();
        assert_eq!(
            released,
            format!(
                "{HEAD}## [Unreleased]\n\n## [0.10.0] - 2026-03-04\n\n### Added\n\n- A feature\n  over two lines.\n\n\
                 ### Fixed\n\n- A fix.\n\n## [0.9.0] - 2026-01-02\n\n### Added\n\n- Old.\n"
            )
        );
        // A second run has nothing left to move and changes nothing.
        assert_eq!(
            release_unreleased(&released, "0.10.0", "2026-03-04").unwrap(),
            released
        );
    }

    #[test]
    fn a_held_release_gets_the_newer_entries_first_and_a_new_date() {
        let changelog = format!(
            "{HEAD}## [Unreleased]\n\n### Security\n\n- Patched.\n\n### Added\n\n- Newer.\n\n### Notes\n\n- Read me.\n\n\
             ## [0.10.0-rc.2] - 2026-03-01\n\n### Added\n\n- Older.\n- Oldest.\n\n### Changed\n\n- Changed before.\n\n\
             ## [0.10.0-rc.1] - 2026-02-01\n\n### Fixed\n\n- Earlier fix.\n"
        );
        let released = release_unreleased(&changelog, "0.10.0-rc.2", "2026-03-04").unwrap();
        assert_eq!(
            released,
            format!(
                "{HEAD}## [Unreleased]\n\n## [0.10.0-rc.2] - 2026-03-04\n\n\
                 ### Added\n\n- Newer.\n\n- Older.\n- Oldest.\n\n### Changed\n\n- Changed before.\n\n\
                 ### Security\n\n- Patched.\n\n### Notes\n\n- Read me.\n\n\
                 ## [0.10.0-rc.1] - 2026-02-01\n\n### Fixed\n\n- Earlier fix.\n"
            )
        );
    }

    #[test]
    fn an_empty_unreleased_only_redates_a_held_release() {
        let changelog =
            format!("{HEAD}## [Unreleased]\n\n## [0.10.0] - 2026-03-01\n\n### Added\n\n- Held.\n");
        assert_eq!(
            release_unreleased(&changelog, "0.10.0", "2026-03-04").unwrap(),
            format!("{HEAD}## [Unreleased]\n\n## [0.10.0] - 2026-03-04\n\n### Added\n\n- Held.\n")
        );
        // With no held section there is nothing to release.
        let error = release_unreleased(&changelog, "0.11.0", "2026-03-04").unwrap_err();
        assert!(
            error.to_string().contains("nothing under Unreleased"),
            "{error}"
        );
    }
}
