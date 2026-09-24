//! Generated API `trellis-test-fixture.echo@v1`.
pub const API_ID: &str = "trellis-test-fixture.echo@v1";
pub const API_DIGEST: &str = "FCaUPdkGVfSxGfAwGJdgOZJ9a-RM59m27NDtXFS-CQQ";
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
pub mod operations {}
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
}
