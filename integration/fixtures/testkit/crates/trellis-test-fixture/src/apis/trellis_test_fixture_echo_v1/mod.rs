//! Generated API `trellis-test-fixture.echo@v1`.
pub const API_ID: &str = "trellis-test-fixture.echo@v1";
pub const API_DIGEST: &str = "WgJsAYzutZb_zpExP-Ds0YxKuOPV5jV2GtKxkLreNyI";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {}
pub mod rpc {
    pub type EchoInput = crate::__types::trellis_test_fixture::Value;
    pub type EchoOutput = crate::__types::trellis_test_fixture::Value;
    pub struct Echo;
    impl Echo {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Echo";
        pub const KEY: &'static str = "echo.Echo";
        pub const SUBJECT: &'static str = "rpc.v1.echo.Echo";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis-test-fixture.echo@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    impl trellis_rs::generated::RpcDescriptor for Echo {
        type Input = EchoInput;
        type Output = EchoOutput;
        type Error = std::convert::Infallible;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            let _ = value;
            Ok(None)
        }
    }
}
pub mod operations {
    pub type SilentInput = crate::__types::trellis_test_fixture::Value;
    pub type SilentOutput = crate::__types::trellis_test_fixture::Value;
    pub struct Silent;
    impl Silent {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.Silent";
        pub const KEY: &'static str = "echo.Silent";
        pub const SUBJECT: &'static str = "operations.v1.echo.Silent";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis-test-fixture.echo@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for Silent {
        type Input = SilentInput;
        type Output = SilentOutput;
        type Progress = serde_json::Value;
        type Update = serde_json::Value;
        type UpdateEvidence = trellis_rs::client::NoOperationUpdates;
        type Error = std::convert::Infallible;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const ERRORS: &'static [&'static str] = &[];
        const SIGNALS: &'static [&'static str] = &[];
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{}";
        const UPLOAD: bool = Self::UPLOAD;
        const HAS_PROGRESS: bool = false;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = None;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            let _ = value;
            Ok(None)
        }
    }
    pub type UpdateOnlyInput = crate::__types::trellis_test_fixture::Value;
    pub type UpdateOnlyOutput = crate::__types::trellis_test_fixture::Value;
    pub type UpdateOnlyUpdate = crate::__types::trellis_test_fixture::Preview;
    pub type UpdateOnlyContinueSignalInput = crate::__types::trellis_test_fixture::Value;
    pub struct UpdateOnly;
    impl UpdateOnly {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.UpdateOnly";
        pub const KEY: &'static str = "echo.UpdateOnly";
        pub const SUBJECT: &'static str = "operations.v1.echo.UpdateOnly";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis-test-fixture.echo@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for UpdateOnly {
        type Input = UpdateOnlyInput;
        type Output = UpdateOnlyOutput;
        type Progress = serde_json::Value;
        type Update = UpdateOnlyUpdate;
        type UpdateEvidence = trellis_rs::client::DeclaredOperationUpdates;
        type Error = std::convert::Infallible;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const ERRORS: &'static [&'static str] = &[];
        const SIGNALS: &'static [&'static str] = &["Continue"];
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{\"Continue\":{\"$defs\":{\"trellis-test-fixture.Value\":{\"additionalProperties\":true,\"properties\":{\"value\":{\"type\":\"string\"}},\"required\":[\"value\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/trellis-test-fixture.Value\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}}";
        const UPLOAD: bool = Self::UPLOAD;
        const HAS_PROGRESS: bool = false;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some(
            "{\"$defs\":{\"trellis-test-fixture.Preview\":{\"additionalProperties\":true,\"properties\":{\"text\":{\"type\":\"string\"}},\"required\":[\"text\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/trellis-test-fixture.Preview\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}",
        );
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            let _ = value;
            Ok(None)
        }
    }
    pub struct UpdateOnlyContinueSignal;
    impl trellis_rs::generated::OperationSignal for UpdateOnlyContinueSignal {
        type Operation = UpdateOnly;
        type Input = UpdateOnlyContinueSignalInput;
        const NAME: &'static str = "Continue";
    }
    pub type UploadInput = crate::__types::trellis_test_fixture::Value;
    pub type UploadOutput = crate::__types::trellis_test_fixture::Value;
    pub struct Upload;
    impl Upload {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.Upload";
        pub const KEY: &'static str = "echo.Upload";
        pub const SUBJECT: &'static str = "operations.v1.echo.Upload";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis-test-fixture.echo@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = true;
    }
    impl trellis_rs::generated::OperationDescriptor for Upload {
        type Input = UploadInput;
        type Output = UploadOutput;
        type Progress = serde_json::Value;
        type Update = serde_json::Value;
        type UpdateEvidence = trellis_rs::client::NoOperationUpdates;
        type Error = std::convert::Infallible;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const ERRORS: &'static [&'static str] = &[];
        const SIGNALS: &'static [&'static str] = &[];
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{}";
        const UPLOAD: bool = Self::UPLOAD;
        const HAS_PROGRESS: bool = false;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = None;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            let _ = value;
            Ok(None)
        }
    }
    pub type WorkInput = crate::__types::trellis_test_fixture::Value;
    pub type WorkOutput = crate::__types::trellis_test_fixture::Value;
    pub type WorkProgress = crate::__types::trellis_test_fixture::Status;
    pub type WorkUpdate = crate::__types::trellis_test_fixture::Preview;
    pub type WorkContinueSignalInput = crate::__types::trellis_test_fixture::Value;
    pub struct Work;
    impl Work {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.Work";
        pub const KEY: &'static str = "echo.Work";
        pub const SUBJECT: &'static str = "operations.v1.echo.Work";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis-test-fixture.echo@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for Work {
        type Input = WorkInput;
        type Output = WorkOutput;
        type Progress = WorkProgress;
        type Update = WorkUpdate;
        type UpdateEvidence = trellis_rs::client::DeclaredOperationUpdates;
        type Error = std::convert::Infallible;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const ERRORS: &'static [&'static str] = &[];
        const SIGNALS: &'static [&'static str] = &["Continue"];
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{\"Continue\":{\"$defs\":{\"trellis-test-fixture.Value\":{\"additionalProperties\":true,\"properties\":{\"value\":{\"type\":\"string\"}},\"required\":[\"value\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/trellis-test-fixture.Value\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}}";
        const UPLOAD: bool = Self::UPLOAD;
        const HAS_PROGRESS: bool = true;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some(
            "{\"$defs\":{\"trellis-test-fixture.Preview\":{\"additionalProperties\":true,\"properties\":{\"text\":{\"type\":\"string\"}},\"required\":[\"text\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/trellis-test-fixture.Preview\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}",
        );
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            let _ = value;
            Ok(None)
        }
    }
    pub struct WorkContinueSignal;
    impl trellis_rs::generated::OperationSignal for WorkContinueSignal {
        type Operation = Work;
        type Input = WorkContinueSignalInput;
        const NAME: &'static str = "Continue";
    }
}
pub mod events {
    pub type ObservedEvent = crate::__types::trellis_test_fixture::Value;
    pub struct Observed;
    impl Observed {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Observed";
        pub const KEY: &'static str = "echo.Observed";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy10ZXN0LWZpeHR1cmUuZWNob0B2MQ.Observed";
        pub const SUBSCRIBE_SUBJECT: &'static str = "events.v1.dHJlbGxpcy10ZXN0LWZpeHR1cmUuZWNob0B2MQ.Observed";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[
            "trellis-test-fixture.echo@v1::public",
        ];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &[
            "trellis-test-fixture.echo@v1::public",
        ];
    }
    impl trellis_rs::generated::EventDescriptor for Observed {
        type Event = ObservedEvent;
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
pub mod lives {}
/// Registers metadata for every RPC in this API.
pub fn register_rpc_metadata(router: &mut trellis_rs::service::Router) {
    let _ = router;
    router.register_rpc_metadata::<rpc::Echo>();
}
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self { inner }
    }
    pub async fn echo(
        &self,
        input: &rpc::EchoInput,
    ) -> Result<
        rpc::EchoOutput,
        trellis_rs::client::CallError<std::convert::Infallible>,
    > {
        self.inner.call::<rpc::Echo>(input).await
    }
    pub fn silent(&self) -> trellis_rs::generated::Operation<'_, operations::Silent> {
        self.inner.operation::<operations::Silent>()
    }
    pub fn update_only(
        &self,
    ) -> trellis_rs::generated::Operation<'_, operations::UpdateOnly> {
        self.inner.operation::<operations::UpdateOnly>()
    }
    pub fn upload(&self) -> trellis_rs::generated::Operation<'_, operations::Upload> {
        self.inner.operation::<operations::Upload>()
    }
    pub fn work(&self) -> trellis_rs::generated::Operation<'_, operations::Work> {
        self.inner.operation::<operations::Work>()
    }
    pub async fn publish_observed(
        &self,
        event: &events::ObservedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::Observed>(event).await
    }
    pub async fn subscribe_observed(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::ObservedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner.subscribe::<events::Observed>(options).await
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
    pub fn register_echo<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::EchoInput) -> Fut + Send
            + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::EchoOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::Echo, _, _>(handler);
    }
    pub fn register_silent<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::SilentInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<operations::Silent>,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::Silent>,
                F,
                Fut,
            >(handler);
    }
    pub fn register_update_only<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::UpdateOnlyInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<operations::UpdateOnly>,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::UpdateOnly>,
                F,
                Fut,
            >(handler);
    }
    pub fn register_upload<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::UploadInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<operations::Upload>,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::Upload>,
                F,
                Fut,
            >(handler);
    }
    pub fn register_work<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::WorkInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<operations::Work>,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::Work>,
                F,
                Fut,
            >(handler);
    }
}
