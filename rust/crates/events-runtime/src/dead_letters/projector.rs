use std::time::Duration;

use async_nats::jetstream::{self, consumer, AckKind};
use futures_util::StreamExt;
use trellis_rs::service::ServerError;

use super::{DeadLetterTransition, DLQ_STREAM, DLQ_SUBJECTS};
use crate::storage::EventsStore;

/// Handle for the rebuildable dead-letter SQL projector.
pub struct DeadLetterProjectorHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl DeadLetterProjectorHandle {
    /// Stop the projector.
    pub async fn stop(self) {
        if let Some(task) = self.task {
            task.abort();
            let _ = task.await;
        }
    }

    /// Drop a completed task after observing its result.
    pub fn discard_completed(&mut self) {
        self.task = None;
    }

    /// Wait for the projector to terminate.
    pub async fn wait(&mut self) -> Result<(), ServerError> {
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(ServerError::Nats(format!(
                "dead-letter projector task failed: {error}"
            ))),
        }
    }
}

/// Start projecting authoritative dead-letter journal transitions into SQLite.
pub async fn start_dead_letter_projector(
    client: async_nats::Client,
    store: EventsStore,
) -> Result<DeadLetterProjectorHandle, ServerError> {
    let projection_id = store
        .projection_id()
        .map_err(|error| ServerError::Nats(error.to_string()))?;
    let durable = format!("events-dlq-projector-{}", sanitize(&projection_id));
    let mut stream = jetstream::new(client)
        .get_stream(DLQ_STREAM)
        .await
        .map_err(|error| ServerError::Nats(error.to_string()))?;
    let checkpoint = store
        .dead_letter_projection_checkpoint()
        .map_err(ServerError::from)?;
    let last_sequence = stream
        .info()
        .await
        .map_err(|error| ServerError::Nats(error.to_string()))?
        .state
        .last_sequence;
    for sequence in checkpoint.saturating_add(1)..=last_sequence {
        let raw = stream
            .get_raw_message(sequence)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        let transition = serde_json::from_slice::<DeadLetterTransition>(&raw.payload)
            .map_err(ServerError::Json)?;
        store
            .project_dead_letter(&transition, sequence)
            .map_err(ServerError::from)?;
    }
    let consumer = stream
        .get_or_create_consumer(
            &durable,
            consumer::pull::Config {
                durable_name: Some(durable.clone()),
                filter_subject: DLQ_SUBJECTS.to_owned(),
                deliver_policy: consumer::DeliverPolicy::All,
                ack_policy: consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| ServerError::Nats(error.to_string()))?;
    let task = tokio::spawn(async move {
        loop {
            let mut messages = consumer
                .fetch()
                .max_messages(100)
                .expires(Duration::from_millis(500))
                .messages()
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;
            while let Some(message) = messages.next().await {
                let message = message.map_err(|error| ServerError::Nats(error.to_string()))?;
                let sequence = message
                    .info()
                    .map_err(|error| ServerError::Nats(error.to_string()))?
                    .stream_sequence;
                let transition = match serde_json::from_slice::<DeadLetterTransition>(
                    &message.payload,
                ) {
                    Ok(transition) => transition,
                    Err(error) => {
                        tracing::error!(%error, sequence, "invalid authoritative dead-letter transition");
                        message
                            .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                            .await
                            .map_err(|error| ServerError::Nats(error.to_string()))?;
                        continue;
                    }
                };
                let projected_store = store.clone();
                match tokio::task::spawn_blocking(move || {
                    projected_store.project_dead_letter(&transition, sequence)
                })
                .await
                {
                    Ok(Ok(())) => message
                        .ack()
                        .await
                        .map_err(|error| ServerError::Nats(error.to_string()))?,
                    Ok(Err(error)) => {
                        tracing::error!(%error, sequence, "dead-letter projection rejected transition");
                        message
                            .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                            .await
                            .map_err(|error| ServerError::Nats(error.to_string()))?;
                    }
                    Err(error) => return Err(ServerError::Nats(error.to_string())),
                }
            }
        }
    });
    Ok(DeadLetterProjectorHandle { task: Some(task) })
}

fn sanitize(value: &str) -> String {
    let value = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect::<String>();
    if value.is_empty() {
        "projection".to_owned()
    } else {
        value
    }
}
