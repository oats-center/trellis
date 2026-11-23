//! `trellis doctor`: one report of an installation's state, from
//! `Auth.Runtime.Diagnostics`, with an ok, warning, or problem marker per
//! section and finding. It fails when anything is not ok, so a script or a
//! monitor can run it.

use std::io::Write as _;

use miette::IntoDiagnostic;
use owo_colors::OwoColorize;
use serde_json::Value;
use trellis_runtime_apis::types::AuthEmpty;

use super::storage::{bytes, usage};
use crate::{app::connect_authenticated_cli_client, cli::OutputFormat, output};

/// `✓`, `!`, or `✗` for `ok`, `warning`, or `problem`, colored.
fn marker(status: &str) -> String {
    match status {
        "ok" => "✓".green().to_string(),
        "warning" => "!".yellow().to_string(),
        _ => "✗".red().to_string(),
    }
}

pub(super) async fn run(format: OutputFormat) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let report = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .runtime_diagnostics(&AuthEmpty {
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    let report = serde_json::to_value(report).into_diagnostic()?;
    output::emit(format, &report, |report| {
        print(report);
        Ok(())
    })?;
    let count = |status: &str| {
        [
            "server",
            "health",
            "jetstream",
            "disk",
            "certificates",
            "failures",
            "work",
        ]
        .iter()
        .flat_map(|section| output::list(&report[section], "findings"))
        .filter(|finding| finding["status"] == status)
        .count()
    };
    match report["status"].as_str() {
        Some("ok") => Ok(()),
        _ => Err(miette::miette!(
            "Trellis needs attention: {} and {}.",
            output::plural(count("problem"), "problem"),
            output::plural(count("warning"), "warning")
        )),
    }
}

fn print(report: &Value) {
    let mut stdout = anstream::stdout().lock();
    let mut heading = |title: &str, section: &Value, fields: &[(&str, String)]| {
        let _ = writeln!(
            stdout,
            "{} {}",
            marker(section["status"].as_str().unwrap_or_default()),
            title.bold()
        );
        let width = fields
            .iter()
            .map(|(label, _)| label.len())
            .max()
            .unwrap_or(0);
        for (label, value) in fields.iter().filter(|(_, value)| !value.is_empty()) {
            let _ = writeln!(
                stdout,
                "    {}{}  {value}",
                label.dimmed(),
                " ".repeat(width - label.len())
            );
        }
        for finding in output::list(section, "findings") {
            let _ = writeln!(
                stdout,
                "  {} {}",
                marker(finding["status"].as_str().unwrap_or_default()),
                output::field(finding, "message")
            );
        }
        let _ = writeln!(stdout);
    };

    let server = &report["server"];
    heading(
        "Server",
        server,
        &[
            ("Version", output::field(server, "version")),
            ("Installation", output::field(server, "installationId")),
            ("Mode", output::field(server, "mode")),
            (
                "Started",
                output::absolute_time(&output::field(server, "startedAt")),
            ),
        ],
    );

    let health = &report["health"];
    let deployments: Vec<(&str, String)> = output::list(health, "deployments")
        .iter()
        .map(|deployment| {
            let instances = output::list(deployment, "instances");
            let states = instances
                .iter()
                .map(|instance| output::state(&output::field(instance, "state")))
                .collect::<Vec<_>>()
                .join(", ");
            let label = deployment["title"].as_str().unwrap_or_default();
            let value = if instances.is_empty() {
                "no instances".to_owned()
            } else {
                format!("{}: {states}", output::plural(instances.len(), "instance"))
            };
            (label, value)
        })
        .collect();
    heading("Health", health, &deployments);

    let jetstream = &report["jetstream"];
    let count = |count: &str, max: &str| match (&jetstream[count], &jetstream[max]) {
        (Value::Null, _) => String::new(),
        (_, Value::Null) => output::field(jetstream, count),
        _ => format!(
            "{} of {}",
            output::field(jetstream, count),
            output::field(jetstream, max)
        ),
    };
    let measured = |key: &str| {
        if jetstream[key].is_null() {
            String::new()
        } else {
            usage(&jetstream[key])
        }
    };
    heading(
        "JetStream",
        jetstream,
        &[
            ("Storage", measured("storage")),
            ("Server storage", measured("serverStorage")),
            ("Memory", measured("memory")),
            ("Server memory", measured("serverMemory")),
            ("Streams", count("streams", "maxStreams")),
            ("Consumers", count("consumers", "maxConsumers")),
        ],
    );

    let disk = &report["disk"];
    heading(
        "Disk",
        disk,
        &[
            ("Path", output::field(disk, "path")),
            (
                "Free",
                if disk["free"].is_null() {
                    String::new()
                } else {
                    format!("{} of {}", bytes(&disk["free"]), bytes(&disk["total"]))
                },
            ),
        ],
    );

    let certificates = &report["certificates"];
    let mut fields = vec![("Transport", output::field(certificates, "transport"))];
    for certificate in output::list(certificates, "certificates") {
        let role = if certificate["role"] == "served" {
            "Served"
        } else {
            "Issuing CA"
        };
        fields.push((
            role,
            format!(
                "{}, issued by {}, expires {}",
                output::field(certificate, "subject"),
                output::field(certificate, "issuer"),
                output::absolute_time(&output::field(certificate, "notAfter"))
            ),
        ));
    }
    heading("Certificates", certificates, &fields);

    let failures = &report["failures"];
    let listed = output::list(failures, "failures");
    let mut fields = Vec::new();
    for failure in listed {
        fields.push((
            "Failure",
            [
                output::absolute_time(&output::field(failure, "at")),
                output::field(failure, "id"),
                output::field(failure, "route"),
                output::field(failure, "code"),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  ")
                + ": "
                + &output::field(failure, "error"),
        ));
    }
    if listed.is_empty() {
        fields.push(("Failures", "none since the server started".to_owned()));
    }
    heading("Recent server failures", failures, &fields);

    let work = &report["work"];
    heading(
        "Background work",
        work,
        &[
            (
                "Stuck enforcement",
                output::list(work, "stuckEnforcement").len().to_string(),
            ),
            (
                "Set aside",
                output::list(work, "setAside").len().to_string(),
            ),
        ],
    );
}
