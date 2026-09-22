use comfy_table::{presets::UTF8_FULL, Cell, Table};
use futures_util::{Stream, StreamExt};
use miette::IntoDiagnostic;
use owo_colors::OwoColorize;
use serde::Serialize;

use crate::cli::OutputFormat;

pub fn print_json<T: Serialize>(value: &T) -> miette::Result<()> {
    let mut value = serde_json::to_value(value).into_diagnostic()?;
    stringify_wide_integers(&mut value);
    println!(
        "{}",
        serde_json::to_string_pretty(&value).into_diagnostic()?
    );
    Ok(())
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
    use std::io::Write as _;

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

fn stringify_wide_integers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(number) => {
            const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
            if number
                .as_u64()
                .is_some_and(|value| value > MAX_SAFE_INTEGER)
                || number
                    .as_i64()
                    .is_some_and(|value| value < -(MAX_SAFE_INTEGER as i64))
            {
                *value = serde_json::Value::String(number.to_string());
            }
        }
        serde_json::Value::Array(values) => {
            values.iter_mut().for_each(stringify_wide_integers);
        }
        serde_json::Value::Object(fields) => {
            fields.values_mut().for_each(stringify_wide_integers);
        }
        _ => {}
    }
}

pub fn print_success(message: &str) {
    println!("{} {}", "ok".green().bold(), message);
}

pub fn print_info(message: &str) {
    println!("{}", message);
}

pub fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(headers.iter().map(|header| Cell::new(*header)));
    for row in rows {
        table.add_row(row);
    }
    table.to_string()
}

pub fn is_json(format: OutputFormat) -> bool {
    matches!(format, OutputFormat::Json)
}

#[cfg(test)]
mod tests {
    use super::stringify_wide_integers;

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
}
