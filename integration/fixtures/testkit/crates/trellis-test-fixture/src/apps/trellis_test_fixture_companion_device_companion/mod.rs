//! Generated application request, not a deployed participant.
/// Source-owned application request metadata.
pub struct Request;
impl trellis_rs::generated::AppRequestDescriptor for Request {
    const ID: &'static str = "trellis-test-fixture.CompanionDevice.Companion";
    const KIND: trellis_rs::generated::AppKind = trellis_rs::generated::AppKind::Native;
    const REQUIRED_CAPABILITIES: &'static [&'static str] = &[
        "trellis-test-fixture.echo@v1::invoke",
        "trellis.auth@v1::deviceCompanionsManage",
        "trellis.state@v1::delete",
        "trellis.state@v1::read",
        "trellis.state@v1::write",
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
    pub fn trellis_state_v1(&self) -> trellis_state_v1::Client {
        trellis_state_v1::Client::from_generated(self.inner.clone())
    }
    /// Register the authored State representation with explicit CAS, then open its typed handle. Pass revision `0` for initial registration; conflicts are never retried.
    pub async fn saved(
        &self,
        expected_revision: &str,
        idempotency_key: trellis_rs::generated::Ulid,
    ) -> Result<
        (
            trellis_rs::client::ConnectedStateHandle<
                crate::__types::trellis_test_fixture::Value,
                crate::apis::trellis_state_v1::lives::Watch,
            >,
            crate::apis::trellis_state_v1::rpc::RegisterOutput,
        ),
        trellis_rs::client::CallError<crate::apis::trellis_state_v1::rpc::RegisterError>,
    > {
        let registration = self
            .inner
            .call::<
                crate::apis::trellis_state_v1::rpc::Register,
            >(
                &crate::apis::trellis_state_v1::rpc::RegisterInput {
                    resource_name: "saved".to_owned().into(),
                    expected_revision: expected_revision.to_owned().into(),
                    idempotency_key,
                    representation: crate::__types::trellis::StateRepresentation {
                        version: 1.into(),
                        schema: "{\"$defs\":{\"trellis-test-fixture.Value\":{\"additionalProperties\":true,\"properties\":{\"value\":{\"type\":\"string\"}},\"required\":[\"value\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/trellis-test-fixture.Value\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}"
                            .as_bytes()
                            .to_vec()
                            .into(),
                        extra: Default::default(),
                    },
                    accepts: vec![],
                    extra: Default::default(),
                },
            )
            .await?;
        Ok((
            self
                .inner
                .registered_state_handle(
                    "saved",
                    trellis_rs::client::ResourceCodec::new(1).expect("generated version"),
                ),
            registration,
        ))
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
pub mod trellis_state_v1 {
    use crate::apis::trellis_state_v1::rpc;
    use crate::apis::trellis_state_v1::lives;
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
        pub async fn register(
            &self,
            input: &rpc::RegisterInput,
        ) -> Result<
            rpc::RegisterOutput,
            trellis_rs::client::CallError<rpc::RegisterError>,
        > {
            self.inner.call::<rpc::Register>(input).await
        }
        pub async fn watch(
            &self,
            input: &lives::WatchInput,
        ) -> Result<
            trellis_rs::LiveSubscription<lives::WatchEvent>,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner.live::<lives::Watch>(input).await
        }
    }
}
pub mod types {
    //! Generated wire type exports.
    pub use crate::__types::trellis_test_fixture::Value;
}
/// Local State value encoding; does not allocate an owned resource.
pub type SavedState = crate::__types::trellis_test_fixture::Value;
pub const SAVED_VERSION: u32 = 1;
