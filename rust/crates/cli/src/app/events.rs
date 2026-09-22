use miette::IntoDiagnostic;
use trellis_runtime_apis::{
    apis::trellis_events_v1::Client,
    types::{
        EventsConsumersInspectRequest, EventsConsumersQueryRequest,
        EventsDeadLettersDismissRequest, EventsDeadLettersInspectRequest,
        EventsDeadLettersQueryRequest, EventsDeadLettersReplayRequest, EventsInspectRequest,
        EventsQueryRequest,
    },
    CursorQuery,
};

use crate::{
    app::{connect_authenticated_cli_client, wire},
    cli::*,
    output,
};

fn page(args: &PageArgs) -> Option<CursorQuery> {
    (args.cursor.is_some() || args.limit.is_some()).then(|| CursorQuery {
        cursor: args.cursor.clone(),
        limit: args.limit,
    })
}

pub(super) async fn run(command: EventsCommand) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let client = Client::from_generated(connected);
    match command {
        EventsCommand::Query(args) => {
            let input = EventsQueryRequest {
                consumer_deployment_id: None,
                consumer_name: None,
                exclude_event_types: None,
                include_event_types: None,
                integrity_exception_only: None,
                owner_contract_id: None,
                owner_event_name: None,
                page: page(&args),
                publisher_deployment_id: None,
                publisher_participant_id: None,
                resolution: None,
                search: None,
                sort: None,
                subject: None,
                verification_status: None,
                window: None,
            };
            if args.all {
                output::print_json_stream(client.query_items(input)).await
            } else {
                output::print_json(&client.query(&input).await.into_diagnostic()?)
            }
        }
        EventsCommand::Inspect(args) => output::print_json(
            &client
                .inspect(&EventsInspectRequest {
                    event_id: args.event_id,
                    stream_sequence: args.stream_sequence.map(wire).transpose()?,
                })
                .await
                .into_diagnostic()?,
        ),
        EventsCommand::Consumers(command) => match command {
            ConsumersCommand::Query(args) => {
                let input = EventsConsumersQueryRequest {
                    contract_id: None,
                    deployment_id: None,
                    owner_contract_id: None,
                    page: page(&args.page),
                    resource_id: args.resource_id,
                    status: None,
                    subject: None,
                };
                if args.page.all {
                    output::print_json_stream(client.consumers_query_items(input)).await
                } else {
                    output::print_json(&client.consumers_query(&input).await.into_diagnostic()?)
                }
            }
            ConsumersCommand::Inspect(args) => output::print_json(
                &client
                    .consumers_inspect(&EventsConsumersInspectRequest {
                        resource_id: args.resource_id,
                    })
                    .await
                    .into_diagnostic()?,
            ),
        },
        EventsCommand::DeadLetters(command) => match command {
            DeadLettersCommand::Query(args) => {
                let input = EventsDeadLettersQueryRequest {
                    page: page(&args.page),
                    resource_id: args.resource_id,
                    state: None,
                };
                if args.page.all {
                    output::print_json_stream(client.dead_letters_query_items(input)).await
                } else {
                    output::print_json(&client.dead_letters_query(&input).await.into_diagnostic()?)
                }
            }
            DeadLettersCommand::Inspect(args) => output::print_json(
                &client
                    .dead_letters_inspect(&EventsDeadLettersInspectRequest {
                        resource_id: args.resource_id,
                        dead_letter_id: args.dead_letter_id,
                    })
                    .await
                    .into_diagnostic()?,
            ),
            DeadLettersCommand::Replay(args) => output::print_json(
                &client
                    .dead_letters_replay(&EventsDeadLettersReplayRequest {
                        resource_id: args.resource_id,
                        dead_letter_id: args.dead_letter_id,
                        expected_revision: wire(args.expected_revision.to_string())?,
                        request_id: args.request_id,
                    })
                    .await
                    .into_diagnostic()?,
            ),
            DeadLettersCommand::Dismiss(args) => output::print_json(
                &client
                    .dead_letters_dismiss(&EventsDeadLettersDismissRequest {
                        resource_id: args.resource_id,
                        dead_letter_id: args.dead_letter_id,
                        expected_revision: wire(args.expected_revision.to_string())?,
                        request_id: args.request_id,
                    })
                    .await
                    .into_diagnostic()?,
            ),
        },
    }
}
