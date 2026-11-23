//! `trellis storage`: what each kind of Trellis data takes on disk, and how
//! close JetStream is to its limits.

use miette::IntoDiagnostic;
use serde_json::Value;
use trellis_runtime_apis::types::HealthStorageRequest;

use crate::{app::connect_authenticated_cli_client, cli::OutputFormat, output};

const CLASSES: [output::Column<'static>; 5] = [
    output::col("Data", "name"),
    output::col("Size", "size"),
    output::col("Records", "records"),
    output::time_col("Oldest", "oldestAt"),
    output::col("Kept", "retention"),
];

/// A decimal byte count in binary units, as in `5.5 MiB`.
pub(super) fn bytes(raw: &Value) -> String {
    let Some(count) = raw
        .as_str()
        .and_then(|raw| raw.parse::<u64>().ok())
        .or_else(|| raw.as_u64())
    else {
        return "—".to_owned();
    };
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// `used 5.5 MiB, reserved 64.0 MiB of 1.0 GiB`.
pub(super) fn usage(usage: &Value) -> String {
    let limit = match &usage["limit"] {
        Value::Null => "no limit".to_owned(),
        limit => format!("of {}", bytes(limit)),
    };
    format!(
        "used {}, reserved {} {limit}",
        bytes(&usage["used"]),
        bytes(&usage["reserved"])
    )
}

pub(super) async fn run(format: OutputFormat) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_health_v1()
        .storage(&HealthStorageRequest {
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    output::emit(format, &response, |response| {
        let jetstream = &response["jetstream"];
        if !jetstream.is_null() {
            let mut details = output::Details::new("JetStream")
                .state("Status", &output::field(jetstream, "status"))
                .field("Storage", usage(&jetstream["storage"]));
            if !jetstream["serverStorage"].is_null() {
                details = details.field("Server storage", usage(&jetstream["serverStorage"]));
            }
            details = details.field("Memory", usage(&jetstream["memory"]));
            if !jetstream["serverMemory"].is_null() {
                details = details.field("Server memory", usage(&jetstream["serverMemory"]));
            }
            let count = |count: &str, max: &str| match &jetstream[max] {
                Value::Null => output::field(jetstream, count),
                _ => format!(
                    "{} of {}",
                    output::field(jetstream, count),
                    output::field(jetstream, max)
                ),
            };
            details = details
                .field("Streams", count("streams", "maxStreams"))
                .field("Consumers", count("consumers", "maxConsumers"));
            let problems = output::list(jetstream, "problems");
            if !problems.is_empty() {
                details = details
                    .section("Problems")
                    .items(problems.iter().filter_map(Value::as_str));
            }
            details.print();
        }
        let classes: Vec<Value> = output::list(response, "classes")
            .iter()
            .map(|class| {
                let mut class = class.clone();
                class["size"] = Value::String(bytes(&class["bytes"]));
                class
            })
            .collect();
        output::table(&classes, &CLASSES, "Trellis measured no storage.")
    })
}
