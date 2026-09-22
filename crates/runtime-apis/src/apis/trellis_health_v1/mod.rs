//! Generated API `trellis.health@v1`.
pub const API_ID: &str = "trellis.health@v1";
pub const API_DIGEST: &str = "850itezlC9qjm-ndTCwrmkEQBCkVmnLqSguqNkYVgaA";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct NotFoundError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl NotFoundError {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::HealthNotFoundErrorData, serde_json::Error> {
            let mut payload = self.error.extra.clone();
            payload.insert(
                "id".to_owned(),
                serde_json::Value::String(self.error.id.clone()),
            );
            payload.insert(
                "type".to_owned(),
                serde_json::Value::String(self.error.error_type.clone()),
            );
            payload.insert(
                "message".to_owned(),
                serde_json::Value::String(self.error.message.clone()),
            );
            if let Some(context) = &self.error.context {
                payload.insert(
                    "context".to_owned(),
                    serde_json::Value::Object(context.clone()),
                );
            }
            if let Some(trace_id) = &self.error.trace_id {
                payload.insert(
                    "traceId".to_owned(),
                    serde_json::Value::String(trace_id.clone()),
                );
            }
            serde_json::from_value(serde_json::Value::Object(payload))
        }
    }
    impl std::fmt::Display for NotFoundError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for NotFoundError {}
    impl trellis_rs::generated::TrellisError for NotFoundError {
        const TYPE: &'static str = "trellis.health@v1::NotFoundError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct UnexpectedError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl std::fmt::Display for UnexpectedError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for UnexpectedError {}
    impl trellis_rs::generated::TrellisError for UnexpectedError {
        const TYPE: &'static str = "trellis.health@v1::UnexpectedError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct ValidationError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl std::fmt::Display for ValidationError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for ValidationError {}
    impl trellis_rs::generated::TrellisError for ValidationError {
        const TYPE: &'static str = "trellis.health@v1::ValidationError";
    }
}
pub mod rpc {
    pub type InspectInput = crate::__types::trellis::HealthInspectRequest;
    pub type InspectOutput = crate::__types::trellis::HealthInspectResponse;
    pub struct Inspect;
    impl Inspect {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Inspect";
        pub const KEY: &'static str = "health.Inspect";
        pub const SUBJECT: &'static str = "rpc.v1.health.Inspect";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.health@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.health@v1::NotFoundError",
            "trellis.health@v1::UnexpectedError",
            "trellis.health@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum InspectError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl InspectError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.health@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.health@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.health@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Inspect {
        type Input = InspectInput;
        type Output = InspectOutput;
        type Error = InspectError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            InspectError::decode(value)
        }
    }
    pub type MetricsInput = crate::__types::trellis::HealthMetricsRequest;
    pub type MetricsOutput = crate::__types::trellis::HealthMetricsResponse;
    pub struct Metrics;
    impl Metrics {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Metrics";
        pub const KEY: &'static str = "health.Metrics";
        pub const SUBJECT: &'static str = "rpc.v1.health.Metrics";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.health@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.health@v1::UnexpectedError",
            "trellis.health@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum MetricsError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl MetricsError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.health@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.health@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Metrics {
        type Input = MetricsInput;
        type Output = MetricsOutput;
        type Error = MetricsError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            MetricsError::decode(value)
        }
    }
    pub type QueryInput = crate::__types::trellis::HealthQueryRequest;
    pub type QueryOutput = crate::__types::trellis::HealthQueryResponse;
    pub struct Query;
    impl Query {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Query";
        pub const KEY: &'static str = "health.Query";
        pub const SUBJECT: &'static str = "rpc.v1.health.Query";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.health@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.health@v1::UnexpectedError",
            "trellis.health@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum QueryError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl QueryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.health@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.health@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Query {
        type Input = QueryInput;
        type Output = QueryOutput;
        type Error = QueryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            QueryError::decode(value)
        }
    }
    pub type SummaryInput = crate::__types::trellis::HealthSummaryRequest;
    pub type SummaryOutput = crate::__types::trellis::HealthSummaryResponse;
    pub struct Summary;
    impl Summary {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Summary";
        pub const KEY: &'static str = "health.Summary";
        pub const SUBJECT: &'static str = "rpc.v1.health.Summary";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.health@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.health@v1::UnexpectedError",
            "trellis.health@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum SummaryError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl SummaryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.health@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.health@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Summary {
        type Input = SummaryInput;
        type Output = SummaryOutput;
        type Error = SummaryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            SummaryError::decode(value)
        }
    }
}
pub mod operations {}
pub mod events {
    pub type StatusChangedEvent = crate::__types::trellis::HealthStatusChangedEvent;
    pub struct StatusChanged;
    impl StatusChanged {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.StatusChanged";
        pub const KEY: &'static str = "health.StatusChanged";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5oZWFsdGhAdjE.StatusChanged";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5oZWFsdGhAdjE.StatusChanged";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.health@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &["trellis.health@v1::read"];
    }
    impl trellis_rs::generated::EventDescriptor for StatusChanged {
        type Event = StatusChangedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
}
pub mod feeds {
    pub type WatchInput = crate::__types::trellis::HealthWatchRequest;
    pub type WatchEvent = crate::__types::trellis::HealthWatchFrame;
    pub struct Watch;
    impl Watch {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "feed.Watch";
        pub const KEY: &'static str = "health.Watch";
        pub const SUBJECT: &'static str = "feed.v1.health.Watch";
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &["trellis.health@v1::read"];
    }
    impl trellis_rs::generated::FeedDescriptor for Watch {
        type Input = WatchInput;
        type Event = WatchEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
}
/// Registers metadata for every RPC in this API.
pub fn register_rpc_metadata(router: &mut trellis_rs::service::Router) {
    let _ = router;
    router.register_rpc_metadata::<rpc::Inspect>();
    router.register_rpc_metadata::<rpc::Metrics>();
    router.register_rpc_metadata::<rpc::Query>();
    router.register_rpc_metadata::<rpc::Summary>();
}
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self { inner }
    }
    pub async fn inspect(
        &self,
        input: &rpc::InspectInput,
    ) -> Result<rpc::InspectOutput, trellis_rs::client::CallError<rpc::InspectError>> {
        self.inner.call::<rpc::Inspect>(input).await
    }
    pub async fn metrics(
        &self,
        input: &rpc::MetricsInput,
    ) -> Result<rpc::MetricsOutput, trellis_rs::client::CallError<rpc::MetricsError>> {
        self.inner.call::<rpc::Metrics>(input).await
    }
    pub async fn query(
        &self,
        input: &rpc::QueryInput,
    ) -> Result<rpc::QueryOutput, trellis_rs::client::CallError<rpc::QueryError>> {
        self.inner.call::<rpc::Query>(input).await
    }
    pub fn query_pages(
        &self,
        input: rpc::QueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::QueryOutput, crate::PaginationError<rpc::QueryError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .query(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn query_items(
        &self,
        input: rpc::QueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::HealthQueryResponseentriesItem,
            crate::PaginationError<rpc::QueryError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::HealthQueryResponseentriesItem>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn summary(
        &self,
        input: &rpc::SummaryInput,
    ) -> Result<rpc::SummaryOutput, trellis_rs::client::CallError<rpc::SummaryError>> {
        self.inner.call::<rpc::Summary>(input).await
    }
    pub async fn publish_status_changed(
        &self,
        event: &events::StatusChangedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::StatusChanged>(event).await
    }
    pub async fn subscribe_status_changed(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::StatusChangedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner.subscribe::<events::StatusChanged>(options).await
    }
    pub async fn watch(
        &self,
        input: &feeds::WatchInput,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<feeds::WatchEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner.feed::<feeds::Watch>(input).await
    }
}
pub struct Provider<'a, P> {
    runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
}
impl<'a, P: trellis_rs::generated::ParticipantDescriptor> Provider<'a, P> {
    pub fn new(runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>) -> Self {
        Self { runtime }
    }
    pub fn register_inspect<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::InspectInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::InspectOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Inspect, _, _>(handler);
    }
    pub fn register_metrics<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::MetricsInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::MetricsOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Metrics, _, _>(handler);
    }
    pub fn register_query<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::QueryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::QueryOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Query, _, _>(handler);
    }
    pub fn register_summary<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::SummaryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::SummaryOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Summary, _, _>(handler);
    }
    pub fn register_watch<F, S>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, feeds::WatchInput) -> S
            + Send
            + Sync
            + 'static,
        S: futures_util::Stream<Item = Result<feeds::WatchEvent, trellis_rs::service::ServerError>>
            + Send
            + 'static,
    {
        self.runtime.register_feed::<feeds::Watch, _, _>(handler);
    }
}
