//! Generated application request, not a deployed participant.
/// Source-owned application request metadata.
pub struct Request;
impl trellis_rs::generated::AppRequestDescriptor for Request {
    const ID: &'static str = "performance-trellis.Caller";
    const KIND: trellis_rs::generated::AppKind = trellis_rs::generated::AppKind::Browser;
    const REQUIRED_CAPABILITIES: &'static [&'static str] = &[
        "performance-trellis.performance@v1::workloads",
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
    pub async fn download_transfer(
        &self,
        grant: &trellis_rs::client::DownloadTransferGrant,
    ) -> Result<Vec<u8>, trellis_rs::client::TrellisClientError> {
        self.inner.download_transfer(grant).await
    }
    pub fn performance_trellis_performance_v1(
        &self,
    ) -> performance_trellis_performance_v1::Client {
        performance_trellis_performance_v1::Client::from_generated(self.inner.clone())
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
                crate::__types::performance_trellis::Value,
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
                        schema: "{\"$defs\":{\"performance-trellis.Value\":{\"additionalProperties\":true,\"properties\":{\"value\":{\"type\":\"string\"}},\"required\":[\"value\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/performance-trellis.Value\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}"
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
pub mod performance_trellis_performance_v1 {
    use crate::apis::performance_trellis_performance_v1::rpc;
    use crate::apis::performance_trellis_performance_v1::operations;
    use crate::apis::performance_trellis_performance_v1::events;
    use crate::apis::performance_trellis_performance_v1::lives;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn download(
            &self,
            input: &rpc::DownloadInput,
        ) -> Result<
            rpc::DownloadOutput,
            trellis_rs::client::CallError<std::convert::Infallible>,
        > {
            self.inner.call::<rpc::Download>(input).await
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
        pub async fn put_record(
            &self,
            input: &rpc::PutRecordInput,
        ) -> Result<
            rpc::PutRecordOutput,
            trellis_rs::client::CallError<std::convert::Infallible>,
        > {
            self.inner.call::<rpc::PutRecord>(input).await
        }
        pub async fn read_record(
            &self,
            input: &rpc::ReadRecordInput,
        ) -> Result<
            rpc::ReadRecordOutput,
            trellis_rs::client::CallError<std::convert::Infallible>,
        > {
            self.inner.call::<rpc::ReadRecord>(input).await
        }
        pub fn await_cancellation(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::AwaitCancellation> {
            self.inner.operation::<operations::AwaitCancellation>()
        }
        pub fn await_object(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::AwaitObject> {
            self.inner.operation::<operations::AwaitObject>()
        }
        pub fn queue_keyed(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::QueueKeyed> {
            self.inner.operation::<operations::QueueKeyed>()
        }
        pub fn queue_work(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::QueueWork> {
            self.inner.operation::<operations::QueueWork>()
        }
        pub fn upload(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::Upload> {
            self.inner.operation::<operations::Upload>()
        }
        pub fn work(&self) -> trellis_rs::generated::Operation<'_, operations::Work> {
            self.inner.operation::<operations::Work>()
        }
        pub async fn publish_changed(
            &self,
            event: &events::ChangedEvent,
        ) -> Result<(), trellis_rs::client::TrellisClientError> {
            self.inner.publish::<events::Changed>(event).await
        }
        pub async fn subscribe_changed(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<events::ChangedEvent, trellis_rs::client::TrellisClientError>,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner.subscribe::<events::Changed>(options).await
        }
        pub async fn subscribe_delivered(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<events::DeliveredEvent, trellis_rs::client::TrellisClientError>,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner.subscribe::<events::Delivered>(options).await
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
    pub use crate::__types::performance_trellis::Value;
}
/// Local State value encoding; does not allocate an owned resource.
pub type SavedState = crate::__types::performance_trellis::Value;
pub const SAVED_VERSION: u32 = 1;
