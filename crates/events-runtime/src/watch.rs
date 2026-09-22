//! `Events.Watch` feed implementation.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::{stream, Stream, StreamExt};
use serde_json::json;
use trellis_rs::service::{Router, ServerError};
use trellis_runtime_apis::apis::trellis_events_v1::{feeds, feeds::Watch};

use crate::projector::{project_message_inner, EventMessageStream, EventVerifier, EventsRuntime};
use crate::storage::ProjectedEvent;
use crate::wire::generated_output;

/// Register the `Events.Watch` feed on the built-in Events router.
pub fn register_events_watch_feed(
    router: &mut Router,
    events_runtime: EventsRuntime,
    verifier: EventVerifier,
) {
    router.register_feed::<Watch, _, _>(move |_ctx, input| {
        watch_events(input, events_runtime.clone(), verifier.clone())
    });
}

fn watch_events(
    input: feeds::WatchInput,
    events_runtime: EventsRuntime,
    verifier: EventVerifier,
) -> impl Stream<Item = Result<feeds::WatchEvent, ServerError>> + Send + 'static {
    stream::unfold(
        WatchState::Init {
            events_runtime,
            input,
            verifier,
        },
        next_watch_frame,
    )
}

enum WatchState {
    Init {
        events_runtime: EventsRuntime,
        input: feeds::WatchInput,
        verifier: EventVerifier,
    },
    Open {
        messages: EventMessageStream,
        input: feeds::WatchInput,
        verifier: EventVerifier,
    },
    Done,
}

async fn next_watch_frame(
    state: WatchState,
) -> Option<(Result<feeds::WatchEvent, ServerError>, WatchState)> {
    let (mut messages, input, verifier) = match state {
        WatchState::Init {
            events_runtime,
            input,
            verifier,
        } => {
            let messages = match events_runtime.live_events().await {
                Ok(messages) => messages,
                Err(error) => {
                    return Some((
                        Err(ServerError::Nats(format!(
                            "failed to start Events.Watch: {error}"
                        ))),
                        WatchState::Done,
                    ));
                }
            };
            (messages, input, verifier)
        }
        WatchState::Open {
            messages,
            input,
            verifier,
        } => (messages, input, verifier),
        WatchState::Done => return None,
    };

    loop {
        let message = match messages.next().await {
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                return Some((
                    Err(ServerError::Nats(format!("Events.Watch failed: {error}"))),
                    WatchState::Done,
                ));
            }
            None => return None,
        };
        let frame = project_message_inner(&message, verifier.clone())
            .await
            .map_err(|error| {
                ServerError::Nats(format!("Events.Watch verification failed: {error}"))
            })
            .and_then(|event| watch_frame(&input, &event));
        let _ = message.ack().await;
        match frame {
            Ok(Some(frame)) => {
                return Some((
                    Ok(frame),
                    WatchState::Open {
                        messages,
                        input,
                        verifier,
                    },
                ));
            }
            Ok(None) => {}
            Err(error) => return Some((Err(error), WatchState::Done)),
        }
    }
}

fn watch_frame(
    input: &feeds::WatchInput,
    event: &ProjectedEvent,
) -> Result<Option<feeds::WatchEvent>, ServerError> {
    if !matches_input(
        input,
        &event.subject,
        event.owner_contract_id.as_deref(),
        event.owner_event_name.as_deref(),
        event.publisher_deployment_id.as_deref(),
        event.publisher_participant_id.as_deref(),
    ) {
        return Ok(None);
    }
    let headers = serde_json::from_str::<BTreeMap<String, Vec<String>>>(&event.headers_json)
        .map_err(|error| ServerError::Nats(format!("Events.Watch headers failed: {error}")))?
        .into_iter()
        .map(|(name, values)| (name, values.join(",")))
        .collect::<BTreeMap<_, _>>();
    generated_output(
        json!({
            "events": [{
                "headers": headers,
                "payload": STANDARD.encode(&event.payload_bytes),
                "row": {
                    "eventId": event.event_id,
                    "eventTime": event.event_time,
                    "headerCount": headers.len(),
                    "ownerContractId": event.owner_contract_id,
                    "ownerEventName": event.owner_event_name,
                    "payloadSizeBytes": event.payload_bytes.len(),
                    "publisherDeploymentId": event.publisher_deployment_id,
                    "publisherInstanceId": event.publisher_instance_id,
                    "publisherKind": event.publisher_kind,
                    "publisherParticipantId": event.publisher_participant_id,
                    "resolution": event.resolution,
                    "streamSequence": event.stream_sequence,
                    "subject": event.subject,
                    "traceId": event.trace_id,
                    "verificationStatus": event.verification_status,
                }
            }],
            "lastStreamSequence": event.stream_sequence,
        }),
        &[
            "headerCount",
            "lastStreamSequence",
            "payloadSizeBytes",
            "streamSequence",
        ],
    )
    .map(Some)
}

fn matches_input(
    input: &feeds::WatchInput,
    subject: &str,
    owner_contract_id: Option<&str>,
    owner_event_name: Option<&str>,
    publisher_deployment_id: Option<&str>,
    publisher_participant_id: Option<&str>,
) -> bool {
    if input
        .subject
        .as_deref()
        .is_some_and(|wanted| wanted != subject)
        || input
            .owner_contract_id
            .as_deref()
            .is_some_and(|wanted| Some(wanted) != owner_contract_id)
        || input
            .owner_event_name
            .as_deref()
            .is_some_and(|wanted| Some(wanted) != owner_event_name)
        || input
            .publisher_deployment_id
            .as_deref()
            .is_some_and(|wanted| Some(wanted) != publisher_deployment_id)
        || input
            .publisher_participant_id
            .as_deref()
            .is_some_and(|wanted| Some(wanted) != publisher_participant_id)
    {
        return false;
    }
    let selected = |contract: &str, event_name: &str| {
        Some(contract) == owner_contract_id && Some(event_name) == owner_event_name
    };
    if input.include_event_types.as_ref().is_some_and(|types| {
        !types
            .iter()
            .any(|event| selected(&event.owner_contract_id, &event.owner_event_name))
    }) {
        return false;
    }
    !input.exclude_event_types.as_ref().is_some_and(|types| {
        types
            .iter()
            .any(|event| selected(&event.owner_contract_id, &event.owner_event_name))
    })
}

#[cfg(test)]
mod tests {
    use super::matches_input;
    use trellis_runtime_apis::apis::trellis_events_v1::feeds;

    #[test]
    fn watch_filters_resolved_event_types() {
        let input: feeds::WatchInput = serde_json::from_value(serde_json::json!({
            "includeEventTypes": [{
                "ownerContractId": "acme-orders.runtime@v1",
                "ownerEventName": "Changed"
            }]
        }))
        .expect("watch input");

        assert!(matches_input(
            &input,
            "events.v1.token.Changed",
            Some("acme-orders.runtime@v1"),
            Some("Changed"),
            None,
            None,
        ));
        assert!(!matches_input(
            &input,
            "events.v1.token.Created",
            Some("acme-orders.runtime@v1"),
            Some("Created"),
            None,
            None,
        ));
    }
}
