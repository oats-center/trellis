use futures_util::{Stream, TryStreamExt as _};
use miette::IntoDiagnostic;
use serde::Serialize;
use serde_json::Value;
use trellis_runtime_apis::{
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

pub(super) async fn run(format: OutputFormat, command: EventsCommand) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let client = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_events_v1();
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
                extra: Default::default(),
            };
            if args.all {
                listed_all(format, client.query_items(input), &EVENTS, NO_EVENTS).await
            } else {
                let page = client.query(&input).await.into_diagnostic()?;
                listed(format, &page, &EVENTS, NO_EVENTS, "trellis events query")
            }
        }
        EventsCommand::Inspect(args) => {
            let response = client
                .inspect(&EventsInspectRequest {
                    event_id: args.event_id,
                    stream_sequence: args
                        .stream_sequence
                        .map(|sequence| wire(sequence.to_string()))
                        .transpose()?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |response| {
                let event = &response["event"];
                let row = &event["row"];
                output::Details::new(output::field(row, "subject"))
                    .field("Event", event_name(row))
                    .field("ID", output::field(row, "eventId"))
                    .field("Sequence", output::field(row, "streamSequence"))
                    .time("Time", &output::field(row, "eventTime"))
                    .field("Publisher", output::field(row, "publisherParticipantId"))
                    .field("Deployment", output::field(row, "publisherDeploymentId"))
                    .field("Instance", output::field(row, "publisherInstanceId"))
                    .state("Verification", &output::field(row, "verificationStatus"))
                    .field("Resolution", output::field(row, "resolution"))
                    .field("Trace", output::field(row, "traceId"))
                    .section("Headers")
                    .object(&event["headers"])
                    .print();
                print_payload(&output::field(event, "payload"));
                Ok(())
            })
        }
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
                    extra: Default::default(),
                };
                if args.page.all {
                    listed_all(
                        format,
                        client.consumers_query_items(input),
                        &CONSUMERS,
                        NO_CONSUMERS,
                    )
                    .await
                } else {
                    let page = client.consumers_query(&input).await.into_diagnostic()?;
                    listed(
                        format,
                        &page,
                        &CONSUMERS,
                        NO_CONSUMERS,
                        "trellis events consumers query",
                    )
                }
            }
            ConsumersCommand::Inspect(args) => {
                let response = client
                    .consumers_inspect(&EventsConsumersInspectRequest {
                        resource_id: args.resource_id,
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?;
                output::emit(format, &response, |response| {
                    let consumer = &response["consumer"];
                    output::Details::new(output::field(consumer, "consumerName"))
                        .field("Resource", output::field(consumer, "resourceId"))
                        .state("Status", &output::field(consumer, "status"))
                        .field("Stream", output::field(consumer, "stream"))
                        .field("Group", output::field(consumer, "group"))
                        .field("Contract", output::field(consumer, "contractId"))
                        .field("Deployment", output::field(consumer, "deploymentId"))
                        .field("Managed by", output::field(consumer, "managedBy"))
                        .field("Pending", output::field(consumer, "pending"))
                        .field("Awaiting ack", output::field(consumer, "ackPending"))
                        .field("Redelivered", output::field(consumer, "redelivered"))
                        .field("Waiting pulls", output::field(consumer, "waitingPulls"))
                        .field("Max deliveries", output::field(consumer, "maxDeliver"))
                        .field("Ack wait (ms)", output::field(consumer, "ackWaitMs"))
                        .time(
                            "Oldest pending",
                            &output::field(consumer, "oldestPendingAt"),
                        )
                        .field(
                            "Oldest event",
                            output::field(consumer, "oldestPendingEventId"),
                        )
                        .section("Subjects")
                        .items(
                            output::list(consumer, "filterSubjects")
                                .iter()
                                .map(|subject| output::field(subject, "")),
                        )
                        .print();
                    Ok(())
                })
            }
        },
        EventsCommand::DeadLetters(command) => match command {
            DeadLettersCommand::Query(args) => {
                let input = EventsDeadLettersQueryRequest {
                    page: page(&args.page),
                    resource_id: args.resource_id,
                    state: None,
                    extra: Default::default(),
                };
                if args.page.all {
                    listed_all(
                        format,
                        client.dead_letters_query_items(input),
                        &DEAD_LETTERS,
                        NO_DEAD_LETTERS,
                    )
                    .await
                } else {
                    let page = client.dead_letters_query(&input).await.into_diagnostic()?;
                    listed(
                        format,
                        &page,
                        &DEAD_LETTERS,
                        NO_DEAD_LETTERS,
                        "trellis events dead-letters query",
                    )
                }
            }
            DeadLettersCommand::Inspect(args) => {
                let response = client
                    .dead_letters_inspect(&EventsDeadLettersInspectRequest {
                        resource_id: args.resource_id,
                        dead_letter_id: args.dead_letter_id,
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?;
                output::emit(format, &response, |response| {
                    let detail = &response["deadLetter"];
                    let letter = &detail["deadLetter"];
                    output::Details::new(format!(
                        "Dead letter {}",
                        output::field(letter, "deadLetterId")
                    ))
                    .state("State", &output::field(letter, "state"))
                    .field("Consumer", output::field(letter, "resourceId"))
                    .field("Subject", output::field(detail, "originalSubject"))
                    .field(
                        "Original",
                        format!(
                            "{} #{}",
                            output::field(letter, "originalStream"),
                            output::field(letter, "originalSequence")
                        ),
                    )
                    .time("Published", &output::field(detail, "originalPublishedAt"))
                    .field("Deliveries", output::field(letter, "deliveries"))
                    .field("Last error", output::field(letter, "lastError"))
                    .state("Verification", &output::field(detail, "verificationStatus"))
                    .field("Revision", output::field(letter, "revision"))
                    .time("Updated", &output::field(letter, "updatedAt"))
                    .section("Headers")
                    .object(&detail["originalHeaders"])
                    .print();
                    print_payload(&output::field(detail, "originalPayload"));
                    Ok(())
                })
            }
            DeadLettersCommand::Replay(args) => dead_letter_changed(
                format,
                "Requested redelivery of",
                &client
                    .dead_letters_replay(&EventsDeadLettersReplayRequest {
                        resource_id: args.resource_id,
                        dead_letter_id: args.dead_letter_id,
                        expected_revision: wire(args.expected_revision.to_string())?,
                        request_id: args.request_id,
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?,
            ),
            DeadLettersCommand::Dismiss(args) => dead_letter_changed(
                format,
                "Dismissed",
                &client
                    .dead_letters_dismiss(&EventsDeadLettersDismissRequest {
                        resource_id: args.resource_id,
                        dead_letter_id: args.dead_letter_id,
                        expected_revision: wire(args.expected_revision.to_string())?,
                        request_id: args.request_id,
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?,
            ),
        },
    }
}

const EVENTS: [output::Column<'static>; 6] = [
    output::muted("Seq", "streamSequence"),
    output::time_col("Time", "eventTime"),
    output::col("Subject", "subject"),
    output::col("Event", "ownerEventName"),
    output::col("Publisher", "publisherParticipantId"),
    output::state_col("Verified", "verificationStatus"),
];
const NO_EVENTS: &str = "No events match.";

const CONSUMERS: [output::Column<'static>; 6] = [
    output::col("Consumer", "consumerName"),
    output::state_col("Status", "status"),
    output::col("Pending", "pending"),
    output::col("Awaiting ack", "ackPending"),
    output::col("Stream", "stream"),
    output::muted("Resource", "resourceId"),
];
const NO_CONSUMERS: &str = "No consumers match.";

const DEAD_LETTERS: [output::Column<'static>; 6] = [
    output::muted("ID", "deadLetterId"),
    output::state_col("State", "state"),
    output::col("Deliveries", "deliveries"),
    output::time_col("Updated", "updatedAt"),
    output::col("Error", "lastError"),
    output::muted("Consumer", "resourceId"),
];
const NO_DEAD_LETTERS: &str = "No dead letters. Every delivery succeeded or is still retrying.";

/// One page of a listing.
fn listed<T: Serialize>(
    format: OutputFormat,
    page: &T,
    columns: &[output::Column<'_>],
    empty: &str,
    command: &str,
) -> miette::Result<()> {
    output::emit(format, page, |page| {
        output::table(output::list(page, "items"), columns, empty)?;
        output::more_pages(page, command);
        Ok(())
    })
}

/// Every page of a listing: a JSON array as it arrives, or one table.
async fn listed_all<T: Serialize, E: std::fmt::Display>(
    format: OutputFormat,
    items: impl Stream<Item = Result<T, E>> + Unpin,
    columns: &[output::Column<'_>],
    empty: &str,
) -> miette::Result<()> {
    if output::is_json(format) {
        return output::print_json_stream(items).await;
    }
    let items = items
        .map_err(|error| miette::miette!("{error}"))
        .try_collect::<Vec<_>>()
        .await?;
    output::table(&items, columns, empty)
}

fn dead_letter_changed<T: Serialize>(
    format: OutputFormat,
    done: &str,
    response: &T,
) -> miette::Result<()> {
    output::emit(format, response, |response| {
        let letter = &response["deadLetter"];
        output::success(format!(
            "{done} dead letter {}; it is now {}",
            output::strong(output::field(letter, "deadLetterId")),
            output::state(&output::field(letter, "state"))
        ));
        Ok(())
    })
}

fn event_name(row: &Value) -> String {
    match (
        output::field(row, "ownerContractId"),
        output::field(row, "ownerEventName"),
    ) {
        (contract, name) if !contract.is_empty() && !name.is_empty() => {
            format!("{contract} {name}")
        }
        (_, name) => name,
    }
}

/// A payload as indented JSON or text when it is either, otherwise its size.
fn print_payload(base64: &str) {
    use base64::Engine as _;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(base64) else {
        return;
    };
    let body = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_default(),
        Err(_) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => format!(
                "{} of binary data",
                output::plural(error.as_bytes().len(), "byte")
            ),
        },
    };
    anstream::println!("\n{}", output::strong("Payload"));
    for line in body.lines() {
        anstream::println!("  {line}");
    }
}
