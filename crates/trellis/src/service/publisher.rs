use std::sync::Arc;

use super::ServerError;
use crate::client::{EventDescriptor, TrellisClient};

/// A descriptor-backed event publisher using the connected Trellis client.
#[derive(Clone)]
pub struct EventPublisher {
    client: Arc<TrellisClient>,
}

impl EventPublisher {
    pub(crate) fn new(client: Arc<TrellisClient>) -> Self {
        Self { client }
    }

    /// Publish one descriptor-backed event.
    ///
    /// # Errors
    ///
    /// As [`Self::publish_prepared`], and a [`ServerError`] when the event
    /// cannot be prepared.
    pub async fn publish<D>(&self, event: &D::Event) -> Result<(), ServerError>
    where
        D: EventDescriptor,
    {
        let prepared = crate::client::prepare_event::<D>(event)?;
        self.publish_prepared(&prepared).await
    }

    /// Publish an event prepared before the current transaction completed.
    ///
    /// # Errors
    ///
    /// [`ServerError::ResourceExhausted`] when JetStream refuses the event for
    /// lack of room, naming its subject; [`ServerError::Internal`] for any
    /// other failure.
    pub async fn publish_prepared(
        &self,
        event: &crate::client::PreparedTrellisEvent,
    ) -> Result<(), ServerError> {
        self.client
            .publish_prepared(event)
            .await
            .map_err(|error| match error {
                crate::client::TrellisClientError::ResourceExhausted(exhausted) => {
                    ServerError::ResourceExhausted(exhausted)
                }
                error => ServerError::Internal(error.to_string()),
            })
    }
}
