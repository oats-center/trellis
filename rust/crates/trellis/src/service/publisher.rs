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
    pub async fn publish<D>(&self, event: &D::Event) -> Result<(), ServerError>
    where
        D: EventDescriptor,
    {
        let prepared = crate::client::prepare_event::<D>(event)?;
        self.client
            .publish_prepared(&prepared)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        Ok(())
    }

    /// Publish an event prepared before the current transaction completed.
    pub async fn publish_prepared(
        &self,
        event: &crate::client::PreparedTrellisEvent,
    ) -> Result<(), ServerError> {
        self.client
            .publish_prepared(event)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))
    }
}
