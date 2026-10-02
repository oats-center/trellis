use crate::jobs::types::JobContext;

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(JobEventHeaders), "`.")]
pub struct JobEventHeaders {
    #[doc = concat!("The `", stringify!(request_id), "` value.")]
    pub request_id: String,
    #[doc = concat!("The `", stringify!(traceparent), "` value.")]
    pub traceparent: String,
    #[doc = concat!("The `", stringify!(tracestate), "` value.")]
    pub tracestate: Option<String>,
}

impl From<&JobContext> for JobEventHeaders {
    fn from(context: &JobContext) -> Self {
        Self {
            request_id: context.request_id.clone(),
            traceparent: context.traceparent.clone(),
            tracestate: context.tracestate.clone(),
        }
    }
}

/// Publishes durable job lifecycle events and transient typed updates.
pub trait JobEventPublisher {
    /// The transport's publication error.
    type Error;

    /// Publishes a lifecycle event and waits for confirmed durable storage.
    fn publish(
        &self,
        subject: String,
        headers: JobEventHeaders,
        payload: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;

    /// Publishes a typed live update without requiring a durable stream.
    fn publish_update(
        &self,
        subject: String,
        headers: JobEventHeaders,
        payload: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;
}
