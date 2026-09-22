use miette::IntoDiagnostic;
use trellis_runtime_apis::{
    apis::trellis_core_v1::Client,
    types::{ResourcesDestroyRequest, ResourcesInspectRequest, ResourcesQueryRequest},
    CursorQuery,
};

use crate::{
    app::{connect_authenticated_cli_client, wire},
    cli::*,
    output,
};

pub(super) async fn run(format: OutputFormat, command: ResourcesCommand) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let client = Client::from_generated(connected);
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
            };
            if args.all {
                output::print_json_stream(client.resources_query_items(input)).await
            } else {
                output::print_json(&client.resources_query(&input).await.into_diagnostic()?)
            }
        }
        ResourcesCommand::Inspect(args) => {
            let response = client
                .resources_inspect(&ResourcesInspectRequest {
                    resource_id: wire(args.resource_id)?,
                })
                .await
                .into_diagnostic()?;
            if output::is_json(format) {
                output::print_json(&response)
            } else {
                output::print_info(&format!("resourceId={}", response.resource.resource_id));
                output::print_info(&format!("state={}", response.resource.state));
                output::print_info("desired:");
                output::print_json(&response.resource.desired)?;
                output::print_info("actual:");
                output::print_json(&response.resource.actual)?;
                output::print_info("history:");
                output::print_json(&response.resource.history)
            }
        }
        ResourcesCommand::Destroy(args) => output::print_json(
            &client
                .resources_destroy(&ResourcesDestroyRequest {
                    resource_id: wire(args.resource_id)?,
                    expected_revision: wire(args.expected_revision.to_string())?,
                    confirm_physical_id: wire(args.confirm_physical_id)?,
                })
                .await
                .into_diagnostic()?,
        ),
    }
}
