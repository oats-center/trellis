//! Generated application request, not a deployed participant.
/// Source-owned application request metadata.
pub struct Request;
impl trellis_rs::generated::AppRequestDescriptor for Request {
    const ID: &'static str = "trellis-test-fixture.OptionalCompanionDevice.Companion";
    const KIND: trellis_rs::generated::AppKind = trellis_rs::generated::AppKind::Native;
    const REQUIRED_CAPABILITIES: &'static [&'static str] = &[
        "trellis-test-fixture.echo@v1::invoke",
        "trellis.auth@v1::deviceCompanionsManage",
    ];
    const OPTIONAL_CAPABILITIES: &'static [&'static str] = &[];
}
const OPTIONAL_ACTIONS: &[trellis_rs::generated::OptionalAction] = &[];
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self {
            inner: inner.with_optional_actions(OPTIONAL_ACTIONS),
        }
    }
    pub fn trellis_test_fixture_echo_v1(&self) -> trellis_test_fixture_echo_v1::Client {
        trellis_test_fixture_echo_v1::Client::from_generated(self.inner.clone())
    }
    pub fn trellis_auth_v1(&self) -> trellis_auth_v1::Client {
        trellis_auth_v1::Client::from_generated(self.inner.clone())
    }
}
pub mod trellis_test_fixture_echo_v1 {
    use crate::apis::trellis_test_fixture_echo_v1::rpc;
    use crate::apis::trellis_test_fixture_echo_v1::operations;
    use crate::apis::trellis_test_fixture_echo_v1::events;
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
        pub fn silent(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::Silent> {
            self.inner.operation::<operations::Silent>()
        }
        pub fn update_only(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::UpdateOnly> {
            self.inner.operation::<operations::UpdateOnly>()
        }
        pub fn work(&self) -> trellis_rs::generated::Operation<'_, operations::Work> {
            self.inner.operation::<operations::Work>()
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
}
pub mod trellis_auth_v1 {
    use crate::apis::trellis_auth_v1::rpc;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn device_companions_attach(
            &self,
            input: &rpc::DeviceCompanionsAttachInput,
        ) -> Result<
            rpc::DeviceCompanionsAttachOutput,
            trellis_rs::client::CallError<rpc::DeviceCompanionsAttachError>,
        > {
            self.inner.call::<rpc::DeviceCompanionsAttach>(input).await
        }
        pub async fn device_companions_revoke(
            &self,
            input: &rpc::DeviceCompanionsRevokeInput,
        ) -> Result<
            rpc::DeviceCompanionsRevokeOutput,
            trellis_rs::client::CallError<rpc::DeviceCompanionsRevokeError>,
        > {
            self.inner.call::<rpc::DeviceCompanionsRevoke>(input).await
        }
        pub async fn devices_reviews_get(
            &self,
            input: &rpc::DevicesReviewsGetInput,
        ) -> Result<
            rpc::DevicesReviewsGetOutput,
            trellis_rs::client::CallError<rpc::DevicesReviewsGetError>,
        > {
            self.inner.call::<rpc::DevicesReviewsGet>(input).await
        }
    }
}
pub mod types {
    //! Generated wire type exports.
}
