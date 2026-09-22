use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use async_nats::jetstream::{self, consumer};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};

use crate::client::TrellisClient;
use crate::jobs::publisher::JobEventHeaders;
use crate::jobs::types::JobEvent;

const FILTERED_REOPEN_MAX_FAILURES: usize = 40;
const FILTERED_REOPEN_DELAY: Duration = Duration::from_millis(250);

/// Jobs subsystem runtime transport facade.
///
/// This keeps NATS and JetStream details inside the Trellis library while Jobs
/// subsystem services work with Jobs-domain operations.
#[derive(Debug, Clone)]
pub struct JobsRuntime {
    nats: async_nats::Client,
}

impl JobsRuntime {
    /// Create a Jobs runtime facade from a connected Trellis client.
    pub(crate) fn from_client(client: &TrellisClient) -> Self {
        Self {
            nats: client.nats().clone(),
        }
    }

    /// Create the Jobs runtime facade for a Trellis-owned built-in provider.
    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub fn from_nats(nats: async_nats::Client) -> Self {
        Self { nats }
    }

    /// Open a new-only ephemeral consumer for `Jobs.Watch`.
    pub async fn watch_messages(
        &self,
        stream_name: &str,
        filter_subject: &str,
    ) -> Result<JobsRuntimeMessageStream, String> {
        let stream = jetstream::new(self.nats.clone())
            .get_stream(stream_name)
            .await
            .map_err(|error| error.to_string())?;
        let consumer = stream
            .create_consumer(consumer::pull::Config {
                filter_subject: filter_subject.to_string(),
                deliver_policy: consumer::DeliverPolicy::New,
                ack_policy: consumer::AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(5),
                metadata: jobs_watch_metadata(),
                ..Default::default()
            })
            .await
            .map_err(|error| error.to_string())?;
        let messages = consumer
            .messages()
            .await
            .map_err(|error| error.to_string())?;
        Ok(Box::pin(messages.map(|message| {
            message
                .map(JobsRuntimeMessage::new)
                .map_err(|error| error.to_string())
        })))
    }

    /// Publish one encoded job lifecycle event to its subject.
    pub async fn publish_event(&self, subject: String, event: &JobEvent) -> Result<(), String> {
        let payload = serde_json::to_vec(event).map_err(|error| error.to_string())?;
        self.publish_event_payload(subject, JobEventHeaders::from(&event.context), payload)
            .await
    }

