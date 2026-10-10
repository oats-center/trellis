//! Generated API `performance-trellis.performance@v1`.
pub const API_ID: &str = "performance-trellis.performance@v1";
pub const API_DIGEST: &str = "rybfbxiawk21ZrWzh38IOO5J7lbpaJA9KYE2PiWq5fw";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {}
pub mod rpc {
    pub type DownloadInput = crate::__types::performance_trellis::Value;
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct DownloadOutput {
        #[serde(flatten)]
        pub response: crate::__types::performance_trellis::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub transfer: Option<trellis_rs::client::DownloadTransferGrant>,
    }
    pub struct Download;
    impl Download {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Download";
        pub const KEY: &'static str = "performance.Download";
        pub const SUBJECT: &'static str = "rpc.v1.performance.Download";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const DOWNLOAD: bool = true;
        pub const CURSOR_PAGINATION: bool = false;
    }
    impl trellis_rs::generated::RpcDescriptor for Download {
        type Input = DownloadInput;
        type Output = DownloadOutput;
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
    pub type EchoInput = crate::__types::performance_trellis::Value;
    pub type EchoOutput = crate::__types::performance_trellis::Value;
    pub struct Echo;
    impl Echo {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Echo";
        pub const KEY: &'static str = "performance.Echo";
        pub const SUBJECT: &'static str = "rpc.v1.performance.Echo";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
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
    pub type PutRecordInput = crate::__types::performance_trellis::Value;
    pub type PutRecordOutput = crate::__types::performance_trellis::Value;
    pub struct PutRecord;
    impl PutRecord {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.PutRecord";
        pub const KEY: &'static str = "performance.PutRecord";
        pub const SUBJECT: &'static str = "rpc.v1.performance.PutRecord";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    impl trellis_rs::generated::RpcDescriptor for PutRecord {
        type Input = PutRecordInput;
        type Output = PutRecordOutput;
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
    pub type ReadRecordInput = crate::__types::performance_trellis::Value;
    pub type ReadRecordOutput = crate::__types::performance_trellis::Value;
    pub struct ReadRecord;
    impl ReadRecord {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ReadRecord";
        pub const KEY: &'static str = "performance.ReadRecord";
        pub const SUBJECT: &'static str = "rpc.v1.performance.ReadRecord";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    impl trellis_rs::generated::RpcDescriptor for ReadRecord {
        type Input = ReadRecordInput;
        type Output = ReadRecordOutput;
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
    pub type AwaitCancellationInput = crate::__types::performance_trellis::Value;
    pub type AwaitCancellationOutput = crate::__types::performance_trellis::Value;
    pub type AwaitCancellationProgress = crate::__types::performance_trellis::Value;
    pub struct AwaitCancellation;
    impl AwaitCancellation {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.AwaitCancellation";
        pub const KEY: &'static str = "performance.AwaitCancellation";
        pub const SUBJECT: &'static str = "operations.v1.performance.AwaitCancellation";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for AwaitCancellation {
        type Input = AwaitCancellationInput;
        type Output = AwaitCancellationOutput;
        type Progress = AwaitCancellationProgress;
        type Update = AwaitCancellationProgress;
        type UpdateEvidence = trellis_rs::client::DeclaredOperationUpdates;
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
        const HAS_PROGRESS: bool = true;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some(
            "{\"$defs\":{\"performance-trellis.Value\":{\"additionalProperties\":true,\"properties\":{\"value\":{\"type\":\"string\"}},\"required\":[\"value\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/performance-trellis.Value\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}",
        );
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            let _ = value;
            Ok(None)
        }
    }
    pub type AwaitObjectInput = crate::__types::performance_trellis::Value;
    pub type AwaitObjectOutput = crate::__types::performance_trellis::Value;
    pub struct AwaitObject;
    impl AwaitObject {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.AwaitObject";
        pub const KEY: &'static str = "performance.AwaitObject";
        pub const SUBJECT: &'static str = "operations.v1.performance.AwaitObject";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for AwaitObject {
        type Input = AwaitObjectInput;
        type Output = AwaitObjectOutput;
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
    pub type QueueKeyedInput = crate::__types::performance_trellis::KeyedValue;
    pub type QueueKeyedOutput = crate::__types::performance_trellis::KeyedValue;
    pub struct QueueKeyed;
    impl QueueKeyed {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.QueueKeyed";
        pub const KEY: &'static str = "performance.QueueKeyed";
        pub const SUBJECT: &'static str = "operations.v1.performance.QueueKeyed";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for QueueKeyed {
        type Input = QueueKeyedInput;
        type Output = QueueKeyedOutput;
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
    pub type QueueWorkInput = crate::__types::performance_trellis::Value;
    pub type QueueWorkOutput = crate::__types::performance_trellis::Value;
    pub struct QueueWork;
    impl QueueWork {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.QueueWork";
        pub const KEY: &'static str = "performance.QueueWork";
        pub const SUBJECT: &'static str = "operations.v1.performance.QueueWork";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for QueueWork {
        type Input = QueueWorkInput;
        type Output = QueueWorkOutput;
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
    pub type UploadInput = crate::__types::performance_trellis::Value;
    pub type UploadOutput = crate::__types::performance_trellis::Value;
    pub struct Upload;
    impl Upload {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.Upload";
        pub const KEY: &'static str = "performance.Upload";
        pub const SUBJECT: &'static str = "operations.v1.performance.Upload";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
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
    pub type WorkInput = crate::__types::performance_trellis::Value;
    pub type WorkOutput = crate::__types::performance_trellis::Value;
    pub type WorkProgress = crate::__types::performance_trellis::Value;
    pub struct Work;
    impl Work {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.Work";
        pub const KEY: &'static str = "performance.Work";
        pub const SUBJECT: &'static str = "operations.v1.performance.Work";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const ERRORS: &'static [&'static str] = &[];
        pub const UPLOAD: bool = false;
    }
    impl trellis_rs::generated::OperationDescriptor for Work {
        type Input = WorkInput;
        type Output = WorkOutput;
        type Progress = WorkProgress;
        type Update = WorkProgress;
        type UpdateEvidence = trellis_rs::client::DeclaredOperationUpdates;
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
        const HAS_PROGRESS: bool = true;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some(
            "{\"$defs\":{\"performance-trellis.Value\":{\"additionalProperties\":true,\"properties\":{\"value\":{\"type\":\"string\"}},\"required\":[\"value\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/performance-trellis.Value\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}",
        );
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            let _ = value;
            Ok(None)
        }
    }
}
pub mod events {
    pub type ChangedEvent = crate::__types::performance_trellis::Value;
    pub struct Changed;
    impl Changed {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Changed";
        pub const KEY: &'static str = "performance.Changed";
        pub const SUBJECT: &'static str = "events.v1.cGVyZm9ybWFuY2UtdHJlbGxpcy5wZXJmb3JtYW5jZUB2MQ.Changed";
        pub const SUBSCRIBE_SUBJECT: &'static str = "events.v1.cGVyZm9ybWFuY2UtdHJlbGxpcy5wZXJmb3JtYW5jZUB2MQ.Changed";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
    }
    impl trellis_rs::generated::EventDescriptor for Changed {
        type Event = ChangedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type DeliveredEvent = crate::__types::performance_trellis::Value;
    pub struct Delivered;
    impl Delivered {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Delivered";
        pub const KEY: &'static str = "performance.Delivered";
        pub const SUBJECT: &'static str = "events.v1.cGVyZm9ybWFuY2UtdHJlbGxpcy5wZXJmb3JtYW5jZUB2MQ.Delivered";
        pub const SUBSCRIBE_SUBJECT: &'static str = "events.v1.cGVyZm9ybWFuY2UtdHJlbGxpcy5wZXJmb3JtYW5jZUB2MQ.Delivered";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::publishDelivered",
        ];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
    }
    impl trellis_rs::generated::EventDescriptor for Delivered {
        type Event = DeliveredEvent;
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
pub mod lives {
    pub type WatchInput = crate::__types::performance_trellis::Value;
    pub type WatchEvent = crate::__types::performance_trellis::Value;
    pub struct Watch;
    impl Watch {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "live.Watch";
        pub const KEY: &'static str = "performance.Watch";
        pub const SUBJECT: &'static str = "live.v1.performance.Watch";
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &[
            "performance-trellis.performance@v1::workloads",
        ];
    }
    impl trellis_rs::generated::LiveDescriptor for Watch {
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
    router.register_rpc_metadata::<rpc::Download>();
    router.register_rpc_metadata::<rpc::Echo>();
    router.register_rpc_metadata::<rpc::PutRecord>();
    router.register_rpc_metadata::<rpc::ReadRecord>();
}
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
    pub fn upload(&self) -> trellis_rs::generated::Operation<'_, operations::Upload> {
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
    pub async fn publish_delivered(
        &self,
        event: &events::DeliveredEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::Delivered>(event).await
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
pub struct Provider<'a, P> {
    runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
}
impl<'a, P: trellis_rs::generated::ParticipantDescriptor> Provider<'a, P> {
    pub fn new(
        runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
    ) -> Self {
        Self { runtime }
    }
    pub fn register_download<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DownloadInput) -> Fut
            + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DownloadOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::Download, _, _>(handler);
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
    pub fn register_put_record<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PutRecordInput) -> Fut
            + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PutRecordOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::PutRecord, _, _>(handler);
    }
    pub fn register_read_record<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ReadRecordInput) -> Fut
            + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ReadRecordOutput>,
            > + Send + 'static,
    {
        self.runtime.register_rpc::<rpc::ReadRecord, _, _>(handler);
    }
    pub fn register_await_cancellation<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::AwaitCancellationInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<
                        operations::AwaitCancellation,
                    >,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::AwaitCancellation>,
                F,
                Fut,
            >(handler);
    }
    pub fn register_await_object<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::AwaitObjectInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<operations::AwaitObject>,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::AwaitObject>,
                F,
                Fut,
            >(handler);
    }
    pub fn register_queue_keyed<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::QueueKeyedInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<operations::QueueKeyed>,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::QueueKeyed>,
                F,
                Fut,
            >(handler);
    }
    pub fn register_queue_work<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::QueueWorkInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<operations::QueueWork>,
                >,
            ) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<operations::QueueWork>,
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
    pub fn register_watch<F, S>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceLiveHandlerContext, lives::WatchInput) -> S
            + Send + Sync + 'static,
        S: futures_util::Stream<
                Item = Result<lives::WatchEvent, trellis_rs::service::ServerError>,
            > + Send + 'static,
    {
        self.runtime.register_live::<lives::Watch, _, _>(handler);
    }
}
