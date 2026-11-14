//! Terminal output shared by every `trellis` command.
//!
//! Results a person asked for (tables, details, secrets) go to stdout. Status
//! lines (`✓`, `!`), hints, and prompts go to stderr, so piping stdout keeps
//! only the data. `--format json` writes one JSON value to stdout and silences
//! status lines and hints. Color appears on terminals unless `NO_COLOR` is
//! set, and `CLICOLOR_FORCE=1` keeps it when piped; tables become
//! headerless tab-separated rows when stdout is not a terminal.

use std::fmt::Display;
use std::io::{IsTerminal as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};

use comfy_table::{presets::NOTHING, Attribute, Cell, Color, ContentArrangement, Row, Table};
use futures_util::{Stream, StreamExt};
use miette::IntoDiagnostic;
use owo_colors::OwoColorize;
use serde::Serialize;
use serde_json::Value;

use crate::cli::OutputFormat;

static JSON: AtomicBool = AtomicBool::new(false);

/// Select the output mode for this process.
pub fn init(format: OutputFormat) {
    JSON.store(matches!(format, OutputFormat::Json), Ordering::Relaxed);
}

pub fn is_json(format: OutputFormat) -> bool {
    matches!(format, OutputFormat::Json)
}

fn json_mode() -> bool {
    JSON.load(Ordering::Relaxed)
}

/// Print `value` as JSON in JSON mode, and otherwise through `human`, which
/// gets the same value as JSON so it can pick fields by name.
pub fn emit<T: Serialize>(
    format: OutputFormat,
    value: &T,
    human: impl FnOnce(&Value) -> miette::Result<()>,
) -> miette::Result<()> {
    if is_json(format) {
        print_json(value)
    } else {
        human(&serde_json::to_value(value).into_diagnostic()?)
    }
}

/// The array at `key` of a JSON value, such as a page's `items`.
pub fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or_default()
}

/// Tell a person how to see the next page of a one-page listing.
pub fn more_pages(page: &Value) {
    if let Some(cursor) = page["page"]["nextCursor"].as_str() {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        hint(format!(
            "More results. Next page: {}",
            code(&next_page_command(&args, cursor))
        ));
    }
}

/// The command that was run, with `cursor` in place of any earlier one. The
/// server binds a cursor to the query's filters, so every other argument stays.
pub(crate) fn next_page_command(args: &[String], cursor: &str) -> String {
    let mut words = vec!["trellis"];
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--cursor" {
            args.next();
        } else if !arg.starts_with("--cursor=") {
            words.push(arg);
        }
    }
    words.extend(["--cursor", cursor]);
    // Arguments come from the operating system and cannot hold a NUL, the
    // only thing quoting refuses.
    shlex::try_join(words).unwrap_or_default()
}

// Status lines, on stderr.

/// A change that happened: `✓ Created user alice`.
pub fn success(message: impl Display) {
    if !json_mode() {
        anstream::eprintln!("{} {message}", "✓".green().bold());
    }
}

/// A request that changed nothing: `• alice already has role readers`.
pub fn unchanged(message: impl Display) {
    if !json_mode() {
        anstream::eprintln!("{} {message}", "•".blue().bold());
    }
}

/// Something the person should act on or know before relying on the result.
pub fn warning(message: impl Display) {
    anstream::eprintln!("{} {message}", "!".yellow().bold());
}

/// Secondary guidance, such as why a list is short.
pub fn hint(message: impl Display) {
    if !json_mode() {
        anstream::eprintln!("{}", message.to_string().dimmed());
    }
}

/// The command a person most likely runs next.
pub fn next(command: &str) {
    if !json_mode() {
        anstream::eprintln!("{} {}", "Next:".dimmed(), code(command));
    }
}

/// A message on stderr without decoration, such as login instructions.
pub fn note(message: impl Display) {
    if !json_mode() {
        anstream::eprintln!("{message}");
    }
}

/// A command or literal inside a sentence.
pub fn code(text: &str) -> String {
    format!("`{}`", text.cyan())
}

