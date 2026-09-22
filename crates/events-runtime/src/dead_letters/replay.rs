use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream, AckKind};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use trellis_rs::service::ServerError;

use super::{
    DeadLetterJournal, DeadLetterState, DeadLetterTransition, ReplayEnvelope, DLQ_STREAM,
    DLQ_SUBJECTS, REPLAY_STREAM,
};

const DISPATCHER_DURABLE: &str = "events-replay-dispatcher";

/// Handle for the targeted replay dispatcher.
pub struct ReplayDispatcherHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl ReplayDispatcherHandle {
    /// Stop the dispatcher.
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

    /// Wait for the dispatcher to terminate.
    pub async fn wait(&mut self) -> Result<(), ServerError> {
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(ServerError::Nats(format!(
                "replay dispatcher task failed: {error}"
            ))),
        }
    }
}

/// Start dispatching durable replay intents to generation-specific work subjects.
pub async fn start_replay_dispatcher(
    client: async_nats::Client,
    journal: DeadLetterJournal,
) -> Result<ReplayDispatcherHandle, ServerError> {
    let context = jetstream::new(client);
    let journal_stream = context.get_stream(DLQ_STREAM).await.map_err(nats)?;
    let replay_stream = context.get_stream(REPLAY_STREAM).await.map_err(nats)?;
    let consumer = journal_stream
        .get_or_create_consumer(
            DISPATCHER_DURABLE,
            consumer::pull::Config {
                durable_name: Some(DISPATCHER_DURABLE.to_owned()),
                filter_subject: DLQ_SUBJECTS.to_owned(),
                deliver_policy: consumer::DeliverPolicy::All,
                ack_policy: consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .map_err(nats)?;
    let task = tokio::spawn(async move {
        loop {
            let mut messages = consumer
                .fetch()
                .max_messages(100)
                .expires(Duration::from_millis(500))
                .messages()
                .await
                .map_err(nats)?;
            while let Some(message) = messages.next().await {
                let message = message.map_err(nats)?;
                let transition = serde_json::from_slice::<DeadLetterTransition>(&message.payload)
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                if transition.state != DeadLetterState::ReplayPending {
                    message.ack().await.map_err(nats)?;
                    continue;
                }
                let (latest, latest_sequence) = journal
                    .latest(&transition.id)
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?
                    .ok_or_else(|| {
                        ServerError::Nats("dead-letter journal entry disappeared".to_owned())
                    })?;
                if latest.state != DeadLetterState::ReplayPending
                    || latest.generation != transition.generation
                {
                    message.ack().await.map_err(nats)?;
                    continue;
                }
                let original = journal
                    .original(&latest)
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?
                    .ok_or_else(|| {
                        ServerError::Nats("dead-letter original evidence disappeared".to_owned())
                    })?;
                let envelope = ReplayEnvelope {
                    dead_letter_id: latest.id.clone(),
                    generation: latest.generation,
                    resource_id: latest.resource_id.clone(),
                    original_record_sequence: latest.original_record_sequence,
                    original_subject: original.subject,
                    original_payload_bytes: original.payload_bytes,
                    original_headers: original.headers,
                };
                let payload = serde_json::to_vec(&envelope)
                    .map(Bytes::from)
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                let subject = replay_subject(&latest.resource_id, &latest.id, latest.generation);
                let replay_sequence = match context.publish(subject.clone(), payload.clone()).await
                {
                    Ok(ack) => match ack.await {
                        Ok(ack) => ack.sequence,
                        Err(error) => match replay_stream
                            .get_last_raw_message_by_subject(&subject)
                            .await
                        {
                            Ok(existing) if existing.payload == payload => existing.sequence,
                            Ok(_) => {
                                return Err(ServerError::Nats(
                                    "targeted replay generation content conflict".to_owned(),
                                ))
                            }
                            Err(read_error) => {
                                tracing::warn!(%error, %read_error, "targeted replay publication unavailable");
                                message
                                    .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                                    .await
                                    .map_err(nats)?;
                                continue;
                            }
                        },
                    },
                    Err(error) => match replay_stream
                        .get_last_raw_message_by_subject(&subject)
                        .await
                    {
                        Ok(existing) if existing.payload == payload => existing.sequence,
                        Ok(_) => {
                            return Err(ServerError::Nats(
                                "targeted replay generation content conflict".to_owned(),
                            ))
                        }
                        Err(read_error)
                            if matches!(
                                read_error.kind(),
                                stream::LastRawMessageErrorKind::NoMessageFound
                            ) =>
                        {
                            tracing::warn!(%error, "targeted replay publication unavailable");
                            message
                                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                                .await
                                .map_err(nats)?;
                            continue;
                        }
                        Err(read_error) => return Err(nats(read_error)),
                    },
                };
                journal
                    .mark_replaying(&latest, latest_sequence, replay_sequence)
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                message.ack().await.map_err(nats)?;
            }
        }
    });
    Ok(ReplayDispatcherHandle { task: Some(task) })
}

/// Return the exact replay subject filter for one logical Consumer resource.
pub fn replay_filter_subject(resource_id: &str) -> String {
    format!(
        "_trellis.consumer.replay.{}.>",
        URL_SAFE_NO_PAD.encode(Sha256::digest(resource_id))
    )
}

pub(super) fn replay_subject(resource_id: &str, dead_letter_id: &str, generation: u64) -> String {
    let resource_token = URL_SAFE_NO_PAD.encode(Sha256::digest(resource_id));
    format!("_trellis.consumer.replay.{resource_token}.{dead_letter_id}.{generation}")
}

fn nats(error: impl std::fmt::Display) -> ServerError {
    ServerError::Nats(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::replay_subject;

    #[test]
    fn replay_subject_is_generation_specific() {
        assert_ne!(
            replay_subject("resource", "dead", 1),
            replay_subject("resource", "dead", 2)
        );
    }
}
