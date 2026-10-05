//! Generated API `trellis.core@v1`.
pub const API_ID: &str = "trellis.core@v1";
pub const API_DIGEST: &str = "ort4pZPv8EIhAelwexctvEIJhNuNaWCZPtIDF4XVLXM";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {
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
        const TYPE: &'static str = "trellis.core@v1::UnexpectedError";
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
        const TYPE: &'static str = "trellis.core@v1::ValidationError";
    }
}
pub mod rpc {
    pub type ResourcesDestroyInput = crate::__types::trellis::ResourcesDestroyRequest;
    pub type ResourcesDestroyOutput = crate::__types::trellis::ResourcesDestroyResponse;
    pub struct ResourcesDestroy;
    impl ResourcesDestroy {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Resources.Destroy";
        pub const KEY: &'static str = "core.Resources.Destroy";
        pub const SUBJECT: &'static str = "rpc.v1.core.Resources.Destroy";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.core@v1::resources_destroy",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.core@v1::UnexpectedError",
            "trellis.core@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesDestroyError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ResourcesDestroyError {
        pub fn decode(
            value: serde_json::Value,
        ) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.core@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnexpectedError,
                    >(value)
                        .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.core@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::ValidationError,
                    >(value)
                        .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ResourcesDestroy {
        type Input = ResourcesDestroyInput;
        type Output = ResourcesDestroyOutput;
        type Error = ResourcesDestroyError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ResourcesDestroyError::decode(value)
        }
    }
    pub type ResourcesInspectInput = crate::__types::trellis::ResourcesInspectRequest;
    pub type ResourcesInspectOutput = crate::__types::trellis::ResourcesInspectResponse;
    pub struct ResourcesInspect;
    impl ResourcesInspect {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Resources.Inspect";
        pub const KEY: &'static str = "core.Resources.Inspect";
        pub const SUBJECT: &'static str = "rpc.v1.core.Resources.Inspect";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.core@v1::resources_read",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.core@v1::UnexpectedError",
            "trellis.core@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesInspectError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ResourcesInspectError {
        pub fn decode(
            value: serde_json::Value,
        ) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.core@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnexpectedError,
                    >(value)
                        .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.core@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::ValidationError,
                    >(value)
                        .map(|value| value.map(Self::ValidationError))
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
        pub const KEY: &'static str = "core.Resources.Query";
        pub const SUBJECT: &'static str = "rpc.v1.core.Resources.Query";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.core@v1::resources_read",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.core@v1::UnexpectedError",
            "trellis.core@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesQueryError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ResourcesQueryError {
        pub fn decode(
            value: serde_json::Value,
        ) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.core@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnexpectedError,
                    >(value)
                        .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.core@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::ValidationError,
                    >(value)
                        .map(|value| value.map(Self::ValidationError))
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
    pub type SurfaceStatusInput = crate::__types::trellis::TrellisSurfaceStatusRequest;
    pub type SurfaceStatusOutput = crate::__types::trellis::TrellisSurfaceStatusResponse;
    pub struct SurfaceStatus;
    impl SurfaceStatus {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Surface.Status";
        pub const KEY: &'static str = "core.Surface.Status";
        pub const SUBJECT: &'static str = "rpc.v1.core.Surface.Status";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.core@v1::authority_read",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.core@v1::UnexpectedError",
            "trellis.core@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum SurfaceStatusError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl SurfaceStatusError {
        pub fn decode(
            value: serde_json::Value,
        ) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.core@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnexpectedError,
                    >(value)
                        .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.core@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::ValidationError,
                    >(value)
                        .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for SurfaceStatus {
        type Input = SurfaceStatusInput;
        type Output = SurfaceStatusOutput;
        type Error = SurfaceStatusError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            SurfaceStatusError::decode(value)
        }
    }
}
pub mod operations {}
pub mod events {}
pub mod lives {}
/// Registers metadata for every RPC in this API.
pub fn register_rpc_metadata(router: &mut trellis_rs::service::Router) {
    let _ = router;
    router.register_rpc_metadata::<rpc::ResourcesDestroy>();
    router.register_rpc_metadata::<rpc::ResourcesInspect>();
    router.register_rpc_metadata::<rpc::ResourcesQuery>();
    router.register_rpc_metadata::<rpc::SurfaceStatus>();
}
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self { inner }
    }
    pub async fn resources_destroy(
        &self,
        input: &rpc::ResourcesDestroyInput,
    ) -> Result<
        rpc::ResourcesDestroyOutput,
        trellis_rs::client::CallError<rpc::ResourcesDestroyError>,
    > {
        self.inner.call::<rpc::ResourcesDestroy>(input).await
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
    ) -> Result<
        rpc::ResourcesQueryOutput,
        trellis_rs::client::CallError<rpc::ResourcesQueryError>,
    > {
        self.inner.call::<rpc::ResourcesQuery>(input).await
    }
    pub fn resources_query_pages(
        &self,
        input: rpc::ResourcesQueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::ResourcesQueryOutput,
            crate::PaginationError<rpc::ResourcesQueryError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(
            futures_util::stream::try_unfold(
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
            ),
        )
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
        Box::pin(
            futures_util::stream::try_unfold(
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
            ),
        )
    }
    pub async fn surface_status(
        &self,
        input: &rpc::SurfaceStatusInput,
    ) -> Result<
        rpc::SurfaceStatusOutput,
        trellis_rs::client::CallError<rpc::SurfaceStatusError>,
    > {
        self.inner.call::<rpc::SurfaceStatus>(input).await
    }
}
pub struct Provider<'a, P> {
    runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
}
impl<'a, P: trellis_rs::generated::ParticipantDescriptor> Provider<'a, P> {
    pub fn new(
        runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
    ) -> Self {
        Self { runtime }
    }
    pub fn register_resources_destroy<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::ResourcesDestroyInput,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesDestroyOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::ResourcesDestroy, _, _>(handler);
    }
    pub fn register_resources_inspect<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::ResourcesInspectInput,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesInspectOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::ResourcesInspect, _, _>(handler);
    }
    pub fn register_resources_query<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::ResourcesQueryInput,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesQueryOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::ResourcesQuery, _, _>(handler);
    }
    pub fn register_surface_status<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::SurfaceStatusInput) -> Fut
            + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::SurfaceStatusOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::SurfaceStatus, _, _>(handler);
    }
}
