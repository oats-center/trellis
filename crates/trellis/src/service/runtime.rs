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
            ServerError::Internal(format!(
                "failed to subscribe to subject '{subject}': {error}"
            ))
        })
}
