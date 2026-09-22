use super::request_loop::{run_nats_request_loop, RequestHandler};
use super::ServerError;

pub(crate) async fn subscribe_subject(
    client: &async_nats::Client,
    subject: &str,
) -> Result<async_nats::Subscriber, ServerError> {
    client
        .queue_subscribe(
            subject.to_string(),
            trellis_protocol::route_queue_group(subject),
        )
        .await
        .map_err(|error| {
            ServerError::Nats(format!(
                "failed to subscribe to subject '{subject}': {error}"
            ))
        })
}

pub(crate) async fn run_multi_subject_service<H>(
    client: async_nats::Client,
    subjects: &[&str],
    handler: H,
) -> Result<(), ServerError>
where
    H: RequestHandler,
{
    let mut subscribers = Vec::with_capacity(subjects.len());
    for subject in subjects {
        subscribers.push(subscribe_subject(&client, subject).await?);
    }
    run_nats_request_loop(
        client,
        futures_util::stream::select_all(subscribers),
        handler,
    )
    .await
}