/// The entity a sentence is about.
pub fn strong(text: impl Display) -> String {
    text.bold().to_string()
}

// Value rendering.

/// The field at a dotted `path` of a JSON value, as display text; an empty
/// path renders the value itself.
pub fn field(value: &Value, path: &str) -> String {
    let value = if path.is_empty() {
        Some(value)
    } else {
        path.split('.')
            .try_fold(value, |value, field| value.get(field))
    };
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => yes_no(*value).to_owned(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::String(value) => value.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(value @ Value::Object(_)) => value.to_string(),
        Some(Value::Null) | None => String::new(),
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

/// `1 package`, `3 packages`.
pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

pub fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

pub fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Good,
    Waiting,
    Bad,
    Plain,
}

fn tone(state: &str) -> Tone {
    match state {
        "enabled" | "active" | "applied" | "approved" | "accepted" | "ready" | "on" | "yes"
        | "resolved" | "ok" | "healthy" | "verified" | "running" => Tone::Good,
        "pending" | "waiting" | "replay_pending" | "replaying" | "provisioning" | "degraded"
        | "stalled" | "draining" => Tone::Waiting,
        "disabled" | "rejected" | "declined" | "revoked" | "failed" | "dead" | "off" | "no"
        | "removed" | "expired" | "superseded" | "unverified" | "invalid" | "unhealthy" => {
            Tone::Bad
        }
        _ => Tone::Plain,
    }
}

/// A state word colored by what it means: green good, yellow waiting, red off.
pub fn state(text: &str) -> String {
    match tone(text) {
        Tone::Good => text.green().to_string(),
        Tone::Waiting => text.yellow().to_string(),
        Tone::Bad => text.red().to_string(),
        Tone::Plain => text.to_owned(),
    }
}

fn parse_time(raw: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

/// `raw` (RFC 3339) relative to now, such as `5 minutes ago` or `in 2 days`.
pub fn relative_time(raw: &str) -> String {
    let Some(at) = parse_time(raw) else {
        return raw.to_owned();
    };
    let seconds = (at - time::OffsetDateTime::now_utc()).whole_seconds();
    let (amount, unit) = match seconds.unsigned_abs() {
        0..60 => return "just now".to_owned(),
        value @ 60..3_600 => (value / 60, "minute"),
        value @ 3_600..86_400 => (value / 3_600, "hour"),
        value @ 86_400..2_592_000 => (value / 86_400, "day"),
        _ => return absolute_date(at),
    };
    let unit = if amount == 1 {
        unit.to_owned()
    } else {
        format!("{unit}s")
    };
    if seconds < 0 {
        format!("{amount} {unit} ago")
    } else {
        format!("in {amount} {unit}")
    }
}

fn absolute_date(at: time::OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!("{} {:02}, {}", at.month(), at.day(), at.year())
}

/// `raw` (RFC 3339) as a UTC time with its distance from now.
pub fn absolute_time(raw: &str) -> String {
    let Some(at) = parse_time(raw) else {
        return raw.to_owned();
    };
    let utc = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{}-{:02}-{:02} {:02}:{:02} UTC ({})",
        utc.year(),
        u8::from(utc.month()),
        utc.day(),
        utc.hour(),
        utc.minute(),
        relative_time(raw)
    )
}

// Tables, on stdout.

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Muted,
    State,
    Time,
}

/// One table column: its header and the dotted field path it shows.
#[derive(Clone, Copy)]
pub struct Column<'a> {
    header: &'a str,
    path: &'a str,
    kind: Kind,
}

/// A column of plain text.
pub const fn col<'a>(header: &'a str, path: &'a str) -> Column<'a> {
    Column {
        header,
        path,
        kind: Kind::Text,
    }
}

/// A column of secondary text, such as IDs and revisions, shown dim.
pub const fn muted<'a>(header: &'a str, path: &'a str) -> Column<'a> {
    Column {
        header,
        path,
        kind: Kind::Muted,
    }
}

