//! Fail when authored Rust has code lines rustfmt could not wrap.
//!
//! rustfmt leaves a whole statement unformatted when one of its pieces cannot
//! fit, and still reports success. A line whose code, with string literal
//! contents elided, is wider than `max_width` therefore marks a statement
//! rustfmt silently skipped. A literal that cannot be broken (a raw string or
//! URL) may overflow on its own.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use miette::{miette, IntoDiagnostic, Result};

const MAX_WIDTH: usize = 100;

/// Line numbers (1-based) whose content lies entirely inside a multi-line
/// string literal.
fn string_line_spans(source: &str) -> BTreeSet<usize> {
    let bytes = source.as_bytes();
    let mut inside = BTreeSet::new();
    let (mut index, mut line) = (0, 1);
    let newlines = |from: usize, to: usize| bytes[from..to].iter().filter(|b| **b == b'\n').count();
    while index < bytes.len() {
        let rest = &bytes[index..];
        if rest.starts_with(b"//") {
            index = source[index..]
                .find('\n')
                .map_or(bytes.len(), |end| index + end);
            continue;
        }
        if rest.starts_with(b"/*") {
            let end = source[index..]
                .find("*/")
                .map_or(bytes.len(), |end| index + end + 2);
            line += newlines(index, end);
            index = end;
            continue;
        }
        let at_word_start =
            index == 0 || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
        let raw = (at_word_start && matches!(bytes[index], b'b' | b'r'))
            .then(|| raw_string_opening(rest))
            .flatten();
        if raw.is_some() || bytes[index] == b'"' {
            let end = match raw {
                Some((opening, hashes)) => {
                    let closing = format!("\"{}", "#".repeat(hashes));
                    source[index + opening..]
                        .find(&closing)
                        .map_or(bytes.len(), |end| index + opening + end + closing.len())
                }
                None => {
                    let mut end = index + 1;
                    while end < bytes.len() && bytes[end] != b'"' {
                        end += if bytes[end] == b'\\' { 2 } else { 1 };
                    }
                    (end + 1).min(bytes.len())
                }
            };
            let lines = newlines(index, end);
            inside.extend(line + 1..line + lines + 1);
            line += lines;
            index = end;
            continue;
        }
        if bytes[index] == b'\''
            && index + 2 < bytes.len()
            && (bytes[index + 2] == b'\''
                || (bytes[index + 1] == b'\\' && bytes.get(index + 3) == Some(&b'\'')))
        {
            index += if bytes[index + 1] == b'\\' { 4 } else { 3 };
            continue;
        }
        if bytes[index] == b'\n' {
            line += 1;
        }
        index += 1;
    }
    inside
}

/// The length and hash count of a raw string opening such as `br##"`.
fn raw_string_opening(rest: &[u8]) -> Option<(usize, usize)> {
    let mut position = usize::from(rest.first() == Some(&b'b'));
    if rest.get(position) != Some(&b'r') {
        return None;
    }
    position += 1;
    let hashes = rest[position..].iter().take_while(|b| **b == b'#').count();
    position += hashes;
    (rest.get(position) == Some(&b'"')).then_some((position + 1, hashes))
}

/// The width of a line once each complete string literal's contents are
/// removed; a literal continuing onto the next line elides to its opening quote.
fn elided_width(line: &str) -> usize {
    let bytes = line.as_bytes();
    let (mut width, mut index) = (0, 0);
    while index < bytes.len() {
        let at_word_start =
            index == 0 || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
        if let Some((opening, hashes)) = (at_word_start && matches!(bytes[index], b'b' | b'r'))
            .then(|| raw_string_opening(&bytes[index..]))
            .flatten()
        {
            let closing = format!("\"{}", "#".repeat(hashes));
            if let Some(end) = line[index + opening..].find(&closing) {
                width += opening + closing.len();
                index += opening + end + closing.len();
                continue;
            }
        }
        if bytes[index] == b'"' {
            let mut end = index + 1;
            while end < bytes.len() && bytes[end] != b'"' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            width += if end < bytes.len() { 2 } else { 1 };
            index = end + 1;
            continue;
        }
        width += line[index..].chars().next().map_or(1, char::len_utf8);
        index += line[index..].chars().next().map_or(1, char::len_utf8);
    }
    width
}

/// Overflowing code lines in one source file, as `(line, columns)`.
fn overflows(source: &str) -> Vec<(usize, usize)> {
    let allowed = string_line_spans(source);
    let mut in_use = false;
    let mut found = Vec::new();
    for (number, text) in source
        .split('\n')
        .enumerate()
        .map(|(index, text)| (index + 1, text))
    {
        let stripped = text.trim();
        // rustfmt fully formats `use` declarations; any overflow left there
        // is an identifier too long to split, not a skipped statement.
        let visibility = stripped
            .strip_prefix("pub")
            .map(|rest| {
                rest.strip_prefix('(')
                    .and_then(|rest| rest.split_once(") ").map(|(_, rest)| rest))
                    .or_else(|| rest.strip_prefix(' '))
                    .unwrap_or(rest)
            })
            .unwrap_or(stripped);
        if visibility.starts_with("use ") {
            in_use = true;
        }
        if in_use {
            in_use = !stripped.ends_with(';');
            continue;
        }
        let columns = text.chars().count();
        if columns > MAX_WIDTH && !allowed.contains(&number) && elided_width(text) > MAX_WIDTH {
            found.push((number, columns));
        }
    }
    found
}

/// Check the given files, or every tracked authored Rust file.
pub(crate) fn run(root: &Path, files: Vec<PathBuf>) -> Result<()> {
    let files = if files.is_empty() {
        let output = Command::new("git")
            .args(["ls-files", "*.rs"])
            .current_dir(root)
            .output()
            .into_diagnostic()?;
        String::from_utf8(output.stdout)
            .into_diagnostic()?
            .lines()
            .filter(|path| !path.contains("/generated/"))
            .map(|path| root.join(path))
            .collect()
    } else {
        files
    };
    let mut failures = 0;
    for path in files {
        let source = std::fs::read_to_string(&path).into_diagnostic()?;
        for (line, columns) in overflows(&source) {
            failures += 1;
            println!("{}:{line}: {columns} columns", path.display());
        }
    }
    if failures > 0 {
        return Err(miette!(
            "{failures} Rust lines exceed {MAX_WIDTH} columns of code; rustfmt skipped their \
            statements. Split long literals or restructure."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_code_line_is_reported_but_long_literals_are_not() {
        let long_code = format!("    let value = {};", "x + ".repeat(30));
        let long_literal = format!("    let value = \"{}\";", "x".repeat(120));
        let long_raw = format!("    let value = r#\"{}\"#;", "x".repeat(120));
        let continued = format!("    let value = \"start\n{}\nend\";", "x".repeat(120));
        let long_use = format!("use crate::{};", "segment".repeat(20));
        let source = [
            long_code.as_str(),
            &long_literal,
            &long_raw,
            &continued,
            &long_use,
        ]
        .join("\n");
        assert_eq!(
            overflows(&source)
                .into_iter()
                .map(|(line, _)| line)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }
}
