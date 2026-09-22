use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use miette::{miette, IntoDiagnostic, Result, WrapErr};

use super::runner::{command_text, run_checked_command, run_output_command, CommandSpec};
use super::versioning::parse_release_tag;

pub(super) fn run_pretag_check(repo_root: &Path, tag: &str, git_ref: &str) -> Result<()> {
    parse_release_tag(tag)?;
    if let Err(error) = require_usable_gh(repo_root) {
        print_pretag_fallback(tag, git_ref, true);
        return Err(error);
    }

    let existing_run_ids: BTreeSet<_> = match list_pretag_workflow_run_ids(repo_root, git_ref) {
        Ok(run_ids) => run_ids.into_iter().collect(),
        Err(error) => {
            print_pretag_fallback(tag, git_ref, true);
            return Err(error);
        }
    };

    let dispatch = pretag_dispatch_command(tag, git_ref);
    if let Err(error) =
        run_checked_command(repo_root, &dispatch, "failed to dispatch Release workflow")
    {
        print_pretag_fallback(tag, git_ref, true);
        return Err(error);
    }

    let run_id = match resolve_new_pretag_workflow_run(repo_root, git_ref, &existing_run_ids) {
        Ok(run_id) => run_id,
        Err(error) => {
            print_pretag_fallback(tag, git_ref, false);
            return Err(error);
        }
    };
    println!("Watching Release workflow run {run_id}.");
    run_checked_command(
        repo_root,
        &pretag_watch_command(&run_id),
        "Release workflow run failed",
    )
}

fn require_usable_gh(repo_root: &Path) -> Result<()> {
    for spec in gh_prerequisite_commands() {
        let output = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(repo_root)
            .output()
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to run {}", command_text(&spec)))?;
        if !output.status.success() {
            return Err(miette!(
                "GitHub CLI prerequisite failed: {}\n{}",
                command_text(&spec),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

fn resolve_new_pretag_workflow_run(
    repo_root: &Path,
    git_ref: &str,
    existing_run_ids: &BTreeSet<String>,
) -> Result<String> {
    for attempt in 1..=12 {
        let new_run_ids: Vec<_> = list_pretag_workflow_run_ids(repo_root, git_ref)?
            .into_iter()
            .filter(|run_id| !existing_run_ids.contains(run_id))
            .collect();
        if new_run_ids.len() == 1 {
            return Ok(new_run_ids[0].clone());
        }
        if new_run_ids.len() > 1 {
            return Err(miette!(
                "found multiple newly dispatched Release workflow runs for ref `{git_ref}`: {}",
                new_run_ids.join(", ")
            ));
        }

        if attempt < 12 {
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    Err(miette!(
        "failed to resolve a newly dispatched workflow_dispatch Release run for ref `{git_ref}`"
    ))
}

fn list_pretag_workflow_run_ids(repo_root: &Path, git_ref: &str) -> Result<Vec<String>> {
    let spec = pretag_list_command(git_ref);
    let output = run_output_command(repo_root, &spec)
        .wrap_err("failed to list Release workflow dry-run candidates")?;
    if !output.status.success() {
        return Err(miette!(
            "{} failed: {}",
            command_text(&spec),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut run_ids = Vec::new();
    for run_id in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !run_id.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(miette!(
                "resolved Release workflow run id `{run_id}` is not numeric"
            ));
        }
        run_ids.push(run_id.to_string());
    }
    Ok(run_ids)
}

fn gh_prerequisite_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("gh", vec!["--version"]),
        CommandSpec::new("gh", vec!["auth", "status"]),
    ]
}

pub(super) fn pretag_dispatch_command(tag: &str, git_ref: &str) -> CommandSpec {
    CommandSpec::new(
        "gh",
        vec![
            "workflow".to_string(),
            "run".to_string(),
            ".github/workflows/release.yml".to_string(),
            "--ref".to_string(),
            git_ref.to_string(),
            "-f".to_string(),
            format!("tag={tag}"),
            "-f".to_string(),
            "publish=false".to_string(),
        ],
    )
}

pub(super) fn pretag_list_command(git_ref: &str) -> CommandSpec {
    CommandSpec::new(
        "gh",
        vec![
            "run",
            "list",
            "--workflow",
            ".github/workflows/release.yml",
            "--event",
            "workflow_dispatch",
            "--branch",
            git_ref,
            "--limit",
            "20",
            "--json",
            "databaseId",
            "--jq",
            ".[].databaseId",
        ],
    )
}

pub(super) fn pretag_watch_command(run_id: &str) -> CommandSpec {
    CommandSpec::new("gh", vec!["run", "watch", run_id, "--exit-status"])
}

fn print_pretag_fallback(tag: &str, git_ref: &str, dispatch_may_be_needed: bool) {
    eprintln!("Unable to verify the pre-tag Release workflow with GitHub CLI (`gh`).");
    eprintln!(
        "Run this fallback manually and do not create or push the release tag until it passes:"
    );
    if dispatch_may_be_needed {
        eprintln!("{}", command_text(&pretag_dispatch_command(tag, git_ref)));
    } else {
        eprintln!("A Release workflow dispatch may already have succeeded; inspect recent runs before dispatching another one.");
    }
    eprintln!("{}", command_text(&pretag_list_command(git_ref)));
    eprintln!("{}", command_text(&pretag_watch_command("<run-id>")));
}
