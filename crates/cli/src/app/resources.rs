use futures_util::TryStreamExt as _;
use miette::IntoDiagnostic;
use serde_json::{json, Value};
use trellis_runtime_apis::{
    types::{ResourcesDestroyRequest, ResourcesInspectRequest, ResourcesQueryRequest},
    CursorQuery,
};

use crate::{
    app::{connect_authenticated_cli_client, wire},
    cli::*,
    output,
};

const COLUMNS: [output::Column<'static>; 6] = [
    output::col("Name", "localName"),
    output::col("Kind", "kind"),
    output::state_col("State", "state"),
    output::col("Owner", "participantId"),
    output::time_col("Updated", "updatedAt"),
    output::muted("ID", "resourceId"),
];

pub(super) async fn run(format: OutputFormat, command: ResourcesCommand) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let client = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_core_v1();
    match command {
        ResourcesCommand::List(args) => {
            let input = ResourcesQueryRequest {
                kind: None,
                owner_id: None,
                owner_kind: None,
                page: (args.cursor.is_some() || args.limit.is_some()).then_some(CursorQuery {
                    cursor: args.cursor,
                    limit: args.limit,
                }),
                participant_id: None,
                state: None,
                extra: Default::default(),
            };
            if args.all {
                if output::is_json(format) {
                    return output::print_json_stream(client.resources_query_items(input)).await;
                }
                let items = client
                    .resources_query_items(input)
                    .try_collect::<Vec<_>>()
                    .await
                    .into_diagnostic()?;
                return output::table(&items, &COLUMNS, "No resources are provisioned.");
            }
            let page = client.resources_query(&input).await.into_diagnostic()?;
            output::emit(format, &page, |page| {
                output::table(
                    output::list(page, "items"),
                    &COLUMNS,
                    "No resources are provisioned.",
                )?;
                output::more_pages(page, "trellis resources list");
                Ok(())
            })
        }
        ResourcesCommand::Inspect(args) => {
            let response = client
                .resources_inspect(&ResourcesInspectRequest {
                    resource_id: wire(args.resource_id)?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |response| {
                print_resource(&response["resource"]);
                Ok(())
            })
        }
        ResourcesCommand::Destroy(args) => {
            let resource_id = args.resource_id.clone();
            let response = client
                .resources_destroy(&ResourcesDestroyRequest {
                    resource_id: wire(args.resource_id)?,
                    expected_revision: wire(args.expected_revision.to_string())?,
                    confirm_physical_id: wire(args.confirm_physical_id)?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |response| {
                let id = output::strong(&resource_id);
                if response["destroyed"] == json!(true) {
                    output::success(format!("Destroyed resource {id} and its data"));
                } else {
                    output::unchanged(format!("Resource {id} was already destroyed"));
                }
                Ok(())
            })
        }
    }
}

fn print_resource(resource: &Value) {
    output::Details::new(output::field(resource, "localName"))
        .field("ID", output::field(resource, "resourceId"))
        .field("Kind", output::field(resource, "kind"))
        .state("State", &output::field(resource, "state"))
        .field(
            "Ready",
            match (
                resource["readiness"]["ready"].as_bool(),
                output::field(resource, "readiness.reason"),
            ) {
                (Some(true), _) => output::state("yes"),
                (Some(false), reason) if !reason.is_empty() => {
                    format!("{} ({reason})", output::state("no"))
                }
                (Some(false), _) => output::state("no"),
                (None, _) => String::new(),
            },
        )
        .field("Participant", output::field(resource, "participantId"))
        .field(
            "Owner",
            format!(
                "{} {}",
                output::field(resource, "ownerKind"),
                output::field(resource, "ownerId")
            )
            .trim(),
        )
        .field("Physical ID", output::field(resource, "physicalId"))
        .field("Version", output::field(resource, "version"))
        .field("Revision", output::field(resource, "bindingRevision"))
        .time("Created", &output::field(resource, "createdAt"))
        .time("Updated", &output::field(resource, "updatedAt"))
        .section("Desired")
        .object(&resource["desired"])
        .section("Actual")
        .object(&resource["actual"])
        .section("Commitment")
        .object(&resource["commitment"])
        .section("History")
        .items(output::list(resource, "history").iter().map(|entry| {
            format!(
                "v{}  {}  {}",
                output::field(entry, "version"),
                output::state(&output::field(entry, "state")),
                output::relative_time(&output::field(entry, "changedAt"))
            )
        }))
        .print();
}
