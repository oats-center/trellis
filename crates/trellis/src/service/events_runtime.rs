//! Narrow Events transport owned by the Trellis runtime.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, consumer};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;

use crate::client::TrellisClient;

const EVENT_STREAM: &str = "trellis";
const DLQ_STREAM: &str = "trellis_consumer_dlq";
const EVENT_SUBJECT_WILDCARD: &str = "events.v1.>";

/// Events-specific transport over the Trellis event stream.
#[derive(Clone)]
pub struct EventsRuntime {
    nats: async_nats::Client,
}

impl EventsRuntime {
    /// Return the current retained sequence bounds for diagnostics.
    pub async fn stream_bounds(&self) -> Result<(u64, u64), String> {
        let mut stream = jetstream::new(self.nats.clone())
            .get_stream(EVENT_STREAM)
            .await
            .map_err(|error| error.to_string())?;
        let info = stream.info().await.map_err(|error| error.to_string())?;
        Ok((info.state.first_sequence, info.state.last_sequence))
    }

    /// Create the Events transport from an authenticated Trellis session.
    #[doc(hidden)]
    pub(crate) fn from_client(client: Arc<TrellisClient>) -> Self {
        Self {
            nats: client.nats().clone(),
        }
    }

    /// Create the Events runtime facade for a Trellis-owned built-in provider.
    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub fn from_nats(nats: async_nats::Client) -> Self {
        Self { nats }
    }

    /// Open a new-only ephemeral event stream consumer.
    pub async fn live_events(&self) -> Result<EventsMessageStream, String> {
        let stream = jetstream::new(self.nats.clone())
            .get_stream(EVENT_STREAM)
            .await
            .map_err(|error| error.to_string())?;
        let consumer = stream
            .create_consumer(consumer::pull::Config {
                filter_subject: EVENT_SUBJECT_WILDCARD.to_string(),
                deliver_policy: consumer::DeliverPolicy::New,
                ack_policy: consumer::AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(5),
                metadata: platform_consumer_metadata("watch"),
                ..Default::default()
            })
            .await
            .map_err(|error| error.to_string())?;
        consumer
            .messages()
            .await
            .map(|messages| {
                messages
                    .map(|message| {
                        message.map_err(|error| {
                            Box::new(error) as Box<dyn std::error::Error + Send + Sync>
                        })
                    })
                    .boxed()
            })
            .map_err(|error| error.to_string())
    }

    /// Open the durable Events projector consumer.
    pub async fn event_consumer(
        &self,
        consumer_name: &str,
        replay_all: bool,
    ) -> Result<consumer::Consumer<consumer::pull::Config>, String> {
        let stream = jetstream::new(self.nats.clone())
            .get_stream(EVENT_STREAM)
            .await
            .map_err(|error| error.to_string())?;
        stream
            .get_or_create_consumer(
                consumer_name,
                consumer::pull::Config {
                    durable_name: Some(consumer_name.to_string()),
                    filter_subject: EVENT_SUBJECT_WILDCARD.to_string(),
                    deliver_policy: if replay_all {
                        consumer::DeliverPolicy::All
                    } else {
                        consumer::DeliverPolicy::New
                    },
                    ack_policy: consumer::AckPolicy::Explicit,
                    metadata: platform_consumer_metadata("projector"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Return Events stream consumer metadata.
    pub async fn consumers(&self) -> Result<Vec<consumer::Info>, String> {
        let stream = jetstream::new(self.nats.clone())
            .get_stream(EVENT_STREAM)
            .await
            .map_err(|error| error.to_string())?;
        let mut consumers = stream.consumers();
        let mut rows = Vec::new();
        while let Some(info) = consumers.next().await {
            rows.push(info.map_err(|error| error.to_string())?);
        }
        Ok(rows)
    }

    /// Return the live consumers on the dead-letter stream.
    pub async fn dlq_consumers(&self) -> Result<Vec<consumer::Info>, String> {
        let stream = jetstream::new(self.nats.clone())
            .get_stream(DLQ_STREAM)
            .await
            .map_err(|error| error.to_string())?;
        let mut consumers = stream.consumers();
        let mut rows = Vec::new();
        while let Some(info) = consumers.next().await {
            rows.push(info.map_err(|error| error.to_string())?);
        }
        Ok(rows)
    }

    /// Return metadata for one Events stream consumer.
    pub async fn consumer(&self, name: &str) -> Result<consumer::Info, String> {
        let stream = jetstream::new(self.nats.clone())
            .get_stream(EVENT_STREAM)
            .await
            .map_err(|error| error.to_string())?;
        stream
            .consumer_info(name)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Events stream messages retained behind the domain runtime.
pub type EventsMessageStream = BoxStream<
    'static,
    Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
>;

fn platform_consumer_metadata(group: &str) -> HashMap<String, String> {
    HashMap::from([
        ("trellis.managed_by".to_string(), "platform".to_string()),
        (
            "trellis.contract_id".to_string(),
            "trellis.events@v1".to_string(),
        ),
        ("trellis.group".to_string(), group.to_string()),
    ])
}
