//! Generated API `trellis.state@v1`.
pub const API_ID: &str = "trellis.state@v1";
pub const API_DIGEST: &str = "bqIK645jHWh6xBk0V8kejZI_2AWodtWsDwG0rawTvo8";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct Conflict {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl Conflict {
        pub fn payload(&self) -> Result<crate::__types::trellis::StateConflict, serde_json::Error> {
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
    impl std::fmt::Display for Conflict {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for Conflict {}
    impl trellis_rs::generated::TrellisError for Conflict {
        const TYPE: &'static str = "trellis.state@v1::Conflict";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct CorruptRepresentation {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl CorruptRepresentation {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::StateRepresentationError, serde_json::Error> {
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
    impl std::fmt::Display for CorruptRepresentation {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for CorruptRepresentation {}
    impl trellis_rs::generated::TrellisError for CorruptRepresentation {
        const TYPE: &'static str = "trellis.state@v1::CorruptRepresentation";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct UnsupportedRepresentation {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl UnsupportedRepresentation {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::StateRepresentationError, serde_json::Error> {
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
    impl std::fmt::Display for UnsupportedRepresentation {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for UnsupportedRepresentation {}
    impl trellis_rs::generated::TrellisError for UnsupportedRepresentation {
        const TYPE: &'static str = "trellis.state@v1::UnsupportedRepresentation";
    }
}
pub mod rpc {
    pub type DeleteInput = crate::__types::trellis::StateDeleteRequest;
    pub type DeleteOutput = crate::__types::trellis::StateDeleteResponse;
    pub struct Delete;
    impl Delete {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Delete";
        pub const KEY: &'static str = "state.Delete";
        pub const SUBJECT: &'static str = "rpc.v1.state.Delete";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.state@v1::public"];
        pub const ERRORS: &'static [&'static str] = &["trellis.state@v1::Conflict"];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeleteError {
        Conflict(super::errors::Conflict),
    }
    impl DeleteError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.state@v1::Conflict") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Conflict>(value)
                        .map(|value| value.map(Self::Conflict))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Delete {
        type Input = DeleteInput;
        type Output = DeleteOutput;
        type Error = DeleteError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeleteError::decode(value)
        }
    }
    pub type GetInput = crate::__types::trellis::StateGetRequest;
    pub type GetOutput = crate::__types::trellis::StateGetResponse;
    pub struct Get;
    impl Get {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Get";
        pub const KEY: &'static str = "state.Get";
        pub const SUBJECT: &'static str = "rpc.v1.state.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.state@v1::public"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.state@v1::CorruptRepresentation",
            "trellis.state@v1::UnsupportedRepresentation",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum GetError {
        CorruptRepresentation(super::errors::CorruptRepresentation),
        UnsupportedRepresentation(super::errors::UnsupportedRepresentation),
    }
    impl GetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.state@v1::CorruptRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::CorruptRepresentation,
                    >(value)
                        .map(|value| value.map(Self::CorruptRepresentation))
                }
                Some("trellis.state@v1::UnsupportedRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnsupportedRepresentation,
                    >(value)
                        .map(|value| value.map(Self::UnsupportedRepresentation))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Get {
        type Input = GetInput;
        type Output = GetOutput;
        type Error = GetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            GetError::decode(value)
        }
    }
    pub type PutInput = crate::__types::trellis::StatePutRequest;
    pub type PutOutput = crate::__types::trellis::StatePutResponse;
    pub struct Put;
    impl Put {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Put";
        pub const KEY: &'static str = "state.Put";
        pub const SUBJECT: &'static str = "rpc.v1.state.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.state@v1::public"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.state@v1::Conflict",
            "trellis.state@v1::CorruptRepresentation",
            "trellis.state@v1::UnsupportedRepresentation",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PutError {
        Conflict(super::errors::Conflict),
        CorruptRepresentation(super::errors::CorruptRepresentation),
        UnsupportedRepresentation(super::errors::UnsupportedRepresentation),
    }
    impl PutError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.state@v1::Conflict") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::Conflict,
                    >(value)
                        .map(|value| value.map(Self::Conflict))
                }
                Some("trellis.state@v1::CorruptRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::CorruptRepresentation,
                    >(value)
                        .map(|value| value.map(Self::CorruptRepresentation))
                }
                Some("trellis.state@v1::UnsupportedRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnsupportedRepresentation,
                    >(value)
                        .map(|value| value.map(Self::UnsupportedRepresentation))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Put {
        type Input = PutInput;
        type Output = PutOutput;
        type Error = PutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PutError::decode(value)
        }
    }
    pub type ResourcesInspectInput = crate::__types::trellis::ResourcesInspectRequest;
    pub type ResourcesInspectOutput = crate::__types::trellis::ResourcesInspectResponse;
    pub struct ResourcesInspect;
    impl ResourcesInspect {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Resources.Inspect";
        pub const KEY: &'static str = "state.Resources.Inspect";
        pub const SUBJECT: &'static str = "rpc.v1.state.Resources.Inspect";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.state@v1::resources_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.state@v1::CorruptRepresentation",
            "trellis.state@v1::UnsupportedRepresentation",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesInspectError {
        CorruptRepresentation(super::errors::CorruptRepresentation),
        UnsupportedRepresentation(super::errors::UnsupportedRepresentation),
    }
    impl ResourcesInspectError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.state@v1::CorruptRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::CorruptRepresentation,
                    >(value)
                        .map(|value| value.map(Self::CorruptRepresentation))
                }
                Some("trellis.state@v1::UnsupportedRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnsupportedRepresentation,
                    >(value)
                        .map(|value| value.map(Self::UnsupportedRepresentation))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ResourcesInspect {
        type Input = ResourcesInspectInput;
        type Output = ResourcesInspectOutput;
        type Error = ResourcesInspectError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ResourcesInspectError::decode(value)
        }
    }
    pub type ResourcesQueryInput = crate::__types::trellis::ResourcesQueryRequest;
    pub type ResourcesQueryOutput = crate::__types::trellis::ResourcesQueryResponse;
    pub struct ResourcesQuery;
    impl ResourcesQuery {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Resources.Query";
        pub const KEY: &'static str = "state.Resources.Query";
        pub const SUBJECT: &'static str = "rpc.v1.state.Resources.Query";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.state@v1::resources_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.state@v1::CorruptRepresentation",
            "trellis.state@v1::UnsupportedRepresentation",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesQueryError {
        CorruptRepresentation(super::errors::CorruptRepresentation),
        UnsupportedRepresentation(super::errors::UnsupportedRepresentation),
    }
    impl ResourcesQueryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.state@v1::CorruptRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::CorruptRepresentation,
                    >(value)
                        .map(|value| value.map(Self::CorruptRepresentation))
                }
                Some("trellis.state@v1::UnsupportedRepresentation") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnsupportedRepresentation,
                    >(value)
                        .map(|value| value.map(Self::UnsupportedRepresentation))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ResourcesQuery {
        type Input = ResourcesQueryInput;
        type Output = ResourcesQueryOutput;
        type Error = ResourcesQueryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ResourcesQueryError::decode(value)
        }
    }
}
pub mod operations {}
pub mod events {}
pub mod lives {}
/// Registers metadata for every RPC in this API.
pub fn register_rpc_metadata(router: &mut trellis_rs::service::Router) {
    let _ = router;
    router.register_rpc_metadata::<rpc::Delete>();
    router.register_rpc_metadata::<rpc::Get>();
    router.register_rpc_metadata::<rpc::Put>();
    router.register_rpc_metadata::<rpc::ResourcesInspect>();
    router.register_rpc_metadata::<rpc::ResourcesQuery>();
}
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self { inner }
    }
    pub async fn delete(
        &self,
        input: &rpc::DeleteInput,
    ) -> Result<rpc::DeleteOutput, trellis_rs::client::CallError<rpc::DeleteError>> {
        self.inner.call::<rpc::Delete>(input).await
    }
    pub async fn get(
        &self,
        input: &rpc::GetInput,
    ) -> Result<rpc::GetOutput, trellis_rs::client::CallError<rpc::GetError>> {
        self.inner.call::<rpc::Get>(input).await
    }
    pub async fn put(
        &self,
        input: &rpc::PutInput,
    ) -> Result<rpc::PutOutput, trellis_rs::client::CallError<rpc::PutError>> {
        self.inner.call::<rpc::Put>(input).await
    }
    pub async fn resources_inspect(
        &self,
        input: &rpc::ResourcesInspectInput,
    ) -> Result<
        rpc::ResourcesInspectOutput,
        trellis_rs::client::CallError<rpc::ResourcesInspectError>,
    > {
        self.inner.call::<rpc::ResourcesInspect>(input).await
    }
    pub async fn resources_query(
        &self,
        input: &rpc::ResourcesQueryInput,
    ) -> Result<rpc::ResourcesQueryOutput, trellis_rs::client::CallError<rpc::ResourcesQueryError>>
    {
        self.inner.call::<rpc::ResourcesQuery>(input).await
    }
    pub fn resources_query_pages(
        &self,
        input: rpc::ResourcesQueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ResourcesQueryOutput, crate::PaginationError<rpc::ResourcesQueryError>>,
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
                    .resources_query(&input)
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
    pub fn resources_query_items(
        &self,
        input: rpc::ResourcesQueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::ResourceInspection,
            crate::PaginationError<rpc::ResourcesQueryError>,
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
                Vec::<crate::__types::trellis::ResourceInspection>::new().into_iter(),
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
                        .resources_query(&input)
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
}
pub struct Provider<'a, P> {
    runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
}
impl<'a, P: trellis_rs::generated::ParticipantDescriptor> Provider<'a, P> {
    pub fn new(runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>) -> Self {
        Self { runtime }
    }
    pub fn register_delete<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeleteInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::DeleteOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Delete, _, _>(handler);
    }
    pub fn register_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::GetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::GetOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Get, _, _>(handler);
    }
    pub fn register_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::PutOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Put, _, _>(handler);
    }
    pub fn register_resources_inspect<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ResourcesInspectInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesInspectOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ResourcesInspect, _, _>(handler);
    }
    pub fn register_resources_query<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ResourcesQueryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesQueryOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ResourcesQuery, _, _>(handler);
    }
}