    /// Publish an already encoded job lifecycle event payload.
    pub async fn publish_event_payload(
        &self,
        subject: String,
        headers: JobEventHeaders,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        let mut nats_headers = async_nats::HeaderMap::new();
        nats_headers.insert("request-id", headers.request_id.as_str());
        nats_headers.insert("traceparent", headers.traceparent.as_str());
        if let Some(tracestate) = headers.tracestate.as_deref() {
            nats_headers.insert("tracestate", tracestate);
        }
        jetstream::new(self.nats.clone())
            .publish_with_headers(subject, nats_headers, payload.into())
            .await
            .map_err(|error| error.to_string())?
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Open a pull consumer over a Jobs-owned stream/filter pair.
    ///
    /// The stream reopens its durable consumer after transient pull failures. If recovery cannot
    /// produce another message within the bounded retry budget, it yields one terminal error and
    /// then ends.
    pub async fn filtered_messages(
        &self,
        stream_name: &str,
        consumer_name: &str,
        filter_subject: &str,
    ) -> Result<JobsRuntimeMessageStream, String> {
        let initial = self
            .open_filtered_messages(stream_name, consumer_name, filter_subject)
            .await?;
        let runtime = self.clone();
        let stream_name = stream_name.to_owned();
        let consumer_name = consumer_name.to_owned();
        let filter_subject = filter_subject.to_owned();
        Ok(Box::pin(futures_util::stream::unfold(
            (Some(initial), 0usize),
            move |(messages, mut consecutive_failures)| {
                let runtime = runtime.clone();
                let stream_name = stream_name.clone();
                let consumer_name = consumer_name.clone();
                let filter_subject = filter_subject.clone();
                async move {
                    let mut messages = messages?;
                    loop {
                        let stream_error = match messages.next().await {
                            Some(Ok(message)) => {
                                return Some((Ok(message), (Some(messages), 0)));
                            }
                            Some(Err(error)) => {
                                tracing::warn!(
                                stream = %stream_name,
                                consumer = %consumer_name,
                                %error,
                                "Jobs pull consumer stream failed; reopening durable consumer"
                                );
                                error
                            }
                            None => {
                                tracing::warn!(
                                    stream = %stream_name,
                                    consumer = %consumer_name,
                                    "Jobs pull consumer stream ended; reopening durable consumer"
                                );
                                "message stream ended".to_owned()
                            }
                        };
                        consecutive_failures += 1;
                        if consecutive_failures >= FILTERED_REOPEN_MAX_FAILURES {
                            return Some((
                                Err(format!(
                                    "Jobs pull consumer '{consumer_name}' on stream '{stream_name}' failed {consecutive_failures} consecutive times: {stream_error}"
                                )),
                                (None, consecutive_failures),
                            ));
                        }
                        tokio::time::sleep(FILTERED_REOPEN_DELAY).await;
                        loop {
                            match runtime
                                .open_filtered_messages(
                                    &stream_name,
                                    &consumer_name,
                                    &filter_subject,
                                )
                                .await
                            {
                                Ok(reopened) => {
                                    messages = reopened;
                                    break;
                                }
                                Err(error) => {
                                    consecutive_failures += 1;
                                    if consecutive_failures == 2
                                        || consecutive_failures.is_multiple_of(10)
                                    {
                                        tracing::warn!(
                                            stream = %stream_name,
                                            consumer = %consumer_name,
                                            %error,
                                            consecutive_failures,
                                            "Jobs durable pull consumer reopen failed"
                                        );
                                    }
                                    if consecutive_failures >= FILTERED_REOPEN_MAX_FAILURES {
                                        return Some((
                                            Err(format!(
                                                "Jobs pull consumer '{consumer_name}' on stream '{stream_name}' could not reopen after {consecutive_failures} consecutive failures: {error}"
                                            )),
                                            (None, consecutive_failures),
                                        ));
                                    }
                                    tokio::time::sleep(FILTERED_REOPEN_DELAY).await;
                                }
                            }
                        }
                    }
                }
            },
        )))
    }

    async fn open_filtered_messages(
        &self,
        stream_name: &str,
        consumer_name: &str,
        filter_subject: &str,
    ) -> Result<JobsRuntimeMessageStream, String> {
        let jetstream = jetstream::new(self.nats.clone());
        let stream = jetstream
            .get_stream(stream_name)
            .await
            .map_err(|error| error.to_string())?;
        let consumer = stream
            .get_or_create_consumer(
                consumer_name,
                consumer::pull::Config {
                    durable_name: Some(consumer_name.to_string()),
                    filter_subject: filter_subject.to_string(),
                    ack_policy: consumer::AckPolicy::Explicit,
                    metadata: HashMap::from([
                        ("trellis.managed_by".to_string(), "platform".to_string()),
                        (
                            "trellis.contract_id".to_string(),
                            "trellis.jobs@v1".to_string(),
                        ),
                        ("trellis.group".to_string(), consumer_name.to_string()),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let messages = consumer
            .messages()
            .await
            .map_err(|error| error.to_string())?;
        Ok(Box::pin(messages.map(|message| {
            message
                .map(JobsRuntimeMessage::new)
                .map_err(|error| error.to_string())
        })))
    }

    /// Read a raw payload by stream sequence.
    pub async fn raw_payload(&self, stream_name: &str, sequence: u64) -> Result<Bytes, String> {
        let jetstream = jetstream::new(self.nats.clone());
        let stream = jetstream
            .get_stream(stream_name)
            .await
            .map_err(|error| error.to_string())?;
        stream
            .get_raw_message(sequence)
            .await
            .map(|message| message.payload)
            .map_err(|error| error.to_string())
    }
}

fn jobs_watch_metadata() -> HashMap<String, String> {
    HashMap::from([
        ("trellis.managed_by".to_string(), "platform".to_string()),
        (
            "trellis.contract_id".to_string(),
            "trellis.jobs@v1".to_string(),
        ),
        ("trellis.group".to_string(), "watch".to_string()),
    ])
}

/// Stream of Jobs runtime messages.
pub type JobsRuntimeMessageStream =
    Pin<Box<dyn Stream<Item = Result<JobsRuntimeMessage, String>> + Send>>;

/// Message delivered by a Jobs runtime stream consumer.
pub struct JobsRuntimeMessage {
    inner: async_nats::jetstream::Message,
}

impl JobsRuntimeMessage {
    fn new(inner: async_nats::jetstream::Message) -> Self {
        Self { inner }
    }

    /// Return the message subject.
    pub fn subject(&self) -> &str {
        self.inner.subject.as_ref()
    }

    /// Return the raw message payload.
    pub fn payload(&self) -> &[u8] {
        &self.inner.payload
    }

    /// Acknowledge successful message handling.
    pub async fn ack(&self) -> Result<(), String> {
        self.inner.ack().await.map_err(|error| error.to_string())
    }

    /// Acknowledge message handling with an explicit JetStream disposition.
    pub async fn ack_with(&self, kind: jetstream::AckKind) -> Result<(), String> {
        self.inner
            .ack_with(kind)
            .await
            .map_err(|error| error.to_string())
    }

    /// Return the source stream sequence carried by this delivery.
    pub fn stream_sequence(&self) -> Result<u64, String> {
        self.inner
            .info()
            .map(|info| info.stream_sequence)
            .map_err(|error| error.to_string())
    }
}