/// A column of state words, colored by meaning.
pub const fn state_col<'a>(header: &'a str, path: &'a str) -> Column<'a> {
    Column {
        header,
        path,
        kind: Kind::State,
    }
}

/// A column of RFC 3339 times, shown relative to now on a terminal.
pub const fn time_col<'a>(header: &'a str, path: &'a str) -> Column<'a> {
    Column {
        header,
        path,
        kind: Kind::Time,
    }
}

/// Print `items` as a table, or `empty` on stderr when there are none.
///
/// A terminal gets aligned columns under a bold header, truncated to its
/// width. Anything else gets one tab-separated line per item with raw values
/// and no header, for `cut`, `awk`, and `grep`.
pub fn table<T: Serialize>(items: &[T], columns: &[Column<'_>], empty: &str) -> miette::Result<()> {
    let items = items
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    if items.is_empty() {
        note(empty);
        return Ok(());
    }
    if !std::io::stdout().is_terminal() {
        let mut stdout = anstream::stdout().lock();
        for item in &items {
            let line = columns
                .iter()
                .map(|column| field(item, column.path).replace(['\t', '\n'], " "))
                .collect::<Vec<_>>()
                .join("\t");
            writeln!(stdout, "{line}").into_diagnostic()?;
        }
        return Ok(());
    }
    let mut table = Table::new();
    table
        .load_style(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_truncation_indicator("…");
    table.style_text_only();
    table.set_header(
        columns
            .iter()
            .map(|column| Cell::new(column.header.to_uppercase()).add_attribute(Attribute::Bold)),
    );
    for item in &items {
        let mut row = Row::from(columns.iter().map(|column| {
            let text = field(item, column.path);
            match column.kind {
                Kind::Text => Cell::new(text),
                Kind::Muted => Cell::new(text).add_attribute(Attribute::Dim),
                Kind::Time => Cell::new(relative_time(&text)).add_attribute(Attribute::Dim),
                Kind::State => {
                    let color = match tone(&text) {
                        Tone::Good => Some(Color::DarkGreen),
                        Tone::Waiting => Some(Color::DarkYellow),
                        Tone::Bad => Some(Color::DarkRed),
                        Tone::Plain => None,
                    };
                    match color {
                        Some(color) => Cell::new(text).fg(color),
                        None => Cell::new(text),
                    }
                }
            }
        }));
        row.max_height(1);
        table.add_row(row);
    }
    let last = columns.len().saturating_sub(1);
    for (index, column) in table.column_iter_mut().enumerate() {
        column.set_padding((0, if index == last { 0 } else { 2 }));
    }
    let mut stdout = anstream::stdout().lock();
    for line in table.lines() {
        writeln!(stdout, "{}", line.trim_end()).into_diagnostic()?;
    }
    Ok(())
}

// Detail views, on stdout.

enum Line {
    Title(String),
    Section(String),
    Field(String, String),
    Item(String),
}

/// One record: a bold title, aligned `Label  value` fields, and sections.
///
/// Fields with empty values are left out, so optional data needs no checks.
pub struct Details {
    lines: Vec<Line>,
}

impl Details {
    pub fn new(title: impl Display) -> Self {
        Self {
            lines: vec![Line::Title(title.to_string())],
        }
    }

    /// Aligned fields without a title, such as what a change produced.
    pub fn untitled() -> Self {
        Self { lines: Vec::new() }
    }

    pub fn field(mut self, label: &str, value: impl Display) -> Self {
        let value = value.to_string();
        if !value.is_empty() {
            self.lines.push(Line::Field(label.to_owned(), value));
        }
        self
    }

    /// A state word, colored by meaning.
    pub fn state(self, label: &str, value: &str) -> Self {
        self.field(label, state(value))
    }

    /// An RFC 3339 time, in UTC with its distance from now.
    pub fn time(self, label: &str, raw: &str) -> Self {
        if raw.is_empty() {
            return self;
        }
        self.field(label, absolute_time(raw))
    }

    /// A heading that starts a new block; left out when nothing follows it.
    pub fn section(mut self, title: &str) -> Self {
        self.lines.push(Line::Section(title.to_owned()));
        self
    }

    /// One entry of a section's list.
    pub fn item(mut self, text: impl Display) -> Self {
        self.lines.push(Line::Item(text.to_string()));
        self
    }

    /// Every field of a JSON object, in order, with nested values compact.
    pub fn object(mut self, value: &Value) -> Self {
        if let Some(fields) = value.as_object() {
            for (key, value) in fields {
                let text = match value {
                    Value::Object(_) | Value::Array(_) if is_empty(value) => continue,
                    Value::Object(_) => value.to_string(),
                    _ => field(value, ""),
                };
                self = self.field(key, text);
            }
        }
        self
    }

    pub fn items<I: Display>(mut self, items: impl IntoIterator<Item = I>) -> Self {
        for item in items {
            self.lines.push(Line::Item(item.to_string()));
        }
        self
    }

    pub fn print(self) {
        let width = self
            .lines
            .iter()
            .filter_map(|line| match line {
                Line::Field(label, _) => Some(label.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let indent = if matches!(self.lines.first(), Some(Line::Title(_))) {
            "  "
        } else {
            ""
        };
        let mut stdout = anstream::stdout().lock();
        let mut lines = self.lines.iter().peekable();
        while let Some(line) = lines.next() {
            let _ = match line {
                Line::Title(title) => writeln!(stdout, "{}", title.bold()),
                Line::Section(title) => {
                    // A section with nothing under it says nothing.
                    if matches!(lines.peek(), None | Some(Line::Section(_))) {
                        continue;
                    }
                    writeln!(stdout, "\n{}", title.bold())
                }
                Line::Field(label, value) => {
                    let padding = width - label.chars().count();
                    writeln!(
                        stdout,
                        "{indent}{}{}  {value}",
                        label.dimmed(),
                        " ".repeat(padding)
                    )
                }
                Line::Item(text) => writeln!(stdout, "  {text}"),
            };
        }
    }
}

/// Say that a secret just printed is shown only this once.
pub fn not_kept(what: &str, advice: &str) {
    warning(format!(
        "Save the {what} now; Trellis does not keep it. {advice}"
    ));
}

/// Ask a person to type `identifier` before an irreversible change.
///
/// Without a terminal on stdin there is nobody to ask, so this refuses and
/// names `flag`, the option that skips the question.
pub fn confirm_typed(identifier: &str, what: &str, flag: &str) -> miette::Result<()> {
    if !std::io::stdin().is_terminal() {
        return Err(miette::miette!(
            help = format!("Pass {flag} to {what} without being asked."),
            "refusing to {what} {identifier} without confirmation"
        ));
    }
    anstream::eprint!(
        "{} This will {what} {}. Type {} to confirm: ",
        "?".yellow().bold(),
        strong(identifier),
        code(identifier)
    );
    std::io::stderr().flush().into_diagnostic()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).into_diagnostic()?;
    miette::ensure!(line.trim() == identifier, "cancelled; nothing changed");
    Ok(())
}

// JSON, on stdout.

pub fn print_json<T: Serialize>(value: &T) -> miette::Result<()> {
    let mut value = serde_json::to_value(value).into_diagnostic()?;
    stringify_wide_integers(&mut value);
    if std::io::stdout().is_terminal() {
        let mut text = String::new();
        write_colored(&mut text, &value, 0);
        anstream::println!("{text}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).into_diagnostic()?
        );
    }
    Ok(())
}

/// Pretty JSON with keys, strings, and literals in distinct colors.
fn write_colored(out: &mut String, value: &Value, indent: usize) {
    use std::fmt::Write as _;
    let quoted = |text: &str| serde_json::to_string(text).expect("strings serialize");
    match value {
        Value::Null => {
            let _ = write!(out, "{}", "null".dimmed());
        }
        Value::Bool(value) => {
            let _ = write!(out, "{}", value.yellow());
        }
        Value::Number(value) => {
            let _ = write!(out, "{}", value.yellow());
        }
        Value::String(value) => {
            let _ = write!(out, "{}", quoted(value).green());
        }
        Value::Array(values) if values.is_empty() => out.push_str("[]"),
        Value::Object(fields) if fields.is_empty() => out.push_str("{}"),
        Value::Array(values) => {
            out.push_str("[\n");
            for (index, value) in values.iter().enumerate() {
                out.push_str(&" ".repeat(indent + 2));
                write_colored(out, value, indent + 2);
                out.push_str(if index + 1 < values.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            out.push_str(&" ".repeat(indent));
            out.push(']');
        }
        Value::Object(fields) => {
            out.push_str("{\n");
            for (index, (key, value)) in fields.iter().enumerate() {
                out.push_str(&" ".repeat(indent + 2));
                let _ = write!(out, "{}: ", quoted(key).blue().bold());
                write_colored(out, value, indent + 2);
                out.push_str(if index + 1 < fields.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
        }
    }
}

pub fn print_json_progress<T: Serialize>(value: &T) -> miette::Result<()> {
    eprintln!("{}", serde_json::to_string(value).into_diagnostic()?);
    Ok(())
}

pub async fn print_json_stream<T, E>(
    mut stream: impl Stream<Item = Result<T, E>> + Unpin,
) -> miette::Result<()>
where
    T: Serialize,
    E: std::fmt::Display,
{
    print!("[");
    let mut first = true;
    while let Some(item) = stream.next().await {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                eprintln!("pagination failed after partial JSON output: {error}");
                return Err(miette::miette!(
                    "pagination failed after partial output: {error}"
                ));
            }
        };
        if !first {
            print!(",");
        }
        first = false;
        let mut item = serde_json::to_value(item).into_diagnostic()?;
        stringify_wide_integers(&mut item);
        serde_json::to_writer(std::io::stdout().lock(), &item).into_diagnostic()?;
        std::io::stdout().flush().into_diagnostic()?;
    }
    println!("]");
    Ok(())
}

fn stringify_wide_integers(value: &mut Value) {
    match value {
        Value::Number(number) => {
            const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
            if number
                .as_u64()
                .is_some_and(|value| value > MAX_SAFE_INTEGER)
                || number
                    .as_i64()
                    .is_some_and(|value| value < -(MAX_SAFE_INTEGER as i64))
            {
                *value = Value::String(number.to_string());
            }
        }
        Value::Array(values) => {
            values.iter_mut().for_each(stringify_wide_integers);
        }
        Value::Object(fields) => {
            fields.values_mut().for_each(stringify_wide_integers);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{relative_time, stringify_wide_integers};

    #[test]
    fn json_preserves_wide_integers_as_decimal_strings() {
        let mut value = serde_json::json!({
            "safe": 9_007_199_254_740_991_u64,
            "wide": 9_007_199_254_740_992_u64,
            "negative": -9_007_199_254_740_992_i64,
        });

        stringify_wide_integers(&mut value);

        assert_eq!(value["safe"], serde_json::json!(9_007_199_254_740_991_u64));
        assert_eq!(value["wide"], "9007199254740992");
        assert_eq!(value["negative"], "-9007199254740992");
    }

    #[test]
    fn times_read_relative_to_now_in_both_directions() {
        let format = time::format_description::well_known::Rfc3339;
        let now = time::OffsetDateTime::now_utc();
        let at = |offset: time::Duration| (now + offset).format(&format).unwrap();

        assert_eq!(relative_time(&at(time::Duration::seconds(-5))), "just now");
        assert_eq!(
            relative_time(&at(time::Duration::minutes(-90))),
            "1 hour ago"
        );
        assert_eq!(
            relative_time(&at(time::Duration::days(3) + time::Duration::minutes(1))),
            "in 3 days"
        );
        assert_eq!(relative_time("not a time"), "not a time");
    }
}
