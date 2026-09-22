use std::path::Path;
use std::process::Command;

use miette::{miette, IntoDiagnostic, Result, WrapErr};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct CommandSpec {
    pub(super) program: String,
    pub(super) args: Vec<String>,
}

impl CommandSpec {
    pub(super) fn new<S, I>(program: &str, args: I) -> Self
    where
        S: Into<String>,
        I: IntoIterator<Item = S>,
    {
        Self {
            program: program.to_string(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

pub(super) fn run_checked_command(
    repo_root: &Path,
    spec: &CommandSpec,
    context: &str,
) -> Result<()> {
    println!("$ {}", command_text(spec));
    let status = Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(repo_root)
        .status()
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to run {}", command_text(spec)))?;
    if status.success() {
        Ok(())
    } else {
        Err(miette!(
            "{context}: {} exited with {status}",
            command_text(spec)
        ))
    }
}

pub(super) fn run_output_command(
    repo_root: &Path,
    spec: &CommandSpec,
) -> Result<std::process::Output> {
    println!("$ {}", command_text(spec));
    Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(repo_root)
        .output()
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to run {}", command_text(spec)))
}

pub(super) fn command_text(spec: &CommandSpec) -> String {
    std::iter::once(spec.program.as_str())
        .chain(spec.args.iter().map(String::as_str))
        .map(shell_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(word: &str) -> String {
    if word
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '=' | ':'))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}
